pub mod kafka_producer;

use sentry_kafka_schemas::SchemaError;

use rust_arroyo::backends::ProducerError;

use crate::types::result::CheckResult;

#[derive(Debug, thiserror::Error)]
pub enum ExtractCodeError {
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Producer(#[from] ProducerError),
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

pub trait ResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError>;
}
