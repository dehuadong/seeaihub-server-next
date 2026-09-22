//! Redis 作为**加速层**的实现。
//!
//! 这个 crate 只做一件事：把 [`CacheStore`] 的三条命令发给 Redis。**语义不在这里**——键名、
//! 值长什么样、什么时候能拿缓存下结论，都在用例层的 `AccelerationService` 里。这样分的理由：
//! 一旦让实现方也懂"余额"与"候选集"，两边的语义就会各自漂移，而漂移的表现是"缓存说的和
//! 数据库说的不一样"。
//!
//! 两条工程约定：
//!
//! - **连不上不是错误**：`REDIS_URL` 没配就没有缓存（[`RedisCache::from_env`] 返回 `None`）；
//!   配了但连不上、超时、命令报错，一律在调用点被当成"这次没命中"，回源数据库；
//! - **凭证不进日志**：连接串里可能带密码，所以任何日志都不打印连接串本身。

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use seeai_application::{ApplicationError, CacheStore};
use std::{env, time::Duration};
use tokio::sync::Mutex;

/// 单次缓存操作的默认超时（毫秒）。
///
/// 缓存是加速层，慢就等于没有：宁可这一次当未命中回源数据库，也不要让一个卡住的缓存把请求
/// 拖到上游超时。
const DEFAULT_OPERATION_TIMEOUT_MS: u64 = 200;

/// 一个 Redis 连接上的加速层。
///
/// 连接是**惰性建立**的：构造这个类型不会去连 Redis（连不上也不该让进程起不来），第一次真正
/// 用的时候才连；任何一次命令失败都会丢掉当前连接，让下一次重新连——Redis 重启之后不必等
/// 进程重启才能恢复。
pub struct RedisCache {
    client: redis::Client,
    connection: Mutex<Option<MultiplexedConnection>>,
    timeout: Duration,
}

impl RedisCache {
    /// 从环境变量构造：`REDIS_URL` 为空或没配就是**没有缓存**（返回 `None`，不是错误）。
    ///
    /// `CACHE_OPERATION_TIMEOUT_MS` 可以调单次操作的超时。
    pub fn from_env() -> Result<Option<Self>, ApplicationError> {
        let Ok(url) = env::var("REDIS_URL") else {
            return Ok(None);
        };
        let url = url.trim();
        if url.is_empty() {
            return Ok(None);
        }
        let timeout = match env::var("CACHE_OPERATION_TIMEOUT_MS") {
            Ok(value) if !value.trim().is_empty() => {
                Duration::from_millis(value.trim().parse::<u64>().map_err(|_| {
                    ApplicationError::Configuration(
                        "CACHE_OPERATION_TIMEOUT_MS must be an integer".to_owned(),
                    )
                })?)
            }
            _ => Duration::from_millis(DEFAULT_OPERATION_TIMEOUT_MS),
        };
        Ok(Some(Self::new(url, timeout)?))
    }

    /// 按连接串构造。**不在这里连接**，因此 Redis 没起来也能构造出来（那时它只是每次都未命中）。
    ///
    /// 连接串本身**不是**合法地址时直接失败：那是配置写错了，不是"缓存暂时不可用"——把配置错
    /// 悄悄降级成"没有缓存"，运维就永远看不到自己写错了地址。
    pub fn new(url: &str, timeout: Duration) -> Result<Self, ApplicationError> {
        let client = redis::Client::open(url).map_err(|error| {
            ApplicationError::Configuration(format!("REDIS_URL is not a usable address: {error}"))
        })?;
        Ok(Self {
            client,
            connection: Mutex::new(None),
            timeout,
        })
    }

    /// 取一个可用连接；连不上或超时就返回 `None`（调用点当成未命中）。
    ///
    /// 建连接这一段持有锁：并发请求一起进来时只会有一次连接尝试，其余的等它——否则 Redis 挂掉
    /// 的时候每个请求各连一次，等待时间会叠在一起。
    async fn connection(&self) -> Option<MultiplexedConnection> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            return Some(connection.clone());
        }
        match tokio::time::timeout(self.timeout, self.client.get_multiplexed_async_connection())
            .await
        {
            Ok(Ok(connection)) => {
                *guard = Some(connection.clone());
                Some(connection)
            }
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "could not connect to the cache service");
                None
            }
            Err(_) => {
                tracing::warn!("connecting to the cache service timed out");
                None
            }
        }
    }

    /// 丢掉当前连接：任何一次失败之后都这么做，下一次调用重新连。
    async fn drop_connection(&self) {
        *self.connection.lock().await = None;
    }

    /// 把一次带超时的命令结果翻成结果或错误，并在失败时丢掉连接。
    async fn settle<T>(
        &self,
        outcome: Result<redis::RedisResult<T>, tokio::time::error::Elapsed>,
    ) -> Result<T, ApplicationError> {
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                self.drop_connection().await;
                Err(ApplicationError::Persistence(format!(
                    "cache command failed: {error}"
                )))
            }
            Err(_) => {
                self.drop_connection().await;
                Err(ApplicationError::Persistence(
                    "cache command timed out".to_owned(),
                ))
            }
        }
    }
}

#[async_trait]
impl CacheStore for RedisCache {
    async fn get(&self, key: &str) -> Result<Option<String>, ApplicationError> {
        let Some(mut connection) = self.connection().await else {
            return Err(ApplicationError::Persistence(
                "the cache service is not reachable".to_owned(),
            ));
        };
        let outcome = tokio::time::timeout(
            self.timeout,
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<String>>(&mut connection),
        )
        .await;
        self.settle(outcome).await
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), ApplicationError> {
        let Some(mut connection) = self.connection().await else {
            return Err(ApplicationError::Persistence(
                "the cache service is not reachable".to_owned(),
            ));
        };
        // 存活时间用毫秒（`PX`）而不是秒：TTL 是"兜住旧值白占内存"的，秒级取整只会让它在边界上
        // 差一整秒，没必要。
        let ttl_millis = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let outcome = tokio::time::timeout(
            self.timeout,
            redis::cmd("SET")
                .arg(key)
                .arg(value)
                .arg("PX")
                .arg(ttl_millis)
                .query_async::<()>(&mut connection),
        )
        .await;
        self.settle(outcome).await
    }

    async fn delete(&self, key: &str) -> Result<(), ApplicationError> {
        let Some(mut connection) = self.connection().await else {
            return Err(ApplicationError::Persistence(
                "the cache service is not reachable".to_owned(),
            ));
        };
        let outcome = tokio::time::timeout(
            self.timeout,
            redis::cmd("DEL")
                .arg(key)
                .query_async::<i64>(&mut connection),
        )
        .await;
        // 删除的条数没有用：键本来就不在也算失效成功（要的就是"现在没有它"）。
        self.settle(outcome).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
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
}
