//! 余额待写队列的进程内用例：合并、有界、丢弃计数。
//!
//! 它只测队列本身。**请求路径不等 Redis** 由结构保证——`enqueue_balance` 只加锁、不 await；
//! 契约层那条写穿用例负责行为一侧（写最终会落到缓存、资金事实不变）。

use super::*;

/// 一份快照：只关心账户与版本，金额给一个能对上版本的值。
fn change(account_id: AccountId, version: i64) -> BalanceChange {
    BalanceChange {
        account_id,
        balance_microusd: version,
        held_microusd: 0,
        available_microusd: version,
        version,
        updated_at: Utc::now(),
    }
}

/// 同一账户只留版本最高的一份：后到的旧快照不覆盖、也不新增条目。
#[test]
fn a_balance_write_queue_keeps_only_the_newest_snapshot_per_account() {
    let queue = BalanceWrites::new();
    let account = AccountId::new();
    queue.enqueue(change(account, 1));
    queue.enqueue(change(account, 5));
    queue.enqueue(change(account, 3));

    let drained = queue.drain();
    assert_eq!(drained.len(), 1, "同账户只该留一份");
    assert_eq!(drained[0].version, 5, "留下的是版本最高的那份");
    assert!(queue.drain().is_empty(), "取走之后队列是空的");
}

/// 队列有界：满了就丢、并把丢弃计数加一；已经在队列里的账户仍可更新。
#[test]
fn a_full_balance_write_queue_drops_new_accounts_and_counts_them() {
    let queue = BalanceWrites::new();
    let accounts: Vec<AccountId> = (0..queue.capacity).map(|_| AccountId::new()).collect();
    for account in &accounts {
        queue.enqueue(change(*account, 1));
    }
    queue.enqueue(change(AccountId::new(), 1));
    assert_eq!(queue.dropped(), 1, "满员之后新账户进来要记一次丢弃");

    // 已在队列里的账户不受容量影响：它只是覆盖自己那一份。
    queue.enqueue(change(accounts[0], 9));
    assert_eq!(queue.dropped(), 1, "覆盖已有账户不算丢弃");

    let drained = queue.drain();
    assert_eq!(drained.len(), queue.capacity);
    assert_eq!(
        drained
            .iter()
            .find(|snapshot| snapshot.account_id == accounts[0])
            .map(|snapshot| snapshot.version),
        Some(9),
        "队列里那份被更新的覆盖了"
    );
}
