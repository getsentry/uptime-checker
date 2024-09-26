use futures_util::future::FutureExt;
use hyper_util::client::legacy::connect::dns::{GaiResolver, Name as HyperName};
use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::borrow::BorrowMut;
use std::error::Error;
use std::str::FromStr;
use std::sync::LazyLock;
use tower_service::Service;

pub static PRIVATE_RANGES: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    let addresses = vec![
        // https://en.wikipedia.org/wiki/Reserved_IP_addresses#IPv4
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.0.0.0/29",
        "192.0.2.0/24",
        "192.88.99.0/24",
        "192.168.0.0/16",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "224.0.0.0/4",
        "240.0.0.0/4",
        "255.255.255.255/32",
        // https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses
        // Subnets match the IPv4 subnets above
        "::ffff:0:0/104",
        "::ffff:a00:0/104",
        "::ffff:6440:0/106",
        "::ffff:7f00:0/104",
        "::ffff:a9fe:0/112",
        "::ffff:ac10:0/108",
        "::ffff:c000:0/125",
        "::ffff:c000:200/120",
        "::ffff:c058:6300/120",
        "::ffff:c0a8:0/112",
        "::ffff:c612:0/111",
        "::ffff:c633:6400/120",
        "::ffff:e000:0/100",
        "::ffff:f000:0/100",
        "::ffff:ffff:ffff/128",
        // https://en.wikipedia.org/wiki/Reserved_IP_addresses#IPv6
        "::1/128",
        "::ffff:0:0:0/96",
        "64:ff9b::/96",
        "64:ff9b:1::/48",
        "100::/64",
        "2001:0000::/32",
        "2001:20::/28",
        "2001:db8::/32",
        "2002::/16",
        "fc00::/7",
        "fe80::/10",
        "ff00::/8",
    ];

    addresses.iter().map(|addr| addr.parse().unwrap()).collect()
});

/// RestrictedResolver handles DNS resolution of checks while restricting checks from communicating
/// to interal network addresses.
pub struct RestrictedResolver {
    /// The internal hyper resolver.
    resolver: GaiResolver,

    /// The set of restricted address ranges to disallow resoluon to. This can be used to specify
    /// for example `127.0.0.0/8` to disallow resolution to the loopback address.
    restricted_ranges: &'static Vec<IpNet>,
}

impl RestrictedResolver {
    pub fn new(restricted_ranges: &'static Vec<IpNet>) -> Self {
        Self {
            resolver: GaiResolver::new(),
            restricted_ranges,
        }
    }
}

type BoxError = Box<dyn Error + Send + Sync>;

impl Resolve for RestrictedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = &mut self.resolver.clone();
        let hyper_name = HyperName::from_str(name.as_str()).unwrap();

        let ranges = self.restricted_ranges.clone();

        Box::pin(resolver.call(hyper_name).map(move |result| match result {
            Err(err) => {
                let boxed_err: BoxError = Box::new(err);
                Err(boxed_err)
            }
            Ok(mut addrs) => {
                for addr in addrs.borrow_mut() {
                    let ip = &addr.ip();
                    if ranges.iter().any(|ip_net| ip_net.contains(ip)) {
                        tracing::info!(
                            host = name.as_str(),
                            %ip,
                            "dns_resolution.restricted_resolution"
                        );
                        let boxed_err: BoxError =
                            Box::from("Host resolved to a restricted address");
                        return Err(boxed_err);
                    }
                }

                let reqwest_addrs: Addrs = Box::new(addrs);
                Ok(reqwest_addrs)
            }
        }))
    }
}
