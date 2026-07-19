# API 速查

连接管理器类型是 `TcMgr`，事务管理器类型是 `TcTxMgr`。

## 启动与模型

| API | 作用 | 调用时机 |
|---|---|---|
| `TcMgr::set_models(code, models)` | 注册单个数据源模型集合 | 首次加载前 |
| `TcMgr::init_model_sets(values)` | 批量注册模型集合 | 应用启动 |
| `TcMgr::models(code)` | 读取已注册模型集合 | 诊断或扩展代码 |
| `TcMgr::register_base(url)` | 显式注册控制连接 | 每次进程启动 |
| `TcMgr::push_base_schema()` | 在新控制库创建 `base_ds` | 一次性初始化或测试 |
| `TcMgr::set_password_resolver(value)` | 安装目录密码解析器 | 注册连接前 |
| `TcMgr::clear_password_resolver()` | 恢复明文密码行为 | 测试或受控重置 |

## 获取和注册连接

| API | 作用 |
|---|---|
| `TcMgr::get(code)` | 返回缓存 `Db`，未命中时从 `base_ds` 懒加载 |
| `TcMgr::add_by_url(code, url)` | 使用已注册模型集合显式建立连接 |
| `TcMgr::add_by_url_with_models(code, url, models)` | 同时提供 URL 和模型集合 |
| `TcMgr::add_by_ds_model(model)` | 从已校验的 `BaseDs` 显式建立连接 |
| `TcMgr::add_by_ds_model_with_models(model, models)` | 同时提供目录模型和模型集合 |
| `TcMgr::add_db(code, db)` | 注入调用方已经构造的 Toasty `Db` |

`add_by_ds_model` 不检查 `state`。正常业务懒加载使用 `get`。

## 运行管理

| API | 作用 |
|---|---|
| `TcMgr::reload(code)` | 重新读取目录并在连接成功后替换缓存 |
| `TcMgr::health(code)` | 懒加载并获取一个物理连接 |
| `TcMgr::remove(code)` | 移除连接，保留模型集合 |
| `TcMgr::unregister(code)` | 移除连接和模型集合 |
| `TcMgr::all_codes()` | 返回已注册源编码和别名 |
| `TcMgr::init_alias(map)` | 整体替换别名映射 |
| `TcConnections::all_metas()` | 返回不含密码的连接元信息快照 |
| `TcConnections::clear()` | 清空连接注册表，主要用于测试 |

## 事务

| API | 作用 |
|---|---|
| `TcTxMgr::transaction(code, callback)` | 单数据源事务回调 |
| `TcTxMgr::t(callback)` | 新建管理器并执行多数据源回调 |
| `TcTxMgr::new().trans(callback)` | 使用显式管理器执行回调 |
| `tx.get_tx(code)` | 按需打开并返回一个 `TcTx` |
| `tx.get_txs([codes])` | 按需打开并返回多个不同编码的 `TcTx` |
| `tx.use_tx(code)` / `tx.use_txs([codes])` | 预先打开事务 |
| `tx.get(code)` | 返回已经打开的事务，不执行懒打开 |
| `tx.txs()` | 查看已打开事务映射 |

## 扩展宏

`tc_mgr_ext!` 可以为固定数据源生成带名称的方法：

```rust,ignore
toasty_mgr::tc_mgr_ext!(
    tenant_a => toasty_mgr::models!(Customer),
    audit => toasty_mgr::models!(AuditEvent),
);

TcMgr::init_tenant_a_models();
let db = TcMgr::tenant_a().await?;

let mut txs = TcTxMgr::new();
let tx = txs.tenant_a().await?;
```

宏生成 `TcMgrExt` 和 `TcTxMgrExt` trait。使用生成方法的模块需要把对应 trait 引入
作用域。动态租户通常直接使用 `set_models` 和 `get(code)`，不需要生成方法。

## 查询规格

`tc_query_spec!` 根据显式白名单生成动态查询参数。它不要求模型 derive `TcQuery`：

```rust,ignore
toasty_mgr::tc_query_spec! {
    pub CustomerSearch for Customer {
        filters {
            name_prefix: String => name.starts_with;
            active: bool => active.eq;
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

let request = CustomerSearch::builder()
    .name_prefix("Al")
    .asc_name()
    .filter(Customer::fields().id().gt(100))
    .build();
```

只有 `filters` 必填。`sort`、`default_order`、`tie_breaker`、`page` 可以依次省略；省略后
不会生成对应 setter 或附加隐藏排序。分页未声明稳定排序时，跨页结果可能随数据库返回
顺序变化。

生成结构体提供：

| API | 行为 |
|---|---|
| `into_expr()` | 只构造筛选 `Expr<bool>` |
| `into_query()` | 构造筛选和排序后的 `QueryMany<Model>`，不分页 |
| `count(executor)` | 只应用筛选，忽略排序和分页字段 |
| `all(executor)` | 应用筛选和排序，不应用分页字段 |
| `fetch_page(executor)` | 校验 1-based page/size，返回 `query::Page<Model>` |
| `filter(expr)` | 追加任意 Toasty `Expr<bool>`，可以重复调用 |

省略 `page { ... }` 时不生成 `page`、`size` setter 和 `fetch_page()`。`Paging` 表示已
校验的请求页，`Page<T>` 包含 `items`、`paging`、`total` 和 `total_pages`。

需要直接扩展 Toasty query builder 时，可以按模型选择性 derive `TcQuery`：

```rust,ignore
#[derive(Debug, toasty_mgr::Model, toasty_mgr::TcQuery)]
pub struct Customer {
    #[key]
    pub id: i64,
    pub name: String,
}

use crate::models::CustomerTcQueryExt;

let query = Customer::all().name_starts_with("Al").asc_id();
```

`TcQuery` 只处理数据库字段，自动忽略 `belongs_to`、`has_one` 和 `has_many` 关系字段。
它生成 `{Model}TcQueryExt` extension trait；跨模块调用链式方法时必须导入该 trait。
`tc_query_spec!` 与 `TcQuery` 没有生成代码依赖，可以单独使用。
