use serde::{Deserialize, Serialize};

/// Common requets methods.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RequestMethod {
    Get,
    Post,
    Head,
    Put,
    Delete,
    Patch,
    Options,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionScheduleMode {
    RoundRobin,
}

impl Default for RequestMethod {
    fn default() -> Self {
        Self::Get
    }
}

impl From<RequestMethod> for reqwest::Method {
    fn from(value: RequestMethod) -> Self {
        match value {
            RequestMethod::Get => reqwest::Method::GET,
            RequestMethod::Post => reqwest::Method::POST,
            RequestMethod::Head => reqwest::Method::HEAD,
            RequestMethod::Put => reqwest::Method::PUT,
            RequestMethod::Delete => reqwest::Method::DELETE,
            RequestMethod::Patch => reqwest::Method::PATCH,
            RequestMethod::Options => reqwest::Method::OPTIONS,
        }
    }
}
