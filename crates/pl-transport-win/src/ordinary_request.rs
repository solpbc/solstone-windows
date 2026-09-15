// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Authority for every ordinary post-pairing HTTP request.
//!
//! The bridge is deliberately absent: it owns a persistent carrier and is not
//! an ordinary request route.

use observer_pl::paths;
use spl_transport::request::ReplayPolicy;

use crate::client::MAX_POST_CONNECT_RESPONSE_BYTES;

/// The complete, closed set of ordinary Windows request routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryRequest {
    ClientsSelfGet,
    ClientsSelfPut,
    RelayAccessGet,
    IngestPost,
    IngestManifestGet,
    IngestManifestDayGet,
    IngestSegmentsDayGet,
    SystemStatusGet,
}

/// One route's transport authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OrdinaryRequestSpec {
    pub(crate) method: &'static str,
    /// Static route or a `<day>` pattern for dynamic civil-day routes.
    pub(crate) path_pattern: &'static str,
    pub(crate) replay: ReplayPolicy,
    pub(crate) response_cap: usize,
}

impl OrdinaryRequest {
    pub(crate) const ALL: [Self; 8] = [
        Self::ClientsSelfGet,
        Self::ClientsSelfPut,
        Self::RelayAccessGet,
        Self::IngestPost,
        Self::IngestManifestGet,
        Self::IngestManifestDayGet,
        Self::IngestSegmentsDayGet,
        Self::SystemStatusGet,
    ];

    pub(crate) const fn spec(self) -> OrdinaryRequestSpec {
        match self {
            Self::ClientsSelfGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: "/app/network/api/clients/self",
                replay: ReplayPolicy::ReplaySafe,
                response_cap: MAX_POST_CONNECT_RESPONSE_BYTES,
            },
            Self::ClientsSelfPut => OrdinaryRequestSpec {
                method: "PUT",
                path_pattern: "/app/network/api/clients/self",
                replay: ReplayPolicy::ForbidAfterWrite,
                response_cap: MAX_POST_CONNECT_RESPONSE_BYTES,
            },
            Self::RelayAccessGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: "/app/network/api/relay/access",
                replay: ReplayPolicy::ReplaySafe,
                response_cap: MAX_POST_CONNECT_RESPONSE_BYTES,
            },
            Self::IngestPost => OrdinaryRequestSpec {
                method: "POST",
                path_pattern: paths::INGEST,
                replay: ReplayPolicy::ReplaySafe,
                response_cap: observer_pl::mux::MAX_ASSEMBLED_BYTES,
            },
            Self::IngestManifestGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: paths::INGEST_MANIFEST,
                replay: ReplayPolicy::ReplaySafe,
                response_cap: observer_pl::mux::MAX_ASSEMBLED_BYTES,
            },
            Self::IngestManifestDayGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: "/app/devices/ingest/manifest/<day>",
                replay: ReplayPolicy::ReplaySafe,
                response_cap: observer_pl::mux::MAX_ASSEMBLED_BYTES,
            },
            Self::IngestSegmentsDayGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: "/app/devices/ingest/segments/<day>",
                replay: ReplayPolicy::ReplaySafe,
                response_cap: observer_pl::mux::MAX_ASSEMBLED_BYTES,
            },
            Self::SystemStatusGet => OrdinaryRequestSpec {
                method: "GET",
                path_pattern: "/api/system/status",
                replay: ReplayPolicy::ReplaySafe,
                response_cap: MAX_POST_CONNECT_RESPONSE_BYTES,
            },
        }
    }

    pub(crate) fn path(self, day: Option<&str>) -> String {
        match self {
            Self::IngestManifestDayGet => {
                format!(
                    "{}/{}",
                    paths::INGEST_MANIFEST,
                    day.expect("day route requires a day")
                )
            }
            Self::IngestSegmentsDayGet => {
                format!(
                    "{}/{}",
                    paths::INGEST_SEGMENTS,
                    day.expect("day route requires a day")
                )
            }
            _ => self.spec().path_pattern.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_the_exact_eight_route_inventory() {
        let expected = [
            (
                OrdinaryRequest::ClientsSelfGet,
                "GET",
                "/app/network/api/clients/self",
                ReplayPolicy::ReplaySafe,
                MAX_POST_CONNECT_RESPONSE_BYTES,
            ),
            (
                OrdinaryRequest::ClientsSelfPut,
                "PUT",
                "/app/network/api/clients/self",
                ReplayPolicy::ForbidAfterWrite,
                MAX_POST_CONNECT_RESPONSE_BYTES,
            ),
            (
                OrdinaryRequest::RelayAccessGet,
                "GET",
                "/app/network/api/relay/access",
                ReplayPolicy::ReplaySafe,
                MAX_POST_CONNECT_RESPONSE_BYTES,
            ),
            (
                OrdinaryRequest::IngestPost,
                "POST",
                paths::INGEST,
                ReplayPolicy::ReplaySafe,
                observer_pl::mux::MAX_ASSEMBLED_BYTES,
            ),
            (
                OrdinaryRequest::IngestManifestGet,
                "GET",
                paths::INGEST_MANIFEST,
                ReplayPolicy::ReplaySafe,
                observer_pl::mux::MAX_ASSEMBLED_BYTES,
            ),
            (
                OrdinaryRequest::IngestManifestDayGet,
                "GET",
                "/app/devices/ingest/manifest/<day>",
                ReplayPolicy::ReplaySafe,
                observer_pl::mux::MAX_ASSEMBLED_BYTES,
            ),
            (
                OrdinaryRequest::IngestSegmentsDayGet,
                "GET",
                "/app/devices/ingest/segments/<day>",
                ReplayPolicy::ReplaySafe,
                observer_pl::mux::MAX_ASSEMBLED_BYTES,
            ),
            (
                OrdinaryRequest::SystemStatusGet,
                "GET",
                "/api/system/status",
                ReplayPolicy::ReplaySafe,
                MAX_POST_CONNECT_RESPONSE_BYTES,
            ),
        ];

        assert_eq!(OrdinaryRequest::ALL, expected.map(|(route, ..)| route));
        for (route, method, path_pattern, replay, response_cap) in expected {
            assert_eq!(route.spec().method, method);
            assert_eq!(route.spec().path_pattern, path_pattern);
            assert_eq!(route.spec().replay, replay);
            assert_eq!(route.spec().response_cap, response_cap);
        }
        assert_eq!(
            OrdinaryRequest::IngestManifestDayGet.path(Some("20260914")),
            "/app/devices/ingest/manifest/20260914"
        );
    }
}
