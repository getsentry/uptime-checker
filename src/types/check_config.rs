use super::shared::{RegionScheduleMode, RequestMethod};
use crate::config_store::Tick;
use chrono::TimeDelta;
use serde::Deserialize;
use serde_repr::Deserialize_repr;
use serde_with::serde_as;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

const ONE_MINUTE: isize = 60;

/// Valid intervals between the checks in seconds.
#[repr(isize)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize_repr)]
pub enum CheckInterval {
    OneMinute = ONE_MINUTE,
    FiveMinutes = ONE_MINUTE * 5,
    TenMinutes = ONE_MINUTE * 10,
    TwentyMinutes = ONE_MINUTE * 20,
    ThirtyMinutes = ONE_MINUTE * 30,
    SixtyMinutes = ONE_MINUTE * 60,
}

/// The largest check interval
pub const MAX_CHECK_INTERVAL_SECS: usize = CheckInterval::SixtyMinutes as usize;

/// The CheckConfig represents a configuration for a single check.
#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CheckConfig {
    /// The subscription this check configuration is associated to in sentry.
    #[serde(with = "uuid::serde::simple")]
    pub subscription_id: Uuid,

    /// The interval between each check run.
    #[serde(rename = "interval_seconds")]
    pub interval: CheckInterval,

    // The total time we will allow to make the request in.
    #[serde(rename = "timeout_ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<i64>")]
    pub timeout: TimeDelta,

    /// The actual HTTP URL to check.
    pub url: String,

    /// The HTTP method to use to make the request.
    #[serde(default)]
    pub request_method: RequestMethod,

    /// Additional HTTP headers to pass through to the request.
    #[serde(default)]
    pub request_headers: Vec<(String, String)>,

    /// The body to pass through to the request.
    #[serde(default)]
    pub request_body: String,

    /// If we should allow sampling on the trace spans.
    #[serde(default)]
    pub trace_sampling: bool,

    #[serde(default)]
    pub active_regions: Option<Vec<String>>,

    #[serde(default)]
    pub region_schedule_mode: Option<RegionScheduleMode>,
}

impl Hash for CheckConfig {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.subscription_id.hash(state);
    }
}

impl CheckConfig {
    /// Compute at which seconds within the TickBuckets the CheckConfig belongs to. This is
    /// consistent on the subscription_id.
    pub fn slots(&self) -> Vec<usize> {
        let interval_secs = self.interval as usize;

        let first_slot = self
            .subscription_id
            .as_u128()
            .checked_rem(self.interval as u128)
            .unwrap() as usize;

        let pattern_count = MAX_CHECK_INTERVAL_SECS / interval_secs;

        (0..pattern_count)
            .map(|c| first_slot + (interval_secs * c))
            .collect()
    }

    pub fn should_run(&self, tick: Tick, current_region: &String) -> bool {
        // Determines if this check should run in the current region at this current tick

        // If region configs aren't present, assume we should always run this check
        if self.region_schedule_mode.is_none() {
            return true;
        }

        let Some(active_regions) = self.active_regions.as_ref() else {
            return true;
        };

        let running_region_idx = (tick.time().timestamp() / self.interval as i64)
            .checked_rem(active_regions.len() as i64)
            .unwrap();
        &active_regions[running_region_idx as usize] == current_region
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckConfig, CheckInterval, RequestMethod};
    use crate::config_store::Tick;
    use crate::types::check_config::MAX_CHECK_INTERVAL_SECS;
    use crate::types::shared::RegionScheduleMode;
    use chrono::{DateTime, TimeDelta, Utc};
    use similar_asserts::assert_eq;
    use uuid::{uuid, Uuid};

    impl Default for CheckConfig {
        fn default() -> Self {
            CheckConfig {
                subscription_id: Uuid::from_u128(0),
                interval: CheckInterval::OneMinute,
                timeout: TimeDelta::seconds(10),
                url: "https://example.com".to_string(),
                request_method: Default::default(),
                request_headers: Default::default(),
                request_body: Default::default(),
                trace_sampling: false,
                active_regions: None,
                region_schedule_mode: None,
            }
        }
    }

    #[test]
    fn test_serialize_msgpack_roundtrip_simple() {
        // Example msgpack taken from
        // sentry-kafka-schemas/examples/uptime-configs/1/example.msgpack
        let example = vec![
            0x86, 0xaf, 0x73, 0x75, 0x62, 0x73, 0x63, 0x72, 0x69, 0x70, 0x74, 0x69, 0x6f, 0x6e,
            0x5f, 0x69, 0x64, 0xd9, 0x20, 0x64, 0x37, 0x36, 0x32, 0x39, 0x63, 0x36, 0x63, 0x38,
            0x32, 0x62, 0x65, 0x34, 0x66, 0x36, 0x37, 0x39, 0x65, 0x65, 0x37, 0x38, 0x61, 0x30,
            0x64, 0x39, 0x37, 0x37, 0x38, 0x35, 0x36, 0x64, 0x32, 0xa3, 0x75, 0x72, 0x6c, 0xb0,
            0x68, 0x74, 0x74, 0x70, 0x3a, 0x2f, 0x2f, 0x73, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x2e,
            0x69, 0x6f, 0xb0, 0x69, 0x6e, 0x74, 0x65, 0x72, 0x76, 0x61, 0x6c, 0x5f, 0x73, 0x65,
            0x63, 0x6f, 0x6e, 0x64, 0x73, 0xcd, 0x01, 0x2c, 0xaa, 0x74, 0x69, 0x6d, 0x65, 0x6f,
            0x75, 0x74, 0x5f, 0x6d, 0x73, 0xcd, 0x01, 0xf4, 0xae, 0x61, 0x63, 0x74, 0x69, 0x76,
            0x65, 0x5f, 0x72, 0x65, 0x67, 0x69, 0x6f, 0x6e, 0x73, 0x92, 0xa7, 0x75, 0x73, 0x2d,
            0x77, 0x65, 0x73, 0x74, 0xa6, 0x65, 0x75, 0x72, 0x6f, 0x70, 0x65, 0xb4, 0x72, 0x65,
            0x67, 0x69, 0x6f, 0x6e, 0x5f, 0x73, 0x63, 0x68, 0x65, 0x64, 0x75, 0x6c, 0x65, 0x5f,
            0x6d, 0x6f, 0x64, 0x65, 0xab, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x5f, 0x72, 0x6f, 0x62,
            0x69, 0x6e,
        ];

        assert_eq!(
            rmp_serde::from_slice::<CheckConfig>(&example).unwrap(),
            CheckConfig {
                subscription_id: uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2"),
                timeout: TimeDelta::milliseconds(500),
                interval: CheckInterval::FiveMinutes,
                url: "http://sentry.io".to_string(),
                request_method: RequestMethod::Get,
                request_headers: vec![],
                request_body: "".to_string(),
                trace_sampling: false,
                active_regions: Some(vec!["us-west".to_string(), "europe".to_string()]),
                region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            }
        );
    }

    #[test]
    fn test_serialize_msgpack_roundtrip_post() {
        // Example msgpack taken from
        // sentry-kafka-schemas/examples/uptime-configs/1/example-post.msgpack
        let example = vec![
            0x89, 0xaf, 0x73, 0x75, 0x62, 0x73, 0x63, 0x72, 0x69, 0x70, 0x74, 0x69, 0x6f, 0x6e,
            0x5f, 0x69, 0x64, 0xd9, 0x20, 0x64, 0x37, 0x36, 0x32, 0x39, 0x63, 0x36, 0x63, 0x38,
            0x32, 0x62, 0x65, 0x34, 0x66, 0x36, 0x37, 0x39, 0x65, 0x65, 0x37, 0x38, 0x61, 0x30,
            0x64, 0x39, 0x37, 0x37, 0x38, 0x35, 0x36, 0x64, 0x32, 0xa3, 0x75, 0x72, 0x6c, 0xb0,
            0x68, 0x74, 0x74, 0x70, 0x3a, 0x2f, 0x2f, 0x73, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x2e,
            0x69, 0x6f, 0xb0, 0x69, 0x6e, 0x74, 0x65, 0x72, 0x76, 0x61, 0x6c, 0x5f, 0x73, 0x65,
            0x63, 0x6f, 0x6e, 0x64, 0x73, 0xcd, 0x01, 0x2c, 0xaa, 0x74, 0x69, 0x6d, 0x65, 0x6f,
            0x75, 0x74, 0x5f, 0x6d, 0x73, 0xcd, 0x01, 0xf4, 0xae, 0x72, 0x65, 0x71, 0x75, 0x65,
            0x73, 0x74, 0x5f, 0x6d, 0x65, 0x74, 0x68, 0x6f, 0x64, 0xa4, 0x50, 0x4f, 0x53, 0x54,
            0xaf, 0x72, 0x65, 0x71, 0x75, 0x65, 0x73, 0x74, 0x5f, 0x68, 0x65, 0x61, 0x64, 0x65,
            0x72, 0x73, 0x93, 0x92, 0xad, 0x41, 0x75, 0x74, 0x68, 0x6f, 0x72, 0x69, 0x7a, 0x61,
            0x74, 0x69, 0x6f, 0x6e, 0xac, 0x42, 0x65, 0x61, 0x72, 0x65, 0x72, 0x20, 0x31, 0x32,
            0x33, 0x34, 0x35, 0x92, 0xac, 0x43, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x2d, 0x54,
            0x79, 0x70, 0x65, 0xb0, 0x61, 0x70, 0x70, 0x6c, 0x69, 0x63, 0x61, 0x74, 0x69, 0x6f,
            0x6e, 0x2f, 0x6a, 0x73, 0x6f, 0x6e, 0x92, 0xb2, 0x58, 0x2d, 0x4d, 0x79, 0x2d, 0x43,
            0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x48, 0x65, 0x61, 0x64, 0x65, 0x72, 0xad, 0x65,
            0x78, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x20, 0x76, 0x61, 0x6c, 0x75, 0x65, 0xac, 0x72,
            0x65, 0x71, 0x75, 0x65, 0x73, 0x74, 0x5f, 0x62, 0x6f, 0x64, 0x79, 0xb0, 0x7b, 0x22,
            0x6b, 0x65, 0x79, 0x22, 0x3a, 0x20, 0x22, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x22, 0x7d,
            0xae, 0x61, 0x63, 0x74, 0x69, 0x76, 0x65, 0x5f, 0x72, 0x65, 0x67, 0x69, 0x6f, 0x6e,
            0x73, 0x92, 0xa7, 0x75, 0x73, 0x2d, 0x77, 0x65, 0x73, 0x74, 0xa6, 0x65, 0x75, 0x72,
            0x6f, 0x70, 0x65, 0xb4, 0x72, 0x65, 0x67, 0x69, 0x6f, 0x6e, 0x5f, 0x73, 0x63, 0x68,
            0x65, 0x64, 0x75, 0x6c, 0x65, 0x5f, 0x6d, 0x6f, 0x64, 0x65, 0xab, 0x72, 0x6f, 0x75,
            0x6e, 0x64, 0x5f, 0x72, 0x6f, 0x62, 0x69, 0x6e,
        ];

        assert_eq!(
            rmp_serde::from_slice::<CheckConfig>(&example).unwrap(),
            CheckConfig {
                subscription_id: uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2"),
                timeout: TimeDelta::milliseconds(500),
                interval: CheckInterval::FiveMinutes,
                url: "http://sentry.io".to_string(),
                request_method: RequestMethod::Post,
                request_headers: vec![
                    ("Authorization".to_string(), "Bearer 12345".to_string()),
                    ("Content-Type".to_string(), "application/json".to_string()),
                    (
                        "X-My-Custom-Header".to_string(),
                        "example value".to_string()
                    ),
                ],
                request_body: "{\"key\": \"value\"}".to_string(),
                trace_sampling: false,
                active_regions: Some(vec!["us-west".to_string(), "europe".to_string()]),
                region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            }
        );
    }

    #[test]
    fn test_slots() {
        let all_slots = 0..MAX_CHECK_INTERVAL_SECS;

        let minute_config = CheckConfig::default();

        // Every minute slot includes the config
        assert_eq!(
            minute_config.slots(),
            all_slots.clone().step_by(60).collect::<Vec<usize>>()
        );

        let hour_config = CheckConfig {
            subscription_id: Uuid::from_u128(1),
            interval: CheckInterval::SixtyMinutes,
            ..Default::default()
        };

        // The second slot includes the config because of the subscription_id
        assert_eq!(hour_config.slots(), vec![1]);

        let five_minute_config = CheckConfig {
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        };

        // Every 5th slot includes the config
        assert_eq!(
            five_minute_config.slots(),
            all_slots.clone().step_by(60 * 5).collect::<Vec<usize>>()
        );

        let five_minute_config_offset = CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128 + 15),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        };

        // Every 5th slot includes the config starting at the 15th slot
        assert_eq!(
            five_minute_config_offset.slots(),
            all_slots.skip(15).step_by(60 * 5).collect::<Vec<usize>>()
        );
    }

    #[test]
    fn test_should_run() {
        let tick = Tick::from_time(Utc::now());

        // We should always run if regions aren't configured
        let no_region_config = CheckConfig::default();
        assert!(no_region_config.should_run(tick, &"us_west".to_string()));
        let no_active_regions_config = CheckConfig {
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        };
        assert!(no_active_regions_config.should_run(tick, &"us_west".to_string()));
        let no_schedule_mode_config = CheckConfig {
            active_regions: Some(vec!["us_west".to_string(), "us_east".to_string()]),
            ..Default::default()
        };
        assert!(no_schedule_mode_config.should_run(tick, &"us_west".to_string()));

        // When there's just one region we should also always run
        let one_region_config = CheckConfig {
            active_regions: Some(vec!["us_west".to_string()]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        };
        assert!(one_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(1, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(one_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(61, 0).unwrap()),
            &"us_west".to_string()
        ));

        // If configured region doesn't match at all we should never run
        assert!(!one_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(1, 0).unwrap()),
            &"other_region".to_string()
        ));
        assert!(!one_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(61, 0).unwrap()),
            &"other_region".to_string()
        ));

        let multi_region_config = CheckConfig {
            active_regions: Some(vec![
                "us_west".to_string(),
                "us_east".to_string(),
                "eu_west".to_string(),
            ]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        };
        // With 3 regions configured, the US west region should only run once every 3 minutes
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(1, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(61, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(121, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(181, 0).unwrap()),
            &"us_west".to_string()
        ));
        // Check other regions run
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(61, 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(121, 0).unwrap()),
            &"eu_west".to_string()
        ));

        // Try different spots in the minute
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(29, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(59, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(83, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(83, 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(119, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(119, 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(145, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(145, 0).unwrap()),
            &"eu_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(171, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(171, 0).unwrap()),
            &"eu_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(220, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(200, 0).unwrap()),
            &"us_west".to_string()
        ));

        // Try a more complicated start minute
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * (3 * 12345), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp((60 * (3 * 12345)) + 60, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp((60 * (3 * 12345)) + 120, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config.should_run(
            Tick::from_time(DateTime::from_timestamp((60 * (3 * 12345)) + 180, 0).unwrap()),
            &"us_west".to_string()
        ));

        // Try a larger interval to be sure things work
        let multi_region_config_large_interval = CheckConfig {
            interval: CheckInterval::TwentyMinutes,
            active_regions: Some(vec![
                "us_west".to_string(),
                "us_east".to_string(),
                "eu_west".to_string(),
            ]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        };
        // With 3 regions configured, the US west region should only run once every hour
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(0, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * 25, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * 25, 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * 50, 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * 50, 0).unwrap()),
            &"eu_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(60 * 71, 0).unwrap()),
            &"us_west".to_string()
        ));

        // Try more complicated times
        // ((60 * 60) * 13423) Is just selecting a random large hour, then we add minutes to it
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 15), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 39), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 39), 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 40), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 40), 0).unwrap()),
            &"eu_west".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 59), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 59), 0).unwrap()),
            &"eu_west".to_string()
        ));
        assert!(multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 60), 0).unwrap()),
            &"us_west".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 60), 0).unwrap()),
            &"us_east".to_string()
        ));
        assert!(!multi_region_config_large_interval.should_run(
            Tick::from_time(DateTime::from_timestamp(((60 * 60) * 13423) + (60 * 60), 0).unwrap()),
            &"eu_west".to_string()
        ));
    }
}
