#![cfg(feature = "turso")]

use toasty_mgr::{BaseDs, TcConnections, TcMgr};

#[tokio::test]
async fn built_in_catalog_loads_a_turso_data_source() -> anyhow::Result<()> {
    TcConnections::clear();

    let mut base = TcMgr::register_base("turso::memory:").await?;
    TcMgr::push_base_schema().await?;
    TcMgr::set_models("tenant_turso", toasty_mgr::models!(BaseDs));

    toasty_mgr::create!(BaseDs {
        ds_code: "tenant_turso",
        name: "Turso tenant",
        user_name: "",
        pwd: "",
        db_host: "",
        db_name: ":memory:",
        cur_schema: "",
        time_zone: "",
        port: 0,
        db_type: "turso",
        state: true,
        remark: "",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;

    TcMgr::get("tenant_turso").await?.connection().await?;
    assert_eq!(
        TcConnections::all_metas().await["tenant_turso"].db_type,
        "turso"
    );

    Ok(())
}
