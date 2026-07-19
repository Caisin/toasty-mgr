#![allow(missing_docs)]

use std::{path::Path, sync::Arc};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use url::Url;

use crate::{Executor, Model};

/// Resolves a password stored in [`BaseDs`] before a connection URL is built.
///
/// By default passwords are treated as plaintext. Applications that store
/// encrypted or secret-manager references can install a resolver through
/// [`crate::TcMgr::set_password_resolver`].
#[async_trait]
pub trait PasswordResolver: Send + Sync + 'static {
    async fn resolve(&self, stored: &str) -> Result<String>;
}

static PASSWORD_RESOLVER: std::sync::OnceLock<
    std::sync::RwLock<Option<Arc<dyn PasswordResolver>>>,
> = std::sync::OnceLock::new();

fn password_resolver() -> &'static std::sync::RwLock<Option<Arc<dyn PasswordResolver>>> {
    PASSWORD_RESOLVER.get_or_init(|| std::sync::RwLock::new(None))
}

pub(crate) fn set_password_resolver(resolver: Option<Arc<dyn PasswordResolver>>) {
    *password_resolver()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = resolver;
}

fn current_password_resolver() -> Option<Arc<dyn PasswordResolver>> {
    password_resolver()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Built-in Toasty model used by the `base` data source as its connection catalog.
#[derive(Debug, Clone, Default, Model)]
#[table = "base_ds"]
pub struct BaseDs {
    #[key]
    pub ds_code: String,
    pub name: String,
    pub user_name: String,
    pub pwd: String,
    pub db_host: String,
    pub db_name: String,
    pub cur_schema: String,
    pub time_zone: String,
    pub port: i32,
    pub max_con: Option<i32>,
    pub min_con: Option<i32>,
    pub connect_timeout: Option<i64>,
    pub acquire_timeout: Option<i64>,
    pub idle_timeout: Option<i64>,
    pub db_type: String,
    #[index]
    pub state: bool,
    pub remark: String,
    pub create_at: i64,
}

impl BaseDs {
    /// Return the configured schema, defaulting to PostgreSQL's `public` schema.
    pub fn effective_schema(&self) -> &str {
        if self.cur_schema.is_empty() {
            "public"
        } else {
            &self.cur_schema
        }
    }

    /// Resolve the stored password through the application hook, when installed.
    pub async fn get_pwd(&self) -> Result<String> {
        match current_password_resolver() {
            Some(resolver) => resolver.resolve(&self.pwd).await,
            None => Ok(self.pwd.clone()),
        }
    }

    /// Return the configured timezone, defaulting to UTC+8 for source compatibility.
    pub fn effective_timezone(&self) -> &str {
        if self.time_zone.is_empty() {
            "+08:00"
        } else {
            &self.time_zone
        }
    }

    /// Return an explicit port or the driver's conventional default.
    pub fn effective_port(&self) -> u16 {
        if let Ok(port) = u16::try_from(self.port)
            && port > 0
        {
            return port;
        }

        match self.db_type.as_str() {
            "mysql" => 3306,
            "postgres" | "postgresql" => 5432,
            _ => 0,
        }
    }

    /// Build the Toasty connection URL represented by this catalog row.
    pub async fn toasty_url(&self) -> Result<Url> {
        if matches!(self.db_type.as_str(), "sqlite" | "turso") {
            return self.local_url(&self.db_type);
        }

        let scheme = match self.db_type.as_str() {
            "postgres" => "postgresql",
            value => value,
        };
        let mut url = Url::parse(&format!("{scheme}://{}", self.db_host))?;
        url.set_path(&self.db_name);

        if scheme == "postgresql" {
            url.query_pairs_mut().append_pair("sslmode", "disable");
        }

        let port = self.effective_port();
        if port > 0 {
            url.set_port(Some(port))
                .map_err(|_| anyhow!("invalid port for data source [{}]", self.ds_code))?;
        }

        let password = self.get_pwd().await?;
        if !password.is_empty() {
            url.set_password(Some(&password))
                .map_err(|_| anyhow!("invalid password for data source [{}]", self.ds_code))?;
        }
        if !self.user_name.is_empty() {
            url.set_username(&self.user_name)
                .map_err(|_| anyhow!("invalid username for data source [{}]", self.ds_code))?;
        }
        Ok(url)
    }

    fn local_url(&self, scheme: &str) -> Result<Url> {
        let path = Path::new(&self.db_host).join(&self.db_name);
        let path = path.to_string_lossy();
        if path == ":memory:" {
            return Ok(Url::parse(&format!("{scheme}::memory:"))?);
        }
        Ok(Url::parse(&format!("{scheme}:{path}"))?)
    }

    pub async fn get_ds<E>(db: &mut E, code: &str) -> Result<Self>
    where
        E: Executor,
    {
        Ok(Self::get_by_ds_code(db, code).await?)
    }

    pub async fn get_active_ds<E>(db: &mut E, code: &str) -> Result<Self>
    where
        E: Executor,
    {
        let value = Self::get_ds(db, code).await?;
        if !value.state {
            bail!("data source [{code}] disabled");
        }
        Ok(value)
    }

    pub async fn all_active<E>(db: &mut E) -> Result<Vec<Self>>
    where
        E: Executor,
    {
        Ok(Self::filter_by_state(true).exec(db).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(db_type: &str, port: i32) -> BaseDs {
        BaseDs {
            ds_code: "base".into(),
            name: "base".into(),
            user_name: "user".into(),
            pwd: "pwd".into(),
            db_host: "127.0.0.1".into(),
            db_name: "demo".into(),
            port,
            db_type: db_type.into(),
            state: true,
            ..Default::default()
        }
    }

    #[test]
    fn default_ports_follow_driver_conventions() {
        assert_eq!(ds("mysql", 0).effective_port(), 3306);
        assert_eq!(ds("postgres", 0).effective_port(), 5432);
        assert_eq!(ds("postgresql", 0).effective_port(), 5432);
        assert_eq!(ds("sqlite", 0).effective_port(), 0);
    }

    #[tokio::test]
    async fn postgres_url_is_compatible_with_builtin_driver() {
        let url = ds("postgres", 0).toasty_url().await.unwrap();

        assert_eq!(url.scheme(), "postgresql");
        assert_eq!(url.port(), Some(5432));
        assert_eq!(url.path(), "/demo");
        assert_eq!(url.query(), Some("sslmode=disable"));
    }

    #[tokio::test]
    async fn sqlite_memory_url_is_supported() {
        let mut value = ds("sqlite", 0);
        value.db_host.clear();
        value.db_name = ":memory:".into();

        assert_eq!(
            value.toasty_url().await.unwrap().as_str(),
            "sqlite::memory:"
        );
    }

    #[tokio::test]
    async fn turso_memory_url_is_supported() {
        let mut value = ds("turso", 0);
        value.db_host.clear();
        value.db_name = ":memory:".into();

        assert_eq!(value.toasty_url().await.unwrap().as_str(), "turso::memory:");
    }
}
