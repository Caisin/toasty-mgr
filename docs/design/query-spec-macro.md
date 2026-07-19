# 类型安全查询条件宏

## Summary

`toasty-mgr` 增加可选的 `#[derive(TcQuery)]` 和函数式过程宏 `tc_query_spec!`。`TcQuery` 遍历
Toasty 模型的全部数据库字段，为 Toasty 模型 query builder 生成
`<field>_<operation>()` 和 `asc_<field>()` / `desc_<field>()`。`tc_query_spec!` 独立生成
动态查询参数、Expr、count、all 和分页入口，并使用 `bon` 提供链式 builder。
生成代码只构造 Toasty AST，不拼接 SQL，也不接受未经白名单约束的数据库字段名。
Toasty 关系字段不属于生成范围。

两个宏没有生成代码依赖。只需要请求查询结构体的应用可以单独使用 `tc_query_spec!`，
不会为模型全部字段生成链式方法；只有明确需要直接扩展 Toasty query builder 的模型才
额外 derive `TcQuery`。

过滤条件是核心能力，分页不是默认行为。生成类型可以转换为 `Expr<bool>` 或 Toasty
query builder，也可以直接执行 count、查询全部匹配数据或显式执行 1-based offset
分页。Cursor 分页保留为后续能力，不与第一版混合。

## Motivation

Toasty 0.8 已提供静态查询宏：

```rust,ignore
let query = toasty_mgr::query!(
    Customer
    FILTER .state == true
    ORDER BY .created_at DESC
    LIMIT 20
);
```

该宏适合编译时已知的完整查询，但不能直接表达常见列表接口中的动态条件：

- 请求字段为 `None` 时不添加对应条件。
- 应用需要复用生成条件作为 `Expr<bool>`，或与已有 Toasty `Expr<bool>` 继续组合。
- 调用方从白名单中选择一个或多个排序字段及方向。
- 页码和页大小需要统一校验，并且必须避免 Toasty `offset` 在缺少 `limit` 时 panic。
- 列表响应通常同时需要当前页数据和过滤后的总记录数。
- count 和全量查询不能被隐式附加分页条件。
- 每个列表接口重复手写 `if let`、排序 `match`、offset 计算和 count 查询，容易出现
  页码从 0/1 开始不一致、无稳定排序、页大小无限制等问题。

这个能力生成 Toasty 查询，而不是替代 Toasty 查询语言。复杂的 `OR`、关联子查询、
投影和后端专用表达式继续直接使用 Toasty API。

## User-facing API

### Cargo features

宏本身不增加 Cargo feature。实现增加：

- `bon = "3.9"`，当前 lockfile 解析为 `bon 3.9.3`。
- workspace 成员 `toasty-mgr-macros`，同时实现 `TcQuery` derive 和函数式
  `tc_query_spec!`，两个入口使用独立的解析与生成模块。
- proc-macro 实现依赖 `syn`、`quote`、`proc-macro2` 和 `proc-macro-crate`。

应用仍只需依赖 `toasty-mgr` 并按数据源启用现有 feature：

```toml
[dependencies]
toasty-mgr = { path = "../toasty-mgr", features = ["postgresql"] }
```

`sqlite`、`turso`、`mysql` 和 `postgresql` feature 的含义不变。第一版不增加
`serde` feature；HTTP 或 RPC 层负责把外部字符串转换为生成的排序枚举。

`toasty-mgr` 通过 `pub use toasty_mgr_macros::{TcQuery, tc_query_spec}` 重导出两个宏。
宏 crate 不依赖运行时 crate，使用 `proc-macro-crate` 解析下游对 `toasty-mgr` 的实际
依赖名称，避免 Cargo rename 导致生成路径失效。仓库根包同时作为 workspace root。

函数式过程宏通过 `proc-macro-crate` 找到实际的 `toasty-mgr` 依赖名，再使用隐藏的
`toasty_mgr::bon` 重导出和
`#[builder(crate = toasty_mgr::bon, on(String, into))]`。因此下游应用不需要额外声明
`bon`，Cargo rename 也不会破坏展开路径。字符串 setter 不要求调用方先转换成
`String`。该路径用法由
[bon builder reference](https://bon-rs.com/reference/builder/top-level/crate) 支持。

### Derive Toasty Query builder

`TcQuery` 必须与 Toasty `Model` derive 用在同一个命名结构体上：

```rust,ignore
#[derive(Debug, toasty_mgr::Model, toasty_mgr::TcQuery)]
pub struct Customer {
    #[key]
    pub id: i64,
    pub name: String,
    pub state: bool,
    pub created_at: i64,
    pub nickname: Option<String>,
}
```

在 Toasty 0.8 上该扩展的可行性高，但不能从一个 derive 宏稳定引用另一个 derive 宏
生成的 nominal 类型名。`TcQuery` 因此生成模型专用的 `{Model}TcQueryExt` extension
trait，并为 Toasty 稳定公开的 `QueryMany<Model>` 实现它，不生成固有 `impl`。同模块
调用无需额外操作；从其他模块调用时需要把该 trait 引入作用域。该方案不需要 fork
Toasty，也不依赖 `{Model}Query<T>` 的命名。

生成的 API 使用字段名在前、操作在后：

```rust,ignore
let query = Customer::all()
    .name_starts_with("Al")
    .state_eq(true)
    .created_at_ge(start)
    .id_in_list([1_i64, 2, 3])
    .nickname_is_some()
    .desc_created_at()
    .asc_id();
```

默认操作矩阵：

| 字段分类 | 生成方法 |
|---|---|
| 标量 | `<field>_eq`、`<field>_ne`、`<field>_in_list` |
| 有序标量 | 标量方法，加 `<field>_gt`、`<field>_ge`、`<field>_lt`、`<field>_le`、`<field>_between`、`asc_<field>`、`desc_<field>` |
| 字符串 | 有序标量方法，加 `<field>_starts_with` |
| `Option<T>` | 按内部 `T` 生成可用操作，加 `<field>_is_none`、`<field>_is_some` |
| `#[belongs_to]`、`#[has_one]`、`#[has_many]` 关系 | 自动排除，不生成任何方法 |
| 集合、嵌入或未知自定义数据库类型 | 必须显式配置操作或使用带原因的 `skip` |

字符串操作不把参数固定为 `String`。生成方法沿用 Toasty 的 `IntoExpr` 约束：非空字符串
字段的比较和前缀方法接收 `impl IntoExpr<String>`，可空字符串字段按操作接收
`impl IntoExpr<Option<String>>` 或 `impl IntoExpr<String>`。Toasty 0.8 已为 `String`、
`&String` 和 `&str` 实现这些转换，因此以下调用都成立：

```rust,ignore
let owned = String::from("Alice");
let query = Customer::all()
    .name_eq("Alice")
    .name_ne(&owned)
    .name_starts_with(owned);
```

`TcQuery` 必须访问输入结构体中的每个命名字段，先根据 Toasty 的 `#[belongs_to]`、
`#[has_one]`、`#[has_many]` 属性排除关系，再对剩余数据库字段分类。关系字段不会生成
过滤或排序方法，也不要求额外添加 `#[tc_query(skip = ...)]`。不能推断安全操作的数据库
字段使 derive 失败，错误指向字段并要求显式处理，例如：

```rust,ignore
#[derive(toasty_mgr::Model, toasty_mgr::TcQuery)]
struct Customer {
    #[key]
    id: i64,

    owner_id: i64,

    #[belongs_to(key = owner_id, references = id)]
    owner: toasty_mgr::Deferred<Owner>,

    #[tc_query(ops(eq, ne), sort)]
    external_id: CustomerId,
}
```

上例生成 `owner_id_eq()` 等数据库字段方法，但不生成 `owner_eq()` 或 `asc_owner()`。
关系查询继续使用 Toasty 原生关系 API，或由调用方构造 `Expr<bool>` 后通过 `.filter()`
追加。

对于非关系字段，`skip` 是显式覆盖记录，不是静默忽略。proc macro 聚合所有无法分类的
数据库字段错误后再返回，避免调用方一次只能修复一个字段。只要存在无法分类的数据库
字段就不产生成功输出。新增数据库字段后，编译器会强制选择默认分类、显式操作或
`skip`，从而保证数据库字段覆盖不会随模型演进漂移。

第一版不为 tuple struct、unit struct、enum 或带泛型的模型生成 API。过程宏从
`DeriveInput` 读取字段、类型和属性；它不能在展开阶段执行或反射另一个 derive 生成的
`Model::fields()`。生成的方法最终调用 `Model::fields()` 产生类型化 path，但该细节只
存在于展开代码，应用不需要直接使用。

### 声明查询规格

查询规格与 Toasty 模型放在同一模块，确保模型及其生成的字段访问器可见：

```rust,ignore
use toasty_mgr::Model;

#[derive(Debug, Model)]
pub struct Customer {
    #[key]
    pub id: i64,
    pub name: String,
    pub state: bool,
    pub created_at: i64,
}

toasty_mgr::tc_query_spec! {
    pub CustomerSearch for Customer {
        filters {
            id: i64 => id.eq;
            name_prefix: String => name.starts_with;
            state: bool => state.eq;
            created_from: i64 => created_at.ge;
            created_to: i64 => created_at.le;
            ids: Vec<i64> => id.in_list;
        }

        sort {
            id;
            name;
            created_at;
        }

        default_order [created_at Desc];
        tie_breaker id Asc;

        page {
            default_size: 20;
            max_size: 100;
        }
    }
}
```

只有 `filters { ... }` 是必填区块；它可以为空，以便只使用重复 `.filter(Expr<bool>)`。
`sort`、`default_order`、`tie_breaker` 和 `page` 均可省略，并按示例中的固定顺序出现。
省略 `sort` 时不生成任何 `.asc_*()` / `.desc_*()` setter，也不添加隐藏排序。省略
`default_order` 时未显式排序的查询保持 Toasty 原生顺序；省略 `tie_breaker` 时不自动
追加唯一键。省略 `page` 配置时仍生成分页能力，默认 `page = 1`、`size = 10`、最大
`size = 100`。分页规格可以不声明排序，但调用方需要自行承担跨页顺序不稳定的风险。

该声明生成的 `CustomerSearch` 在概念上等价于：

```rust,ignore
#[derive(Debug, toasty_mgr::bon::Builder)]
#[builder(crate = toasty_mgr::bon, on(String, into))]
pub struct CustomerSearch {
    id: Option<i64>,
    name_prefix: Option<String>,
    state: Option<bool>,
    created_from: Option<i64>,
    created_to: Option<i64>,
    ids: Option<Vec<i64>>,
    #[builder(field = Vec::new())]
    extra_filters: Vec<toasty_mgr::stmt::Expr<bool>>,
    #[builder(field = Vec::new())]
    orders: Vec<CustomerOrder>,
    #[builder(default = 1)]
    page: u64,
    #[builder(default = 20)]
    size: u64,
}

impl Default for CustomerSearch {
    fn default() -> Self {
        Self::builder().build()
    }
}
```

上面的 `toasty_mgr::bon` 代表过程宏解析后的实际依赖路径，由
`#[doc(hidden)] pub use bon;` 提供，仅用于宏卫生，不属于面向应用的稳定 API。稳定 API
包括：

- `CustomerSearch::builder()`：返回 bon typestate builder。
- 每个筛选项为 `Option<T>`。bon 生成 `.field(T)` 和
  `.maybe_field(Option<T>)` 两种 setter；调用方可以按任意顺序链式调用，每个 setter
  最多调用一次。
- `String` 和 `Option<String>` 筛选项启用 bon 的 `on(String, into)`。因此字符串 setter
  接受 `String`、`&String` 和 `&str`，`.maybe_field(...)` 也接受对应的可选值。
- 每个 sort 字段生成两个 builder 方法，例如 `.asc_name()` 和 `.desc_name()`。方法把
  排序追加到私有 `orders` builder field，返回相同 typestate 的 builder，因此可以连续
  调用并保持调用顺序。
- builder 和构建后的 `CustomerSearch` 都提供 `.filter(expr)`：接受任意 Toasty
  `Expr<bool>` 并追加到私有 `extra_filters`。该方法可重复调用，所有表达式与声明式
  筛选条件使用 `AND` 合并。
- `.page(value)` 和 `.size(value)`：未设置时分别使用 `1` 和规格中的
  `default_size`；省略 `page` 配置时默认大小为 `10`。这两个字段是
  [`Paging`](#page-and-paging) 在查询参数中的拍平形式。
- `CustomerSearch::into_expr()`：只生成过滤条件 `Expr<bool>`。
- `CustomerSearch::into_query()`：生成带过滤和排序、但不带分页的 Toasty query builder。
- `CustomerSearch::count()`：只执行过滤后的 `COUNT(*)`。
- `CustomerSearch::all()`：执行过滤和排序后的全量查询，不应用分页。
- `CustomerSearch::fetch_page()`：唯一应用 `page`、`size` 的终端操作，返回
  `query::Page<Customer>`。

方法签名按模型具体化，并消费条件对象：

```rust,ignore
impl CustomerSearch {
    pub fn filter(self, expr: toasty_mgr::stmt::Expr<bool>) -> Self;
    pub fn into_expr(self) -> Result<toasty_mgr::stmt::Expr<bool>, TcQueryBuildError>;
    pub fn into_query(
        self,
    ) -> Result<toasty_mgr::schema::QueryMany<Customer>, TcQueryBuildError>;

    pub async fn count(
        self,
        executor: &mut dyn toasty_mgr::Executor,
    ) -> Result<u64, TcQueryError>;

    pub async fn all(
        self,
        executor: &mut dyn toasty_mgr::Executor,
    ) -> Result<Vec<Customer>, TcQueryError>;

    pub async fn fetch_page(
        self,
        executor: &mut dyn toasty_mgr::Executor,
    ) -> Result<toasty_mgr::query::Page<Customer>, TcQueryError>;
}
```

消费 `self` 避免要求所有声明字段都实现 `Clone`。需要复用过滤条件时调用
`into_expr()`，再克隆 Toasty 的 `Expr<bool>`；不要从已排序 query 克隆 count query。

排序方法使用 bon 的
[custom methods](https://bon-rs.com/guide/typestate-api/custom-methods) 和
[builder fields](https://bon-rs.com/guide/typestate-api/builder-fields) 契约。概念上的展开为：

```rust,ignore
impl<S: customer_search_builder::State> CustomerSearchBuilder<S> {
    pub fn filter(mut self, expr: toasty_mgr::stmt::Expr<bool>) -> Self {
        self.extra_filters.push(expr);
        self
    }

    pub fn asc_name(mut self) -> Self {
        self.orders.push(CustomerOrder::Name(Asc));
        self
    }

    pub fn desc_name(mut self) -> Self {
        self.orders.push(CustomerOrder::Name(Desc));
        self
    }
}
```

`extra_filters` 和 `orders` 没有普通 bon setter，也不参与 typestate 转换。这样既允许
重复追加表达式和多个排序条件，又不需要启用 bon 的 experimental overwritable 或
implied-bounds feature。函数式过程宏直接生成方法标识符，`sort { created_at; }` 对应
`.asc_created_at()` 和 `.desc_created_at()`。

生成类型实现 `Debug` 和手写 `Default`，其中 `default()` 委托给 bon builder，确保分页
默认值与 `.builder().build()` 一致，不会被派生 `Default` 重置为 `0`。bon 的 typestate
在编译期拒绝对同一 setter 的重复调用；`build()` 因全部字段为 optional/default 而始终
可用。不自动实现序列化 trait，避免强制应用选择 `serde` 或任何 Web 框架。

### 构建并执行

#### 转换为 Expr

```rust,ignore
let tenant_filter: toasty_mgr::stmt::Expr<bool> = tenant_scope();

let expr = CustomerSearch::builder()
    .name_prefix("Al")
    .state(true)
    .filter(tenant_filter)
    .build()
    .filter(customer_visibility_filter())
    .into_expr()?;

let query = Customer::filter(expr);
```

`.filter(expr)` 是 repeatable custom builder method，不是 bon 的一次性字段 setter。
声明式字段条件和所有额外表达式按 `AND` 合并。额外表达式可以包含 `OR`、`NOT`、关系
子查询或其他 Toasty 已支持但宏没有声明式语法的条件。

#### Count 和查询全部

```rust,ignore
use toasty_mgr::TcMgr;

let mut db = TcMgr::get("tenant_a").await?;

let total = CustomerSearch::builder()
    .state(true)
    .build()
    .count(&mut db)
    .await?;

let customers = CustomerSearch::builder()
    .state(true)
    .asc_name()
    .build()
    .all(&mut db)
    .await?;
```

`count()` 只应用过滤条件，不应用排序、`page`、`size`、limit 或 offset。`all()` 应用
过滤和排序，但不应用分页或隐藏的默认 limit；调用方需要自行控制全量结果的内存风险。

需要复用同一组过滤条件时，转换并克隆 `Expr<bool>`：

```rust,ignore
let expr = CustomerSearch::builder()
    .state(true)
    .build()
    .into_expr()?;

let total = Customer::filter(expr.clone())
    .count()
    .exec(&mut db)
    .await?;
let customers = Customer::filter(expr)
    .asc_name()
    .exec(&mut db)
    .await?;
```

`into_query()` 应用过滤和排序，但从不应用分页。返回值是 Toasty 生成的模型 query
builder，调用方仍可继续调用 Toasty 原生的 `.filter()`、`.order_by()`、`.limit()`、
`.offset()`、`.count()`、`.select()` 或 `.delete()`。

不建议在 `into_query()` 已经应用排序后再调用 `.count()`；直接使用生成的 `count()`，
或像上面一样从纯 `Expr<bool>` 构造 count query，确保 count 不携带 `ORDER BY`。

#### 显式分页

```rust,ignore
let page = CustomerSearch::builder()
    .name_prefix("Al")
    .state(true)
    .asc_name()
    .desc_created_at()
    .page(2)
    .size(25)
    .build()
    .fetch_page(&mut db)
    .await?;

assert_eq!(page.paging.page, 2);
assert_eq!(page.paging.size, 25);
println!("{} / {}", page.items.len(), page.total);
```

协议层已经持有 `Option<T>` 时不需要手工拆分：

```rust,ignore
let request = CustomerSearch::builder()
    .maybe_name_prefix(input.name_prefix)
    .maybe_state(input.state)
    .page(input.page.unwrap_or(1))
    .size(input.page_size.unwrap_or(20))
    .build();
```

排序来自字符串协议时，由应用完成白名单映射。所有排序方法返回相同 builder 类型，
所以各个 `match` 分支兼容：

```rust,ignore
let builder = CustomerSearch::builder();
let builder = match (input.sort.as_deref(), input.descending) {
    (Some("name"), false) => builder.asc_name(),
    (Some("name"), true) => builder.desc_name(),
    (Some("created_at"), false) => builder.asc_created_at(),
    (Some("created_at"), true) => builder.desc_created_at(),
    (None, _) => builder,
    (Some(_), _) => return Err(InvalidSortField.into()),
};
let request = builder.build();
```

bon 的 `build()` 只完成结构体构建。验证按实际出口分层执行：`into_expr()` 校验条件，
`into_query()` 和 `all()` 额外校验排序，`fetch_page()` 再校验页大小和 offset。setter
本身不执行跨字段验证。

### 生成语义

生成条件和 query 等价于以下手写代码：

```rust,ignore
let expr = toasty_mgr::stmt::Expr::and_all([
    Customer::fields().name().starts_with("Al"),
    Customer::fields().state().eq(true),
    tenant_filter,
]);

let query = Customer::filter(expr)
    .order_by(Customer::fields().name().asc())
    .order_by(Customer::fields().created_at().desc())
    .order_by(Customer::fields().id().asc());
```

所有已提供的筛选项使用 `AND` 组合。排序按 `.asc_<field>()` / `.desc_<field>()` 的
调用顺序应用。未调用排序方法时使用 `default_order`；调用方没有显式包含
`tie_breaker` 时，生成代码将其追加到最后，保证页间顺序稳定。

### Toasty Query builder extension

`#[derive(TcQuery)]` 为 Toasty `Model` derive 生成模型专用 extension trait。
`tc_query_spec!` 不负责生成这些方法；即使模型没有声明查询规格，应用也可以直接使用
字段化链式 API：

```rust,ignore
use crate::models::CustomerTcQueryExt;

let query = Customer::all()
    .name_starts_with("Al")
    .state_eq(true)
    .created_at_ge(start)
    .filter(tenant_filter)
    .desc_created_at()
    .asc_id();

let customers = query.exec(&mut db).await?;
```

概念上的展开为：

```rust,ignore
pub trait CustomerTcQueryExt: Sized {
    fn state_eq<V>(self, value: V) -> Self
    where
        V: toasty_mgr::stmt::IntoExpr<bool>;

    fn name_starts_with<V>(self, value: V) -> Self
    where
        V: toasty_mgr::stmt::IntoExpr<String>;

    fn desc_created_at(self) -> Self;
}

impl CustomerTcQueryExt for toasty_mgr::schema::QueryMany<Customer> {
    fn state_eq<V>(self, value: V) -> Self
    where
        V: toasty_mgr::stmt::IntoExpr<bool>,
    {
        self.filter(Customer::fields().state().eq(value))
    }

    fn name_starts_with<V>(self, value: V) -> Self
    where
        V: toasty_mgr::stmt::IntoExpr<String>,
    {
        self.filter(Customer::fields().name().starts_with(value))
    }

    fn desc_created_at(self) -> Self {
        self.order_by(Customer::fields().created_at().desc())
    }
}
```

筛选方法统一使用 `<field>_<operation>` 命名。排序是唯一保留方向前置的 API，使用
`asc_<field>` / `desc_<field>`。这些方法只实现在 `QueryMany<Model>`；调用
`.first()`、`.one()`、`.count()` 或 `.select()` 改变返回形态后不再提供列表筛选和排序
快捷方法。调用 `.filter()`、`.order_by()`、`.limit()` 或 `.offset()` 后仍保持列表
query 类型，可以继续调用生成方法。

derive proc macro 不读取另一个 derive 的展开结果，也不计算 `{Model}Query` 类型名。
生成 trait 的可见性与模型一致。升级 Toasty 时需要通过编译测试锁定
`schema::QueryMany`、`fields()`、`filter()` 和 `order_by()` 契约。

`tc_query_spec!` 不使用 `TcQuery` 生成的方法。函数式过程宏直接解析 `字段.操作`，例如
`name.starts_with`，然后生成 Toasty 字段表达式：

```rust,ignore
Customer::fields().name().starts_with(value)
```

宏识别 `starts_with`、`in_list`、`between`、`eq`、`ne`、`gt`、`ge`、`lt` 和 `le`。
无法识别的操作在操作标识符处报错；不存在的字段或值类型
不匹配由 Toasty 类型化 accessor 产生编译错误。排序同样直接生成
`Model::fields().field().asc()/desc()`。生成查询返回 `QueryMany<Model>`，不依赖 Toasty
生成的 `{Model}Query` 名称。

这种少量表达式代码重复是刻意的解耦：删除 `TcQuery` derive 不影响任何查询规格，修改
查询规格也不会改变模型 query builder 的方法集合。

第一版支持下列映射：

| 声明操作 | Toasty 表达式 | 约束 |
|---|---|---|
| `eq` / `ne` | `field.eq(value)` / `field.ne(value)` | 标量字段 |
| `gt` / `ge` / `lt` / `le` | 对应有序比较 | 字段和值必须类型匹配 |
| `between` | `field.between(low, high)` | 输入使用二元组 |
| `in_list` | `field.in_list(values)` | 集合元素必须与字段类型匹配 |
| `starts_with` | `field.starts_with(value)` | 字符串字段 |

NULL 三态、`like`、`ilike`、数组操作和关系 `any/all` 不进入可移植的第一版操作集合。
应用可以继续直接调用 Toasty；后续只有在输入表示、后端语义和转义规则明确后才扩充
宏操作。

### Page and Paging

分页请求和分页结果是独立于具体模型的公共结构体：

```rust,ignore
pub mod query {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Paging {
        pub page: u64,
        pub size: u64,
    }

    #[derive(Debug)]
    pub struct Page<T> {
        pub items: Vec<T>,
        pub paging: Paging,
        pub total: u64,
        pub total_pages: u64,
    }
}
```

`Paging` 是请求语义，`Page<T>` 是响应语义。它们放在 `toasty_mgr::query` 模块中，避免
与 Toasty 的 cursor 分页类型 `toasty::stmt::Page` 混淆。

`page { ... }` 是 `tc_query_spec!` 的可选配置区块，不是分页能力开关。宏始终拍平生成
`page` 和 `size`，builder 始终提供 `.page()`、`.size()`，并始终生成 `fetch_page()`。
省略区块时使用 `default_size = 10`、`max_size = 100`；显式区块覆盖这两个值。
`fetch_page()` 校验后构造 `Paging`，再把同一个值放进响应的 `Page<T>::paging`。其他
出口明确忽略 page/size。

筛选参数名 `filter`、`extra_filters`、`orders` 以及所有生成的 `asc_<field>`、
`desc_<field>`、`page`、`size` 和 `fetch_page` 为宏保留名称。
声明同名 filter 或重复 sort 字段时产生编译错误，避免字段或 builder 方法冲突。

### 分页契约

- `page` 从 1 开始，`0` 返回 `InvalidPageNumber`。
- `size` 必须处于 `1..=max_size`；默认构造使用声明中的 `default_size`，省略分页配置时
  使用 `10`。
- offset 使用 `(page - 1).checked_mul(size)` 计算，并在调用 Toasty 前验证 `size` 和
  offset 可转换为 `usize`，避免溢出或 panic。
- 超出最后一页不是错误：`items` 为空，`total` 和 `total_pages` 仍返回实际值。
- count 和数据查询是两个语句，不保证同一快照。需要快照一致性时，调用方在同一个
  数据库事务中执行 `fetch_page(tx)`。
- `size` 是上限。Toasty 可能在数据库返回后继续过滤，因此应用以返回的 `items` 和
  `total` 为准，不能假设非末页一定恰好有 `size` 条记录。

### Why offset first

Toasty 0.8 也提供 `Paginate` cursor API，但第一版不暴露它：

- 常见管理列表需要页码和总数，offset 模型更直接。
- Toasty cursor 是内部 `stmt::Value`，需要先定义稳定且安全的外部编码格式。
- Cursor 分页必须有确定的唯一排序；复合排序的游标比较还需要逐后端验证。
- 同时支持两种分页会让生成类型、错误和响应协议在首版明显膨胀。

Cursor 分页可以作为独立设计加入，不能复用 offset 的页码响应冒充 cursor。

## Alternatives

### Directly use `toasty::query!`

固定查询继续推荐使用 Toasty 自带宏。它已经支持编译期 `FILTER`、单字段
`ORDER BY`、`LIMIT` 和 `OFFSET`，没有必要复制其语法。

本设计不使用它来实现动态列表，因为可选筛选项和运行时排序仍需在宏外重复分支，
且 `offset` 必须在 `limit` 后调用。

### Declarative-only generation

仅使用 `macro_rules!` 可以从 `tc_query_spec!` 中显式列出的字段生成方法，但难以对字段、
操作、排序白名单、默认排序和可选分页配置区块同时提供精确 span 错误。函数式过程宏只分析
查询规格中显式声明的字段，不扫描模型，因此生成量仍与查询规格大小成正比。

本设计拒绝声明式宏承担查询规格生成。可选的 `#[derive(TcQuery)]` 负责数据库字段完整
覆盖和 extension trait，并排除关系字段；函数式 `tc_query_spec!` 独立负责某个请求允许
暴露的条件、排序、执行出口和可选分页配置。链式请求 setter 仍委托给 `bon::Builder`，不
自行实现 typestate builder。

### String-based generic builder

不接受 `HashMap<String, Value>`、`sort=name,-created_at` 直接映射数据库字段，也不通过
`Model::field_name_to_id` 动态构造表达式。字符串方案会丢失字段和值的编译期类型检查，
扩大非法字段、后端差异和注入类错误的处理面。

## Catalog changes

不修改 `BaseDs`，不修改 `base_ds` 表、字段、默认值或 URL 构造。已有数据库无需
migration。查询规格属于应用编译期代码，不存储在控制数据库中。

## Lifecycle behavior

- **Registration**：模型仍通过 `TcMgr::set_models` 或 `init_model_sets` 注册；宏不自动
  注册模型。
- **Lazy loading**：`TcMgr::get` 的连接懒加载顺序不变。条件和 query builder 只在调用方
  取得 `Db` 后执行。
- **Cache publication**：查询参数、表达式和 query builder 不进入全局连接缓存。
- **Aliases**：通过别名取得的 `Db` 与物理数据源使用相同查询规格。
- **Reload**：reload 只替换连接；已声明的 Rust 查询规格不变。应用不应跨 reload
  长期保存未执行的计划。
- **Health**：宏不隐式执行 health check；连接获取和执行错误按现有 Toasty/TcMgr
  路径返回。
- **Removal**：remove/unregister 不保存或清理查询状态，因为查询规格没有进程级状态。
- **Model sets**：宏引用的模型必须已经包含在目标数据源的 `ModelSet` 中。

## Backend behavior

| 能力 | SQLite | Turso | MySQL | PostgreSQL |
|---|---|---|---|---|
| 标量比较与 `IN` | 支持 | 支持 | 支持 | 支持 |
| 多字段排序 | 支持 | 支持 | 支持 | 支持 |
| `LIMIT` + `OFFSET` | 支持 | 支持 | 支持 | 支持 |
| `COUNT(*)` | 支持 | 支持 | 支持 | 支持 |
| `starts_with` | Toasty 下推 | Toasty 下推 | Toasty 下推 | Toasty 下推 |

排序字段白名单应优先选择不可空字段。不同数据库对 NULL 默认排序位置和文本 collation
的规则不同；宏不会伪造统一语义。跨后端结果必须一致的应用应使用非空、同类型字段，
并以非空唯一键作为 `tie_breaker`。

offset 分页会随 offset 增大而变慢，这是数据库执行特性，不在宏内通过隐藏查询改写
规避。高吞吐或深翻页场景应使用后续 cursor 设计。

## Failure behavior

转换方法返回结构化 `TcQueryBuildError`：

- `InvalidPageNumber`
- `InvalidPageSize { size, max }`
- `OffsetOverflow`
- `DuplicateSort { field }`

验证范围与出口一致：

- `into_expr()` 和 `count()` 不校验排序和分页。`in_list` 的集合语义直接交给 Toasty，
  不根据生成的方法标识符反向解析操作。
- `into_query()` 和 `all()` 再校验重复排序。
- `fetch_page()` 额外校验 page、size 和 offset。

未知筛选字段、未知数据库字段、非法操作、保留名称和字段/值类型不匹配是编译错误，
不是运行时错误。`TcQuery` 遇到没有默认分类且缺少显式配置或 `skip` 的数据库字段时，
同样在 derive 阶段失败，并尽量一次报告全部未处理字段。关系字段自动排除，不参与该
错误集合。外部排序字符串到 `.asc_<field>()` / `.desc_<field>()` 的映射失败由应用
协议层处理。

额外 `.filter(expr)` 只要求类型为 `Expr<bool>`。表达式是否引用当前模型、关系和目标
后端支持的操作由 Toasty 在 query 构建或执行阶段验证。

执行辅助方法返回统一的 `TcQueryError`，包装 `TcQueryBuildError` 和 Toasty 错误。
`fetch_page()` 中 count 成功而数据查询失败时不返回半成品 `Page`；错误直接返回。
数据查询不会修改连接缓存，失败也不会使旧连接自动失效。

## Security

- 宏只生成 Toasty AST，筛选值继续作为 Toasty 参数传递，不拼接 SQL。
- `.filter(expr)` 直接接收类型化 Toasty AST，不接受 SQL 字符串。
- 排序字段和操作来自编译时白名单；外部输入不能成为任意字段路径或 SQL 片段。
- 本 crate 不记录筛选值、分页内容或生成的 SQL。参数错误只包含字段名、数量和边界。
- `like`/`ilike` 未进入第一版，避免把调用方提供的 `%`、`_` 和 escape 规则包装成
  看似统一但实际后端不同的接口。
- 此功能不读取 `BaseDs.pwd`，不改变 URL redaction、TLS 或连接日志行为。

## Transaction behavior

`count()` 和 `all()` 各执行一个只读语句。`fetch_page()` 执行 count 和数据查询两个
只读语句；默认情况下两个语句之间的数据可能变化，因此 `total` 与 `items` 不是严格
快照。调用方可以把同一数据源的 `Transaction` 作为 executor 传入，以获得该数据库
隔离级别允许的一致性。

查询规格不访问第二个数据源，不触发 `TcTxMgr` 的多数据源提交。若应用自行在
`TcTxMgr` 回调中组合查询和写入，原有跨数据源部分提交风险不变。

## Compatibility

- 新增 API，不修改已有 `TcMgr`、`TcTxMgr` 或 Toasty 重导出。
- 不修改 `BaseDs` 或已有数据库表。
- 不新增 Cargo feature。新增 `bon = "3.9"` 直接依赖，用于生成查询参数 builder；
  `Cargo.lock` 当前已通过 Toasty 依赖图包含 `bon 3.9.3`。
- `bon` 通过隐藏路径重导出，宏展开不要求下游应用直接依赖它。该隐藏路径仅服务宏
  卫生，不承诺供应用代码调用。
- `Page<T>` 和 `Paging` 位于 `toasty_mgr::query`，不遮蔽 Toasty 的
  `toasty::stmt::Page` cursor 类型。
- 保持无 KX、无 SeaORM 依赖边界。
- `#[derive(TcQuery)]` 生成 `{Model}TcQueryExt`，并为 `QueryMany<Model>` 实现。跨模块
  调用需要显式导入该 trait，但不依赖 Toasty 0.8 的 nominal query 类型命名。
- `tc_query_spec!` 直接使用 Toasty `Model::fields()` 和 `QueryMany<Model>`，不引用
  `TcQuery` 的任何生成项，也不依赖 Toasty nominal query 类型名。
- 宏展开依赖 Toasty 0.8 的 `Model::Query`、`QueryMany`、模型 `fields()` 以及生成查询
  类型的 `filter/order_by/count/limit/offset/exec` 契约。升级 Toasty 时必须运行宏展开
  的编译测试和四后端查询测试。

## Test plan

### Unit and compile coverage

- `#[derive(Model, TcQuery)]` 遍历模型的每个命名字段，自动排除带 `#[belongs_to]`、
  `#[has_one]`、`#[has_many]` 的关系字段；所有数据库字段必须被默认分类、通过
  `#[tc_query(...)]` 显式配置或通过带原因的 `skip` 排除。
- 新增一个支持的标量、字符串、可空或有序字段后，对应 `<field>_<operation>` 与
  `asc_<field>` / `desc_<field>` 方法可以编译调用。
- 字符串 query 方法分别接受 `String`、`&String` 和 `&str`；字符串查询规格 setter
  同样接受这三种输入，不要求调用方使用 `.to_owned()`。
- `belongs_to`、`has_one`、`has_many` 和 `via` 关系不生成过滤或排序方法，也不要求
  `skip`；关系使用的独立外键数据库字段仍生成对应方法。
- 未知自定义数据库类型、集合或嵌入字段没有配置时 derive 编译失败，错误指向所有未处理
  数据库字段；增加显式 ops/sort 或 `skip = "reason"` 后通过。
- tuple struct、unit struct、enum、无原因的 `skip`、未知操作和重复生成的方法名产生
  字段级编译错误。
- `TcQuery` 生成的 extension trait 方法构造正确的 Toasty 表达式，跨模块导入 trait 后
  可以保持链式调用。
- 未 derive `TcQuery` 的最小 Toasty 模型能够独立展开 `tc_query_spec!`，生成类型及默认值
  可编译；只 derive `TcQuery` 时也不需要声明查询规格。
- `tc_query_spec!` 的未知操作、未知字段和字段/值类型不匹配分别产生编译错误。
- `CustomerSearch::builder()` 可以按任意顺序链式设置筛选字段、排序和 page/size。
- `Option<T>` 同时生成 `.field(T)` 与 `.maybe_field(Option<T>)`；省略 setter 时字段为
  `None`。
- bon typestate 在编译期拒绝同一个筛选、`page` 或 `size` setter 调用两次。
- builder 和已构建条件对象的 `.filter(Expr<bool>)` 都可以重复调用，表达式与声明条件按
  AND 合并。
- 无声明条件和额外表达式时，`into_expr()` 返回恒真的 `Expr<bool>`。
- 声明条件、多个额外表达式以及包含 OR/NOT 的额外表达式组合结果正确。
- `into_query()` 返回可继续调用 Toasty 原生 query API 的模型 query builder。
- 生成的模型 query builder 支持 `*_eq`、范围比较、字符串条件和排序快捷方法。
- 每个排序字段生成 `.asc_<field>()` 和 `.desc_<field>()`，可连续调用且保持顺序。
- 排序方法返回相同 builder typestate，不影响之前或之后的筛选 setter。
- `.asc_name().asc_name()` 和 `.asc_name().desc_name()` 都返回 `DuplicateSort`。
- `filter`、`extra_filters`、`orders`、`page`、`size`、`fetch_page`、生成排序方法与声明
  字段冲突时生成编译错误。
- 省略 page 区块时默认生成 `page = 1`、`size = 10`，并可调用 `fetch_page`；显式区块
  覆盖默认大小和最大值。
- 查询结构体拍平保存 `page`、`size`；只有 `fetch_page` 应用它们并组装 `Paging` 和
  `Page<T>`。
- 每种 portable filter 操作生成正确类型的 Toasty 表达式。
- 未提供筛选项时 `into_expr()` 返回恒真表达式，`into_query()` 的结果等价于
  `Customer::all()`。
- `count()` 不应用排序或分页；`all()` 应用排序但不应用 limit/offset。
- `fetch_page()` 使用过滤条件构造无排序 count query，并单独构造排序、limit、offset
  数据 query。
- page 0、size 0、超过 max、乘法溢出分别返回对应错误且不 panic。
- 重复排序返回结构化错误；空 `in_list` 按 Toasty 原生表达式语义执行。
- 声明了 `default_order` 时空排序使用默认顺序；自定义排序保持输入顺序；声明了
  tie-breaker 时缺失则自动追加，已存在时不重复追加。省略对应区块时不附加隐藏行为。
- rustdoc `compile_fail` 示例覆盖未知字段和字段/值类型不匹配，不引入 `trybuild`。

### SQLite and Turso

使用内存数据库插入包含相同排序值的数据，验证：

- 多个可选条件按 AND 组合。
- `.filter(expr)` 加入的条件影响 `into_query`、count、all 和分页结果。
- count 返回全部匹配记录数，即使条件对象设置了 page、size 和排序。
- all 返回全部匹配记录并遵循排序，即使条件对象设置了 page 和 size。
- 默认排序、自定义升降序和唯一键 tie-breaker 顺序稳定。
- `Customer::all().state_eq(...).name_starts_with(...).desc_created_at()` 与原生
  `fields()` 表达式结果一致。
- 第 1/2 页无重复或遗漏，总数和 `total_pages` 正确。
- 超出末页返回空 `items` 和正确总数。
- 通过 `TcMgr` 的物理编码和别名执行结果一致。

### Local MySQL and PostgreSQL

增加默认 ignored 的真实数据库测试，URL 继续读取
`TOASTY_TEST_MYSQL_URL` 和 `TOASTY_TEST_POSTGRES_URL`。测试创建唯一临时数据，覆盖
范围筛选、多字段排序、第二页和 count，并在成功与失败路径都删除测试数据；只有测试
创建了表时才删除表。

PostgreSQL 和 MySQL 测试还要验证文本排序结果由数据库 collation 决定，文档不声称
跨后端完全相同。

## Documentation

功能实现后同步：

- `src/query.rs` 的 `Paging`、`Page<T>`、查询错误及命名冲突说明。
- `TcQuery` derive 的数据库字段分类、关系字段自动排除、
  `#[tc_query(ops(...), sort)]`、显式 `skip` 和完整数据库字段覆盖编译错误。
- `tc_query_spec!` 的 `Expr`、query builder、count、all、分页展开语义和 compile-fail
  示例。
- `Cargo.toml` 的 `bon = "3.9"` 直接依赖及隐藏宏重导出。
- `toasty-mgr-macros` 的两个独立过程宏入口、重导出和 Cargo rename 路径解析。
- `docs/guide/src/api-reference.md` 的查询规格 API 速查。
- `docs/guide/src/application-integration.md` 的列表接口调用序列。
- `docs/guide/src/testing.md` 的查询规格后端测试要求。
- `docs/templates/application.rs` 的可编译 Expr、额外 filter、count、all、排序和分页示例。
- `tests/doc_templates.rs` 对模板的编译覆盖。

实现完成后，稳定的使用说明进入 guide；本设计保留取舍、兼容边界和 cursor 后续约束。
