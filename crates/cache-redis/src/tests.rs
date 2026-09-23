use super::RedisCache;
use seeai_application::CacheStore;
use std::time::Duration;

#[test]
fn a_malformed_address_is_a_configuration_error() {
    assert!(RedisCache::new("not-a-redis-url", Duration::from_millis(50)).is_err());
}

#[tokio::test]
async fn an_unreachable_cache_reports_a_miss_instead_of_hanging() {
    // 指向一个没人监听的端口：命令必须**很快**失败，而不是把请求挂在那儿。
    let cache = RedisCache::new("redis://127.0.0.1:1", Duration::from_millis(50))
        .expect("a syntactically valid address");
    assert!(cache.get("route:model").await.is_err());
    assert!(
        cache
            .set("route:model", "{}", Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(cache.delete("route:model").await.is_err());
}
