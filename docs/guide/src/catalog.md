# 控制表与配置

`BaseDs` 是本 crate 内置的 Toasty 模型，对应控制数据库中的 `base_ds` 表。
`ds_code` 是主键，也是连接缓存、模型集合和别名共同使用的标识。

## 字段契约

| 字段 | Rust 类型 | 用途 |
|---|---|---|
| `ds_code` | `String` | 唯一数据源编码 |
| `name` | `String` | 展示名称 |
| `user_name` | `String` | 数据库用户名 |
| `pwd` | `String` | 明文密码或交给 `PasswordResolver` 的存储值 |
| `db_host` | `String` | 网络主机；SQLite/Turso 下为文件目录 |
| `db_name` | `String` | 数据库名；SQLite/Turso 下为文件名或 `:memory:` |
| `cur_schema` | `String` | schema 元数据，空值的逻辑默认值为 `public` |
| `time_zone` | `String` | 时区元数据，空值的逻辑默认值为 `+08:00` |
| `port` | `i32` | 端口；`0` 使用 MySQL/PostgreSQL 默认端口 |
| `max_con` | `Option<i32>` | 最大连接池容量；设置时必须大于零 |
| `min_con` | `Option<i32>` | 保留字段，Toasty 0.8.0 builder 当前不使用 |
| `connect_timeout` | `Option<i64>` | 创建连接超时秒数；非正值视为未设置 |
| `acquire_timeout` | `Option<i64>` | 获取连接超时秒数；非正值视为未设置 |
| `idle_timeout` | `Option<i64>` | 连接最大空闲秒数；非正值视为未设置 |
| `db_type` | `String` | `sqlite`、`turso`、`mysql`、`postgresql` 或 `postgres` |
| `state` | `bool` | 是否允许加载 |
| `remark` | `String` | 应用备注 |
| `create_at` | `i64` | 由应用定义语义的时间戳 |

Migration 必须保持字段名、可空性和与上述 Rust 类型兼容的数据库类型。修改模型后
需要同步 migration、本文字段表和四种后端的集成测试。

## URL 映射

| `db_type` | 生成结果 | 默认端口 |
|---|---|---|
| `sqlite` | `sqlite:<db_host>/<db_name>` 或 `sqlite::memory:` | 无 |
| `turso` | `turso:<db_host>/<db_name>` 或 `turso::memory:` | 无 |
| `mysql` | `mysql://user:password@host:port/database` | 3306 |
| `postgresql` / `postgres` | `postgresql://user:password@host:port/database?sslmode=disable` | 5432 |

SQLite 和 Turso 将 `db_host` 作为目录、`db_name` 作为文件名。两者拼接结果为
`:memory:` 时生成内存 URL。Turso 的专用 builder 选项不能通过 `BaseDs` 表达；
需要这些选项时手工构造 `Db` 后调用 `TcMgr::add_db`。

PostgreSQL URL 当前固定使用 `sslmode=disable`。要求 TLS 或额外 URL 参数的数据源
不应通过 `BaseDs` 懒加载，应使用 `TcMgr::add_by_url` 显式注册，或者先扩展本项目
的目录模型与 URL 构造逻辑。

`cur_schema` 和 `time_zone` 目前只作为目录元数据保存，Toasty 0.8.0 的 URL driver
不会自动应用。应用应通过数据库角色默认值、migration SQL 或其他连接初始化方式
设置。

## 启用、禁用与发布规则

`TcMgr::get` 只加载 `state = true` 的行。`reload` 读到禁用行时会移除旧源连接及
其别名缓存并返回错误。直接调用 `add_by_ds_model` 不检查 `state`，适合调用方已经
完成外部校验的场景。

新配置只有在 URL 构造、密码解析和连接池创建全部成功后才发布到缓存。普通重载
失败不会覆盖仍可使用的旧连接。

## 密码解析

默认把 `pwd` 当作明文。生产应用可以存储加密文本或 secret reference，并在启动
最早阶段安装解析器：

实现示例需要在下游项目添加 `async-trait = "0.1"`。

```rust,ignore
use anyhow::Result;
use async_trait::async_trait;
use toasty_mgr::{PasswordResolver, TcMgr};

struct Secrets;

#[async_trait]
impl PasswordResolver for Secrets {
    async fn resolve(&self, stored: &str) -> Result<String> {
        load_secret(stored).await
    }
}

TcMgr::set_password_resolver(Secrets);
```

解析结果只用于创建连接，不写回 `base_ds`。`TcConnMeta` 的 `Debug` 和
`TcConnections::all_metas` 会移除密码，但应用仍不应记录原始 `BaseDs`、解析结果
或完整连接 URL。测试或受控重置可调用 `TcMgr::clear_password_resolver()` 恢复明文
行为。

## Schema 管理

`TcMgr::push_base_schema()` 只适用于全新开发库和临时测试库。它不会检查表是否已
存在，也不记录版本。持久化环境应使用应用已有的 migration 工具创建 `base_ds`，
并使用独立 seed/管理命令维护记录。
