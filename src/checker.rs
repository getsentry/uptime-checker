pub mod dummy_checker;
pub mod http_checker;
pub mod ip_filter;

use std::future::Future;

use crate::{
    config_store::Tick,
    types::{check_config::CheckConfig, result::CheckResult},
};

/// A Checker is responsible for actually making checks given a [`CheckConfig`] and the [`Tick`] at
/// which the check is being made.
pub trait Checker: Send + Sync {
    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    fn check_url(
        &self,
        config: &CheckConfig,
        tick: &Tick,
    ) -> impl Future<Output = CheckResult> + Send;
}
