pub mod dummy_producer;
pub mod kafka_producer;
pub mod vector_producer;

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
    #[error("Vector request failed")]
    VectorError,
}

pub trait ResultsProducer: Send + Sync {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError>;
}
