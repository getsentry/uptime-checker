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
        "192.0.0.0/24",
        "192.0.2.0/24",
        "192.88.99.0/24",
        "192.168.0.0/16",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "203.0.113.0/24",
        "224.0.0.0/4",
        "240.0.0.0/4",
        "255.255.255.255/32",
        // https://en.wikipedia.org/wiki/Reserved_IP_addresses#IPv6
        // plus the deprecated IPv4-compatible (::/96), IPv4-translated
        // (::ffff:0:0:0/96) and site-local (fec0::/10) ranges
        "::/96",
        "::ffff:0:0/96",
        "::ffff:0:0:0/96",
        "64:ff9b::/96",
        "64:ff9b:1::/48",
        "100::/64",
        "2001:0000::/32",
        "2001:20::/28",
        "2001:db8::/32",
        "2002::/16",
        "3fff::/20",
        "5f00::/16",
        "fc00::/7",
        "fe80::/10",
        "fec0::/10",
        "ff00::/8",
    ];

    addresses
        .iter()
        .map(|addr| addr.parse().expect("Addresses parse by construction"))
        .collect()
});

pub fn is_external_ip(ip: IpAddr) -> bool {
    if PRIVATE_RANGES.iter().any(|network| network.contains(&ip)) {
        tracing::debug!("Blocked attempt to connect to reserved IP address: {}", ip);
        false
    } else {
        true
    }
}
