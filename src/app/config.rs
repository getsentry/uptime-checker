use std::{borrow::Cow, collections::BTreeMap, net::SocketAddr};

use figment::{
    providers::{Env, Format, Serialized, Yaml},
    Figment,
};

use serde::{Deserialize, Serialize};
use serde_with::formats::CommaSeparator;
use serde_with::serde_as;

use crate::{app::cli, logging};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigProviderMode {
    Kafka,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerMode {
    Kafka,
    Vector,
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

    /// The kafka cluster to load configs from to. Expected to be a string of comma separated
    /// addresses.
    ///
    /// ```txt
    /// 10.0.0.1:5000,10.0.0.2:6000
    /// ```
    #[serde_as(as = "serde_with::StringWithSeparator::<CommaSeparator, String>")]
    pub configs_kafka_cluster: Vec<String>,

    /// The topic to load [`CheckConfig`]s from.
    pub configs_kafka_topic: String,

    /// Which config provider to use to load configs into memory
    pub config_provider_mode: ConfigProviderMode,

    /// which producer to use to send results
    pub producer_mode: ProducerMode,

    /// The general purpose redis node to use with this service
    pub redis_host: String,

    /// The region that this checker is running in
    pub region: String,

    /// Allow uptime checks against internal IP addresses
    pub allow_internal_ips: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sentry_dsn: None,
            sentry_env: None,
            checker_concurrency: 200,
            log_level: logging::Level::Warn,
            log_format: logging::LogFormat::Auto,
            metrics: MetricsConfig {
                statsd_addr: "127.0.0.1:8126".parse().unwrap(),
                default_tags: BTreeMap::new(),
                hostname_tag: None,
            },
            results_kafka_cluster: vec![],
            results_kafka_topic: "uptime-results".to_owned(),
            configs_kafka_cluster: vec![],
            configs_kafka_topic: "uptime-configs".to_owned(),
            config_provider_mode: ConfigProviderMode::Kafka,
            producer_mode: ProducerMode::Vector,
            redis_host: "redis://127.0.0.1:6379".to_owned(),
            region: "default".to_owned(),
            allow_internal_ips: false,
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

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::BTreeMap, path::PathBuf};

    use figment::Jail;
    use similar_asserts::assert_eq;

    use crate::{app::{cli, config::ProducerMode}, logging};

    use super::{Config, ConfigProviderMode, MetricsConfig};

    #[test]
    fn test_simple() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.yaml",
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
            )?;

            let app = cli::CliApp {
                config: Some(PathBuf::from("config.yaml")),
                log_level: None,
                log_format: None,
                command: cli::Commands::Run,
            };

            let config = Config::extract(&app).expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env")),
                    checker_concurrency: 100,
                    log_level: logging::Level::Warn,
                    log_format: logging::LogFormat::Auto,
                    metrics: MetricsConfig {
                        statsd_addr: "10.0.0.1:8126".parse().unwrap(),
                        default_tags: BTreeMap::from([("someTag".to_owned(), "value".to_owned()),]),
                        hostname_tag: Some("pod_name".to_owned()),
                    },
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:9000".to_owned()],
                    configs_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:9000".to_owned()],
                    results_kafka_topic: "uptime-results".to_owned(),
                    configs_kafka_topic: "uptime-configs".to_owned(),
                    config_provider_mode: ConfigProviderMode::Kafka,
                    producer_mode: ProducerMode::Kafka,
                    redis_host: "redis://127.0.0.1:6379".to_owned(),
                    region: "default".to_owned(),
                    allow_internal_ips: false,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_overrides() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.yaml",
                r#"
                sentry_dsn: my_dsn
                sentry_env: my_env
                log_format: json
                "#,
            )?;

            jail.set_env("UPTIME_CHECKER_SENTRY_ENV", "my_env_override");
            jail.set_env(
                "UPTIME_CHECKER_RESULTS_KAFKA_CLUSTER",
                "10.0.0.1,10.0.0.2:7000",
            );
            jail.set_env(
                "UPTIME_CHECKER_CONFIGS_KAFKA_CLUSTER",
                "10.0.0.1,10.0.0.2:7000",
            );
            jail.set_env("UPTIME_CHECKER_CONFIG_PROVIDER_MODE", "kafka");
            jail.set_env("UPTIME_CHECKER_STATSD_ADDR", "10.0.0.1:1234");
            jail.set_env("UPTIME_CHECKER_REDIS_HOST", "10.0.0.3:6379");
            jail.set_env("UPTIME_CHECKER_REGION", "us-west");
            jail.set_env("UPTIME_CHECKER_ALLOW_INTERNAL_IPS", "true");
            let app = cli::CliApp {
                config: Some(PathBuf::from("config.yaml")),
                log_level: Some(logging::Level::Trace),
                log_format: None,
                command: cli::Commands::Run,
            };

            let config = Config::extract(&app).expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env_override")),
                    checker_concurrency: 200,
                    log_level: logging::Level::Trace,
                    log_format: logging::LogFormat::Json,
                    metrics: MetricsConfig {
                        statsd_addr: "10.0.0.1:1234".parse().unwrap(),
                        default_tags: BTreeMap::new(),
                        hostname_tag: None,
                    },
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:7000".to_owned()],
                    configs_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:7000".to_owned()],
                    results_kafka_topic: "uptime-results".to_owned(),
                    configs_kafka_topic: "uptime-configs".to_owned(),
                    config_provider_mode: ConfigProviderMode::Kafka,
                    producer_mode: ProducerMode::Kafka,
                    redis_host: "10.0.0.3:6379".to_owned(),
                    region: "us-west".to_owned(),
                    allow_internal_ips: true,
                }
            );
            Ok(())
        });
    }
}
