use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use sentry_kafka_schemas::Schema;

pub struct DummyResultsProducer {
    schema: Schema,
}

impl DummyResultsProducer {
    pub fn new(topic_name: &str) -> Self {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        Self { schema }
    }
}

impl ResultsProducer for DummyResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;
        Ok(())
    }
}
