use crate::app::config::MetricsConfig;

use metrics_exporter_statsd::StatsdBuilder;

/// Configures the global metrics recorder.
pub fn init(config: &MetricsConfig) {
    // Parse address into host and port
    let address = config.statsd_addr;

    let mut builder = StatsdBuilder::from(address.ip().to_string(), address.port())
        .with_queue_size(5000)
        .with_buffer_size(1024);

    for (key, value) in &config.default_tags {
        builder = builder.with_default_tag(key, value);
    }

    if let Some(hostname_tag) = &config.hostname_tag {
        if let Some(hostname) = hostname::get().ok().and_then(|s| s.into_string().ok()) {
            builder = builder.with_default_tag(hostname_tag, hostname);
        }
    }

    let recorder = builder
        .build(Some("uptime_checker"))
        .expect("Could not create StatsdRecorder");

    metrics::set_global_recorder(recorder).expect("Could not set global metrics recorder")
}
