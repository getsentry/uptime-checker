use std::error::Error;
use std::time::Duration;

use reqwest::{ClientBuilder, StatusCode};

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CheckResult {
    SUCCESS,
    FAILURE(FailureReason),
    MISSED,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FailureReason {
    Timeout,
    DnsError(String),
    ConfigError(String),
    Error(String),
}

pub async fn check_domain(domain: String) -> CheckResult {
    // TODO: Timeout should come from config
    let timeout = Duration::new(5, 0);
    let client_result = ClientBuilder::new().timeout(timeout).build();
    let client = match client_result {
        Err(e) => {
            return CheckResult::FAILURE(FailureReason::ConfigError(e.to_string()));
        }
        Ok(client) => client,
    };
    let mut response_result = client.head(&domain).send().await;
    if let Ok(response) = &response_result {
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            // If this domain doesn't accept HEAD, attempt GET request
            response_result = client.get(&domain).send().await;
        }
    }

    match response_result {
        Ok(_) => {
            return CheckResult::SUCCESS;
        }
        Err(e) => {
            if e.is_timeout() {
                return CheckResult::FAILURE(FailureReason::Timeout);
            } else {
                // TODO: More reasons

                let mut source = e.source();
                while let Some(inner) = source {
                    if inner.source().is_none() {
                        break;
                    }
                    source = inner.source();

                    // TODO: Would be better to get specific errors without string matching like this
                    // Not sure if there's a better way
                    if source.unwrap().to_string().contains("dns error") {
                        return CheckResult::FAILURE(FailureReason::DnsError(
                            source.unwrap().source().unwrap().to_string(),
                        ));
                    }
                }

                return CheckResult::FAILURE(FailureReason::Error(format!("{:?}", e)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_domain, CheckResult, FailureReason};

    #[tokio::test]
    async fn test_add() {
        assert_eq!(
            check_domain("https://google.com".to_string()).await,
            CheckResult::SUCCESS
        );
        assert_eq!(
            check_domain("https://sentry.io".to_string()).await,
            CheckResult::SUCCESS
        );
        assert_eq!(check_domain("https://hjkhjkljkh.io".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
        assert_eq!(check_domain("https://santry.io".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
    }
}
