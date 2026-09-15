// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::relay::*;
    #[test]
    fn rewrites_https_to_wss() {
        assert_eq!(
            dial_url("https://link.solstone.app", "inst").unwrap(),
            "wss://link.solstone.app/session/dial?instance=inst"
        );
    }

    #[test]
    fn rewrites_http_to_ws() {
        assert_eq!(
            dial_url("http://127.0.0.1:7657", "inst").unwrap(),
            "ws://127.0.0.1:7657/session/dial?instance=inst"
        );
    }

    #[test]
    fn trims_one_trailing_slash() {
        assert_eq!(
            dial_url("https://link.solstone.app/", "inst").unwrap(),
            "wss://link.solstone.app/session/dial?instance=inst"
        );
    }

    #[test]
    fn percent_encodes_query_value() {
        assert_eq!(
            dial_url("https://link.solstone.app", "inst one/two").unwrap(),
            "wss://link.solstone.app/session/dial?instance=inst%20one%2Ftwo"
        );
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert_eq!(
            dial_url("wss://link.solstone.app", "inst").unwrap_err(),
            DialUrlError::UnsupportedScheme
        );
    }

    #[test]
    fn builds_normal_relay_url() {
        assert_eq!(
            dial_url("https://link.solstone.app", "inst-123").unwrap(),
            "wss://link.solstone.app/session/dial?instance=inst-123"
        );
    }
}
