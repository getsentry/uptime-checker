use std::{borrow::Cow, path::PathBuf};

use figment::{
    providers::{Env, Format, Yaml},
    Figment,
};

use serde::{Deserialize, Serialize};

#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub struct Config {
    /// The sentry DSN to use for error reporting.
    pub sentry_dsn: Option<String>,

    /// The environment to report to sentry errors to.
    pub sentry_env: Option<Cow<'static, str>>,

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

fn list_deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let str_sequence = String::deserialize(deserializer)?;
    let result = str_sequence
        .split(',')
        .map(|item| item.to_owned())
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
    pub fn extract(path: &Option<PathBuf>) -> anyhow::Result<Config> {
        let builder = Figment::new();

        let builder = match path {
            Some(path) => builder.merge(Yaml::file(path)),
            _ => builder,
        };

        // Override env variables if provided
        let config: Config = builder.merge(Env::prefixed("UPTIME_CHECKER_")).extract()?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use figment::Jail;
    use similar_asserts::assert_eq;

    use super::*;

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

            let config = Config::extract(&Some(PathBuf::from("config.yaml")))
                .expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env")),
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:9000".to_owned()]
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_onv_override() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.yaml",
                r#"
                sentry_dsn: my_dsn
                sentry_env: my_env
                "#,
            )?;

            jail.set_env("UPTIME_CHECKER_SENTRY_ENV", "my_env_override");
            jail.set_env(
                "UPTIME_CHECKER_RESULTS_KAFKA_CLUSTER",
                "10.0.0.1,10.0.0.2:7000",
            );

            let config = Config::extract(&Some(PathBuf::from("config.yaml")))
                .expect("Invalid configuration");

            assert_eq!(
                config,
                Config {
                    sentry_dsn: Some("my_dsn".to_owned()),
                    sentry_env: Some(Cow::from("my_env_override")),
                    results_kafka_cluster: vec!["10.0.0.1".to_owned(), "10.0.0.2:7000".to_owned()]
                }
            );
            Ok(())
        });
    }
}
