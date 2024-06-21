use uuid::Uuid;

/// Valid intervals between the checks.
#[derive(Debug, Copy, Clone)]
pub enum CheckInterval {
    OneMinute = 1,
    FiveMinutes = 5,
    TenMinutes = 10,
    TwentyMinutes = 20,
    ThirtyMintues = 30,
    SixtyMinutes = 60,
}

/// The CheckConfig represents a configuration for a single check.
#[derive(Debug)]
pub struct CheckConfig {
    /// The subscription this check configuration is associated to in sentry.
    subscription_id: Uuid,

    /// The interval between each check run.
    interval: CheckInterval,

    // The total time we will allow to make the request in seconds.
    timeout: u16,

    /// The actual HTTP URL to check.
    url: String,
}
