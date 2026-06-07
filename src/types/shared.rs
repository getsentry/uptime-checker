use serde::{Deserialize, Serialize};

/// Common requets methods.
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RequestMethod {
    #[default]
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

impl From<RequestMethod> for http::Method {
    fn from(value: RequestMethod) -> Self {
        match value {
            RequestMethod::Get => http::Method::GET,
            RequestMethod::Post => http::Method::POST,
            RequestMethod::Head => http::Method::HEAD,
            RequestMethod::Put => http::Method::PUT,
            RequestMethod::Delete => http::Method::DELETE,
            RequestMethod::Patch => http::Method::PATCH,
            RequestMethod::Options => http::Method::OPTIONS,
        }
    }
}
