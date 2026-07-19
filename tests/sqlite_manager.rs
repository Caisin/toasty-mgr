#![cfg(feature = "sqlite")]

use toasty_mgr::{BaseDs, TcConnections, TcMgr};

#[tokio::test]
async fn built_in_catalog_lazily_loads_a_second_data_source() -> anyhow::Result<()> {
    TcConnections::clear();

    let mut base = TcMgr::register_base("sqlite::memory:").await?;
    TcMgr::push_base_schema().await?;
    TcMgr::set_models("tenant", toasty_mgr::models!(BaseDs));

    toasty_mgr::create!(BaseDs {
        ds_code: "tenant",
        name: "Tenant database",
        user_name: "",
        pwd: "",
        db_host: "",
        db_name: ":memory:",
        cur_schema: "",
        time_zone: "",
        port: 0,
        db_type: "sqlite",
        state: true,
        remark: "",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;

    let tenant = TcMgr::get("tenant").await?;
    tenant.connection().await?;

    let metas = TcConnections::all_metas().await;
    assert!(metas.contains_key("base"));
    assert_eq!(metas["tenant"].db_type, "sqlite");

    TcMgr::remove("tenant");
    TcMgr::get("tenant").await?.connection().await?;

    Ok(())
}
