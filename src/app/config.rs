use figment::{
    providers::{Env, Format, Serialized, Yaml},
    Figment,
};
use std::net::IpAddr;
use std::{borrow::Cow, collections::BTreeMap, net::SocketAddr};

use serde::{Deserialize, Deserializer, Serialize};
use serde_with::formats::CommaSeparator;
use serde_with::serde_as;
use std::str::FromStr;

use crate::{app::cli, logging};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigProviderMode {
    Redis,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerMode {
    Kafka,
    Vector,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckerMode {
    Reqwest,
    // XXX(epurkhiser): In the future there may be other implementations of the HttpChecker
    // interface.
}

#[serde_as]
#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// The statsd address to report metrics to.
    pub statsd_addr: SocketAddr,

    /// Default tags to apply to all metrics.
    pub default_tags: BTreeMap<String, String>,

    /// Tag name to report the hostname to for each metric. Defaults to not sending such a tag.
    pub hostname_tag: Option<String>,
}

#[serde_as]
#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub struct Config {
    /// The sentry DSN to use for error reporting.
    pub sentry_dsn: Option<String>,

    /// The environment to report to sentry errors to.
    pub sentry_env: Option<Cow<'static, str>>,

    /// The log level to filter logging to.
    pub log_level: logging::Level,

    /// The log format to output.
    pub log_format: logging::LogFormat,

    /// The number of HTTP checks that will be executed at once.
    pub checker_concurrency: usize,

    /// Whether the HTTP checks are multiplexed on more than one thread.
    pub checker_parallel: bool,

    /// The network interface to bind the uptime checker HTTP client to if set.
    pub interface: Option<String>,

    /// Metric configurations
    #[serde(flatten)]
    pub metrics: MetricsConfig,

    /// The kafka cluster to report results to. Expected to be a string of comma separated
    /// addresses.
    ///
    /// ```txt
    /// 10.0.0.1:5000,10.0.0.2:6000
    /// ```
    #[serde_as(as = "serde_with::StringWithSeparator::<CommaSeparator, String>")]
    pub results_kafka_cluster: Vec<String>,

    /// The topic to produce uptime checks into.
    pub results_kafka_topic: String,

    /// Which config provider to use to load configs into memory
    pub config_provider_mode: ConfigProviderMode,

    /// Which checker implementation to use to run the HTTP checks.
    pub checker_mode: CheckerMode,

    /// which producer to use to send results
    pub producer_mode: ProducerMode,

    /// How frequently to poll redis for config updates when using the redis config provider
    pub config_provider_redis_update_ms: u64,

    /// How many config partitions do we want to keep in redis? We shouldn't change this once
    /// assigned unless we plan a migration process.
    pub config_provider_redis_total_partitions: u16,

    /// The batch size to use for vector producer
    pub vector_batch_size: usize,

    /// The vector endpoint to send results to
    pub vector_endpoint: String,

    /// Whether we're using redis in standalone or cluster mode
    pub redis_enable_cluster: bool,

    /// Whether to retry sending results to vector indefinitely
    pub retry_vector_errors_forever: bool,

    /// The general purpose redis node to use with this service
    pub redis_host: String,

    /// The region that this checker is running in
    #[serde(default, deserialize_with = "deserialize_region")]
    pub region: &'static str,

    /// Allow uptime checks against internal IP addresses
    pub allow_internal_ips: bool,

    /// Whether to disable connection re-use in the http checker
    pub disable_connection_reuse: bool,

    /// Whether to record metrics for check executor tasks. May be used to diagnose scheduling
    /// problems with the http check executors.
    pub record_task_metrics: bool,

    /// Sets the maximum time in seconds to keep idle sockets alive in the http checker.
    pub pool_idle_timeout_secs: u64,

    /// The unique index of this checker out of the total nuimber of checkers. Should be
    /// zero-indexed.
    pub checker_number: u16,

    /// Total number of uptime checkers running
    pub total_checkers: u16,

    /// The number of times to retry failed checks before reporting them as failed
    pub failure_retries: u16,

    /// How many threads we should create per cpu core
    pub thread_cpu_scale_factor: usize,

    /// DNS name servers to use when making checks in the http checker
    #[serde(default, deserialize_with = "deserialize_nameservers")]
    pub http_checker_dns_nameservers: Option<Vec<IpAddr>>,

    /// Redis connection/response timeouts, in milliseconds.
    pub redis_timeouts_ms: u64,

    /// Whether to collect connection-level metrics (only available on Isahc)
    pub enable_metrics: bool,

    /// Whether this uptime checker will write to redis or not.
    pub redis_readonly: bool,

    /// The port on which to run the checker webserver.
    pub webserver_port: u16,

    /// Disable runtime assert evaluation.
    pub disable_asserts: bool,

    /// Enable response capture feature. When enabled, response body and headers
    /// will be captured on failures and included in the check result.
    pub response_capture_enabled: bool,

    /// The maximum "complexity" an assertion is allowed to be; this roughly represents
    /// how much time an assertion has to complete its ops; assertions with JSONPath queries
    /// and glob pattern matching will contribute more to consuming this value (in proportion
    /// to their individual complexity.)
    pub assertion_complexity: u32,

    /// The maximum number of assertion operations allowed in a complete assertion (including
    /// logical operations like AND and OR and NOT.)
    pub max_assertion_ops: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sentry_dsn: None,
            sentry_env: None,
            checker_concurrency: 200,
            checker_parallel: false,
            log_level: logging::Level::Warn,
            log_format: logging::LogFormat::Auto,
            interface: None,
            metrics: MetricsConfig {
                statsd_addr: "127.0.0.1:8126".parse().expect("Parsable by construction"),
                default_tags: BTreeMap::new(),
                hostname_tag: None,
            },
            results_kafka_cluster: vec!["127.0.0.1:9092".to_owned()],
            results_kafka_topic: "uptime-results".to_owned(),
            config_provider_mode: ConfigProviderMode::Redis,
            checker_mode: CheckerMode::Reqwest,
            vector_batch_size: 10,
            vector_endpoint: "http://localhost:8020".to_owned(),
            retry_vector_errors_forever: false,
            producer_mode: ProducerMode::Kafka,
            config_provider_redis_update_ms: 1000,
            config_provider_redis_total_partitions: 128,
            redis_enable_cluster: false,
            redis_host: "redis://127.0.0.1:6379".to_owned(),
            region: "default",
            allow_internal_ips: false,
            disable_connection_reuse: true,
            pool_idle_timeout_secs: 90,
            record_task_metrics: false,
            checker_number: 0,
            total_checkers: 1,
            failure_retries: 0,
            http_checker_dns_nameservers: None,
            thread_cpu_scale_factor: 1,
            redis_timeouts_ms: 30_000,
            enable_metrics: false,
            redis_readonly: false,
            webserver_port: 12345,
            disable_asserts: false,
            response_capture_enabled: false,
            assertion_complexity: 100,
            max_assertion_ops: 16,
        }
    }
}

impl Config {
    /// Load configuration from an optional configuration file and environment
    pub fn extract(app: &cli::CliApp) -> anyhow::Result<Config> {
        let mut builder = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = &app.config {
            builder = builder.merge(Yaml::file(path));
        };

        // Override with env variables if provided
        builder = builder.merge(Env::prefixed("UPTIME_CHECKER_"));

        // Override some values from the CliApp
        if let Some(log_level) = app.log_level {
            builder = builder.merge(Serialized::default("log_level", log_level))
        }
        if let Some(log_format) = app.log_format {
            builder = builder.merge(Serialized::default("log_format", log_format))
        }

        let config: Config = builder.extract()?;
        Ok(config)
    }
}

fn deserialize_region<'de, D>(deserializer: D) -> Result<&'static str, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;

    // We're going to deliberately leak the region string here--at the cost of some unfreeable
    // memory, we eliminate a major source of clones, especially in the inner-loops of the
    // schedueler and executor.
    Ok(s.leak())
}

fn deserialize_nameservers<'de, D>(deserializer: D) -> Result<Option<Vec<IpAddr>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;

    Ok(match s {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) => Some(
            s.split(',')
                .map(|s| IpAddr::from_str(s.trim()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(serde::de::Error::custom)?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use figment::Jail;
    use similar_asserts::assert_eq;
    use std::net::IpAddr;
    use std::{borrow::Cow, collections::BTreeMap, path::PathBuf};

    use crate::{app::cli, logging};

    use super::{CheckerMode, Config, ConfigProviderMode, MetricsConfig, ProducerMode};

    fn test_with_config<F>(yaml: &str, env_vars: &[(&str, &str)], test_fn: F)
    where
        F: FnOnce(Config),
    {
        Jail::expect_with(|jail| {
            jail.create_file("config.yaml", yaml)?;

            for (key, value) in env_vars {
                jail.set_env(key, value);
            }

            let app = cli::CliApp {
                config: Some(PathBuf::from("config.yaml")),
                log_level: None,
                log_format: None,
                command: cli::Commands::Run,
            };

            let config = Config::extract(&app).unwrap();
            test_fn(config);
            Ok(())
        })
    }

    #[test]
    fn test_simple() {
        test_with_config(
            r#"
            sentry_dsn: my_dsn
            sentry_env: my_env
            checker_concurrency: 100
            results_kafka_cluster: '10.0.0.1,10.0.0.2:9000'
            configs_kafka_cluster: '10.0.0.1,10.0.0.2:9000'
            statsd_addr: 10.0.0.1:8126
            default_tags: {"someTag": "value"}
            hostname_tag: pod_name
            redis_host: redis://127.0.0.1:6379
            "#,
            &[],
            |config| {
                assert_eq!(
                    config,
                    Config {
                        sentry_dsn: Some("my_dsn".to_owned()),
                        sentry_env: Some(Cow::from("my_env")),
                        checker_concurrency: 100,
                        checker_parallel: false,
                        log_level: logging::Level::Warn,
                        log_format: logging::LogFormat::Auto,
                        interface: None,
                        metrics: MetricsConfig {
                            statsd_addr: "10.0.0.1:8126".parse().unwrap(),
                            default_tags: BTreeMap::from([(
                                "someTag".to_owned(),
                                "value".to_owned()
                            ),]),
                            hostname_tag: Some("pod_name".to_owned()),
                        },
                        results_kafka_cluster: vec![
                            "10.0.0.1".to_owned(),
                            "10.0.0.2:9000".to_owned()
                        ],
                        results_kafka_topic: "uptime-results".to_owned(),
                        config_provider_mode: ConfigProviderMode::Redis,
                        checker_mode: CheckerMode::Reqwest,
                        config_provider_redis_update_ms: 1000,
                        config_provider_redis_total_partitions: 128,
                        redis_enable_cluster: false,
                        redis_host: "redis://127.0.0.1:6379".to_owned(),
                        region: "default",
                        allow_internal_ips: false,
                        disable_connection_reuse: true,
                        record_task_metrics: false,
                        pool_idle_timeout_secs: 90,
                        checker_number: 0,
                        total_checkers: 1,
                        producer_mode: ProducerMode::Kafka,
                        vector_batch_size: 10,
                        vector_endpoint: "http://localhost:8020".to_owned(),
                        retry_vector_errors_forever: false,
                        failure_retries: 0,
                        http_checker_dns_nameservers: None,
                        thread_cpu_scale_factor: 1,
                        redis_timeouts_ms: 30_000,
                        enable_metrics: false,
                        redis_readonly: false,
                        webserver_port: 12345,
                        disable_asserts: false,
                        response_capture_enabled: false,
                        assertion_complexity: 100,
                        max_assertion_ops: 16,
                    }
                );
            },
        )
    }

    #[test]
    fn test_overrides() {
        test_with_config(
            r#"
            sentry_dsn: my_dsn
            sentry_env: my_env
            log_format: json
            "#,
            &[
                ("UPTIME_CHECKER_SENTRY_ENV", "my_env_override"),
                (
                    "UPTIME_CHECKER_RESULTS_KAFKA_CLUSTER",
                    "10.0.0.1,10.0.0.2:7000",
                ),
                (
                    "UPTIME_CHECKER_CONFIGS_KAFKA_CLUSTER",
                    "10.0.0.1,10.0.0.2:7000",
                ),
                ("UPTIME_CHECKER_CONFIG_PROVIDER_MODE", "redis"),
                ("UPTIME_CHECKER_CONFIG_PROVIDER_REDIS_UPDATE_MS", "2000"),
                (
                    "UPTIME_CHECKER_CONFIG_PROVIDER_REDIS_TOTAL_PARTITIONS",
                    "32",
                ),
                ("UPTIME_CHECKER_STATSD_ADDR", "10.0.0.1:1234"),
                ("UPTIME_CHECKER_REDIS_ENABLE_CLUSTER", "true"),
                ("UPTIME_CHECKER_REDIS_HOST", "10.0.0.3:6379"),
                ("UPTIME_CHECKER_REGION", "us-west"),
                ("UPTIME_CHECKER_ALLOW_INTERNAL_IPS", "true"),
                ("UPTIME_CHECKER_DISABLE_CONNECTION_REUSE", "false"),
                ("UPTIME_CHECKER_POOL_IDLE_TIMEOUT_SECS", "600"),
                ("UPTIME_CHECKER_CHECKER_NUMBER", "2"),
                ("UPTIME_CHECKER_TOTAL_CHECKERS", "5"),
                ("UPTIME_CHECKER_FAILURE_RETRIES", "2"),
                ("UPTIME_CHECKER_THREAD_CPU_SCALE_FACTOR", "3"),
                (
                    "UPTIME_CHECKER_HTTP_CHECKER_DNS_NAMESERVERS",
                    "8.8.8.8,8.8.4.4",
                ),
                ("UPTIME_CHECKER_INTERFACE", "eth0"),
                ("UPTIME_CHECKER_REDIS_READONLY", "true"),
                ("UPTIME_CHECKER_WEBSERVER_PORT", "81"),
                ("UPTIME_CHECKER_ASSERTION_COMPLEXITY", "120"),
                ("UPTIME_CHECKER_MAX_ASSERTION_OPS", "15"),
            ],
            |config| {
                assert_eq!(
                    config,
                    Config {
                        sentry_dsn: Some("my_dsn".to_owned()),
                        sentry_env: Some(Cow::from("my_env_override")),
                        checker_concurrency: 200,
                        checker_parallel: false,
                        log_level: logging::Level::Warn,
                        log_format: logging::LogFormat::Json,
                        interface: Some("eth0".to_owned()),
                        metrics: MetricsConfig {
                            statsd_addr: "10.0.0.1:1234".parse().unwrap(),
                            default_tags: BTreeMap::new(),
                            hostname_tag: None,
                        },
                        results_kafka_cluster: vec![
                            "10.0.0.1".to_owned(),
                            "10.0.0.2:7000".to_owned()
                        ],
                        results_kafka_topic: "uptime-results".to_owned(),
                        config_provider_mode: ConfigProviderMode::Redis,
                        checker_mode: CheckerMode::Reqwest,
                        config_provider_redis_update_ms: 2000,
                        config_provider_redis_total_partitions: 32,
                        redis_enable_cluster: true,
                        redis_host: "10.0.0.3:6379".to_owned(),
                        region: "us-west",
                        allow_internal_ips: true,
                        disable_connection_reuse: false,
                        record_task_metrics: false,
                        pool_idle_timeout_secs: 600,
                        checker_number: 2,
                        total_checkers: 5,
                        producer_mode: ProducerMode::Kafka,
                        vector_batch_size: 10,
                        vector_endpoint: "http://localhost:8020".to_owned(),
                        retry_vector_errors_forever: false,
                        failure_retries: 2,
                        http_checker_dns_nameservers: Some(vec![
                            IpAddr::from([8, 8, 8, 8]),
                            IpAddr::from([8, 8, 4, 4])
                        ]),
                        thread_cpu_scale_factor: 3,
                        redis_timeouts_ms: 30_000,
                        enable_metrics: false,
                        redis_readonly: true,
                        webserver_port: 81,
                        disable_asserts: false,
                        response_capture_enabled: false,
                        assertion_complexity: 120,
                        max_assertion_ops: 15,
                    }
                );
            },
        )
    }

    #[test]
    fn test_config_default_checker_ordinal() {
        test_with_config(
            r#"
            sentry_dsn: my_dsn
            sentry_env: my_env
            "#,
            &[],
            |config| {
                assert_eq!(config.checker_number, 0);
            },
        )
    }

    #[test]
    fn test_config_empty_http_checker_dns_nameservers() {
        test_with_config(
            r#"
            sentry_dsn: my_dsn
            sentry_env: my_env
            "#,
            &[("UPTIME_CHECKER_HTTP_CHECKER_DNS_NAMESERVERS", "")],
            |config| {
                assert_eq!(config.http_checker_dns_nameservers, None);
            },
        )
    }
}
