use anyhow::Result;
use redis::aio::ConnectionLike;
use redis::cluster::{ClusterClient, ClusterConfig};
use redis::{AsyncCommands, Client, Cmd, Pipeline, RedisError, RedisFuture, Value};
use std::time::Duration;

use crate::check_config_provider::redis_config_provider::{ConfigUpdate, ConfigUpdateAction};

pub enum RedisOperations {
    Read(RedisAsyncConnection),
    ReadWrite(RedisAsyncConnection),
}

impl RedisOperations {
    fn get_readwrite_conn(&mut self) -> Option<&mut RedisAsyncConnection> {
        match self {
            Self::Read(_) => None,
            Self::ReadWrite(c) => Some(c),
        }
    }

    fn get_conn(&mut self) -> &mut RedisAsyncConnection {
        match self {
            Self::Read(c) => c,
            Self::ReadWrite(c) => c,
        }
    }

    pub fn is_readonly(&self) -> bool {
        match self {
            Self::Read(_) => true,
            Self::ReadWrite(_) => false,
        }
    }

    pub async fn read_watermark(
        &mut self,
        progress_key: &String,
    ) -> Result<Option<i64>, redis::RedisError> {
        let conn = self.get_conn();
        let progress: Option<String> = conn.get(progress_key).await?;

        Ok(progress.and_then(|v| v.parse().ok()))
    }

    // Watermark writes are still considered acceptable in read-only mode.
    pub async fn write_watermark(
        &mut self,
        progress_key: &String,
        progress: i64,
    ) -> Result<(), redis::RedisError> {
        let conn = self.get_conn();

        conn.set(progress_key, progress.to_string()).await
    }

    pub async fn read_configs(
        &mut self,
        config_key: &String,
    ) -> Result<Vec<Vec<u8>>, redis::RedisError> {
        let conn = self.get_conn();
        conn.hvals(config_key).await
    }

    pub async fn consume_config_updates(
        &mut self,
        update_key: &String,
    ) -> (Vec<ConfigUpdate>, Vec<ConfigUpdate>, Vec<ConfigUpdate>) {
        let conn = self
            .get_readwrite_conn()
            .expect("must be in read-write mode to access");

        let mut pipe = redis::pipe();
        // We fetch all updates from the list and then delete the key. We do this
        // atomically so that there isn't any chance of a race
        let (config_upserts, config_deletes, response_capture_toggles) = pipe
            .atomic()
            .hvals(update_key)
            .del(update_key)
            .query_async::<(Vec<Vec<u8>>, ())>(conn)
            .await
            .unwrap_or_else(|err| {
                tracing::error!(?err, "redis_config_provider.redis_query_failed");
                (vec![], ())
            })
            .0 // Get just the LRANGE results
            .iter()
            .map(|payload| {
                rmp_serde::from_slice::<ConfigUpdate>(payload).map_err(|err| {
                    tracing::error!(?err, "config_consumer.invalid_config_message");
                    err
                })
            })
            .filter_map(Result::ok)
            .fold(
                (vec![], vec![], vec![]),
                |(mut upserts, mut deletes, mut toggles), update| {
                    match update.action {
                        ConfigUpdateAction::Upsert => upserts.push(update),
                        ConfigUpdateAction::Delete => deletes.push(update),
                        ConfigUpdateAction::SetResponseCapture => toggles.push(update),
                    }
                    (upserts, deletes, toggles)
                },
            );

        (config_upserts, config_deletes, response_capture_toggles)
    }

    pub async fn get_config_key_payloads(
        &mut self,
        config_key: &String,
        config_keys: Vec<String>,
    ) -> Vec<Vec<u8>> {
        let conn = self.get_conn();
        conn.hget(config_key, config_keys)
            .await
            .unwrap_or_else(|err| {
                tracing::error!(?err, "redis_config_provider.config_key_get_failed");
                vec![]
            })
    }
}

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

enum ClientKind {
    Cluster(ClusterClient),
    Single(Client),
}

pub struct RedisClient {
    client: ClientKind,
    timeout_ms: u64,
    readonly: bool,
}

pub fn build_redis_client(
    redis_url: &str,
    enable_cluster: bool,
    timeout_ms: u64,
    readonly: bool,
) -> Result<RedisClient> {
    let client = if enable_cluster {
        ClientKind::Cluster(ClusterClient::builder(vec![redis_url.to_string()]).build()?)
    } else {
        ClientKind::Single(redis::Client::open(redis_url)?)
    };

    Ok(RedisClient {
        client,
        timeout_ms,
        readonly,
    })
}

fn to_ops(connection: RedisAsyncConnection, readonly: bool) -> RedisOperations {
    match readonly {
        true => RedisOperations::Read(connection),
        false => RedisOperations::ReadWrite(connection),
    }
}

impl RedisClient {
    pub async fn get_async_connection(&self) -> Result<RedisOperations, RedisError> {
        // If timeout is set to 0, we create a connection with no timeout; this is a testing-related
        // feature, as any timer-related future (when a test is paused) will immediately succeed,
        // which will instantly trigger the connection timeout, causing the test to fail.
        let timeout = Duration::from_millis(self.timeout_ms);
        match &self.client {
            ClientKind::Cluster(client) => {
                let mut config = ClusterConfig::new();

                if self.timeout_ms > 0 {
                    config = config
                        .set_connection_timeout(timeout)
                        .set_response_timeout(timeout);
                }

                let connection = RedisAsyncConnection::Cluster(
                    client.get_async_connection_with_config(config).await?,
                );

                Ok(to_ops(connection, self.readonly))
            }
            ClientKind::Single(client) => {
                let connection = if self.timeout_ms > 0 {
                    client
                        .get_multiplexed_tokio_connection_with_response_timeouts(timeout, timeout)
                        .await?
                } else {
                    client.get_multiplexed_tokio_connection().await?
                };
                let connection = RedisAsyncConnection::Single(connection);

                Ok(to_ops(connection, self.readonly))
            }
        }
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly
    }
}
