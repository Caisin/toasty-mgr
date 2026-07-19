#![cfg(any(feature = "mysql", feature = "postgresql"))]

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

async fn exercise(url: &str, code: &str) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    let mut base = TcMgr::register_base(url).await?;
    let created_catalog = ensure_catalog(&mut base).await?;
    TcMgr::set_models(code, toasty_mgr::models!(BaseDs));

    let row = toasty_mgr::create!(BaseDs {
        ds_code: code,
        name: "toasty-mgr local integration test",
        user_name: parsed.username(),
        pwd: parsed.password().unwrap_or_default(),
        db_host: parsed.host_str().unwrap_or("127.0.0.1"),
        db_name: parsed.path().trim_start_matches('/'),
        cur_schema: "",
        time_zone: "",
        port: i32::from(parsed.port().unwrap_or_default()),
        db_type: parsed.scheme(),
        state: true,
        remark: "temporary integration-test row",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;

    let outcome = async {
        TcMgr::get(code).await?.connection().await?;
        TcMgr::health(code).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    TcMgr::unregister(code);
    let row_cleanup = row
        .delete()
        .exec(&mut base)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from);
    TcMgr::remove(toasty_mgr::BASE);
    let table_cleanup = if created_catalog {
        drop_catalog(&mut base).await
    } else {
        Ok(())
    };

    outcome.and(row_cleanup).and(table_cleanup)
}

fn database_url(variable: &str) -> Result<String> {
    std::env::var(variable).map_err(|_| anyhow!("{variable} must be set"))
}

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "requires a local MySQL test database"]
async fn mysql_catalog_loads_managed_connection() -> Result<()> {
    let url = database_url("TOASTY_TEST_MYSQL_URL")?;
    exercise(&url, "__toasty_mgr_mysql_it__").await
}

#[cfg(feature = "postgresql")]
#[tokio::test]
#[ignore = "requires a local PostgreSQL test database"]
async fn postgresql_catalog_loads_managed_connection() -> Result<()> {
    let url = database_url("TOASTY_TEST_POSTGRES_URL")?;
    exercise(&url, "__toasty_mgr_postgresql_it__").await
}
