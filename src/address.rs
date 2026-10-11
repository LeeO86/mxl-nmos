//! Announced address, network interfaces and hostname of the Node.

use std::net::Ipv4Addr;

use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use serde_json::{json, Value};

/// The first non-loopback, non-link-local IPv4 address of this host.
pub fn first_routable_ipv4() -> Option<Ipv4Addr> {
    let interfaces = NetworkInterface::show().ok()?;
    interfaces
        .iter()
        .filter(|iface| !is_loopback(iface))
        .flat_map(|iface| iface.addr.iter())
        .find_map(|addr| match addr {
            Addr::V4(v4)
                if !v4.ip.is_loopback() && !v4.ip.is_unspecified() && !v4.ip.is_link_local() =>
            {
                Some(v4.ip)
            }
            _ => None,
        })
}

/// IS-04 `interfaces`: every non-loopback interface with a MAC address (loopback
/// only when there is nothing else). `attached_network_device` is omitted because
/// there is no LLDP data; `null` is not valid there.
pub(crate) fn interfaces_json() -> Value {
    let Ok(all) = NetworkInterface::show() else {
        return json!([]);
    };
    let mut rows = interface_rows(all.iter().filter(|iface| !is_loopback(iface)));
    if rows.is_empty() {
        rows = interface_rows(all.iter().filter(|iface| is_loopback(iface)));
    }
    Value::Array(rows)
}

fn interface_rows<'a>(interfaces: impl Iterator<Item = &'a NetworkInterface>) -> Vec<Value> {
    let mut names: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for iface in interfaces {
        if names.contains(&iface.name.as_str()) {
            continue;
        }
        if let Some(port_id) = nmos_mac(iface.mac_addr.as_deref()) {
            names.push(&iface.name);
            rows.push(json!({
                "name": iface.name,
                "chassis_id": Value::Null,
                "port_id": port_id
            }));
        }
    }
    rows
}

fn is_loopback(iface: &NetworkInterface) -> bool {
    iface.name.starts_with("lo")
        || iface.addr.iter().any(|addr| match addr {
            Addr::V4(v4) => v4.ip.is_loopback(),
            Addr::V6(v6) => v6.ip.is_loopback(),
        })
}

/// IS-04 `port_id`: six lowercase hex octets separated by hyphens.
pub(crate) fn nmos_mac(mac: Option<&str>) -> Option<String> {
    let mac = mac?;
    let bytes = mac
        .split([':', '-', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let [a, b, c, d, e, f] = bytes.try_into().ok()?;
    Some(format!("{a:02x}-{b:02x}-{c:02x}-{d:02x}-{e:02x}-{f:02x}"))
}

/// The system hostname, as nmos-cpp announces it in the Node `hostname` field.
pub(crate) fn hostname_string() -> Option<String> {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::nmos_mac;

    #[test]
    fn mac_uses_is04_hyphen_form() {
        assert_eq!(
            nmos_mac(Some("AA:BB:CC:DD:EE:FF")).as_deref(),
            Some("aa-bb-cc-dd-ee-ff")
        );
        assert_eq!(
            nmos_mac(Some("aa-bb-cc-dd-ee-ff")).as_deref(),
            Some("aa-bb-cc-dd-ee-ff")
        );
        assert!(nmos_mac(None).is_none());
        assert!(nmos_mac(Some("enp0s5")).is_none());
    }
}
