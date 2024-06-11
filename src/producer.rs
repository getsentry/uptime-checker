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

pub struct ResultProducer {
    producer: KafkaProducer,
    topic: TopicOrPartition,
}

impl ResultProducer {
    pub fn new(topic_name: &str, config: KafkaConfig) -> Self {
        let producer = KafkaProducer::new(config);
        let topic = TopicOrPartition::Topic(Topic::new(topic_name));
        Self { producer, topic }
    }

    pub async fn produce_checker_result(
        &self,
        result: &CheckResult,
    ) -> Result<(), ExtractCodeError> {
        // TODO: Note that this will rarely fail directly here - mostly will only happen if the queue
        // is full. We need to have a callback that tells us whether the other produces succeed so we
        // know whether to mark the check as successful. We never want to just silently drop a result -
        // we'd much prefer to retry it later and produce a miss, so that we at least know that we missed
        // and don't have holes.
        Ok(self.producer.produce(
            &self.topic,
            KafkaPayload::new(None, None, Some(serde_json::to_vec(result)?)),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtractCodeError, ResultProducer};
    use rust_arroyo::backends::kafka::config::KafkaConfig;
    use uuid::Uuid;
    use crate::types::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo, RequestType};

    pub async fn send_result(result: CheckStatus) -> Result<(), ExtractCodeError>{
        let guid = Uuid::new_v4();
        let result = CheckResult {
            guid: guid,
            monitor_id: 123,
            monitor_environment_id: 456,
            status: result,
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
                http_status_code: Some(200),
            }),
        };
        // TODO: Have an actual Kafka running for a real test. At the moment this is fine since
        // it will fail async
        let config = KafkaConfig::new_config(["0.0.0.0".to_string()].to_vec(), None);
        let producer = ResultProducer::new("test-topic", config);
        producer.produce_checker_result(&result).await
    }

    #[tokio::test]
    async fn test() {
        let result = send_result(CheckStatus::Success).await;
        assert_eq!(result.unwrap(), ());
    }

}