# 测试与本地数据库验证

下游应用应测试自己的模型集合、目录记录和启动顺序，而不只测试 Toasty CRUD。
本仓库提供无外部服务测试和显式启用的本地数据库测试。

## 仓库验证

```bash
cargo fmt --all -- --check
cargo test --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --all-features --no-deps
```

`tests/sqlite_manager.rs` 和 `tests/turso_manager.rs` 会：

1. 创建内存控制数据库。
2. 创建 `base_ds`。
3. 写入第二个数据源目录记录。
4. 通过 `TcMgr::get` 懒加载连接。
5. 验证移除后可以重新加载。

`tests/doc_templates.rs` 会编译 `docs/templates/` 中的 Rust 示例，保证文档使用的
类型名和方法名与公共 API 一致。

`tests/query_spec.rs` 分别验证独立的 `tc_query_spec!`、可选的 `TcQuery` derive、关系
字段排除、字符串借用输入、重复排序错误，以及 SQLite/Turso 上的条件、count、all 和
offset 分页行为。

## 本地 MySQL

测试凭据通过环境变量传入，不写入源码：

```bash
TOASTY_TEST_MYSQL_URL='mysql://<user>:<password>@127.0.0.1:3306/test' \
  cargo test --features mysql --test local_databases \
  mysql_catalog_loads_managed_connection -- --ignored --exact
```

## 本地 PostgreSQL

```bash
TOASTY_TEST_POSTGRES_URL='postgresql://<user>:<password>@127.0.0.1:5432/test?sslmode=disable' \
  cargo test --features postgresql --test local_databases \
  postgresql_catalog_loads_managed_connection -- --ignored --exact
```

本地测试默认 `ignore`，避免普通 `cargo test` 意外访问开发者数据库。测试会检测
`base_ds` 是否存在，只在缺失时临时创建；插入唯一临时记录，完成懒加载和 health
check 后删除记录；只有测试创建了控制表时才删除该表。

可复制的测试骨架见
[`docs/templates/local-database-test.rs`](../../templates/local-database-test.rs)。

## 下游项目建议测试

至少保留一个使用应用真实模型的集成测试：

```rust,ignore
#[tokio::test]
async fn tenant_is_loaded_from_catalog() -> anyhow::Result<()> {
    TcMgr::set_models("tenant_test", toasty_mgr::models!(Customer));
    let mut base = TcMgr::register_base("sqlite::memory:").await?;
    TcMgr::push_base_schema().await?;

    // Insert a sqlite BaseDs row for tenant_test, then load it.
    TcMgr::get("tenant_test").await?.connection().await?;
    Ok(())
}
```

测试进程共享静态注册表。并行测试使用不同数据源编码，或在隔离测试开头调用
`TcConnections::clear()`。外部数据库测试必须在成功和失败路径都清理临时目录记录。
