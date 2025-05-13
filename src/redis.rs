use anyhow::Result;
use redis::aio::ConnectionLike;
use redis::cluster::{ClusterClient, ClusterConfig};
use redis::{Cmd, Pipeline, RedisError, RedisFuture, Value};
use std::time::Duration;

#[derive(Clone)]
pub enum RedisAsyncConnection {
    Cluster(redis::cluster_async::ClusterConnection),
    Single(redis::aio::MultiplexedConnection),
}

impl ConnectionLike for RedisAsyncConnection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            RedisAsyncConnection::Cluster(conn) => Box::pin(conn.req_packed_command(cmd)),
            RedisAsyncConnection::Single(conn) => Box::pin(conn.req_packed_command(cmd)),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            RedisAsyncConnection::Cluster(conn) => {
                Box::pin(conn.req_packed_commands(cmd, offset, count))
            }
            RedisAsyncConnection::Single(conn) => {
                Box::pin(conn.req_packed_commands(cmd, offset, count))
            }
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            RedisAsyncConnection::Cluster(conn) => conn.get_db(),
            RedisAsyncConnection::Single(conn) => conn.get_db(),
        }
    }
}

pub enum RedisClient {
    Cluster(ClusterClient),
    Single(redis::Client),
}

pub fn build_redis_client(redis_url: &str, enable_cluster: bool) -> Result<RedisClient> {
    let client = if enable_cluster {
        RedisClient::Cluster(ClusterClient::builder(vec![redis_url.to_string()]).build()?)
    } else {
        RedisClient::Single(redis::Client::open(redis_url)?)
    };

    Ok(client)
}

impl RedisClient {
    pub async fn get_async_connection(
        &self,
        redis_timeouts_ms: u64,
    ) -> Result<RedisAsyncConnection, RedisError> {
        // If timeout is set to 0, we create a connection with no timeout; this is a testing-related
        // feature, as any timer-related future (when a test is paused) will immediately succeed,
        // which will instantly trigger the connection timeout, causing the test to fail.
        let timeout = Duration::from_millis(redis_timeouts_ms);
        match self {
            RedisClient::Cluster(client) => {
                let mut config = ClusterConfig::new();

                if redis_timeouts_ms > 0 {
                    config = config
                        .set_connection_timeout(timeout)
                        .set_response_timeout(timeout);
                }

                let connection = client.get_async_connection_with_config(config).await?;
                Ok(RedisAsyncConnection::Cluster(connection))
            }
            RedisClient::Single(client) => {
                let connection = if redis_timeouts_ms > 0 {
                    client
                        .get_multiplexed_tokio_connection_with_response_timeouts(timeout, timeout)
                        .await?
                } else {
                    client.get_multiplexed_tokio_connection().await?
                };
                Ok(RedisAsyncConnection::Single(connection))
            }
        }
    }
}
