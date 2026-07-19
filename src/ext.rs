use std::{collections::HashMap, sync::Arc};

use anyhow::Result;

use crate::{
    BaseDs, Db, ModelSet, PasswordResolver, TcConnections, TcDbAliases, TcModelSets,
    ToastyConnectionManager,
};

/// Generate data-source-specific shortcuts for [`ToastyConnectionManager`] and
/// [`crate::TcTxMgr`].
#[macro_export]
macro_rules! tc_mgr_ext {
    ($($code:ident => $models:expr),* $(,)?) => {
        $crate::paste::paste! {
            #[allow(async_fn_in_trait)]
            pub trait TcMgrExt {
                $(
                    fn [<init_ $code _models>]() {
                        $crate::ToastyConnectionManager::set_models(stringify!($code), $models);
                    }

                    async fn $code() -> $crate::anyhow::Result<$crate::Db> {
                        $crate::ToastyConnectionManager::get(stringify!($code)).await
                    }
                )*
            }
        }
        impl TcMgrExt for $crate::ToastyConnectionManager {}

        #[allow(async_fn_in_trait)]
        pub trait TcTxMgrExt {
            $(
                async fn $code(&mut self) -> $crate::anyhow::Result<&mut $crate::TcTx>;
            )*
        }
        impl TcTxMgrExt for $crate::TcTxMgr {
            $(
                async fn $code(&mut self) -> $crate::anyhow::Result<&mut $crate::TcTx> {
                    self.get_tx(stringify!($code)).await
                }
            )*
        }
    };
}

impl ToastyConnectionManager {
    /// Install an application-specific password resolver.
    pub fn set_password_resolver<R>(resolver: R)
    where
        R: PasswordResolver,
    {
        crate::base_ds::set_password_resolver(Some(Arc::new(resolver)));
    }

    /// Restore plaintext password handling.
    pub fn clear_password_resolver() {
        crate::base_ds::set_password_resolver(None);
    }

    /// Associate a Toasty model set with a managed data-source code.
    pub fn set_models(code: &str, models: ModelSet) {
        TcModelSets::set(code, models);
    }

    /// Register multiple model sets.
    pub fn init_model_sets<I, S>(models: I)
    where
        I: IntoIterator<Item = (S, ModelSet)>,
        S: AsRef<str>,
    {
        for (code, models) in models {
            Self::set_models(code.as_ref(), models);
        }
    }

    pub fn models(code: &str) -> Result<ModelSet> {
        TcModelSets::get(code)
    }

    /// Register the control connection using the built-in [`BaseDs`] model set.
    pub async fn register_base(url: &str) -> Result<Db> {
        Self::add_by_url(crate::BASE, url).await
    }

    /// Create the built-in `base_ds` table on a fresh control database.
    ///
    /// Toasty's `push_schema` is intended for fresh databases. Persistent schema
    /// evolution should use Toasty migrations instead.
    pub async fn push_base_schema() -> Result<()> {
        Self::get(crate::BASE).await?.push_schema().await?;
        Ok(())
    }

    /// Replace the alias map and initialize aliases for already-connected sources.
    pub async fn init_alias(values: HashMap<String, String>) -> Result<()> {
        let old_aliases = TcDbAliases::all();
        for alias in values.keys() {
            if !old_aliases.contains_key(alias) && TcConnections::get(alias).await.is_ok() {
                anyhow::bail!("alias [{alias}] conflicts with a registered data source");
            }
        }

        TcDbAliases::init(values.clone())?;
        for alias in old_aliases.keys() {
            TcConnections::remove(alias);
        }
        TcConnections::init_aliases(&values).await
    }

    pub async fn add_by_url(name: &str, url: &str) -> Result<Db> {
        Ok(TcConnections::add_by_url(name, url).await?.db())
    }

    pub async fn add_by_url_with_models(name: &str, url: &str, models: ModelSet) -> Result<Db> {
        Ok(TcConnections::add_by_url_with_models(name, url, models)
            .await?
            .db())
    }

    pub async fn add_by_ds_model(model: &BaseDs) -> Result<Db> {
        Ok(TcConnections::add_by_ds_model(model).await?.db())
    }

    pub async fn add_by_ds_model_with_models(model: &BaseDs, models: ModelSet) -> Result<Db> {
        Ok(TcConnections::add_by_ds_model_with_models(model, models)
            .await?
            .db())
    }

    pub async fn add_db(name: &str, db: Db) -> Db {
        TcConnections::add_db(name, db).db()
    }

    /// Get a cached connection or lazily load it from the `base_ds` table.
    pub async fn get(code: &str) -> Result<Db> {
        TcConnections::get_db(code).await
    }

    /// Re-read a catalog row and atomically publish its new connection.
    pub async fn reload(code: &str) -> Result<Db> {
        Ok(TcConnections::reload(code).await?.db())
    }

    pub async fn health(code: &str) -> Result<()> {
        TcConnections::health(code).await
    }

    /// Evict a connection while retaining its model set for lazy reload.
    pub fn remove(code: &str) {
        TcConnections::remove(code);
    }

    /// Remove both the connection and its application model set.
    pub fn unregister(code: &str) {
        TcConnections::remove(code);
        TcModelSets::remove(code);
    }
}
