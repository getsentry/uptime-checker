use std::net::SocketAddr;

use crate::app::config::Config;

use metrics_exporter_statsd::StatsdBuilder;

pub struct MetricsConfig {
    statsd_addr: SocketAddr,
}

impl MetricsConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            statsd_addr: config.statsd_addr,
        }
    }
}

/// Configures the global metrics recorder.
pub fn init(config: MetricsConfig) {
    // Parse address into host and port
    let address = config.statsd_addr;

    let recorder = StatsdBuilder::from(address.ip().to_string(), address.port())
        .with_queue_size(5000)
        .with_buffer_size(1024)
        .build(Some("prefix"))
        .expect("Could not create StatsdRecorder");

    metrics::set_global_recorder(recorder).expect("Could not set global metrics recorder")
}
