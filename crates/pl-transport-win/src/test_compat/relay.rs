// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::relay::pair_dial_url;
    use spl_transport::RelayError;

    #[test]
    fn build_pair_dial_request_sets_pair_key_without_authorization() {
        let url = pair_dial_url("https://link.solstone.app").expect("shared pair-dial URL");
        assert_eq!(url, "wss://link.solstone.app/session/pair-dial");
        assert!(!url.contains("Authorization"));
    }

    #[test]
    fn pair_upgrade_401_maps_to_pair_window_closed() {
        assert_eq!(
            RelayError::PairWindowClosed.to_string(),
            "the pairing window is closed or expired — regenerate the link on your journal"
        );
    }

    #[test]
    fn pair_upgrade_402_maps_to_unpaid() {
        assert_eq!(RelayError::Unpaid.to_string(), "unpaid");
    }
}
