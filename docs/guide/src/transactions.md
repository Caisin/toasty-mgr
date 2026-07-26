# 事务

`TcTxMgr` 是 Toasty transaction 的管理层。它按需为每个数据源打开一个事务，
让服务代码在同一个回调中使用多个数据源。

## 单数据源事务

```rust,ignore
TcTxMgr::trans("tenant_a", async |tx| {
    toasty_mgr::create!(Customer {
        id: 1,
        name: "Alice",
    })
    .exec(tx)
    .await?;
    Ok(())
})
.await?;
```

回调返回 `Ok` 时提交，返回 `Err` 时回滚。获取连接或开始事务失败时回调不会运行。

## 可重试事务

乐观并发更新应重试完整事务，而不是只重放最后一条 update：

```rust,ignore
const MAX_ATTEMPTS: usize = 3;

TcTxMgr::trans_on_condition_failed(
    "tenant_a",
    MAX_ATTEMPTS,
    async move |tx| {
        let mut customer = Customer::get_by_id(tx, &customer_id).await?;
        customer.name.clone_from(&new_name);
        customer.update().exec(tx).await?;
        Ok(customer)
    },
)
.await?;
```

`max_attempts` 包含第一次执行且必须大于零。每次失败事务完成回滚后才开始下一次；
`trans_on_condition_failed` 只重试 Toasty condition-failed，包括包裹在
`anyhow` context 中的错误，验证错误和其他 driver 错误立即返回。需要自定义临时错误
分类时使用 `trans_with_retry(code, max_attempts, should_retry, callback)`。

可重试 callback 是 `AsyncFnMut`。它必须拥有跨 attempt 使用的数据；需要借用请求时，
在 `async move` callback 内为当前 attempt clone 一份请求，不能长期持有 handler 栈引用。

## 可复用执行函数

只需要执行 Toasty 语句、不负责开启或提交事务的内部函数使用 Toasty 自身的动态边界：

```rust,ignore
use toasty_mgr::Executor;

async fn save_customer(
    executor: &mut dyn Executor,
    customer: &mut Customer,
) -> anyhow::Result<()> {
    customer.update().exec(executor).await?;
    Ok(())
}
```

`&mut dyn Executor` 同时接受 `Db`、`Transaction` 和 `TcTx`。不能使用 `&E`，因为
Toasty 执行和开启事务都要求独占的可变借用。只有事务入口负责原子性；helper 接受
`Executor` 不代表多表写入可以脱离事务调用。

## 多数据源事务

```rust,ignore
TcTxMgr::coordinate(async |tx| {
    let [tenant, audit] = tx.get_txs(["tenant_a", "audit"]).await?;

    toasty_mgr::create!(Customer { id: 1, name: "Alice" })
        .exec(tenant)
        .await?;
    toasty_mgr::create!(AuditEvent {
        id: 1,
        message: "customer created",
    })
    .exec(audit)
    .await?;
    Ok(())
})
.await?;
```

同一编码在一个 `TcTxMgr` 中只打开一次。`get_txs` 要求编码唯一，因为它同时返回
多个可变事务引用；重复编码会直接报错。需要逐个访问时可以使用 `get_tx(code)`。

不要让 `TcTx` 引用离开回调。`TcTxMgr` 拥有事务并在回调结束后消费它们。

## 原子性边界

`TcTxMgr` 不是两阶段提交或分布式事务协议：

- 回调成功后，各数据库事务按顺序提交。
- 后续提交失败时，已经提交的数据库无法回滚。
- 回调失败时，管理器依次回滚仍打开的事务。
- 回滚本身失败时，最先返回的业务错误仍可能掩盖额外恢复工作需求。

严格原子写入应放进同一个数据库事务。必须跨库时使用 outbox、幂等消费者、补偿
操作或专用分布式事务方案，并在业务设计中明确部分提交状态。
