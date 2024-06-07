use rust_arroyo::backends::kafka::config::KafkaConfig;
use rust_arroyo::backends::kafka::producer::KafkaProducer;
use rust_arroyo::backends::kafka::types::KafkaPayload;

use rust_arroyo::types::{Topic, TopicOrPartition};

use crate::types::{
    CheckResult,
};
use rust_arroyo::backends::{Producer, ProducerError};

#[derive(Debug, thiserror::Error)]
pub enum ExtractCodeError {
    #[error(transparent)]
    SerializationError(#[from] serde_json::Error),
    #[error(transparent)]
    ProducerError(#[from] ProducerError),
}

pub async fn produce_checker_result(
    result: CheckResult,
    topic: Topic,
    config: KafkaConfig,
) -> Result<(), ExtractCodeError> {
    let producer = KafkaProducer::new(config.clone());
    let topic = TopicOrPartition::Topic(topic);
    // TODO: Note that this will rarely fail directly here - mostly will only happen if the queue
    // is full. We need to have a callback that tells us whether the other produces succeed so we
    // know whether to mark the check as successful. We never want to just silently drop a result -
    // we'd much prefer to retry it later and produce a miss, so that we at least know that we missed
    // and don't have holes.
    Ok(producer.produce(
        &topic,
        KafkaPayload::new(None, None, Some(serde_json::to_vec(&result)?)),
    )?)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDateTime;
    use chrono::offset::Local;
    use super::{ExtractCodeError, produce_checker_result};
    use rust_arroyo::backends::kafka::config::KafkaConfig;
    use rust_arroyo::types::Topic;
    use uuid::Uuid;
    use crate::checker::{check_url, FailureReason, UrlCheckResult};
    use crate::checker::UrlCheckResult::Success;
    use crate::types::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo, RequestType};

    pub async fn send_result(result: UrlCheckResult) -> Result<(), ExtractCodeError>{
        let guid = Uuid::new_v4();
        let result = CheckResult {
            guid: guid,
            monitor_id: 123,
            monitor_environment_id: 456,
            status: CheckStatus::Success,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "hi".to_string(),
            }),
            trace_id: guid,
            scheduled_check_time: 789,
            actual_check_time: 123456,
            duration_ms: Some(100),
            request_info: Some(RequestInfo {
                request_type: RequestType::Head,
                http_status_code: 200,
            }),
        };
        // TODO: Have an actual Kafka running for a real test. At the moment this is fine since
        // it will fail async
        let config = KafkaConfig::new_config(["0.0.0.0".to_string()].to_vec(), None);
        produce_checker_result(result, Topic::new("test-topic"), config).await
    }

    #[tokio::test]
    async fn test() {
        let result = send_result(Success).await;
        assert_eq!(result.unwrap(), ());


    }

}