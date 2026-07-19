# 完整应用示例

本示例展示一个下游服务如何管理 `tenant_a` 和 `audit` 两个 PostgreSQL 数据源。
它覆盖模型注册、控制连接、目录 seed、服务查询、跨数据源事务、别名、健康检查和
重载。完整可编译版本位于
[`docs/templates/application.rs`](../../templates/application.rs)，并由
`tests/doc_templates.rs` 纳入编译测试。

## 项目依赖

```toml
[dependencies]
anyhow = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
toasty-mgr = { path = "../toasty-mgr", features = ["postgresql"] }
```

## 模型与启动模块

```rust,ignore
use toasty_mgr::{BaseDs, Model, TcMgr};

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

pub async fn start(control_url: &str) -> anyhow::Result<()> {
    TcMgr::init_model_sets([
        ("tenant_a", toasty_mgr::models!(Customer)),
        ("audit", toasty_mgr::models!(AuditEvent)),
    ]);

    TcMgr::register_base(control_url).await?;
    TcMgr::health(toasty_mgr::BASE).await?;
    Ok(())
}
```

应用配置只需要控制库 URL，例如：

```text
TOASTY_CONTROL_URL=postgresql://app:password@127.0.0.1:5432/control?sslmode=disable
```

不要把真实 URL 或密码提交到仓库。应用入口读取环境变量后调用 `start`，然后再
启动对外服务。

## 一次性目录初始化

在新开发库上先执行 `register_base` 和 `push_base_schema`。生产环境使用 migration
创建表。然后由管理命令写入两个 `BaseDs` 记录：

```rust,ignore
pub async fn seed_sources() -> anyhow::Result<()> {
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
```

示例使用 `create!` 是为了展示字段；真实 seed 应使用项目 migration 工具或幂等
upsert。

## 服务层 CRUD

`toasty-mgr` 只负责选择 `Db`。拿到连接后就是普通 Toasty 调用：

```rust,ignore
pub async fn create_customer(id: i64, name: &str) -> anyhow::Result<Customer> {
    let mut db = TcMgr::get("tenant_a").await?;
    Ok(toasty_mgr::create!(Customer { id, name })
        .exec(&mut db)
        .await?)
}

pub async fn list_customers() -> anyhow::Result<Vec<Customer>> {
    let mut db = TcMgr::get("tenant_a").await?;
    Ok(Customer::all().exec(&mut db).await?)
}
```

更多查询和更新语法直接查阅
[Toasty 上游指南](https://github.com/tokio-rs/toasty/tree/main/docs/guide/src)。

## 同时写入业务库与审计库

```rust,ignore
use toasty_mgr::TcTxMgr;

pub async fn create_customer_with_audit(
    id: i64,
    name: &str,
) -> anyhow::Result<()> {
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
```

这段代码不是分布式原子事务。两个事务按顺序提交，第二个提交失败时第一个无法
回滚。需要严格一致性时使用同库事务、outbox 或补偿流程。

## 运行时管理

```rust,ignore
use std::collections::HashMap;

pub async fn configure_and_check() -> anyhow::Result<()> {
    TcMgr::init_alias(HashMap::from([(
        "current_tenant".to_owned(),
        "tenant_a".to_owned(),
    )]))
    .await?;

    TcMgr::health("current_tenant").await?;
    let _codes = TcMgr::all_codes().await;

    // Call after changing tenant_a in base_ds.
    TcMgr::reload("tenant_a").await?;

    // Evict only the cached connection. A later get lazily reloads it.
    TcMgr::remove("tenant_a");
    TcMgr::get("tenant_a").await?;
    Ok(())
}
```

生产环境的更新和故障处理步骤见[运行与运维](./operations.md)。
