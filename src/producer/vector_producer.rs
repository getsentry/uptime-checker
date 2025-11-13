use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

struct VectorRequestWorkerConfig {
    vector_batch_size: usize,
    endpoint: String,
    retry_vector_errors_forever: bool,
    region: String,
}

pub struct VectorResultsProducer {
    schema: Schema,
    sender: UnboundedSender<Vec<u8>>,
    pending_items: Arc<AtomicUsize>,
}

impl VectorResultsProducer {
    pub fn new(
        topic_name: &str,
        endpoint: String,
        vector_batch_size: usize,
        retry_vector_errors_forever: bool,
        region: String,
    ) -> (Self, JoinHandle<()>) {
        let schema =
            sentry_kafka_schemas::get_schema(topic_name, None).expect("Schema should exist");
        let client = Client::builder()
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Client builder for vector should not fail");
        let pending_items = Arc::new(AtomicUsize::new(0));

        let config = VectorRequestWorkerConfig {
            vector_batch_size,
            endpoint,
            retry_vector_errors_forever,
            region,
        };

        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = Self::spawn_worker(config, client, receiver, pending_items.clone());

        (
            Self {
                schema,
                sender,
                pending_items,
            },
            worker,
        )
    }

    fn spawn_worker(
        config: VectorRequestWorkerConfig,
        client: Client,
        mut receiver: UnboundedReceiver<Vec<u8>>,
        pending_items: Arc<AtomicUsize>,
    ) -> JoinHandle<()> {
        tracing::info!("vector_worker.starting");

        tokio::spawn(async move {
            // Avoid cloning this string over and over again.
            let region: &'static str = config.region.clone().leak();
            let mut batch = Vec::with_capacity(config.vector_batch_size);

            while let Some(json) = receiver.recv().await {
                let new_count = pending_items.fetch_sub(1, Ordering::Relaxed);
                metrics::gauge!("producer.pending_items").set(new_count as f64);

                batch.push(json);

                if batch.len() < config.vector_batch_size {
                    continue;
                }

                let batch_to_send =
                    std::mem::replace(&mut batch, Vec::with_capacity(config.vector_batch_size));
                if let Err(e) = {
                    let start = std::time::Instant::now();
                    let result = send_batch(
                        batch_to_send,
                        client.clone(),
                        config.endpoint.clone(),
                        config.retry_vector_errors_forever,
                        region,
                    )
                    .await;
                    metrics::histogram!("vector_producer.send_batch.duration", "uptime_region" => region, "histogram" => "timer").record(start.elapsed().as_secs_f64());
                    result
                } {
                    tracing::error!(error = ?e, "vector_batch.send_failed");
                }
            }

            if !batch.is_empty() {
                if let Err(e) = send_batch(
                    batch,
                    client,
                    config.endpoint,
                    config.retry_vector_errors_forever,
                    region,
                )
                .await
                {
                    tracing::error!(error = ?e, "final_batch.send_failed");
                }
            }

            tracing::info!("vector_worker.shutdown");
        })
    }
}

impl ResultsProducer for VectorResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;

        // Send the serialized result to the worker task
        if self.sender.send(json).is_err() {
            tracing::error!("event.send_failed_channel_closed");
            return Err(ExtractCodeError::VectorError);
        }
        self.pending_items
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }
}

async fn send_batch(
    batch: Vec<Vec<u8>>,
    client: Client,
    endpoint: String,
    retry_vector_errors_forever: bool,
    region: &'static str,
) -> Result<(), ExtractCodeError> {
    if batch.is_empty() {
        return Ok(());
    }

    let body: String = batch
        .iter()
        .filter_map(|json| String::from_utf8(json.clone()).ok())
        .map(|s| s + "\n")
        .collect();

    const BASE_DELAY_MS: u64 = 100;
    const MAX_DELAY_MS: u64 = 2000;

    let mut num_of_retries = 0;
    loop {
        let response = client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
            .await;

        // Calculate delay with a maximum cap
        let delay =
            Duration::from_millis((BASE_DELAY_MS * (2_u64.pow(num_of_retries))).max(MAX_DELAY_MS));

        match response {
            Ok(resp) if !resp.status().is_server_error() => {
                let stats = resp.stats();
                metrics::gauge!("vector.num_retries", "uptime_region" => region)
                    .set(num_of_retries as f64);

                if let Some(s) = stats.redirects().first() {
                    if let Some(c) = s.get_http_stats().get_connection_stats() {
                        let start = c.get_start_instant();
                        let end = if let Some(tls) = c.get_tls_end() {
                            tls
                        } else {
                            c.get_connect_end()
                        };

                        metrics::histogram!("vector.connect_duration", "histogram" => "timer", "uptime_region" => region)
                            .record(end.duration_since(start).as_millis() as f64);
                        metrics::histogram!("vector.request_sent_duration", "histogram" => "timer", "uptime_region" => region).record(s.get_response_start().duration_since(start).as_millis() as f64);
                        metrics::histogram!("vector.request_duration", "histogram" => "timer", "uptime_region" => region)
                            .record(s.get_request_end().duration_since(start).as_millis() as f64);
                    }
                }
                return Ok(());
            }
            r => {
                num_of_retries += 1;
                match r {
                    Ok(resp) => {
                        tracing::warn!(
                            status = ?resp.status(),
                            retry = num_of_retries,
                            delay_ms = ?delay.as_millis(),
                            batch_size = batch.len(),
                            "request.failed_to_vector_retrying"
                        );
                    }
                    Err(e) => {
                        // Categorize the error for better diagnostics
                        let error_type = if e.is_timeout() {
                            "timeout"
                        } else if e.is_connect() {
                            "connection_refused"
                        } else if e.is_request() {
                            "request_error"
                        } else if e.is_body() {
                            "body_error"
                        } else if e.is_decode() {
                            "decode_error"
                        } else {
                            "unknown"
                        };

                        tracing::warn!(
                            error = ?e,
                            error_type = error_type,
                            "request.failed_to_vector_retrying"
                        );
                    }
                }
                if !retry_vector_errors_forever {
                    return Err(ExtractCodeError::VectorError);
                }
                sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VectorResultsProducer;
    use crate::{
        producer::ResultsProducer,
        types::{
            result::{
                CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType,
                RequestDurations, RequestInfo,
            },
            shared::RequestMethod,
        },
    };
    use chrono::{TimeDelta, Utc};
    use httpmock::prelude::*;
    use httpmock::Method;
    use sentry::protocol::SpanId;
    use std::time::Duration;
    use tokio::time::sleep;
    use uuid::{uuid, Uuid};

    const TEST_BATCH_SIZE: usize = 2;

    fn create_test_result() -> CheckResult {
        let ri = RequestInfo {
            certificate_info: None,
            request_type: RequestMethod::Get,
            http_status_code: Some(200),
            url: "http://santry.ayo".to_string(),
            request_body_size_bytes: 0,
            response_body_size_bytes: 0,
            request_duration_us: 123456,
            durations: RequestDurations::default(),
        };

        CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: uuid!("23d6048d67c948d9a19c0b47979e9a03"),
            status: CheckStatus::Success,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "hi".to_string(),
            }),
            trace_id: Uuid::new_v4(),
            span_id: SpanId::default(),
            scheduled_check_time: Utc::now(),
            scheduled_check_time_us: Utc::now(),
            actual_check_time: Utc::now(),
            actual_check_time_us: Utc::now(),
            duration: Some(TimeDelta::seconds(1)),
            duration_us: Some(TimeDelta::seconds(1)),
            request_info: Some(ri.clone()),
            request_info_list: vec![ri.clone()],
            region: "us-west-1",
        }
    }

    #[tokio::test]
    async fn test_batch_events() {
        let mock_server = MockServer::start();
        let (producer, worker) = VectorResultsProducer::new(
            "uptime-results",
            mock_server.url("/").to_string(),
            TEST_BATCH_SIZE,
            false,
            "us-west-1".to_string(),
        );

        // Create and send BATCH_SIZE + 1 event
        for _ in 0..(TEST_BATCH_SIZE + 1) {
            let test_result = create_test_result();
            let result = producer.produce_checker_result(&test_result);
            assert!(result.is_ok());
        }

        // Mock for full batch and remainder
        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        let len = lines.len();
                        if len != TEST_BATCH_SIZE && len != 1 {
                            return false;
                        }
                        // Verify each line is valid JSON with expected fields
                        lines.iter().all(|line| {
                            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
                                return value["subscription_id"]
                                    == "23d6048d67c948d9a19c0b47979e9a03"
                                    && value["status"] == "success"
                                    && value["region"] == "us-west-1";
                            }
                            false
                        })
                    } else {
                        false
                    }
                });
            then.status(200);
        });

        // Wait a bit for the requests to be processed
        sleep(Duration::from_millis(100)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;

        // We expect 2 requests: one with BATCH_SIZE events and one with 2 events
        mock.assert_hits(2);
    }

    #[tokio::test]
    async fn test_flush_on_shutdown() {
        let mock_server = MockServer::start();
        // Create a mock that expects a single request with less than BATCH_SIZE events
        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        // We expect only 1 event, which is less than BATCH_SIZE
                        if lines.len() != 1 {
                            return false;
                        }
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(lines[0]) {
                            return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
                                && value["status"] == "success"
                                && value["region"] == "us-west-1";
                        }
                    }
                    false
                });
            then.status(200);
        });

        let (producer, worker) = VectorResultsProducer::new(
            "uptime-results",
            mock_server.url("/").to_string(),
            TEST_BATCH_SIZE,
            false,
            "us-west-1".to_string(),
        );

        // Send a single event (less than BATCH_SIZE)
        let test_result = create_test_result();
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Give the worker a moment to process the event
        sleep(Duration::from_millis(50)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;

        // Verify that the event was sent despite not reaching BATCH_SIZE
        mock.assert();
    }
}
