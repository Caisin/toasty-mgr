// Copy the relevant test into `tests/` and replace the environment variable,
// model set, and data-source code. Keep external-service tests ignored.

use anyhow::{Result, anyhow};
use toasty_mgr::{
    BaseDs, Db, Executor, TcMgr,
    codegen_support::core::driver::operation::{RawSql, RawSqlRet},
};

async fn ensure_catalog(base: &mut Db) -> Result<bool> {
    if BaseDs::all().exec(base).await.is_ok() {
        return Ok(false);
    }
    base.push_schema().await?;
    Ok(true)
}

async fn drop_catalog(base: &mut Db) -> Result<()> {
    base.exec_raw_sql(RawSql {
        sql: "DROP TABLE base_ds".into(),
        params: Vec::new(),
        ret: RawSqlRet::None,
    })
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local database"]
async fn managed_connection_from_local_catalog() -> Result<()> {
    let url = std::env::var("TOASTY_TEST_DATABASE_URL")
        .map_err(|_| anyhow!("TOASTY_TEST_DATABASE_URL must be set"))?;
    let parsed = url::Url::parse(&url)?;
    let mut base = TcMgr::register_base(&url).await?;
    let created_catalog = ensure_catalog(&mut base).await?;
    TcMgr::set_models("managed_test", toasty_mgr::models!(BaseDs));

    let row = toasty_mgr::create!(BaseDs {
        ds_code: "managed_test",
        name: "managed test",
        user_name: parsed.username(),
        pwd: parsed.password().unwrap_or_default(),
        db_host: parsed.host_str().unwrap_or_default(),
        db_name: parsed.path().trim_start_matches('/'),
        cur_schema: "",
        time_zone: "",
        port: i32::from(parsed.port().unwrap_or_default()),
        db_type: parsed.scheme(),
        state: true,
        remark: "temporary test row",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;

    let outcome = async {
        TcMgr::get("managed_test").await?.connection().await?;
        TcMgr::health("managed_test").await
    }
    .await;

    TcMgr::unregister("managed_test");
    let row_cleanup = row
        .delete()
        .exec(&mut base)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from);
    let table_cleanup = if created_catalog {
        drop_catalog(&mut base).await
    } else {
        Ok(())
    };

    outcome.and(row_cleanup).and(table_cleanup)
}
