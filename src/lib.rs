//! Toasty multi-data-source management backed by an internal `base_ds` model.
//!
//! The `base` connection is registered explicitly. Other connections are loaded
//! on demand from its `base_ds` table, then cached by data-source code.

extern crate self as toasty_mgr;

pub mod base_ds;
pub mod ext;
pub mod query;
pub mod registry;
pub mod transaction;

#[doc(hidden)]
pub use anyhow;
pub use base_ds::{BaseDs, PasswordResolver};
#[doc(hidden)]
pub use bon;
#[doc(hidden)]
pub use pastey;
pub use query::{Page, Paging, TcQueryBuildError, TcQueryError};
pub use registry::{TcConn, TcConnMeta, TcConnections, TcDbAliases, TcModelSets};
pub use toasty::*;
pub use toasty_mgr_macros::{TcQuery, tc_query_spec};
pub use transaction::{TcTx, TcTxMgr};

/// Code reserved for the control data source containing the `base_ds` table.
pub const BASE: &str = "base";

/// Process-wide Toasty connection manager.
pub struct TcMgr;

impl TcMgr {
    /// Return all registered source and alias codes.
    pub async fn all_codes() -> Vec<String> {
        let mut codes = TcConnections::all_metas()
            .await
            .into_keys()
            .collect::<Vec<_>>();
        codes.sort();
        codes
    }
}

tc_mgr_ext!(base => toasty::models!(BaseDs));
