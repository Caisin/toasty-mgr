# 项目概览

`toasty-mgr` 解决的是应用如何管理多个 Toasty `Db`，而不是如何使用 Toasty 定义
模型或执行 CRUD。

应用先显式注册一个编码为 `base` 的控制数据库。控制数据库内置 `BaseDs` 模型，
对应 `base_ds` 表。表中的每一行描述一个可管理的数据源。应用再为每个数据源编码
注册编译期 `ModelSet`，业务代码即可通过 `TcMgr::get(code)` 获取对应的 Toasty
`Db`。

```text
application models ---- set_models(code) -----+
                                                |
base database ---- base_ds row ---- get(code) --+--> Toasty Db pool
                                                        |
service code <------------------------------------------+
```

## 本项目负责什么

- 管理 `base` 控制连接和业务数据源连接。
- 从 `base_ds` 懒加载连接配置并缓存连接池。
- 在运行时执行健康检查、重载、移除和别名映射。
- 把 `BaseDs` 的连接池配置应用到 Toasty builder。
- 为单数据源或多个数据源协调 Toasty transaction。

## 本项目不负责什么

- 不从数据库动态生成 Toasty 模型；模型必须编译进应用。
- 不替代应用的 migration 工具。
- 不讲解 Toasty 的模型、关联、查询和 CRUD；这些内容见
  [Toasty 上游文档](https://github.com/tokio-rs/toasty/tree/main/docs/guide/src)。
- 不提供数据库代理、跨进程连接缓存或分布式事务协议。
- 不自动监听 `base_ds` 变化；修改配置后由应用调用 `reload`。

## 依赖边界

本 crate 直接使用 crates.io 的 `toasty = "0.8.0"`，没有 KX crate、SeaORM，
也不直接依赖 `toasty-driver-postgresql`。数据库支持通过 `sqlite`、`turso`、
`mysql`、`postgresql` Cargo feature 转发给 Toasty；启用 `postgresql` 时由 Toasty
在其内部传递选择对应 driver。

第一次接入请按[应用接入指南](./application-integration.md)完成启动顺序；需要完整
代码时直接查看[完整应用示例](./complete-example.md)。
