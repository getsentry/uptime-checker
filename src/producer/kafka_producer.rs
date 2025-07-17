use sentry_arroyo::backends::kafka::config::KafkaConfig;
use sentry_arroyo::backends::kafka::producer::KafkaProducer;
use sentry_arroyo::backends::kafka::types::KafkaPayload;

use sentry_arroyo::types::{Topic, TopicOrPartition};
use sentry_kafka_schemas::Schema;

use crate::types::result::CheckResult;
use sentry_arroyo::backends::Producer;

use super::{ExtractCodeError, ResultsProducer};

pub struct KafkaResultsProducer {
    producer: KafkaProducer,
    topic: TopicOrPartition,
    schema: Schema,
}

impl KafkaResultsProducer {
    pub fn new(topic_name: &str, config: KafkaConfig) -> Self {
        let producer = KafkaProducer::new(config);
        let topic = TopicOrPartition::Topic(Topic::new(topic_name));
        let schema =
            sentry_kafka_schemas::get_schema("uptime-results", None).expect("Schema should exist");

        Self {
            producer,
            topic,
            schema,
        }
    }
}

impl ResultsProducer for KafkaResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;

        // TODO: Note that this will rarely fail directly here - mostly will only happen if the queue
        // is full. We need to have a callback that tells us whether the other produces succeed so we
        // know whether to mark the check as successful. We never want to just silently drop a result -
        // we'd much prefer to retry it later and produce a miss, so that we at least know that we missed
        // and don't have holes.
        Ok(self.producer.produce(
            &self.topic,
            KafkaPayload::new(Some(result.subscription_id.into()), None, Some(json)),
        )?)
    }
}

#[cfg(test)]
mod tests {

    use super::KafkaResultsProducer;
    use crate::{
        producer::{ExtractCodeError, ResultsProducer},
        types::{
            result::{
                CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo,
            },
            shared::RequestMethod,
        },
    };
    use chrono::{TimeDelta, Utc};
    use sentry_arroyo::backends::kafka::config::KafkaConfig;
    use sentry::protocol::SpanId;
    use uuid::{uuid, Uuid};

    pub fn send_result(result: CheckStatus) -> Result<(), ExtractCodeError> {
        let guid = Uuid::new_v4();
        let result = CheckResult {
            guid,
            subscription_id: uuid!("23d6048d67c948d9a19c0b47979e9a03"),
            status: result,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "hi".to_string(),
            }),
            trace_id: guid,
            span_id: SpanId::default(),
            scheduled_check_time: Utc::now(),
            actual_check_time: Utc::now(),
            duration: Some(TimeDelta::seconds(1)),
            request_info: Some(RequestInfo {
                request_type: RequestMethod::Get,
                http_status_code: Some(200),
            }),
            region: "us-west-1",
        };
        // TODO: Have an actual Kafka running for a real test. At the moment this is fine since
        // it will fail async
        let config = KafkaConfig::new_config(["0.0.0.0".to_string()].to_vec(), None);
        let producer = KafkaResultsProducer::new("uptime-results", config);
        producer.produce_checker_result(&result)
    }

    #[test]
    fn test() {
        let _ = send_result(CheckStatus::Success);
    }
}
