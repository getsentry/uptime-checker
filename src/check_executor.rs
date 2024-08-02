use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::checker::Checker;
use crate::config_store::Tick;
use crate::producer::ResultsProducer;
use crate::types::check_config::CheckConfig;
use crate::types::result::{CheckResult, CheckStatus, CheckStatusReasonType};

#[derive(Debug)]
pub struct ScheduledCheck {
    pub tick: Tick,
    pub config: Arc<CheckConfig>,
    pub resolve: Sender<CheckResult>,
}

pub fn run_executor(
    concurrency: usize,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
) -> (UnboundedSender<ScheduledCheck>, JoinHandle<()>) {
    tracing::info!("Starting check executor");

    let (sender, reciever) = mpsc::unbounded_channel();
    let executor =
        tokio::spawn(async move { executor_loop(concurrency, checker, producer, reciever).await });

    (sender, executor)
}

async fn executor_loop(
    concurrency: usize,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    reciever: UnboundedReceiver<ScheduledCheck>,
) {
    let schedule_check_stream: UnboundedReceiverStream<_> = reciever.into();

    schedule_check_stream
        .for_each_concurrent(concurrency, |scheduled_check| {
            let job_checker = checker.clone();
            let job_producer = producer.clone();

            // TODO(epurkhiser): Record metrics on the size of the size of the

            async move {
                let ScheduledCheck {
                    config,
                    tick,
                    resolve,
                } = scheduled_check;

                let _ = tokio::spawn(async move {
                    let check_result = job_checker.check_url(&config, &tick).await;

                    if let Err(e) = job_producer.produce_checker_result(&check_result) {
                        tracing::error!(error = ?e, "Failed to produce check result");
                    }

                    record_result_metrics(&check_result);
                    tracing::info!(result = ?check_result, "Check complete");

                    resolve
                        .send(check_result)
                        .expect("Failed to resolve completed check");
                })
                .await;
            }
        })
        .await;
}

fn record_result_metrics(result: &CheckResult) {
    // Record metrics
    let CheckResult {
        status,
        scheduled_check_time,
        actual_check_time,
        duration,
        status_reason,
        ..
    } = result;

    let status_label = match status {
        CheckStatus::Success => "success",
        CheckStatus::Failure => "failure",
        CheckStatus::MissedWindow => "missed_window",
    };
    let failure_reason = match status_reason.as_ref().map(|r| r.status_type) {
        Some(CheckStatusReasonType::Failure) => Some("failure"),
        Some(CheckStatusReasonType::DnsError) => Some("dns_error"),
        Some(CheckStatusReasonType::Timeout) => Some("timeout"),
        None => None,
    };

    // Record duration of check
    if let Some(duration) = duration {
        metrics::histogram!(
            "check_result.duration_ms",
            "histogram" => "timer",
            "status" => status_label,
            "failure_reason" => failure_reason.unwrap_or("ok"),
        )
        .record(duration.num_milliseconds() as f64);
    }

    // Record time between scheduled and actual check
    metrics::histogram!(
        "check_result.delay_ms",
        "histogram" => "timer",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
    )
    .record((*actual_check_time - *scheduled_check_time).num_milliseconds() as f64);

    // Record status of the check
    metrics::counter!(
        "check_result.processed",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
    )
    .increment(1);
}
