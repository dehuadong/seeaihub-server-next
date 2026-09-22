//! 对着**真实 Redis** 验适配器：命令语法、RESP 收发与 TTL。
//!
//! 端到端验收用的是进程内假 Redis（可以改坏值、可以让写入失败），但它认不出的命令一律报错之外
//! 没有别的判据——真正发出去的命令语法只有真实服务能证。这个用例默认 `#[ignore]`，只有显式
//! `REDIS_URL` 指向一个真实 Redis 时才有意义；没给就跳过，不假装验过。

use seeai_application::CacheStore;
use seeai_cache_redis::RedisCache;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires a real Redis via REDIS_URL"]
async fn a_real_redis_round_trip_keeps_values_and_honours_the_ttl() {
    let Ok(url) = std::env::var("REDIS_URL") else {
        eprintln!("skipped: REDIS_URL is not set");
        return;
    };
    let cache = RedisCache::new(&url, Duration::from_millis(500)).expect("a usable address");
    let key = "seeai-cache-probe";

    cache.delete(key).await.expect("DEL");
    assert_eq!(
        cache.get(key).await.expect("GET"),
        None,
        "不存在的键是 None"
    );

    cache
        .set(key, "{\"a\":1}", Duration::from_secs(30))
        .await
        .expect("SET with PX");
    assert_eq!(
        cache.get(key).await.expect("GET"),
        Some("{\"a\":1}".to_owned())
    );

    // 存活时间真的发出去了：换成 1 秒之后再等过期。
    cache
        .set(key, "short", Duration::from_secs(1))
        .await
        .expect("SET with a short PX");
    assert_eq!(cache.get(key).await.expect("GET"), Some("short".to_owned()));
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert_eq!(cache.get(key).await.expect("GET"), None, "TTL 到期后键消失");

    cache.delete(key).await.expect("DEL");
}
