pub mod http_checker;

use crate::{
    config_store::Tick,
    types::{check_config::CheckConfig, result::CheckResult},
};

/// A Checker is responsible for actually making checks given a [`CheckConfig`] and the [`Tick`] at
/// which the check is being made.
pub trait Checker {
    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    async fn check_url(&self, config: &CheckConfig, tick: &Tick) -> CheckResult;
}
