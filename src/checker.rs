use std::error::Error;

use reqwest::StatusCode;

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
    Error(String),
}

pub async fn check_domain(client: &reqwest::Client, domain: String) -> CheckResult {
    let mut response_result = client.head(&domain).send().await;
    if let Ok(response) = &response_result {
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            // If this domain doesn't accept HEAD, attempt GET request
            response_result = client.get(&domain).send().await;
        }
    }

    match response_result {
        Ok(_) => CheckResult::SUCCESS,
        Err(e) => {
            if e.is_timeout() {
                return CheckResult::FAILURE(FailureReason::Timeout);
            }
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
            CheckResult::FAILURE(FailureReason::Error(format!("{:?}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_domain, CheckResult, FailureReason};
    use reqwest::ClientBuilder;
    use std::time::Duration;

    #[tokio::test]
    async fn test_add() {
        let timeout = Duration::new(5, 0);
        let client = ClientBuilder::new().timeout(timeout).build().unwrap();

        assert_eq!(
            check_domain(&client, "https://google.com".to_string()).await,
            CheckResult::SUCCESS
        );
        assert_eq!(
            check_domain(&client, "https://sentry.io".to_string()).await,
            CheckResult::SUCCESS
        );
        assert_eq!(check_domain(&client, "https://hjkhjkljkh.io".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
        assert_eq!(check_domain(&client, "https://santry.io".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
    }
}
