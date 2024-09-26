use ipnet::IpNet;
use std::net::IpAddr;
use std::sync::LazyLock;

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

pub fn is_external_ip(ip: IpAddr) -> bool {
    if PRIVATE_RANGES.iter().any(|network| network.contains(&ip)) {
        tracing::debug!("Blocked attempt to connect to reserved IP address: {}", ip);
        false
    } else {
        true
    }
}
