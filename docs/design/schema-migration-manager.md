# 多数据源迁移管理与数据库结构归一化

> 本设计定义 `toasty-mgr` 的通用 migration service。应用继续负责注册 Rust 模型、选择迁移目录和暴露 CLI；`toasty-mgr` 负责按 source 获取连接、读取数据库结构、归一化为 Toasty schema、生成计划和执行迁移。

## 当前实现状态

当前阶段实现 `generate`、`baseline`、`apply`、`rollback`、`check`、`status`、
filesystem/owned/embedded artifact 输入、backend registry、PostgreSQL catalog introspection 与
`apply + AdoptBaseline`。MySQL、SQLite、Turso 各自位于独立 adapter 文件，并返回明确的
fail-closed unsupported 错误。

本设计后半部分的 versioned `plan`/`apply_plan` manifest、checksum/lineage、跨进程 migration
group lock、table comment/enum 完整归一化，以及 MySQL/SQLite/Turso catalog 实现属于后续阶段，
不是当前公开 API。当前部署必须在同一物理数据库外部串行执行 `migration apply`；不能把后续设计
描述当成已经生效的运行时保护。

## 问题

`toasty-mgr` 已经拥有以下信息：

- source code 与 alias；
- 每个 source 注册的 Toasty `ModelSet`；
- `Db`、driver capability、连接 URL/backend 元数据；
- PostgreSQL、MySQL、SQLite、Turso feature 边界。

但迁移目前仍由每个应用自行实现。应用通常需要重复解决：

1. 类似 `toasty-cli migration generate/apply/check/status` 的文件迁移流程；
2. 多 source 的迁移目录、连接和模型选择；
3. 从实时数据库 catalog 读取表、列、主键和索引；
4. 把不同数据库返回的 metadata 归一化为 Toasty `schema::db::Schema`；
5. 将实时数据库 schema 与当前模型 schema 比较，生成可审查、可执行的 SQL plan；
6. 对 unsupported metadata、破坏性 DDL、计划过期和共享数据库迁移 ID 冲突进行保护。

直接在 kx-adm 中实现 PostgreSQL introspection 会把通用数据库逻辑绑到业务聚合 crate，后续 MySQL、SQLite、Turso 或新 driver 接入时还会再次复制。该能力应归属 `toasty-mgr`；应用只提供策略。

## 目标与非目标

### 目标

- 提供不依赖 `clap` 的 typed migration API，应用可以映射成自己的 CLI。
- 当前支持 `generate`、`apply`、`check`、`status` 和 `baseline`；为后续 `plan`、`apply_plan`
  保留 backend-neutral 设计。
- 同时支持三种 previous schema 来源：最新 snapshot、实时数据库、空 schema。
- 使用 `toasty::sql::query/statement` 执行 backend-specific raw SQL。
- 通过公开 `MigrationBackend` trait 注册 introspection、归一化、锁和 DDL capability，新增 backend
  不修改 migration core。
- 对观测到但 Toasty 无法表达的数据库对象 fail closed，不静默丢失语义。
- 生成计划携带数据库和模型 fingerprint，执行前检测 stale plan。

### 非目标

- 不在 `toasty-mgr` 中固定任何应用 CLI 路径、全局 migration ID 区间或发布目录。
- 不自动扫描所有 `TcMgr::all_codes()` 并迁移；只有显式注册的 migration source 才可操作。
- 不把 `push_schema()` 作为持久化数据库迁移策略。
- 不尝试从当前模型恢复已经丢失的历史数据迁移、rename 决策或回填逻辑。
- 不为 DynamoDB 伪造 SQL introspection；非 SQL backend 返回明确 unsupported。
- 不管理数据库 foreign key 或 cascade action。

## 公开 API

### Migration source registry

应用在注册模型后显式注册 migration source：

```rust
pub struct MigrationSourceConfig {
    pub code: String,
    /// generate/check 使用的可写、可审查 artifact 目录。
    pub artifact_root: PathBuf,
    /// 指向同一物理数据库的 source 使用同一个 group，以便应用层串行编排。
    pub migration_group: MigrationGroupKey,
    pub backend_override: Option<BackendId>,
    pub namespace: Option<String>,
    pub scope: SchemaScope,
    pub id_allocator: Arc<dyn MigrationIdAllocator>,
    pub sql_policy: Arc<dyn MigrationSqlPolicy>,
}

pub enum SchemaScope {
    /// 只管理当前模型表、最新 snapshot 表和显式 rename 来源。
    Managed,
    /// 管理显式表名集合。
    Tables(BTreeSet<String>),
    /// 管理匹配前缀的表；适合共享 database/schema。
    Prefixes(Vec<String>),
    /// 整个 namespace 归当前 source 独占。
    NamespaceExclusive,
}
```

- `code` 必须对应 `TcMgr::models(code)` 和可加载的 `TcMgr::get(code)`。
- alias 不自动继承 migration 配置；需要迁移独立模型 alias 时必须显式注册。
- `namespace` 缺省时由 backend introspector 查询当前 schema/database；不能只从 URL 猜测。
- `SchemaScope::Managed` 是默认值，避免 single database 中一个 source 把其他 source 的表识别为待删除对象。
- `NamespaceExclusive` 必须显式配置，不能由连接是否独立自动推断。
- `artifact_root` 是 authoring store，不等于生产 apply 时的隐式 filesystem fallback。
  apply 的 artifact 来源必须在 source 注册阶段显式绑定。
- 每个 source 拥有独立 ID 空间；空谱系从 `1` 开始，已有 history 使用本 source 最大 ID 加一。
- artifact 文件名使用 `<source_code>_<id>_<slug>.sql`，source code 只参与可读命名，不编码进数据库
  数字 ID。
- manager 按 `migration_group` 获取数据库锁和串行编排；applied/unknown 判定由复合键中的
  `source_code` 隔离，不再为共享数据库分配数字区间。

### Migration manager

```rust
pub struct TcMigrationMgr;

impl TcMigrationMgr {
    pub fn register_model_sources(config: MigrationSourcesConfig) -> Result<Vec<String>>;
    pub fn register_source(config: MigrationSourceConfig) -> Result<()>;
    pub fn set_registered_artifacts(source: &str, value: MigrationArtifactInput) -> Result<()>;
    pub fn register_backend(value: Arc<dyn MigrationBackend>) -> Result<()>;

    pub async fn generate(request: MigrationGenerateRequest) -> Result<MigrationGenerateOutcome>;
    pub async fn generate_all(name: impl Into<String>) -> Result<Vec<MigrationGenerateOutcome>>;
    pub async fn sync(source: &str, name: &str)
        -> Result<(MigrationGenerateOutcome, MigrationApplyReport)>;
    pub async fn sync_all(name: impl Into<String>)
        -> Result<Vec<(MigrationGenerateOutcome, MigrationApplyReport)>>;
    pub async fn baseline(source: &str, name: &str) -> Result<MigrationGenerateOutcome>;
    pub async fn apply(source: &str, mode: MigrationApplyMode) -> Result<MigrationApplyReport>;
    pub async fn apply_all() -> Result<MigrationApplyReport>;
    pub async fn rollback(source: &str, selection: MigrationRollbackSelection)
        -> Result<MigrationRollbackReport>;
    pub async fn check(source: Option<&str>) -> Result<MigrationCheckReport>;
    pub async fn status(source: &str, inspect_database: bool)
        -> Result<MigrationStatusReport>;
    pub async fn status_all(inspect_database: bool) -> Result<Vec<MigrationStatusReport>>;
}
```

应用通常只调用 `register_model_sources()`：manager 从 `TcMgr::model_codes()` 发现全部 ModelSet，
保证隐式 `base` 排在第一位，并按 `<artifact_root>/<source_code>` 建立 source 配置。单独
`register_source()` 只保留给需要覆盖 namespace/scope/backend 的特殊 source。应用直接复用本模块
的 outcome/report，不再声明一组镜像 DTO，也不增加只调用 manager 一次的 facade。
`apply_all()`、`status_all()`、`generate_all()` 与 `sync_all()` 负责真正包含循环、预检和汇总的多
source 编排。`sync` 固定使用 `SchemaOrigin::LiveDatabase`，先写 tracked artifact，再通过同一个
apply engine 执行；空谱系观察到既存表时自动选择经 live schema 校验的 `AdoptBaseline`。
single-database 配置仍逐 logical source 执行，依靠 `SchemaScope` 和复合账本隔离。

artifact 来源在 source 注册阶段显式绑定：

```rust
pub enum MigrationArtifactInput {
    /// 从已注册 source 的 artifact_root 读取；适合 toasty-cli 和源码工作区。
    RegisteredFilesystem,
    /// 调用方已经加载并校验所有权的数据。
    Owned(MigrationArtifactSet),
    /// 发布二进制编译期嵌入的数据。
    Embedded(MigrationSet),
}

pub enum MigrationApplyMode {
    Execute,
    AdoptBaseline,
}
```

`register_source()` 默认绑定 `RegisteredFilesystem`；发布构建在注册 source 后调用
`set_registered_artifacts(source, Embedded(set))` 覆盖。三种输入先转换为同一个 owned migration
view，再进入同一 apply engine。manager 不根据文件是否存在自动在 filesystem/embedded 之间回退，
避免生产主机意外执行工作目录中的未发布 SQL。

`apply/rollback/status` 都从同一个 source 注册项解析 artifact input；不能只有 apply 选择 embedded，
而 status 又默认读取 `artifact_root`。`generate`、`baseline` 和 artifact write check 固定使用
source 的 authoring store。

generate/plan 请求使用同一 previous schema 枚举：

```rust
pub enum SchemaOrigin {
    Auto,
    LatestSnapshot,
    LiveDatabase,
    Empty,
}
```

- `generate + Auto`：默认行为；artifact 为 `Missing`/合法 `Empty` 时从空 schema 生成首迁移，
  为 `Ready` 时从最新 snapshot 追加，`Partial`/`Invalid` 时失败。
- `generate + LatestSnapshot`：标准发布迁移，从最新 snapshot 追加 SQL/snapshot/history。
- `generate + LiveDatabase`：接管已有数据库并保留 inspection diagnostics，但不得把 live database
  到 model 的 delta 直接作为 history 首迁移或追加到
  drifted history。空谱系时它生成可重放的 `Empty -> ObservedSchema` baseline artifact，再从该
  snapshot 生成 `ObservedSchema -> Model`；已有谱系时必须先证明 live database 与最新 snapshot
  fingerprint 相同，否则返回 `tracked_schema_drift`。
- `generate + Empty`：显式要求创建首迁移，只允许 `Missing`/合法 `Empty` 且不覆盖产物；应用 CLI
  通常使用 `Auto`，不需要再暴露独立 bootstrap 命令。
- `baseline`：只用于接管 legacy database。它 introspect live database，并生成完整可重放的
  `Empty -> ObservedSchema` SQL 和 observed snapshot，不生成 no-op migration，也不修改数据库。
- legacy database 通过 `apply + AdoptBaseline` 接管：重新 introspect 并确认 live fingerprint 与
  baseline snapshot 完全一致后，只跳过首个 baseline SQL并记录该 ID/checksum；随后在同一把
  同一 apply 调用中正常执行 history 中剩余 pending delta。全新数据库使用 `Execute`，正常执行
  baseline SQL 和全部后续 migration。
- `plan + LiveDatabase`：生成临时 SQL plan，不写 history。
- `apply`：应用已有 tracked artifacts，并记录 `__toasty_mgr_migrations`。
- `rollback`：只操作一个 source。`--steps N` 从最新记录开始回滚 N 条；`--target ID` 从最新记录
  倒序回滚并包含目标 ID。目标不是已应用迁移、applied 顺序与 tracked lineage 不一致、或任一条
  缺少逆向 artifact 时，在执行第一条 DDL 前失败。
- `apply_plan`：应用带 fingerprint 的临时计划，默认不记录 migration history；上层必须提供开发/测试安全策略。
- `check`：校验 artifact 完整性、snapshot/model parity、ID/名称和应用提供的 SQL policy。
- `status`：返回 tracked、applied、pending、unknown-applied 和 drift 摘要，不修改文件或数据库。

本功能不修改 Toasty driver，也不调用其固定的
`applied_migrations()`/`apply_migration()`。`MigrationBackend` 通过 Toasty 已公开的
`Db::transaction()` 与 `toasty::sql::{query, statement}` 自行维护复合账本；`status` 只读查询，
不创建账本。PostgreSQL 的单条 apply/rollback 在同一事务中执行 DDL 和账本写入/删除。

```sql
CREATE TABLE __toasty_mgr_migrations (
    source_code TEXT NOT NULL,
    id BIGINT NOT NULL,
    name TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (source_code, id)
);
```

表中不建立 foreign key。旧 `__toasty_migrations` 不读取、不导入也不修改；已有数据库通过
`generate --from database` 生成 observed baseline 与模型差异，再使用经过实时结构校验的
`apply --adopt-baseline` 建立新复合账本。

### CLI 映射

`toasty-mgr` core 不依赖 `clap`。应用可以稳定映射为：

```text
migration generate [--source <code>] [--name <slug>]
migration generate --from database --source <code>
migration baseline --source <code> --name <slug>
migration apply [--source <code>]
migration apply --source <code> --adopt-baseline
migration rollback --source <code> [--steps <n> | --target <id>]
migration check [--source <code>]
migration status [--source <code>]
```

`--adopt-baseline` 当前只接受 registered filesystem artifacts，并且只允许首个 tracked artifact、当前 source domain 内无 applied ID、且 live
normalized fingerprint 与 baseline snapshot 相同；它不是任意 `mark-applied`。记录 baseline 后在
同一临界区继续普通 apply。后续若多个应用需要完全相同的 clap surface，可以另加可选 `cli`
feature；第一阶段不让命令行依赖进入 migration core。

## 模块边界

建议目录：

```text
src/migration/
  mod.rs
  manager.rs
  artifact.rs
  types.rs
  backend/
    mod.rs
    postgresql.rs
    mysql.rs
    sqlite.rs
    turso.rs
```

- `manager`：解析 source、连接、模型和 workflow；不包含 backend SQL。
- `artifact`：读取和原子写入 history、snapshot、SQL。
- `types`：source、request/report、rollback selection 和 schema origin。
- `backend/mod.rs`：只定义 trait、backend-neutral observed types 和 re-export。
- `backend/<database>.rs`：每种数据库独立保存 catalog SQL、类型归一化及 ledger/apply/rollback；禁止把多个数据库实现合并进同一文件。

当前交付完整实现 PostgreSQL、MySQL、SQLite 与 Turso。每个 backend 仍在独立文件实现 trait、声明
backend ID/alias，并拥有对应账本与锁语义。SQLite 与 Turso 只复用经过两种 driver 集成测试的
SQLite-family catalog/归一化/事务执行 helper；不能因 URL 或 SQL 方言相似而把 Turso 静默注册成
SQLite backend。

Toasty 继续拥有 schema diff 和 driver DDL generation；`toasty-mgr` 不复制
`schema::diff` 或 PostgreSQL/MySQL/SQLite migration serializer。

## Backend migration trait

### Backend 标识

backend ID 使用开放字符串 newtype，不使用封闭 enum：

```rust
pub struct BackendId(String);
```

内置 ID 为 `postgresql`、`mysql`、`sqlite`、`turso`。解析顺序：

1. source config 的显式 backend override；
2. `Db::capability().driver_name` 的已注册映射；
3. `TcConnMeta.db_type` 只作为一致性校验和错误信息，不作为唯一事实来源。

这样未来 driver 可以注册新的 ID/alias，不需要扩展 core enum。

### Trait

trait 必须 dyn-compatible，同时拥有 introspection、归一化、迁移锁和 DDL capability，避免每增加
一个 backend 就修改 manager 泛型：

```rust
pub type InspectFuture<'a> = Pin<
    Box<dyn Future<Output = Result<ObservedSchema>> + Send + 'a>,
>;

pub type LockFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Box<dyn MigrationLockGuard + 'a>>> + Send + 'a>,
>;

pub trait MigrationBackend: Send + Sync + 'static {
    fn backend_id(&self) -> &BackendId;
    fn aliases(&self) -> &[&str];
    fn ddl_atomicity(&self) -> DdlAtomicity;

    fn inspect<'a>(
        &'a self,
        context: SchemaInspectContext<'a>,
    ) -> InspectFuture<'a>;

    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema>;

    // ledger/apply/rollback 方法由 adapter 在同一锁/事务边界内实现。
}

pub struct SchemaInspectContext<'a> {
    pub source_code: &'a str,
    pub namespace: Option<&'a str>,
    pub scope: &'a ResolvedSchemaScope,
    pub executor: &'a mut dyn Executor,
}

pub struct SchemaNormalizeContext<'a> {
    pub scope: &'a ResolvedSchemaScope,
    pub target: &'a db::Schema,
    pub rename_hints: &'a RenameHints,
}

pub enum DdlAtomicity {
    Transactional,
    ImplicitCommit,
    Unsupported,
}
```

`normalize` 接收当前模型 schema 作为逻辑类型提示：catalog 仍决定 storage type、nullability、identity
与索引事实；仅当物理表示等价时复用 target 的 `stmt::Type` 或 `Document/List/Enum` 语义。

manager 先根据 `SchemaScope`、target、latest snapshot 和 rename hints 解析出不可变的
`ResolvedSchemaScope`，避免 adapter 自己猜测哪些表属于当前 source。每个 backend adapter 同时拥有
catalog 查询和 native type/default/identity/index 到 Toasty 表示的转换逻辑；公共 helper 只负责确定性
排序、ID 分配、diagnostic 汇总和 fingerprint。新增 backend 通过注册新的 trait object 接入，
不在 manager 中增加 `match backend` 分支。

apply 按 physical `migration_group` 获取锁，锁必须覆盖“读取 applied IDs -> 校验 -> 执行全部
group migration -> 写最终记录”的完整临界区：

- PostgreSQL：transaction-level/session-level advisory lock；
- MySQL：按当前 database 获取迁移专用 `GET_LOCK`/`RELEASE_LOCK`，同库 logical source 串行且不得
  使用 `RELEASE_ALL_LOCKS()` 影响业务 advisory lock；明确 DDL 为 `ImplicitCommit`，执行前写
  `__toasty_mgr_migration_runs`，异常残留时返回 `migration_recovery_ambiguous`，不得重复猜测执行；
- SQLite：`BEGIN IMMEDIATE` 或 `BEGIN EXCLUSIVE`；
- Turso：使用经过集成测试确认的 `BEGIN IMMEDIATE` 写事务语义。

`MigrationLockGuard` 持有专用 connection，并暴露实际执行使用的 executor。SQLite/Turso 若用写事务
实现锁，该 guard 同时拥有事务，公共 apply engine 不得在其中再次 `BEGIN`；PostgreSQL/MySQL 的
session lock 则允许按 backend capability 创建每 migration transaction。

锁超时返回 `migration_lock_timeout: <group>`，不得退化为无锁执行。

introspector 通过：

```rust
toasty::sql::query(sql)
    .bind(...)
    .column_types([...])
    .exec(context.executor)
    .await
```

执行 catalog SQL。每个 adapter 自己拥有 SQL 和 positional row decoder；bind placeholder 必须根据
`Capability::sql_placeholder` 生成，不能跨 backend 固定写成 `?`、`?1` 或 `$1`。不构造一条所谓跨数据库通用 `information_schema` 查询。

## Backend-neutral 观测模型

不能把 raw SQL 行直接转换后丢弃信息。先构建：

```rust
pub struct ObservedSchema {
    pub backend: BackendId,
    pub namespace: String,
    pub tables: Vec<ObservedTable>,
    pub diagnostics: Vec<SchemaDiagnostic>,
}

pub struct ObservedTable {
    pub name: String,
    pub kind: TableKind,
    pub comment: Option<String>,
    pub columns: Vec<ObservedColumn>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<ObservedIndex>,
    pub constraints: Vec<ObservedConstraint>,
}

pub struct ObservedColumn {
    pub ordinal: u32,
    pub name: String,
    pub native_type: String,
    pub storage_type: Option<db::Type>,
    pub nullable: bool,
    pub identity: bool,
    pub generated: bool,
    pub default_sql: Option<String>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

pub struct ObservedIndex {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
    pub method: Option<String>,
    pub columns: Vec<ObservedIndexPart>,
    pub predicate: Option<String>,
    pub included_columns: Vec<String>,
}
```

当前代码中的轻量观测结构使用 `auto_increment` 表示上述 `identity`，`diagnostics` 保存稳定错误码与
对象路径；遇到 generated column、foreign key、CHECK、partial/expression index、SQLite STRICT/
WITHOUT ROWID 等当前不能无损表达的结构时，`normalize` fail closed，不生成可能丢结构的迁移。

`ObservedConstraint` 至少记录 foreign key、check、exclude 和 backend-specific constraint。它们即使不进入 Toasty `db::Schema`，也必须进入 diagnostics 和 fingerprint。

## 归一化为 Toasty schema

`MigrationBackend::normalize(observed, context)` 返回：

```rust
pub struct NormalizedSchema {
    pub schema: db::Schema,
    pub diagnostics: Vec<SchemaDiagnostic>,
    pub observed_fingerprint: SchemaFingerprint,
}
```

规则：

1. 只纳入 `SchemaScope` 拥有的普通表；排除数据库系统表和 `__toasty_migrations`。
2. 表、列和索引按确定性顺序排列并重新分配 Toasty ID。
3. 同名目标列复用 target 的 `stmt::Type`，但 nullable、storage type、identity、主键等取实时数据库事实。数据库 catalog 无法完整还原 Rust logical type。
4. database-only 列只映射生成 drop/diagnostic 所需的最小 `stmt::Type`。
5. rename hints 在归一化完成后解析为 previous ID 到 target ID，不能依赖 catalog 返回顺序。
6. 等价 backend 类型归一到同一 Toasty `db::Type`；PostgreSQL `int8/bigint`、MySQL
   `BIGINT` 和 SQLite integer affinity 的映射分别由对应 adapter 实现，core 不维护后端类型表。
7. default、generated expression、collation、partial/expression index、include column、foreign key 和 check constraint 等 Toasty 当前不能表达的属性不得静默丢弃。

诊断级别：

| 级别 | 行为 |
|---|---|
| `Info` | 已知且无迁移影响的 backend 细节 |
| `Warning` | 可稳定保留但 Toasty 不比较的属性；计划中必须展示 |
| `Blocked` | 生成的 DDL 可能丢失或错误重建该属性；拒绝 generate/apply |

只要 `Blocked` 位于 managed table，默认拒绝生成。调用方不能用一个全局
`ignore_unsupported` 跳过；未来放宽必须按 diagnostic code 和 object 精确授权。

## 内置 adapter

### PostgreSQL

使用 `pg_catalog` 和 `information_schema` 查询：

- table/schema、column ordinal/nullability；
- `format_type`、typmod、array、JSON/JSONB、UUID、numeric、timestamp/date/time；
- identity/sequence ownership；
- primary key、普通/唯一索引、列顺序、sort direction；
- enum type 和 labels；
- table/column comments；
- index predicate/expression/include/method；
- foreign key/check/generated column，仅进入 diagnostics。

### MySQL

使用 `information_schema.TABLES`、`COLUMNS`、`STATISTICS`、
`TABLE_CONSTRAINTS`、`KEY_COLUMN_USAGE`：

- 区分 signed/unsigned integer、varchar 长度、decimal precision/scale、JSON、datetime/time；
- `AUTO_INCREMENT` 转换为 identity；
- inline ENUM 解析为 unnamed `TypeEnum`；
- prefix/expression index、generated column、collation 和 foreign key 进入 diagnostics。
- migration apply 使用当前 database 对应的 connection-scoped `GET_LOCK`；连接池 checkout 只尝试
  清理同名迁移锁，不释放业务锁。由于 DDL implicit commit，run marker 在 DDL 前持久化，成功写复合
  账本后删除。发现遗留 run marker 时停止并要求人工核对 live schema。

### SQLite

使用 `sqlite_schema`、`PRAGMA table_xinfo`、`index_list`、`index_xinfo`：

- 按 SQLite affinity 归一化 native type；
- 保留 `INTEGER PRIMARY KEY` 的自增语义区别；
- 读取 index columns 和 uniqueness；
- `WITHOUT ROWID`、STRICT table、expression/partial index、generated column、CHECK 和 foreign key 进入 diagnostics；
- 只有 driver migration capability 明确支持时才生成 table rebuild。
- apply/record/rollback 使用 `BEGIN IMMEDIATE`，DDL 与复合账本在同一事务提交。

### Turso

提供独立 `TursoMigrationBackend`，内部可复用 SQLite catalog reader，但必须通过 Turso integration
tests 证明 PRAGMA、transaction、migration lock 和返回类型兼容。不能仅因 URL 类似 SQLite 就静默
选择 SQLite backend。

Turso adapter 与 SQLite adapter 的 ID、alias、trait impl 保持独立；共享 helper 不包含 backend
注册或 facade API，避免复制两份完全相同的 PRAGMA decoder 和账本事务代码。

## Plan、生成与执行

### MigrationPlan

计划至少包含：

- source code、backend ID、namespace；
- previous origin；
- observed/snapshot fingerprint；
- current model fingerprint；
- rename hints；
- 结构化 changes 和风险级别；
- diagnostics；
- driver 生成的 SQL；
- 是否允许 transaction、是否包含 destructive DDL。

计划不是裸 `.sql` 文件。标准持久化格式为 versioned TOML manifest，例如
`auth.plan.toml`，其中包含 canonical metadata、fingerprint、diagnostics、risk、按顺序排列的 SQL
statements 及每条 statement checksum。可以额外导出只读 `.sql` 供人工 review，但
`apply_plan` 只接受 manifest。manifest 的整体 checksum 根据去除自身 checksum 字段后的 canonical
序列化计算，避免把可编辑注释当安全边界。

风险分级：

| 风险 | 示例 | 默认行为 |
|---|---|---|
| `Safe` | create table、add nullable column、create index | 可生成 |
| `Review` | add NOT NULL with backend default、index rebuild | 生成但 apply 需显式策略 |
| `Destructive` | drop table/column、type narrowing、set NOT NULL | apply 默认拒绝 |
| `Blocked` | 无法保留的 expression index/generated column/constraint | 不生成可执行 SQL |

### Stale plan 防护

`apply_plan` 执行前必须：

1. 重新 introspect 当前数据库；
2. 比较 observed fingerprint；
3. 重新读取当前模型 schema 并比较 model fingerprint；
4. 校验 backend、namespace、source 和 SQL checksum；
5. 任一不一致则返回 `migration_plan_stale`，不执行任何 DDL。
6. backend 必须报告 `DdlAtomicity::Transactional`；首版对 `ImplicitCommit`/`Unsupported` 只允许
   plan，不允许 apply-plan，返回 `migration_plan_non_atomic_backend`。

### Tracked apply

`apply` 读取完整 history 和 SQL，校验后按顺序执行 pending migration，并由 backend 写入
`__toasty_mgr_migrations`。每条 PostgreSQL migration 的全部 statement 和复合账本 INSERT 位于同一
事务。manager 先读取当前 source 的 applied records，并拒绝该 source 的 unknown ID。

unknown applied 判定规则：复合账本中 `source_code = 当前 source` 且 ID 不在该 source history 即阻止；
其他 source 的相同数字 ID 与当前 source 无关。

生成器同时计算 `previous -> current` 和 `current -> previous`。正向 SQL 写入 `migrations/`，逆向
SQL 写入 `rollbacks/`，二者使用相同文件名。无法生成逆向 SQL 时仍可保留正向迁移，但该迁移明确
不可自动 rollback；`rollback` 必须 fail closed。旧 history 可继续 apply，只有存在匹配逆向 artifact
的迁移可回滚。embedded provider 若未携带逆向 artifact，同样拒绝 rollback。

`DdlAtomicity::Transactional` backend 在同一事务内执行 SQL 和 applied record。对于 MySQL 等
`ImplicitCommit` backend，不能假装 rollback 有效：Toasty 公共 apply engine 增加
`__toasty_migration_runs` 恢复账本，记录 migration checksum、statement index、每步 before/after
normalized fingerprint 和状态。崩溃重试时，只有 live fingerprint 精确匹配 before 或 after 才能
继续或确认该步；处于两者之外时返回 `migration_recovery_ambiguous`，不得自动插入 applied record。
无法生成稳定 per-step fingerprint 的 raw/data SQL 在 non-transactional backend 默认拒绝，除非
artifact 明确携带应用提供的幂等 recovery policy。

`MigrationArtifactSet`、`OwnedMigrationFile`、filesystem loader 和 owned apply engine 全部归属
`toasty-mgr::migration`。embedded `MigrationSet` 通过公开的 `migrations()` 转换为 owned view；
filesystem history 由 manager 加载。执行时 manager 直接使用 Toasty 已公开的 driver connection
API，不修改 Toasty 源码，也不要求 `toasty-cli` 与 `toasty-mgr` 共用实现。

Toasty `history.toml` 继续保持 format v1 兼容。lineage、SQL/snapshot checksum、previous/next
fingerprint 和 recovery metadata 若启用，存放在 `toasty-mgr` 管理的独立 manifest 中，避免要求
Toasty parser 或 `embed_migrations!` 认识扩展格式。filesystem 与 embedded 输入必须返回相同的
applied/skipped/failed report 语义。

### Temporary apply

`apply_plan` 默认不写 tracked history，只适合开发/测试临时同步。调用方必须传入显式
`MigrationPlanApplyPolicy`，至少声明：

- 是否允许 destructive change；
- 是否要求数据库为空；
- 是否允许指定 diagnostic code；

`apply_plan` 永远不写正式 history。需要进入生产谱系时，应使用
可重放的 baseline/snapshot generate 固化 SQL/snapshot/history，再执行普通 `apply`，不能把
temporary plan 当发布记录。执行过 temporary plan 的数据库默认为 disposable；在应用正式 tracked
migration 前必须重建，不能把“live schema 看起来已经一致”自动转换成 applied record。

## Artifact 与原子性

- `generate` 先写同一 artifact root 下的临时目录，校验通过后 rename 到正式文件。
- 不覆盖已存在 SQL/snapshot；history 只在配套文件成功落盘后替换。
- Toasty `history.toml` 继续使用 format v1，确保现有 `embed_migrations!` 可直接消费。manager 扩展的
  SQL/snapshot checksum、previous/next fingerprint 和 lineage ID 后续写入独立 manifest；manifest
  未生成前不得伪装已有 checksum/recovery 保证。
- 单 source 文件原子性由 temp + rename 保证；多 source 不声称跨目录原子提交。
- 多 source generate 先完成所有内存 plan 和全局 policy 检查，再逐 source 落盘。
- 数据库 apply 失败后保留 artifact；后续按 applied migration ID 幂等续跑。

## Feature 与依赖

建议增加：

```toml
[features]
migration = ["toasty/migration", "toasty/serde"]
schema-introspection = ["migration"]
```

内置 backend 继续跟随现有 `postgresql`、`mysql`、`sqlite`、`turso` feature。未启用对应 feature
时不注册。core 不直接依赖各 driver crate；通过 `Db`、`Executor`、raw SQL 和 capability 工作。

第一阶段不增加 `clap`、SQL parser 或数据库 client 依赖。需要解析 SQLite/MySQL DDL
中无法由 catalog/PRAGMA 给出的结构时，先返回 `Blocked` diagnostic；只有经过单独依赖评估后才引入 parser。

## 失败与安全

- backend 无 adapter：`schema_introspection_unsupported: <backend>`。
- source 未注册 migration config：`migration_source_not_registered: <source>`。
- 观测结构无法安全归一化：`schema_normalization_blocked: <diagnostic_code> <object>`。
- plan 已过期：`migration_plan_stale`。
- history 与数据库出现 group-level unknown applied ID 时，`status/check` 报错，普通 apply 不继续。
- 无法取得 migration lock：`migration_lock_timeout: <group>`。
- non-transactional recovery 无法判定：`migration_recovery_ambiguous`。
- raw catalog SQL 只绑定 namespace/table 参数；不能拼接未经 backend identifier quote 处理的用户输入。
- 日志只记录 source、backend、namespace、object、change/risk 和 checksum，不记录 URL password。
- introspection 必须只读；apply 必须通过 backend adapter 获取并持有 physical group lock，不能把
  并发正确性留给调用方部署约定。
- 不生成 foreign key、`REFERENCES`、`ON DELETE CASCADE` 或 `ON UPDATE CASCADE`；应用可以通过 `MigrationSqlPolicy` 增加更严格规则。

## 数据与兼容性

本功能不修改 `base_ds` 表及任何业务表定义，不新增数据库 foreign key。migration ledger 属于
`toasty-mgr` 内部 schema：

- `__toasty_mgr_migrations` 使用 `(source_code, id)` 复合主键；各 source 可独立从 `1` 递增；
- 旧 `__toasty_migrations` 不属于本管理器的数据来源；
- 后续 checksum/lineage 字段只扩展 `__toasty_mgr_migrations`，不要求修改 Toasty driver；
- `__toasty_migration_runs` 只用于 `ImplicitCommit` backend 的崩溃恢复，保存 step 状态和
  fingerprint，不保存业务结果；
- 两张内部表之间不建立 foreign key；

`toasty-mgr` 仍处于 `1.0.0` 前，可以增加 `migration` feature 和 public API；已有
`TcMgr::set_models/get/reload` 行为保持不变。默认 feature 是否启用 migration 由实现评估决定，优先保持 migration 为显式 opt-in，避免未使用方承担文件和 serde 能力。

## 测试计划

### 单元测试

- backend registry、alias 和 source override 解析。
- positional raw row decoder 对 null、整数宽度、bool、string 和 record 长度严格报错。
- 四个 adapter 的 native type 到 `db::Type` 映射矩阵。
- deterministic table/column/index ID 和 fingerprint。
- scope 不包含其他 source/system/migration tables。
- unsupported metadata 产生正确 diagnostic 和 fail-closed 行为。
- rename hints 在 catalog 顺序变化后仍映射正确。
- plan risk 分类和 stale fingerprint。
- artifact 临时写入、checksum、拒绝覆盖和失败清理。
- migration group 与不重叠 ID domain 注册、group-level unknown 判定。
- versioned plan manifest canonical checksum；裸 SQL 不能用于 apply-plan。

### Toasty migration 回归

- snapshot -> model、database -> model、empty -> model 三种 origin 产生预期 diff。
- `generate` 无 diff 不落文件；`Auto` 在空谱系且有非空模型时生成首迁移。
- baseline 从 Empty 到 live observed schema 生成完整 tracked artifact，fresh database 可重放。
- adopt baseline 只有在 live fingerprint 与 baseline snapshot 完全一致、无 applied ID 时成功。
- `generate + LiveDatabase` 在空谱系生成 baseline + delta；已有谱系存在 live drift 时拒绝。
- filesystem apply 与 embedded apply 对相同 ID 的 applied/skipped 结果一致。
- embedded artifact 携带与 filesystem 相同的 lineage、history/SQL/snapshot checksum 和 fingerprint。
- `toasty-cli` 改用公共 apply API 后现有命令行为不变。

### 数据库集成测试

每个启用 backend 至少覆盖：

- 创建包含主键、普通/唯一索引、nullable、常用标量、JSON/document、时间类型的 fixture；
- introspect -> normalize 后与等价 Toasty model schema 无 diff；
- 人工制造缺列、错类型和缺索引后生成预期 SQL；
- unsupported expression/partial/generated/foreign-key fixture 被阻止；
- plan 生成后修改数据库，`apply_plan` 返回 stale；
- transactional backend 的 tracked apply 执行两次，第二次全部 skipped；
- 并发 apply 由 backend migration lock 串行化，锁超时不执行 DDL；
- shared group 的其他 source ID 不算 unknown，domain 内孤立 ID 阻止 apply；
- implicit-commit backend 在每个 crash window 重试时依据 step fingerprint 继续或明确 blocked；
- implicit-commit backend 的 apply-plan 返回 `migration_plan_non_atomic_backend`。

PostgreSQL 额外覆盖 enum、array、JSONB、identity 和 comment；MySQL 覆盖 unsigned、inline enum 和 auto increment；SQLite/Turso 覆盖 affinity、PRAGMA index 和 table rebuild 边界。

## 文档

实现时更新：

- `README.md`：从“不替代 migration 工具”调整为“提供可选 migration service，应用拥有 CLI 和发布策略”。
- `docs/guide/src/introduction.md`、`architecture.md`、`operations.md`、`api-reference.md`。
- `docs/templates/application.rs`：展示 model、connection 和 migration source 注册顺序。
- 新增 migration、schema introspection 和自定义 backend adapter guide。
- kx-adm 的 migration 设计、README、guide 和 repository skills 改为调用 `TcMigrationMgr`，不保留 PostgreSQL introspection 副本。
