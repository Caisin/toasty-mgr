use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, OnceLock, RwLock},
    time::Duration,
};

use anyhow::{Result as AnyResult, anyhow};
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    BaseDs, Db, Executor, ModelSet, Transaction,
    codegen_support::core::{
        Schema,
        driver::{Capability, ExecResponse, operation::RawSql},
        stmt,
    },
};

/// A registered Toasty connection and its management metadata.
#[derive(Clone)]
pub struct TcConn {
    inner: Db,
    meta: TcConnMeta,
    connect_url: Option<String>,
    pool_options: TcPoolOptions,
}

impl fmt::Debug for TcConn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcConn")
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Executor for TcConn {
    async fn transaction(&mut self) -> crate::Result<Transaction<'_>> {
        self.inner.transaction().await
    }

    async fn exec_untyped(&mut self, stmt: stmt::Statement) -> crate::Result<ExecResponse> {
        self.inner.exec_untyped(stmt).await
    }

    async fn exec_raw_sql(&mut self, raw: RawSql) -> crate::Result<ExecResponse> {
        self.inner.exec_raw_sql(raw).await
    }

    fn capability(&mut self) -> &Capability {
        self.inner.capability()
    }

    fn schema(&mut self) -> &Arc<Schema> {
        self.inner.schema()
    }
}

impl TcConn {
    pub fn meta(&self) -> &TcConnMeta {
        &self.meta
    }

    pub fn ds_code(&self) -> &str {
        &self.meta.ds_code
    }

    pub fn ds_name(&self) -> &str {
        &self.meta.ds_name
    }

    pub fn db(&self) -> Db {
        self.inner.clone()
    }
}

/// Non-secret metadata describing a managed connection.
#[derive(Clone, Default)]
pub struct TcConnMeta {
    pub url: String,
    pub ds_code: String,
    pub ds_name: String,
    pub db_type: String,
    pub user_name: String,
    /// Never included by the [`Debug`] implementation or registry snapshots.
    pub password: String,
    pub host: String,
    pub port: u16,
    pub db_name: String,
}

impl fmt::Debug for TcConnMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcConnMeta")
            .field("url", &redact_url(&self.url))
            .field("ds_code", &self.ds_code)
            .field("ds_name", &self.ds_name)
            .field("db_type", &self.db_type)
            .field("user_name", &self.user_name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("db_name", &self.db_name)
            .finish_non_exhaustive()
    }
}

impl TcConnMeta {
    pub fn new(code: &str, name: &str, url: &str) -> Self {
        let mut meta = Self {
            url: url.to_owned(),
            ds_code: code.to_owned(),
            ds_name: name.to_owned(),
            ..Default::default()
        };

        if let Ok(value) = url::Url::parse(url) {
            meta.db_type = value.scheme().to_owned();
            meta.user_name = value.username().to_owned();
            meta.password = value.password().unwrap_or_default().to_owned();
            meta.host = value.host_str().unwrap_or_default().to_owned();
            meta.port = value.port().unwrap_or_default();
            meta.db_name = value.path().trim_start_matches('/').to_owned();
        }

        meta
    }

    fn redacted(&self) -> Self {
        let mut meta = self.clone();
        meta.url = redact_url(&meta.url);
        meta.password.clear();
        meta
    }
}

fn redact_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return String::new();
    };
    let _ = url.set_password(None);
    url.to_string()
}

#[derive(Debug, Clone, Default)]
struct TcPoolOptions {
    max_size: Option<usize>,
    create_timeout: Option<Duration>,
    wait_timeout: Option<Duration>,
    max_idle_time: Option<Duration>,
}

impl TcPoolOptions {
    fn from_ds(ds: &BaseDs) -> AnyResult<Self> {
        let max_size = match ds.max_con {
            Some(value) if value <= 0 => {
                return Err(anyhow!(
                    "data source [{}] max_con must be positive",
                    ds.ds_code
                ));
            }
            Some(value) => Some(
                usize::try_from(value)
                    .map_err(|_| anyhow!("data source [{}] max_con invalid", ds.ds_code))?,
            ),
            None => None,
        };

        Ok(Self {
            max_size,
            create_timeout: positive_timeout_secs(ds.connect_timeout),
            wait_timeout: positive_timeout_secs(ds.acquire_timeout),
            max_idle_time: positive_timeout_secs(ds.idle_timeout),
        })
    }

    fn apply(&self, builder: &mut crate::db::Builder) {
        if let Some(max_size) = self.max_size {
            builder.max_pool_size(max_size);
        }
        builder.pool_create_timeout(self.create_timeout);
        builder.pool_wait_timeout(self.wait_timeout);
        builder.pool_max_connection_idle_time(self.max_idle_time);
    }
}

static LOAD_LOCK: Mutex<()> = Mutex::const_new(());
static CONNECTIONS: OnceLock<RwLock<HashMap<String, TcConn>>> = OnceLock::new();
static MODEL_SETS: OnceLock<RwLock<HashMap<String, ModelSet>>> = OnceLock::new();
static ALIASES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn connections() -> &'static RwLock<HashMap<String, TcConn>> {
    CONNECTIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn model_sets() -> &'static RwLock<HashMap<String, ModelSet>> {
    MODEL_SETS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn aliases() -> &'static RwLock<HashMap<String, String>> {
    ALIASES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Registry of business aliases mapped to physical data-source codes.
pub struct TcDbAliases;

impl TcDbAliases {
    pub fn init(values: HashMap<String, String>) -> AnyResult<()> {
        Self::check_values(&values)?;
        *write(aliases()) = values;
        Ok(())
    }

    pub fn source_code(alias: &str) -> Option<String> {
        read(aliases()).get(alias).cloned()
    }

    pub fn all() -> HashMap<String, String> {
        read(aliases()).clone()
    }

    fn aliases_for_source(source_code: &str) -> Vec<String> {
        read(aliases())
            .iter()
            .filter_map(|(alias, source)| (source == source_code).then_some(alias.clone()))
            .collect()
    }

    fn check_values(values: &HashMap<String, String>) -> AnyResult<()> {
        let sources = values.values().collect::<HashSet<_>>();
        for source in sources {
            if values.contains_key(source) {
                return Err(anyhow!("alias source [{source}] is itself an alias"));
            }
        }
        Ok(())
    }
}

/// Registry of Toasty model sets keyed by data-source code.
pub struct TcModelSets;

impl TcModelSets {
    pub fn set(code: &str, models: ModelSet) {
        write(model_sets()).insert(code.to_owned(), models);
    }

    pub fn get(code: &str) -> AnyResult<ModelSet> {
        Self::get_opt(code).ok_or_else(|| anyhow!("models for data source [{code}] not set"))
    }

    pub fn get_opt(code: &str) -> Option<ModelSet> {
        read(model_sets())
            .get(code)
            .cloned()
            .or_else(|| (code == crate::BASE).then(|| crate::models!(BaseDs)))
    }

    pub fn remove(code: &str) {
        write(model_sets()).remove(code);
    }
}

/// Process-wide Toasty connection registry.
pub struct TcConnections;

impl TcConnections {
    pub async fn all_metas() -> HashMap<String, TcConnMeta> {
        let mut metas = read(connections())
            .iter()
            .map(|(code, con)| (code.clone(), con.meta.redacted()))
            .collect::<HashMap<_, _>>();

        for (alias, source_code) in TcDbAliases::all() {
            if metas.contains_key(&alias) {
                continue;
            }
            if let Some(source) = Self::get_item(&source_code) {
                metas.insert(alias.clone(), Self::alias_meta(&alias, &source).redacted());
            }
        }
        metas
    }

    pub async fn add_by_url(name: &str, url: &str) -> AnyResult<TcConn> {
        let models = TcModelSets::get(name)?;
        Self::add_by_url_with_models(name, url, models).await
    }

    pub async fn add_by_url_with_models(
        name: &str,
        url: &str,
        models: ModelSet,
    ) -> AnyResult<TcConn> {
        let pool_options = TcPoolOptions::default();
        let db = Self::connect_by_url(url, models.clone(), &pool_options).await?;
        let con = TcConn {
            inner: db,
            meta: TcConnMeta::new(name, name, url),
            connect_url: Some(url.to_owned()),
            pool_options,
        };
        Self::publish_source(con, models).await
    }

    pub async fn add_by_ds_model(model: &BaseDs) -> AnyResult<TcConn> {
        let models = TcModelSets::get(&model.ds_code)?;
        Self::add_by_ds_model_with_models(model, models).await
    }

    pub async fn add_by_ds_model_with_models(
        model: &BaseDs,
        models: ModelSet,
    ) -> AnyResult<TcConn> {
        let url = model.toasty_url().await?.to_string();
        let pool_options = TcPoolOptions::from_ds(model)?;
        let db = Self::connect_by_url(&url, models.clone(), &pool_options).await?;
        let con = TcConn {
            inner: db,
            meta: TcConnMeta::new(&model.ds_code, &model.name, &url),
            connect_url: Some(url),
            pool_options,
        };
        Self::publish_source(con, models).await
    }

    pub async fn add_alias_by_con(alias: &str, source: &TcConn) -> AnyResult<TcConn> {
        let con = Self::build_alias(alias, source).await?;
        Self::add_con(con.clone());
        Ok(con)
    }

    pub async fn init_aliases(values: &HashMap<String, String>) -> AnyResult<()> {
        for (alias, source_code) in values {
            if let Some(source) = Self::get_item(source_code) {
                if TcModelSets::get_opt(alias).is_some() {
                    Self::add_alias_by_con(alias, &source).await?;
                } else {
                    Self::remove(alias);
                }
                tracing::info!(alias, source_code, "initialized Toasty data-source alias");
            }
        }
        Ok(())
    }

    pub fn add_db(name: &str, db: Db) -> TcConn {
        let con = TcConn {
            inner: db,
            meta: TcConnMeta::new(name, name, ""),
            connect_url: None,
            pool_options: TcPoolOptions::default(),
        };
        Self::add_con(con.clone());
        con
    }

    pub fn add_con(con: TcConn) {
        write(connections()).insert(con.ds_code().to_owned(), con);
    }

    pub async fn get(code: &str) -> AnyResult<TcConn> {
        Self::get_item(code).ok_or_else(|| anyhow!("data source [{code}] not registered"))
    }

    pub async fn get_or_load(code: &str) -> AnyResult<TcConn> {
        if let Some(con) = Self::get_cached(code) {
            return Ok(con);
        }

        let _guard = LOAD_LOCK.lock().await;
        if let Some(con) = Self::get_cached(code) {
            return Ok(con);
        }

        if let Some(source_code) = TcDbAliases::source_code(code) {
            let source = Self::load_source(&source_code).await?;
            if TcModelSets::get_opt(code).is_none() {
                return Ok(Self::alias_view(code, &source));
            }
            if let Some(con) = Self::get_item(code) {
                return Ok(con);
            }
            return Self::add_alias_by_con(code, &source).await;
        }

        Self::load_source(code).await
    }

    pub async fn get_db(code: &str) -> AnyResult<Db> {
        Ok(Self::get_or_load(code).await?.db())
    }

    pub async fn reload(code: &str) -> AnyResult<TcConn> {
        let source_code = TcDbAliases::source_code(code).unwrap_or_else(|| code.to_owned());
        if source_code == crate::BASE {
            return Err(anyhow!("base data source requires explicit registration"));
        }

        let _guard = LOAD_LOCK.lock().await;
        let mut base = Self::get(crate::BASE).await?;
        let model = BaseDs::get_ds(&mut base, &source_code).await?;
        if !model.state {
            Self::evict_source(&source_code);
            return Err(anyhow!("data source [{source_code}] disabled"));
        }

        let source = Self::add_by_ds_model(&model).await?;
        if code == source_code {
            return Ok(source);
        }
        Self::get_cached(code).ok_or_else(|| anyhow!("data source alias [{code}] unavailable"))
    }

    pub async fn health(code: &str) -> AnyResult<()> {
        Self::get_db(code).await?.connection().await?;
        Ok(())
    }

    pub fn remove(code: &str) {
        write(connections()).remove(code);
    }

    pub fn clear() {
        write(connections()).clear();
    }

    fn get_item(code: &str) -> Option<TcConn> {
        read(connections()).get(code).cloned()
    }

    fn get_cached(code: &str) -> Option<TcConn> {
        match TcDbAliases::source_code(code) {
            Some(source_code) if TcModelSets::get_opt(code).is_none() => {
                Self::get_item(&source_code).map(|source| Self::alias_view(code, &source))
            }
            _ => Self::get_item(code),
        }
    }

    async fn load_source(code: &str) -> AnyResult<TcConn> {
        if let Some(con) = Self::get_item(code) {
            return Ok(con);
        }
        if code == crate::BASE {
            return Err(anyhow!("base data source not registered"));
        }

        let mut base = Self::get(crate::BASE).await?;
        let model = BaseDs::get_active_ds(&mut base, code).await?;
        Self::add_by_ds_model(&model).await
    }

    fn evict_source(source_code: &str) {
        Self::remove(source_code);
        for alias in TcDbAliases::aliases_for_source(source_code) {
            Self::remove(&alias);
        }
    }

    async fn publish_source(con: TcConn, models: ModelSet) -> AnyResult<TcConn> {
        let aliases = TcDbAliases::aliases_for_source(con.ds_code());
        let mut derived = Vec::new();
        for alias in &aliases {
            if TcModelSets::get_opt(alias).is_some() {
                derived.push((alias.clone(), Self::build_alias(alias, &con).await?));
            }
        }

        TcModelSets::set(con.ds_code(), models);
        Self::add_con(con.clone());
        for alias in aliases {
            match derived.iter().find(|(code, _)| code == &alias) {
                Some((_, alias_con)) => Self::add_con(alias_con.clone()),
                None => Self::remove(&alias),
            }
        }
        Ok(con)
    }

    async fn build_alias(alias: &str, source: &TcConn) -> AnyResult<TcConn> {
        if let Some(models) = TcModelSets::get_opt(alias) {
            let url = source.connect_url.as_deref().ok_or_else(|| {
                anyhow!(
                    "alias source data source [{}] has no reconnectable URL",
                    source.ds_code()
                )
            })?;
            let db = Self::connect_by_url(url, models, &source.pool_options).await?;
            return Ok(TcConn {
                inner: db,
                meta: Self::alias_meta(alias, source),
                connect_url: source.connect_url.clone(),
                pool_options: source.pool_options.clone(),
            });
        }

        Ok(Self::alias_view(alias, source))
    }

    fn alias_view(alias: &str, source: &TcConn) -> TcConn {
        TcConn {
            inner: source.inner.clone(),
            meta: Self::alias_meta(alias, source),
            connect_url: source.connect_url.clone(),
            pool_options: source.pool_options.clone(),
        }
    }

    fn alias_meta(alias: &str, source: &TcConn) -> TcConnMeta {
        let mut meta = source.meta.clone();
        meta.ds_code = alias.to_owned();
        meta
    }

    async fn connect_by_url(
        url: &str,
        models: ModelSet,
        pool_options: &TcPoolOptions,
    ) -> AnyResult<Db> {
        let mut builder = Db::builder();
        builder.models(models);
        pool_options.apply(&mut builder);

        Ok(builder.connect(url).await?)
    }
}

fn positive_timeout_secs(value: Option<i64>) -> Option<Duration> {
    value
        .filter(|value| *value > 0)
        .and_then(|value| u64::try_from(value).ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_model_set_is_always_available() {
        TcModelSets::remove(crate::BASE);
        assert!(TcModelSets::get(crate::BASE).is_ok());
    }

    #[test]
    fn alias_rejects_chains() {
        let values = HashMap::from([
            ("biz".to_owned(), "base".to_owned()),
            ("base".to_owned(), "physical".to_owned()),
        ]);
        assert!(TcDbAliases::check_values(&values).is_err());
    }

    #[test]
    fn metadata_snapshots_redact_passwords() {
        let meta = TcConnMeta::new(
            "base",
            "base",
            "postgresql://user:secret@127.0.0.1:5432/demo",
        );
        let redacted = meta.redacted();

        assert_eq!(meta.password, "secret");
        assert!(redacted.password.is_empty());
        assert!(!redacted.url.contains("secret"));
    }

    #[test]
    fn pool_capacity_must_be_positive() {
        let ds = BaseDs {
            ds_code: "invalid".into(),
            max_con: Some(0),
            ..Default::default()
        };
        assert!(TcPoolOptions::from_ds(&ds).is_err());
    }
}
