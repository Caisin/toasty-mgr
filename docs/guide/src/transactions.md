# 事务

`TcTxMgr` 是 Toasty transaction 的管理层。它按需为每个数据源打开一个事务，
让服务代码在同一个回调中使用多个数据源。

## 单数据源事务

```rust,ignore
TcTxMgr::transaction("tenant_a", async |tx| {
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

## 多数据源事务

```rust,ignore
TcTxMgr::t(async |tx| {
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

## 显式管理回调

已有管理器实例时使用 `trans`：

```rust,ignore
let result = TcTxMgr::new()
    .trans(async |tx| {
        tx.use_txs(["tenant_a", "audit"]).await?;
        let tenant = tx.get("tenant_a")?;
        // Execute Toasty statements through tenant.
        Ok::<_, anyhow::Error>(42)
    })
    .await?;
```

不要让 `TcTx` 引用离开回调。`TcTxMgr` 拥有事务并在回调结束后消费它们。

## 原子性边界

`TcTxMgr` 不是两阶段提交或分布式事务协议：

- 回调成功后，各数据库事务按顺序提交。
- 后续提交失败时，已经提交的数据库无法回滚。
- 回调失败时，管理器依次回滚仍打开的事务。
- 回滚本身失败时，最先返回的业务错误仍可能掩盖额外恢复工作需求。

严格原子写入应放进同一个数据库事务。必须跨库时使用 outbox、幂等消费者、补偿
操作或专用分布式事务方案，并在业务设计中明确部分提交状态。
