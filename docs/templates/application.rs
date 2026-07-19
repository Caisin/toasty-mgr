use std::collections::HashMap;

use anyhow::Result;
use toasty_mgr::{BaseDs, Model, TcMgr, TcTxMgr};

#[derive(Debug, Model)]
pub struct Customer {
    #[key]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Model)]
pub struct AuditEvent {
    #[key]
    pub id: i64,
    pub message: String,
}

/// Normal process startup: register compile-time models and the control DB.
pub async fn start(control_url: &str) -> Result<()> {
    TcMgr::init_model_sets([
        ("tenant_a", toasty_mgr::models!(Customer)),
        ("audit", toasty_mgr::models!(AuditEvent)),
    ]);
    TcMgr::register_base(control_url).await?;
    TcMgr::health(toasty_mgr::BASE).await?;
    Ok(())
}

/// One-time initialization for a fresh development or test database.
pub async fn provision_fresh_control_database(control_url: &str) -> Result<()> {
    start(control_url).await?;
    TcMgr::push_base_schema().await?;
    Ok(())
}

/// Example seed command. Production seeds should use idempotent upserts.
pub async fn seed_sources() -> Result<()> {
    let mut base = TcMgr::get(toasty_mgr::BASE).await?;

    toasty_mgr::create!(BaseDs {
        ds_code: "tenant_a",
        name: "Tenant A",
        user_name: "app",
        pwd: "tenant-password-reference",
        db_host: "127.0.0.1",
        db_name: "tenant_a",
        cur_schema: "",
        time_zone: "",
        port: 5432,
        db_type: "postgresql",
        state: true,
        remark: "",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;

    toasty_mgr::create!(BaseDs {
        ds_code: "audit",
        name: "Audit database",
        user_name: "app",
        pwd: "audit-password-reference",
        db_host: "127.0.0.1",
        db_name: "audit",
        cur_schema: "",
        time_zone: "",
        port: 5432,
        db_type: "postgresql",
        state: true,
        remark: "",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;
    Ok(())
}

/// Typical service-layer write using a caller-selected data-source code.
pub async fn create_customer(ds_code: &str, id: i64, name: &str) -> Result<Customer> {
    let mut db = TcMgr::get(ds_code).await?;
    Ok(toasty_mgr::create!(Customer { id, name })
        .exec(&mut db)
        .await?)
}

/// Typical service-layer query.
pub async fn list_customers(ds_code: &str) -> Result<Vec<Customer>> {
    let mut db = TcMgr::get(ds_code).await?;
    Ok(Customer::all().exec(&mut db).await?)
}

/// Best-effort coordination of two database-local transactions.
pub async fn create_customer_with_audit(id: i64, name: &str) -> Result<()> {
    TcTxMgr::t(async |tx| {
        let [tenant, audit] = tx.get_txs(["tenant_a", "audit"]).await?;
        toasty_mgr::create!(Customer { id, name })
            .exec(tenant)
            .await?;
        toasty_mgr::create!(AuditEvent {
            id,
            message: "customer created",
        })
        .exec(audit)
        .await?;
        Ok(())
    })
    .await
}

/// Runtime operations normally exposed through an authenticated admin surface.
pub async fn configure_and_check() -> Result<Vec<String>> {
    TcMgr::init_alias(HashMap::from([(
        "current_tenant".to_owned(),
        "tenant_a".to_owned(),
    )]))
    .await?;
    TcMgr::health("current_tenant").await?;
    Ok(TcMgr::all_codes().await)
}

/// Apply a BaseDs change after it has been committed to the control database.
pub async fn apply_catalog_change(code: &str) -> Result<()> {
    TcMgr::reload(code).await?;
    TcMgr::health(code).await
}

/// Evict a cached pool while keeping its ModelSet for the next lazy load.
pub async fn evict_and_reload(code: &str) -> Result<()> {
    TcMgr::remove(code);
    TcMgr::get(code).await?.connection().await?;
    Ok(())
}
