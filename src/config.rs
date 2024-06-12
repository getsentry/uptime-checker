use std::borrow::Cow;

use figment::{
    providers::{Env, Format, Serialized, Yaml},
    Figment,
};

use serde::{Deserialize, Serialize};

use crate::{cliapp, logging};

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

    /// The kafka cluster to report results to. Expected to be a string of comma separated
    /// addresses.
    ///
    /// ```txt
    /// 10.0.0.1:5000,10.0.0.2:6000
    /// ```
    #[serde(
        deserialize_with = "list_deserialize",
        serialize_with = "list_serializer"
    )]
    pub results_kafka_cluster: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sentry_dsn: None,
            sentry_env: None,
            log_level: logging::Level::Warn,
            log_format: logging::LogFormat::Auto,
            results_kafka_cluster: vec![],
        }
    }
}

fn list_deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let str_sequence = String::deserialize(deserializer)?;
    let result = str_sequence
        .split(',')
        .map(|item| item.trim().to_owned())
        .collect();
    Ok(result)
}

fn list_serializer<S>(list: &[String], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    list.join(",").serialize(serializer)
}

impl Config {
    /// Load configuration from an optional configuration file and environment
    pub fn extract(app: &cliapp::CliApp) -> anyhow::Result<Config> {
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
    use std::{borrow::Cow, path::PathBuf};

    use figment::Jail;
    use similar_asserts::assert_eq;

    use crate::{cliapp, logging};

    use super::Config;

    #[test]
    fn test_simple() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.yaml",
                r#"
                sentry_dsn: my_dsn
                sentry_env: my_env
                results_kafka_cluster: '10.0.0.1,10.0.0.2:9000'
                "#,
            )?;

            let app = cliapp::CliApp {
                config: Some(PathBuf::from("config.yaml")),
                log_level: None,
                log_format: None,
                command: cliapp::Commands::Run,
            };

            let config = Config::extract(&app).expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env")),
                    log_level: logging::Level::Warn,
                    log_format: logging::LogFormat::Auto,
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:9000".to_owned()]
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

            let app = cliapp::CliApp {
                config: Some(PathBuf::from("config.yaml")),
                log_level: Some(logging::Level::Trace),
                log_format: None,
                command: cliapp::Commands::Run,
            };

            let config = Config::extract(&app).expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env_override")),
                    log_level: logging::Level::Trace,
                    log_format: logging::LogFormat::Json,
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:7000".to_owned()]
                }
            );
            Ok(())
        });
    }
}
