use anyhow::Result;
use toasty_mgr::{BaseDs, Model, TcMgr};

#[derive(Debug, Model)]
struct AppRow {
    #[key]
    id: i64,
    value: String,
}

fn register_models() {
    TcMgr::set_models("app", toasty_mgr::models!(AppRow));
}

/// Normal application startup. Do not create or seed schema here.
async fn start(control_url: &str) -> Result<()> {
    register_models();
    TcMgr::register_base(control_url).await?;
    TcMgr::health(toasty_mgr::BASE).await?;
    Ok(())
}

/// Run only for a fresh development or test control database.
async fn provision_fresh_control_database(control_url: &str) -> Result<()> {
    start(control_url).await?;
    TcMgr::push_base_schema().await?;
    Ok(())
}

/// Run from a migration seed or management command, not every startup.
async fn seed_app_source() -> Result<()> {
    let mut base = TcMgr::get(toasty_mgr::BASE).await?;
    toasty_mgr::create!(BaseDs {
        ds_code: "app",
        name: "Application database",
        user_name: "app",
        pwd: "secret-reference",
        db_host: "127.0.0.1",
        db_name: "app",
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
