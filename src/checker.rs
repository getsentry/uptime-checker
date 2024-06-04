use std::error::Error;

use reqwest::{Response, StatusCode};

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

pub async fn do_request(
    client: &reqwest::Client,
    domain: String,
) -> Result<Response, reqwest::Error> {
    /*
    Fetches the response from a URL. First attempts to fetch just the head, and if not supported falls
    back to fetching the entire body.
     */
    let response = client.head(&domain).send().await?;
    match response.status() {
        StatusCode::METHOD_NOT_ALLOWED => client.get(&domain).send().await,
        _ => Ok(response),
    }
}

pub async fn check_domain(client: &reqwest::Client, domain: String) -> CheckResult {
    /*
    Checks whether a domain is up and working. Makes a request to the domain and if a 2xx is
    returned then the domain is considered working.
     */

    match do_request(client, domain).await {
        Ok(_) => CheckResult::SUCCESS,
        Err(e) => {
            if e.is_timeout() {
                return CheckResult::FAILURE(FailureReason::Timeout);
            }
            // TODO: More reasons
            let mut inner = &e as &dyn Error;
            while let Some(source) = inner.source() {
                inner = source;

                // TODO: Would be better to get specific errors without string matching like this
                // Not sure if there's a better way
                let inner_message = inner.to_string();
                if inner_message.contains("dns error") {
                    return CheckResult::FAILURE(FailureReason::DnsError(
                        inner.source().unwrap().to_string(),
                    ));
                }
            }
            // TODO: Should incorporate status code somehow
            CheckResult::FAILURE(FailureReason::Error(format!("{:?}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_domain, CheckResult, FailureReason};
    use httpmock::prelude::*;
    use httpmock::Method;
    use reqwest::{ClientBuilder, StatusCode};
    use std::time::Duration;
    // use crate::checker::FailureReason;

    #[tokio::test]
    async fn test_add() {
        let server = MockServer::start();
        let head_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/head");
            then.status(200);
        });
        let head_disallowed_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/no-head");
            then.status(StatusCode::METHOD_NOT_ALLOWED.as_u16());
        });
        let get_mock = server.mock(|when, then| {
            when.method(Method::GET).path("/no-head");
            then.status(200);
        });
        let timeout_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/timeout");
            then.delay(Duration::from_millis(300)).status(200);
        });

        let timeout = Duration::from_millis(200);
        let client = ClientBuilder::new().timeout(timeout).build().unwrap();

        assert_eq!(
            check_domain(&client, server.url("/head")).await,
            CheckResult::SUCCESS
        );
        assert_eq!(
            check_domain(&client, server.url("/no-head")).await,
            CheckResult::SUCCESS
        );
        assert_eq!(
            check_domain(&client, server.url("/timeout")).await,
            CheckResult::FAILURE(FailureReason::Timeout)
        );
        head_mock.assert();
        head_disallowed_mock.assert();
        get_mock.assert();
        timeout_mock.assert();
        // TODO: Figure out how to simulate a DNS failure
        // assert_eq!(check_domain(&client, "https://hjkhjkljkh.io/".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
   }
}
