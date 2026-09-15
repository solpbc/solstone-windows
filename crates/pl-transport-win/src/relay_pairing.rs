// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Relay-form pairing ceremony adapter.
//!
//! Production relay pairing delegates directly to `spl_transport`'s shared implementation.

use spl_core::pairlink::RelayPairLink;

use crate::credential::Credential;
use crate::observe::ObserverHandle;
use crate::TransportError;

pub async fn pair_over_relay(
    link: &RelayPairLink,
    device_label: &str,
) -> Result<Credential, TransportError> {
    pair_over_relay_observed(link, device_label, None).await
}

/// [`pair_over_relay`] with an operation-scoped observation seam attached.
pub async fn pair_over_relay_observed(
    link: &RelayPairLink,
    device_label: &str,
    observer: ObserverHandle,
) -> Result<Credential, TransportError> {
    let shared_observer = spl_transport::observe::OperationObserver::new_unshared();
    let empty_map = serde_json::Map::new();
    let result = spl_transport::relay_pairing::pair_over_relay_observed(
        link,
        device_label,
        &empty_map,
        Some(&shared_observer),
    )
    .await;

    crate::pairing::handle_shared_pairing_result(result, &shared_observer, observer)
}
