// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::HashSet;
use std::net::IpAddr;

use crate::credential::EndpointAddr;

/// Parse the local endpoints response body.
///
/// Returns `None` if the payload does not strictly match the required schema:
/// a JSON object with integer `v >= 2`, and an `endpoints` array.
/// Invalid individual endpoints within the array are skipped.
pub(crate) fn parse_local_endpoints(bytes: &[u8]) -> Option<Vec<EndpointAddr>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object()?;

    let v = obj.get("v")?.as_i64()?;
    if v < 2 {
        return None;
    }

    let endpoints_arr = obj.get("endpoints")?.as_array()?;

    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for entry in endpoints_arr {
        let Some(entry_obj) = entry.as_object() else {
            continue;
        };
        let Some(ip_str) = entry_obj.get("ip").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(port_u64) = entry_obj.get("port").and_then(|v| v.as_u64()) else {
            continue;
        };
        let Ok(port) = u16::try_from(port_u64) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let Ok(parsed_ip) = ip_str.parse::<IpAddr>() else {
            continue;
        };
        if seen.insert((parsed_ip, port)) {
            out.push(EndpointAddr {
                host: parsed_ip.to_string(),
                port,
            });
        }
    }

    Some(out)
}

/// Merge stored endpoints with freshly listed endpoints.
///
/// If `relay` is true, exactly `listed` is returned.
/// If direct (`relay` is false), `listed` is followed by at most two stored entries
/// not already present in `listed` (evaluated by parsed `(IpAddr, port)`), in stored order,
/// preserving their stored host strings.
pub(crate) fn merge_dial_endpoints(
    stored: &[EndpointAddr],
    listed: &[EndpointAddr],
    relay: bool,
) -> Vec<EndpointAddr> {
    if relay {
        return listed.to_vec();
    }

    let mut result = listed.to_vec();
    let mut listed_addrs: HashSet<(IpAddr, u16)> = HashSet::new();
    for ep in listed {
        if let Ok(ip) = ep.host.parse::<IpAddr>() {
            listed_addrs.insert((ip, ep.port));
        }
    }

    let mut added_stored = 0;
    for ep in stored {
        if added_stored == 2 {
            break;
        }
        match ep.host.parse::<IpAddr>() {
            Ok(ip) => {
                if !listed_addrs.contains(&(ip, ep.port)) {
                    result.push(ep.clone());
                    added_stored += 1;
                }
            }
            Err(_) => {
                result.push(ep.clone());
                added_stored += 1;
            }
        }
    }

    result
}

/// Check if two endpoint lists match pairwise.
///
/// Same length, pairwise address equality. Parsed addresses compare as `(IpAddr, port)`.
/// If both fail to parse as `IpAddr`, equal only when host string and port match.
/// One parsed and one unparsed: not equal.
pub(crate) fn endpoints_match(a: &[EndpointAddr], b: &[EndpointAddr]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (ep_a, ep_b) in a.iter().zip(b.iter()) {
        if ep_a.port != ep_b.port {
            return false;
        }
        match (ep_a.host.parse::<IpAddr>(), ep_b.host.parse::<IpAddr>()) {
            (Ok(ip_a), Ok(ip_b)) => {
                if ip_a != ip_b {
                    return false;
                }
            }
            (Err(_), Err(_)) => {
                if ep_a.host != ep_b.host {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Check whether the stored endpoints represent only loopback addresses on the same computer.
///
/// Returns true if non-empty and every host parses as `IpAddr` with `is_loopback()`.
/// Empty is false. An unparseable host is false.
pub(crate) fn is_same_computer(stored: &[EndpointAddr]) -> bool {
    if stored.is_empty() {
        return false;
    }
    stored.iter().all(|ep| {
        ep.host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_local_endpoints_schema_validation() {
        // Missing v
        assert!(parse_local_endpoints(br#"{"endpoints":[]}"#).is_none());
        // v not an integer (float)
        assert!(parse_local_endpoints(br#"{"v":2.0,"endpoints":[]}"#).is_none());
        // v as string
        assert!(parse_local_endpoints(br#"{"v":"2","endpoints":[]}"#).is_none());
        // v < 2
        assert!(parse_local_endpoints(br#"{"v":1,"endpoints":[]}"#).is_none());
        // v bool
        assert!(parse_local_endpoints(br#"{"v":true,"endpoints":[]}"#).is_none());
        // v null
        assert!(parse_local_endpoints(br#"{"v":null,"endpoints":[]}"#).is_none());
        // missing endpoints
        assert!(parse_local_endpoints(br#"{"v":2}"#).is_none());
        // endpoints not an array
        assert!(parse_local_endpoints(br#"{"v":2,"endpoints":{}}"#).is_none());
        // Not a JSON object
        assert!(parse_local_endpoints(br#"[]"#).is_none());

        // v >= 2 accepted, unknown fields ignored
        let valid = br#"{
            "v": 2,
            "ttl_s": 60,
            "generated_at": 1234567,
            "scope": "local",
            "unknown_field": "test",
            "endpoints": [
                {"ip": "192.168.1.10", "port": 7657},
                {"ip": "[::1]", "port": 7657},
                {"ip": "::1", "port": 7657},
                {"ip": "0:0:0:0:0:0:0:1", "port": 7657},
                {"ip": "192.168.1.10", "port": 7657},
                {"ip": "bad_ip", "port": 7657},
                {"ip": "192.168.1.11", "port": 0},
                {"ip": "192.168.1.12", "port": 70000}
            ]
        }"#;

        let parsed = parse_local_endpoints(valid).expect("should parse");
        assert_eq!(
            parsed,
            vec![
                EndpointAddr {
                    host: "192.168.1.10".into(),
                    port: 7657,
                },
                EndpointAddr {
                    host: "::1".into(),
                    port: 7657,
                },
            ]
        );

        // v = 3 accepted
        assert!(parse_local_endpoints(br#"{"v":3,"endpoints":[]}"#).is_some());
    }

    #[test]
    fn merge_dial_endpoints_rules() {
        let o1 = EndpointAddr {
            host: "192.168.1.1".into(),
            port: 7657,
        };
        let o2 = EndpointAddr {
            host: "192.168.1.2".into(),
            port: 7657,
        };
        let o3 = EndpointAddr {
            host: "192.168.1.3".into(),
            port: 7657,
        };
        let n1 = EndpointAddr {
            host: "10.0.0.1".into(),
            port: 7657,
        };
        let n2 = EndpointAddr {
            host: "10.0.0.2".into(),
            port: 7657,
        };
        let n3 = EndpointAddr {
            host: "172.16.0.1".into(),
            port: 7657,
        };
        let n4 = EndpointAddr {
            host: "172.16.0.2".into(),
            port: 7657,
        };

        // Direct [O1] + [N1, N2] -> [N1, N2, O1]
        let m1 = merge_dial_endpoints(std::slice::from_ref(&o1), &[n1.clone(), n2.clone()], false);
        assert_eq!(m1, vec![n1.clone(), n2.clone(), o1.clone()]);
        // Same list again stays [N1, N2, O1]
        let m1_again = merge_dial_endpoints(&m1, &[n1.clone(), n2.clone()], false);
        assert_eq!(m1_again, vec![n1.clone(), n2.clone(), o1.clone()]);

        // Direct [O1, O2, O3] + [N1] -> [N1, O1, O2] (at most two stored)
        let m2 = merge_dial_endpoints(
            &[o1.clone(), o2.clone(), o3.clone()],
            std::slice::from_ref(&n1),
            false,
        );
        assert_eq!(m2, vec![n1.clone(), o1.clone(), o2.clone()]);

        // Direct [N1, N2, O1] + [N3, N4] -> [N3, N4, N1, N2]
        let m3 = merge_dial_endpoints(
            &[n1.clone(), n2.clone(), o1.clone()],
            &[n3.clone(), n4.clone()],
            false,
        );
        assert_eq!(m3, vec![n3.clone(), n4.clone(), n1.clone(), n2.clone()]);

        // Relay, any stored, [N1, N2] -> [N1, N2]
        let m_relay = merge_dial_endpoints(
            &[o1.clone(), o2.clone(), o3.clone()],
            &[n1.clone(), n2.clone()],
            true,
        );
        assert_eq!(m_relay, vec![n1.clone(), n2.clone()]);

        // Listed [] relay -> empty
        let m_empty_relay = merge_dial_endpoints(&[o1.clone(), o2.clone()], &[], true);
        assert!(m_empty_relay.is_empty());

        // Listed [] direct with 1 or 2 stored -> merged equals stored
        let m_empty_direct1 = merge_dial_endpoints(std::slice::from_ref(&o1), &[], false);
        assert_eq!(m_empty_direct1, vec![o1.clone()]);
        let m_empty_direct2 = merge_dial_endpoints(&[o1.clone(), o2.clone()], &[], false);
        assert_eq!(m_empty_direct2, vec![o1.clone(), o2.clone()]);
    }

    #[test]
    fn endpoints_match_and_ipv6_variants() {
        let ep_v6_short = EndpointAddr {
            host: "::1".into(),
            port: 7657,
        };
        let ep_v6_full = EndpointAddr {
            host: "0:0:0:0:0:0:0:1".into(),
            port: 7657,
        };
        assert!(endpoints_match(&[ep_v6_short], &[ep_v6_full]));

        let unparseable1 = EndpointAddr {
            host: "my-host.local".into(),
            port: 80,
        };
        let unparseable2 = EndpointAddr {
            host: "my-host.local".into(),
            port: 80,
        };
        assert!(endpoints_match(
            std::slice::from_ref(&unparseable1),
            &[unparseable2]
        ));

        let unparseable_diff = EndpointAddr {
            host: "other.local".into(),
            port: 80,
        };
        assert!(!endpoints_match(&[unparseable1], &[unparseable_diff]));
    }

    #[test]
    fn is_same_computer_checks() {
        assert!(!is_same_computer(&[]));

        let loopback1 = EndpointAddr {
            host: "127.0.0.1".into(),
            port: 9,
        };
        let loopback2 = EndpointAddr {
            host: "127.0.0.2".into(),
            port: 7657,
        };
        let loopback_v6 = EndpointAddr {
            host: "::1".into(),
            port: 7657,
        };
        assert!(is_same_computer(&[loopback1.clone(), loopback2.clone()]));
        assert!(is_same_computer(&[loopback_v6]));

        let non_loopback = EndpointAddr {
            host: "192.168.1.1".into(),
            port: 7657,
        };
        assert!(!is_same_computer(&[loopback1.clone(), non_loopback]));

        let unparseable = EndpointAddr {
            host: "localhost".into(),
            port: 7657,
        };
        assert!(!is_same_computer(&[unparseable]));
    }
}
