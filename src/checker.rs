use std::error::Error;

use reqwest::{Response, StatusCode};

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CheckResult {
    Success,
    Failure(FailureReason),
    Missed,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FailureReason {
    Timeout,
    DnsError(String),
    Error(String),
}
/// Fetches the response from a URL.
///
/// First attempts to fetch just the head, and if not supported falls back to fetching the entire body.
pub async fn do_request(
    client: &reqwest::Client,
    url: String,
) -> Result<Response, reqwest::Error> {
    let response = client.head(&url).send().await?;
    match response.status() {
        StatusCode::METHOD_NOT_ALLOWED => client.get(&url).send().await,
        _ => Ok(response),
    }
}


/// Makes a request to a url to determine whether it is up.
/// Up is defined as returning a 2xx within a specific timeframe.
pub async fn check_url(client: &reqwest::Client, url: String) -> CheckResult {
    match do_request(client, url).await {
        Ok(_) => CheckResult::Success,
        Err(e) => {
            if e.is_timeout() {
                return CheckResult::Failure(FailureReason::Timeout);
            }
            // TODO: More reasons
            let mut inner = &e as &dyn Error;
            while let Some(source) = inner.source() {
                inner = source;

                // TODO: Would be better to get specific errors without string matching like this
                // Not sure if there's a better way
                let inner_message = inner.to_string();
                if inner_message.contains("dns error") {
                    return CheckResult::Failure(FailureReason::DnsError(
                        inner.source().unwrap().to_string(),
                    ));
                }
            }
            // TODO: Should incorporate status code somehow
            CheckResult::Failure(FailureReason::Error(format!("{:?}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_url, CheckResult, FailureReason};
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
            check_url(&client, server.url("/head")).await,
            CheckResult::Success
        );
        assert_eq!(
            check_url(&client, server.url("/no-head")).await,
            CheckResult::Success
        );
        assert_eq!(
            check_url(&client, server.url("/timeout")).await,
            CheckResult::Failure(FailureReason::Timeout)
        );
        head_mock.assert();
        head_disallowed_mock.assert();
        get_mock.assert();
        timeout_mock.assert();
        // TODO: Figure out how to simulate a DNS failure
        // assert_eq!(check_domain(&client, "https://hjkhjkljkh.io/".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
   }
}
