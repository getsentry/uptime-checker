use rust_arroyo::backends::kafka::config::KafkaConfig;
use rust_arroyo::backends::kafka::producer::KafkaProducer;
use rust_arroyo::backends::kafka::types::KafkaPayload;

use rust_arroyo::types::{Topic, TopicOrPartition};
use sentry_kafka_schemas::{Schema, SchemaError};

use crate::types::result::CheckResult;
use rust_arroyo::backends::{Producer, ProducerError};

#[derive(Debug, thiserror::Error)]
pub enum ExtractCodeError {
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Producer(#[from] ProducerError),
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

pub struct ResultProducer {
    producer: KafkaProducer,
    topic: TopicOrPartition,
    schema: Schema,
}

impl ResultProducer {
    pub fn new(topic_name: &str, config: KafkaConfig) -> Self {
        let producer = KafkaProducer::new(config);
        let topic = TopicOrPartition::Topic(Topic::new(topic_name));
        let schema = sentry_kafka_schemas::get_schema("uptime-results", None).unwrap();

        Self {
            producer,
            topic,
            schema,
        }
    }

    pub async fn produce_checker_result(
        &self,
        result: &CheckResult,
    ) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;

        // TODO: Note that this will rarely fail directly here - mostly will only happen if the queue
        // is full. We need to have a callback that tells us whether the other produces succeed so we
        // know whether to mark the check as successful. We never want to just silently drop a result -
        // we'd much prefer to retry it later and produce a miss, so that we at least know that we missed
        // and don't have holes.
        Ok(self
            .producer
            .produce(&self.topic, KafkaPayload::new(None, None, Some(json)))?)
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtractCodeError, ResultProducer};
    use crate::types::result::{
        CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo,
        RequestType,
    };
    use rust_arroyo::backends::kafka::config::KafkaConfig;
    use sentry::protocol::{SpanId, TraceId};
    use uuid::{uuid, Uuid};

    pub async fn send_result(result: CheckStatus) -> Result<(), ExtractCodeError> {
        let guid = Uuid::new_v4();
        let result = CheckResult {
            guid,
            subscription_id: uuid!("23d6048d67c948d9a19c0b47979e9a03"),
            status: result,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "hi".to_string(),
            }),
            trace_id: TraceId::default(),
            span_id: SpanId::default(),
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
        let producer = ResultProducer::new("uptime-results", config);
        producer.produce_checker_result(&result).await
    }

    #[tokio::test]
    async fn test() {
        let _ = send_result(CheckStatus::Success).await;
    }
}
