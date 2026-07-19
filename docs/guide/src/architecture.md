# 内部架构

本章说明 `toasty-mgr` 如何把控制表、模型集合和 Toasty 连接池组合起来，供接入
排障和维护公共 API 时参考。

## 核心状态

进程内有三类注册表：

| 注册表 | Key | Value | 来源 |
|---|---|---|---|
| `TcConnections` | 数据源或别名编码 | `TcConn` | 显式注册或懒加载 |
| `TcModelSets` | 数据源编码 | Toasty `ModelSet` | 应用启动代码 |
| `TcDbAliases` | 业务别名 | 物理数据源编码 | 应用配置 |

连接、模型和别名都不是跨进程状态。`base_ds` 只保存物理连接配置，不保存 Rust
模型集合，也不保存别名。

## `get` 数据流

```text
TcMgr::get(code)
  |
  +-- connection cache hit --------------------------> clone Db handle
  |
  +-- cache miss
       |
       +-- acquire async load lock
       +-- check cache again
       +-- resolve alias, when configured
       +-- get registered ModelSet
       +-- read active BaseDs row through `base`
       +-- resolve password and build URL
       +-- build Toasty pool
       +-- publish source and derived aliases
       +------------------------------------------------> clone Db handle
```

全局异步加载锁避免并发缓存未命中时重复创建连接池。锁内会再次检查缓存。这个锁
目前覆盖所有数据源的首次加载，因此大量不同数据源同时冷启动时会串行建池。

## 模型集合为何必须由应用注册

Toasty 在构造 `Db` 时需要 `ModelSet` 来建立 schema 元数据。`base_ds` 只有连接
信息，无法表达 Rust 模型类型、关联和生成代码。因此动态添加一个目录行还不够，
应用二进制必须已经包含对应模型并调用 `set_models`。

同一 schema 的多个租户可以分别把相同的 `models!(...)` 注册到各自编码。不同
schema 的数据源应注册不同集合。

## 连接发布规则

创建过程先在局部变量中完成 URL、池参数和 `Db` 构造，成功后才写入连接注册表。
因此：

- 首次加载失败不会留下半初始化缓存。
- 普通 `reload` 失败不会替换旧连接。
- 成功重载会发布新 `Db`，已持有的旧 `Db` clone 仍可继续存活到调用方释放。
- 禁用记录会主动驱逐源及别名，因为禁用状态优先于旧连接可用性。

## 别名数据流

没有独立模型集合的别名只是源 `TcConn` 的视图，复用同一个 `Db`。有独立模型集合
的别名会用源的 URL 和池参数建立另一个 `Db`，从而使用不同 Toasty schema 元数据。
别名只允许一层，避免递归解析和生命周期歧义。

## 事务所有权

`TcTxMgr` 持有每个数据源的 Toasty transaction，并通过回调限制引用生命周期。
多数据源回调成功后逐个提交，失败后逐个回滚。该实现只协调本地事务生命周期，
没有 prepare/commit 协议，因此不能保证跨数据库原子性。

## 依赖和后端边界

```text
application (may use SeaORM)
  |
  +-- toasty-mgr
        |
        +-- toasty 0.8.0
              +-- enabled built-in driver features
```

`toasty-mgr` 不依赖 KX、SeaORM，也不直接声明独立 PostgreSQL driver crate。
feature 只转发给 Toasty；启用 `postgresql` 时由 Toasty 传递选择自己的 driver。
业务应用可以同时保留自己的 SeaORM 依赖，两者不共享 manager 状态或模型类型。
