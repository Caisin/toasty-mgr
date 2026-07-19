# 运行与运维

本章面向启动探针、配置变更、连接故障和受控移除。所有管理状态都在当前进程内；
多个应用实例需要分别执行重载。

## 启动检查

注册控制连接后先检查 `base`，再检查启动必需的数据源：

```rust,ignore
TcMgr::register_base(control_url).await?;
TcMgr::health(toasty_mgr::BASE).await?;
TcMgr::health("primary").await?;
```

`health(code)` 会执行正常的懒加载，然后从池中获取一个物理连接。它不执行应用
查询，也不验证业务表是否完成 migration。非必需租户库可以不在启动阶段检查，
保留首次请求时懒加载。

## 配置变更与重载

修改 `base_ds` 后，调用：

```rust,ignore
TcMgr::reload("tenant_a").await?;
```

重载顺序为：读取最新行、验证启用状态、解析密码、创建新连接池、发布新连接。
连接失败时返回错误并保留旧连接。`state = false` 是例外：旧连接及其别名会被主动
移除，调用返回禁用错误。

配置变更发布流程建议为：

1. 在控制库更新目标行。
2. 对一个应用实例调用管理接口触发 `reload`。
3. 调用 `health` 并执行一条只读业务查询。
4. 对其他应用实例重复重载。
5. 失败时恢复旧目录值；旧池仍在时不需要抢先移除。

本 crate 不监听数据库通知，也不广播重载。应用可以在自己的管理接口、消息总线
消费者或配置轮询器中调用 `reload`。

## 移除与注销

```rust,ignore
TcMgr::remove("tenant_a");
TcMgr::unregister("retired_source");
```

- `remove` 只移除连接缓存，保留 `ModelSet`。下一次 `get` 会从 `base_ds` 重新加载。
- `unregister` 同时移除连接和模型集合。再次使用前必须重新调用 `set_models`。
- `TcConnections::clear()` 清空全部连接，主要用于进程隔离测试和受控关闭。

禁用数据源的推荐做法是先把 `state` 更新为 `false`，再调用 `reload`。仅调用
`remove` 不会禁用数据源，下一次访问会重新加载。

## 别名

别名让稳定的业务编码指向物理数据源编码：

```rust,ignore
use std::collections::HashMap;

TcMgr::init_alias(HashMap::from([(
    "reporting".to_owned(),
    "analytics_primary".to_owned(),
)]))
.await?;

let db = TcMgr::get("reporting").await?;
```

别名没有独立模型集合时，共享源 `Db` 和连接池。为别名注册独立模型集合时，管理器
使用源 URL 和池配置创建新的 `Db`。该模式要求源连接由可重连 URL 创建；通过
`add_db` 注入的 `Db` 不保存 URL。

别名不支持链式映射，不能覆盖同名物理数据源。调用 `init_alias` 会整体替换映射并
移除旧别名缓存。别名配置由应用提供，不存储在 `base_ds`。

## 状态与日志

```rust,ignore
let codes = TcMgr::all_codes().await;
let metas = toasty_mgr::TcConnections::all_metas().await;
```

`all_codes` 同时包含源编码和可用别名。`all_metas` 返回去除密码的快照，可用于管理
页或指标标签。不要输出目录原始行或完整 URL。

建议记录这些应用级事件：

- 数据源编码、操作类型和成功/失败，不记录密码。
- `reload` 的开始、完成和错误。
- `health` 延迟与错误分类。
- 首次懒加载失败次数。

## 故障排查

| 现象 | 检查 | 处理 |
|---|---|---|
| `base data source not registered` | 启动顺序 | 先执行 `register_base` |
| `models ... not set` | 数据源编码与模型注册 | 补 `set_models`，然后重试 |
| `data source ... disabled` | `base_ds.state` | 确认是否应启用；修改后 `reload` |
| 连接超时 | host、port、feature、pool timeout | 先从应用宿主机验证网络和凭据 |
| PostgreSQL TLS 拒绝 | 目录 URL 固定禁用 TLS | 改用 `add_by_url` 显式注册 TLS URL |
| 修改配置未生效 | 连接仍在缓存 | 对每个进程调用 `reload` |
| 重载失败但请求仍成功 | 旧连接被保留 | 修正目录值后再次重载 |
| 别名不可用 | 映射链或同名冲突 | 使用单层、唯一的别名映射 |
