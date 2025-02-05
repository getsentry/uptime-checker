use redis::aio::ConnectionLike;
use redis::cluster::ClusterClient;
use redis::{Cmd, Pipeline, RedisFuture, Value};

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

pub fn build_redis_client(redis_url: &str, enable_cluster: bool) -> RedisClient {
    if enable_cluster {
        RedisClient::Cluster(
            ClusterClient::builder(vec![redis_url.to_string()])
                .build()
                .expect("Failed to build cluster client"),
        )
    } else {
        RedisClient::Single(redis::Client::open(redis_url).expect("Failed to open redis client"))
    }
}

impl RedisClient {
    pub async fn get_async_connection(&self) -> RedisAsyncConnection {
        match self {
            RedisClient::Cluster(client) => RedisAsyncConnection::Cluster(
                client
                    .get_async_connection()
                    .await
                    .expect("Unable to connect to Redis"),
            ),
            RedisClient::Single(client) => RedisAsyncConnection::Single(
                client
                    .get_multiplexed_tokio_connection()
                    .await
                    .expect("Unable to connect to Redis"),
            ),
        }
    }
}
