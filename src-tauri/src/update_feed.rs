// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::path::Path;
use std::sync::mpsc::Sender;

use url::Url;
use velopack::bundle::Manifest;
use velopack::download;
use velopack::sources::UpdateSource;
use velopack::{Error, VelopackAsset, VelopackAssetFeed};

/// A custom Velopack update source for the static R2 feed. Unlike Velopack's
/// `HttpSource`, the manifest GET carries NO query string — no app version, no app
/// id, no staging id (Article 8). The manager still owns version comparison, delta
/// selection, checksum, staging, and relaunch; this source only fetches the feed
/// and downloads assets (after a first-party same-origin check).
#[derive(Clone)]
pub struct R2FeedSource {
    base: String,
}

impl R2FeedSource {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

fn manifest_url(base: &str, channel: &str) -> String {
    format!("{}/releases.{}.json", base.trim_end_matches('/'), channel)
}

fn parse_feed(json: &str) -> Result<VelopackAssetFeed, Error> {
    serde_json::from_str(json).map_err(Error::from)
}

fn resolve_asset_url(base: &str, file_name: &str) -> Result<String, Error> {
    let base_url = Url::parse(&format!("{}/", base.trim_end_matches('/')))?;
    let resolved = base_url.join(file_name)?;

    let same_origin = resolved.scheme() == base_url.scheme()
        && resolved.host_str().is_some()
        && resolved.host_str() == base_url.host_str()
        && resolved.port_or_known_default() == base_url.port_or_known_default()
        && resolved.path().starts_with(base_url.path());

    if !same_origin {
        return Err(Error::Other(format!(
            "asset URL is outside update feed origin: {resolved}"
        )));
    }

    Ok(resolved.to_string())
}

impl UpdateSource for R2FeedSource {
    fn get_release_feed(
        &self,
        channel: &str,
        _app: &Manifest,
        _staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, Error> {
        let url = manifest_url(&self.base, channel);
        let json = download::download_url_as_string(&url)?;
        parse_feed(&json)
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress_sender: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        let url = resolve_asset_url(&self.base, &asset.FileName)?;
        download::download_url_to_file(&url, local_file, move |p| {
            if let Some(s) = &progress_sender {
                let _ = s.send(p);
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    const BASE: &str = "https://updates.solstone.app/solstone-windows";

    #[test]
    fn manifest_url_for_win_has_no_query() {
        let raw = manifest_url(BASE, "win");
        let parsed = Url::parse(&raw).unwrap();

        assert_eq!(parsed.path(), "/solstone-windows/releases.win.json");
        assert!(parsed.query().is_none());
        assert!(!raw.contains('?'));
        assert!(!raw.contains("localVersion"));
        assert!(!raw.contains("stagingId"));
        assert!(!raw.contains("id="));
    }

    #[test]
    fn manifest_url_honors_channel_arg() {
        assert_eq!(
            manifest_url(BASE, "beta"),
            "https://updates.solstone.app/solstone-windows/releases.beta.json"
        );
    }

    #[test]
    fn parse_feed_accepts_no_update_feed() {
        let feed = parse_feed(r#"{"Assets":[]}"#).unwrap();

        assert_eq!(feed.Assets.len(), 0);
    }

    #[test]
    fn parse_feed_accepts_available_full_asset() {
        let feed = parse_feed(
            r#"{"Assets":[{"Version":"1.2.3","Type":"Full","FileName":"Solstone-1.2.3-full.nupkg"}]}"#,
        )
        .unwrap();

        assert_eq!(feed.Assets.len(), 1);
        assert_eq!(feed.Assets[0].Type, "Full");
        assert_eq!(feed.Assets[0].FileName, "Solstone-1.2.3-full.nupkg");
    }

    #[test]
    fn parse_feed_preserves_delta_assets() {
        let feed = parse_feed(
            r#"{"Assets":[{"Version":"1.2.3","Type":"Full","FileName":"Solstone-1.2.3-full.nupkg"},{"Version":"1.2.3","Type":"Delta","FileName":"Solstone-1.2.3-1.2.2-delta.nupkg"}]}"#,
        )
        .unwrap();

        assert_eq!(feed.Assets.len(), 2);
        assert!(feed.Assets.iter().any(|asset| asset.Type == "Delta"));
    }

    #[test]
    fn parse_feed_rejects_malformed_json() {
        assert!(parse_feed("{ not json").is_err());
    }

    #[test]
    fn resolve_asset_url_accepts_full_nupkg() {
        let raw = resolve_asset_url(BASE, "Solstone-1.2.3-full.nupkg").unwrap();
        let parsed = Url::parse(&raw).unwrap();

        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("updates.solstone.app"));
        assert_eq!(parsed.path(), "/solstone-windows/Solstone-1.2.3-full.nupkg");
    }

    #[test]
    fn resolve_asset_url_accepts_delta_nupkg() {
        let raw = resolve_asset_url(BASE, "Solstone-1.2.3-1.2.2-delta.nupkg").unwrap();
        let parsed = Url::parse(&raw).unwrap();

        assert_eq!(
            parsed.path(),
            "/solstone-windows/Solstone-1.2.3-1.2.2-delta.nupkg"
        );
    }

    #[test]
    fn resolve_asset_url_accepts_percent_encoded_filename() {
        let raw = resolve_asset_url(BASE, "Solstone%20App-1.2.3-full.nupkg").unwrap();
        let parsed = Url::parse(&raw).unwrap();

        assert_eq!(
            parsed.path(),
            "/solstone-windows/Solstone%20App-1.2.3-full.nupkg"
        );
    }

    #[test]
    fn resolve_asset_url_rejects_absolute_cross_origin() {
        assert!(resolve_asset_url(BASE, "https://evil.example/x.nupkg").is_err());
    }

    #[test]
    fn resolve_asset_url_rejects_protocol_relative_cross_origin() {
        assert!(resolve_asset_url(BASE, "//evil.example/x.nupkg").is_err());
    }

    #[test]
    fn resolve_asset_url_rejects_path_escape() {
        assert!(resolve_asset_url(BASE, "/other/x.nupkg").is_err());
        assert!(resolve_asset_url(BASE, "../../../etc/passwd").is_err());
    }
}
