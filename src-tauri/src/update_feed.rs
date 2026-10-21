// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::path::Path;
use std::sync::mpsc::Sender;

use velopack::bundle::Manifest;
use velopack::sources::UpdateSource;
use velopack::{Error, VelopackAsset, VelopackAssetFeed};

/// A custom Velopack update source for the static R2 feed. Unlike Velopack's
/// `HttpSource`, the manifest GET carries NO query string — no app version, no app
/// id, no staging id (Article 8). Only full packages are admitted. The source
/// verifies and persists their signatures before Velopack can extract a helper
/// or retire a previously staged package. Feed and asset GETs refuse redirects.
#[derive(Clone)]
pub struct R2FeedSource {
    base: String,
    #[cfg(test)]
    test_key: Option<&'static str>,
}

impl R2FeedSource {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            #[cfg(test)]
            test_key: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn fixture(base: String, key: &'static str) -> Self {
        Self {
            base,
            test_key: Some(key),
        }
    }
}

fn manifest_url(base: &str, channel: &str) -> String {
    format!("{}/releases.{}.json", base.trim_end_matches('/'), channel)
}

fn parse_feed(json: &str) -> Result<VelopackAssetFeed, Error> {
    serde_json::from_str(json).map_err(Error::from)
}

#[cfg(test)]
fn resolve_asset_url(base: &str, file_name: &str) -> Result<String, Error> {
    observer_update::admission::check_plain_filename(file_name)
        .map_err(|e| Error::Other(e.to_string()))?;
    observer_update::admission::sibling_url(base, file_name).map_err(Error::Other)
}

impl UpdateSource for R2FeedSource {
    fn open_admitted_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
    ) -> Result<Option<std::fs::File>, Error> {
        use observer_update::admission::{admit_cached_package, PackageVerifier};
        use pl_transport_win::release_signature::ProductionReleaseVerifier;
        fn admit(
            asset: &VelopackAsset,
            path: &Path,
            verifier: &impl PackageVerifier,
        ) -> Result<Option<std::fs::File>, Error> {
            let package = admit_cached_package(path, verifier)
                .map_err(|_| Error::Other("final package authentication failed".into()))?;
            if !asset.Type.eq_ignore_ascii_case("Full")
                || package.identity.version.to_string() != asset.Version
                || package.held.file.metadata()?.len() != asset.Size
            {
                return Err(Error::Other("final package identity mismatch".into()));
            }
            Ok(Some(package.held.file))
        }
        #[cfg(test)]
        if let Some(key) = self.test_key {
            return admit(
                asset,
                local_file,
                &pl_transport_win::release_signature::KeyedTestReleaseVerifier(key),
            );
        }
        admit(asset, local_file, &ProductionReleaseVerifier)
    }

    fn get_release_feed(
        &self,
        channel: &str,
        _app: &Manifest,
        _staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, Error> {
        let url = manifest_url(&self.base, channel);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .build()
            .into();
        let response = agent
            .get(&url)
            .call()
            .map_err(|e| Error::Other(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Other(format!(
                "feed GET returned HTTP status {}",
                response.status()
            )));
        }
        let json = response
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Other(e.to_string()))?;
        parse_feed(&json)
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress_sender: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        println!(
            "{}",
            observer_update::format_downloading_asset(&asset.FileName, &asset.Type)
        );
        #[cfg(test)]
        if let Some(key) = self.test_key {
            return pl_transport_win::release_signature::download_and_verify_release_entry_with_key(
                key, &self.base, &asset.Type, &asset.FileName, &asset.Version, local_file, progress_sender)
                .map(|_| ()).map_err(Error::Other);
        }
        pl_transport_win::release_signature::download_and_verify_release_entry(
            &self.base,
            &asset.Type,
            &asset.FileName,
            &asset.Version,
            local_file,
            progress_sender,
        )
        .map_err(Error::Other)?;

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
    fn resolve_asset_url_rejects_percent_encoded_filename() {
        assert!(resolve_asset_url(BASE, "Solstone%20App-1.2.3-full.nupkg").is_err());
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
