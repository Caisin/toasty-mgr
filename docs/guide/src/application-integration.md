# 应用接入指南

本章描述下游应用如何初始化和调用 `toasty-mgr`。正常启动只做三件事：注册模型
集合、注册 `base` 连接、按编码获取业务连接。建表和写入 `base_ds` 属于部署或
管理流程，不应在每次启动时执行。

## 1. 添加依赖

```toml
[dependencies]
anyhow = "1"
toasty-mgr = { path = "../toasty-mgr", features = ["postgresql", "mysql"] }
```

控制数据库和所有托管数据库使用到的 driver feature 都必须启用。例如控制库是
PostgreSQL、租户库是 MySQL 时，需要同时启用 `postgresql` 和 `mysql`。可用
feature 为：

| Feature | `BaseDs.db_type` |
|---|---|
| `sqlite` | `sqlite` |
| `turso` | `turso` |
| `mysql` | `mysql` |
| `postgresql` | `postgresql` 或 `postgres` |

下游代码直接使用 `toasty_mgr` 重导出的 `Model`、`models!`、`create!` 和 `Db`，
不需要再添加一个不同版本的 Toasty。

## 2. 定义并注册业务模型

模型仍然是普通 Toasty 模型。每种 schema 对应一个 `ModelSet`：

```rust,ignore
use toasty_mgr::{Model, TcMgr};

#[derive(Debug, Model)]
pub struct Customer {
    #[key]
    pub id: i64,
    pub name: String,
}

pub fn register_models() {
    TcMgr::init_model_sets([
        ("tenant_a", toasty_mgr::models!(Customer)),
        ("tenant_b", toasty_mgr::models!(Customer)),
    ]);
}
```

`base` 默认使用内置的 `models!(BaseDs)`。只有控制数据库还承载其他 Toasty 模型
时，才需要显式注册一个包含 `BaseDs` 的完整模型集合：

```rust,ignore
TcMgr::set_models(
    toasty_mgr::BASE,
    toasty_mgr::models!(BaseDs, ControlAuditLog),
);
```

模型必须在对应数据源第一次 `get`、`reload` 或 `add_by_url` 前注册。模型集合不是
从 `base_ds` 推断出来的。

## 3. 注册控制数据库

将控制数据库 URL 放在应用自己的配置或环境变量中，不要把它也存进 `base_ds`：

```rust,ignore
pub async fn start(control_url: &str) -> anyhow::Result<()> {
    register_models();
    TcMgr::register_base(control_url).await?;
    TcMgr::health(toasty_mgr::BASE).await?;
    Ok(())
}
```

`register_base` 建立并发布 `base` 连接。进程重启后必须重新调用。不要为 `base`
创建别名，也不要依赖 `get("base")` 自动加载它。

## 4. 创建和迁移 `base_ds`

新建开发库或测试库可以执行一次：

```rust,ignore
TcMgr::register_base(control_url).await?;
TcMgr::push_base_schema().await?;
```

`push_base_schema` 会发出完整建表语句，不检测表是否存在，也不维护 schema 版本。
生产环境应由项目现有 migration 流程创建和演进 `base_ds`，字段契约见
[控制表与配置](./catalog.md)。应用正常启动时不要调用 `push_base_schema`。

## 5. 写入数据源配置

部署脚本、管理命令或管理后台负责写入目录。下面的 seed 代码演示 PostgreSQL
租户库：

```rust,ignore
use toasty_mgr::{BaseDs, TcMgr};

pub async fn seed_tenant() -> anyhow::Result<()> {
    let mut base = TcMgr::get(toasty_mgr::BASE).await?;
    toasty_mgr::create!(BaseDs {
        ds_code: "tenant_a",
        name: "Tenant A",
        user_name: "app",
        pwd: "secret-reference",
        db_host: "127.0.0.1",
        db_name: "tenant_a",
        cur_schema: "",
        time_zone: "",
        port: 5432,
        db_type: "postgresql",
        state: true,
        remark: "primary tenant database",
        create_at: 0,
    })
    .exec(&mut base)
    .await?;
    Ok(())
}
```

在持久化环境中应使用 upsert 或 migration seed 保证重复执行安全；上面的 `create!`
只演示字段赋值，不是幂等 seed。

## 6. 在服务层获取连接

服务函数只需要数据源编码，不需要保存全局 `Db`：

```rust,ignore
pub async fn create_customer(
    ds_code: &str,
    id: i64,
    name: &str,
) -> anyhow::Result<Customer> {
    let mut db = TcMgr::get(ds_code).await?;
    let customer = toasty_mgr::create!(Customer { id, name })
        .exec(&mut db)
        .await?;
    Ok(customer)
}
```

`get` 返回可克隆的 Toasty `Db` handle。第一次调用发生缓存未命中时才读取
`base_ds` 并建立连接池；后续调用直接从进程内缓存返回。

需要把 API 请求转换成动态条件时，为该请求声明查询规格，不需要修改模型：

```rust,ignore
toasty_mgr::tc_query_spec! {
    pub CustomerSearch for Customer {
        filters {
            name_prefix: String => name.starts_with;
        }
        sort {
            id;
            name;
        }
        default_order [id Asc];
        tie_breaker id Asc;
        page {
            default_size: 20;
            max_size: 100;
        }
    }
}

pub async fn search_customers(
    ds_code: &str,
    name_prefix: Option<String>,
) -> anyhow::Result<toasty_mgr::query::Page<Customer>> {
    let mut db = TcMgr::get(ds_code).await?;
    Ok(CustomerSearch::builder()
        .maybe_name_prefix(name_prefix)
        .asc_name()
        .build()
        .fetch_page(&mut db)
        .await?)
}
```

`.filter(expr)` 可以追加租户、权限或关系条件。宏只生成规格里声明的条件；其他条件继续
使用 Toasty 原生 `Model::fields()` 构造。`count()` 和 `all()` 不应用 page/size，只有
`fetch_page()` 执行 offset 分页。

## 7. 应用启动顺序

推荐顺序固定为：

1. 安装 `PasswordResolver`，如果目录密码不是明文。
2. 注册全部模型集合。
3. 调用 `register_base`。
4. 初始化别名。
5. 对启动必需的数据源调用 `health`；非必需源保留懒加载。
6. 启动 HTTP、RPC 或任务消费者。

完整实现见[完整应用示例](./complete-example.md)和可编译模板
[`docs/templates/application.rs`](../../templates/application.rs)。

## 常见接入错误

| 错误 | 原因 | 处理 |
|---|---|---|
| `base data source not registered` | 未调用 `register_base` | 在服务启动前注册控制连接 |
| `models for data source [...] not set` | 未注册对应 `ModelSet` | 在首次 `get` 前调用 `set_models` |
| `data source [...] disabled` | `base_ds.state = false` | 确认配置后启用并调用 `reload` |
| driver 不可用 | Cargo feature 未启用 | 启用与 `db_type` 对应的 feature |
| 重复建表失败 | 每次启动都调用 `push_base_schema` | 把建表移到 migration 或一次性初始化 |
