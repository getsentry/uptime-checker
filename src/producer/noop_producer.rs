use crate::{
    producer::{ExtractCodeError, ResultsProducer},
    types::result::CheckResult,
};

/// A trivial results producer that does nothing; this is useful (for example) in the
/// check endpoint, where we don't produce the check result to anything asynchronously.
pub struct NoopResultsProducer {}

impl NoopResultsProducer {
    pub fn new() -> Self {
        Self {}
    }
}

impl ResultsProducer for NoopResultsProducer {
    fn produce_checker_result(&self, _: &CheckResult) -> Result<(), ExtractCodeError> {
        Ok(())
    }
}
