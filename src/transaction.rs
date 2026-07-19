use std::{collections::HashMap, future::Future, ops::AsyncFnOnce, pin::Pin, sync::Arc};

use anyhow::{Result, anyhow};

use crate::{
    Executor, ToastyConnectionManager, Transaction,
    codegen_support::core::{
        Schema,
        driver::{Capability, ExecResponse, operation::RawSql},
        stmt,
    },
};

/// Coordinates one Toasty transaction per requested data source.
///
/// This is best-effort coordination, not a distributed transaction protocol:
/// commits happen sequentially and cannot be rolled back after a partial commit.
pub struct TcTxMgr {
    tx_map: HashMap<String, TcTx>,
}

/// A transaction owned by [`TcTxMgr`].
pub struct TcTx {
    inner: Transaction<'static>,
}

impl TcTx {
    fn new(inner: Transaction<'static>) -> Self {
        Self { inner }
    }

    async fn commit(self) -> crate::Result<()> {
        self.inner.commit().await
    }

    async fn rollback(self) -> crate::Result<()> {
        self.inner.rollback().await
    }
}

// Toasty 0.8 expands Executor into boxed futures; implement that form directly.
impl Executor for TcTx {
    fn transaction<'borrow, 'future>(
        &'borrow mut self,
    ) -> Pin<Box<dyn Future<Output = crate::Result<Transaction<'borrow>>> + Send + 'future>>
    where
        'borrow: 'future,
        Self: 'future,
    {
        Box::pin(async move { self.inner.transaction().await })
    }

    fn exec_untyped<'borrow, 'future>(
        &'borrow mut self,
        stmt: stmt::Statement,
    ) -> Pin<Box<dyn Future<Output = crate::Result<ExecResponse>> + Send + 'future>>
    where
        'borrow: 'future,
        Self: 'future,
    {
        Box::pin(async move { self.inner.exec_untyped(stmt).await })
    }

    fn exec_raw_sql<'borrow, 'future>(
        &'borrow mut self,
        raw: RawSql,
    ) -> Pin<Box<dyn Future<Output = crate::Result<ExecResponse>> + Send + 'future>>
    where
        'borrow: 'future,
        Self: 'future,
    {
        Box::pin(async move { self.inner.exec_raw_sql(raw).await })
    }

    fn capability(&mut self) -> &Capability {
        self.inner.capability()
    }

    fn schema(&mut self) -> &Arc<Schema> {
        self.inner.schema()
    }
}

impl Default for TcTxMgr {
    fn default() -> Self {
        Self::new()
    }
}

impl TcTxMgr {
    pub fn new() -> Self {
        Self {
            tx_map: HashMap::new(),
        }
    }

    pub fn txs(&self) -> &HashMap<String, TcTx> {
        &self.tx_map
    }

    pub async fn use_txs<const N: usize>(&mut self, codes: [&str; N]) -> Result<()> {
        for code in codes {
            self.use_tx(code).await?;
        }
        Ok(())
    }

    pub async fn use_tx(&mut self, code: &str) -> Result<&mut Self> {
        if !self.tx_map.contains_key(code) {
            self.tx_map
                .insert(code.to_owned(), Self::begin_owned_tx(code).await?);
        }
        Ok(self)
    }

    pub async fn get_tx(&mut self, code: &str) -> Result<&mut TcTx> {
        self.use_tx(code).await?;
        self.get(code)
    }

    pub fn get(&mut self, code: &str) -> Result<&mut TcTx> {
        self.tx_map
            .get_mut(code)
            .ok_or_else(|| anyhow!("data source transaction [{code}] not opened"))
    }

    pub async fn get_txs<const N: usize>(&mut self, codes: [&str; N]) -> Result<[&mut TcTx; N]> {
        Self::ensure_unique_codes(&codes)?;
        self.use_txs(codes).await?;

        let pointers = codes
            .into_iter()
            .map(|code| {
                self.tx_map
                    .get_mut(code)
                    .map(|tx| tx as *mut TcTx)
                    .ok_or_else(|| anyhow!("data source transaction [{code}] not opened"))
            })
            .collect::<Result<Vec<_>>>()?;

        // Codes are unique, and the map is not mutated after pointers are collected.
        let transactions = pointers
            .into_iter()
            .map(|tx| unsafe { &mut *tx })
            .collect::<Vec<_>>();
        transactions
            .try_into()
            .map_err(|_| anyhow!("transaction count mismatch"))
    }

    pub async fn t<F, T>(callback: F) -> Result<T>
    where
        F: for<'a> AsyncFnOnce(&'a mut Self) -> Result<T> + Send,
        T: Send,
    {
        Self::new().trans(callback).await
    }

    pub async fn trans<F, T>(mut self, callback: F) -> Result<T>
    where
        F: for<'a> AsyncFnOnce(&'a mut Self) -> Result<T> + Send,
        T: Send,
    {
        match callback(&mut self).await {
            Ok(value) => {
                for (_, tx) in self.tx_map {
                    tx.commit().await?;
                }
                Ok(value)
            }
            Err(error) => {
                for (_, tx) in self.tx_map {
                    tx.rollback().await?;
                }
                Err(error)
            }
        }
    }

    pub async fn transaction<F, T>(code: &str, callback: F) -> Result<T>
    where
        F: for<'a> AsyncFnOnce(&'a mut Transaction<'_>) -> Result<T> + Send,
        T: Send,
    {
        let mut db = ToastyConnectionManager::get(code).await?;
        let mut tx = db.transaction().await?;
        match callback(&mut tx).await {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    async fn begin_owned_tx(code: &str) -> Result<TcTx> {
        let mut db = ToastyConnectionManager::get(code).await?;
        let tx = db.transaction().await?;

        // Toasty creates ConnRef::Owned for Db transactions. The lifetime is a
        // type-level exclusivity guard and does not reference `db`; this wrapper
        // never exposes that Db handle while the transaction is alive.
        let tx = unsafe { std::mem::transmute::<Transaction<'_>, Transaction<'static>>(tx) };
        Ok(TcTx::new(tx))
    }

    fn ensure_unique_codes(codes: &[&str]) -> Result<()> {
        for (index, code) in codes.iter().enumerate() {
            if codes[(index + 1)..].contains(code) {
                return Err(anyhow!("duplicate data source code: {code}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_transaction_callbacks_type_check() {
        let future = TcTxMgr::new().trans(async |tx| -> Result<i64> {
            let [_sys, _base] = tx.get_txs(["sys", "base"]).await?;
            Ok(1)
        });
        drop(future);
    }

    #[test]
    fn duplicate_codes_are_rejected() {
        let error = TcTxMgr::ensure_unique_codes(&["base", "base"]).unwrap_err();
        assert!(error.to_string().contains("duplicate data source code"));
    }
}
