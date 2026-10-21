// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Package signature verification, production download, and sidecar validation.

use base64::Engine as _;
#[cfg(test)]
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::mpsc::Sender;

use minisign::{PublicKeyBox, SignatureBox};

const PRODUCTION_PUBLIC_KEY: &str = include_str!("../../../packaging/keys/solstone-release.pub");

fn parse_signature(text: &str) -> Result<SignatureBox, String> {
    let mut lines = text.lines();
    let global = lines.nth(3).ok_or("missing comment signature")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(global.trim())
        .map_err(|e| format!("invalid comment signature: {e}"))?;
    if bytes.len() != 64 || lines.next().is_some() {
        return Err("comment signature must contain exactly 64 bytes".to_string());
    }
    let signature =
        SignatureBox::from_string(text).map_err(|e| format!("invalid signature format: {e}"))?;
    if !signature.is_prehashed() {
        return Err("legacy Ed signature rejected: signature must be prehashed ED".to_string());
    }
    signature
        .trusted_comment()
        .map_err(|e| format!("invalid trusted comment: {e}"))?;
    Ok(signature)
}

/// Internal verification helper supporting key injection for unit tests.
fn verify_signature_with_key<R: Read + Seek>(
    key_text: &str,
    signature_text: &str,
    package: R,
) -> Result<(), String> {
    let pk = PublicKeyBox::from_string(key_text)
        .map_err(|e| format!("invalid public key box: {e}"))?
        .into_public_key()
        .map_err(|e| format!("invalid public key: {e}"))?;

    let sig_box = parse_signature(signature_text)?;

    minisign::verify(&pk, &sig_box, package, true, false, false)
        .map_err(|e| format!("signature verification failed: {e}"))
}

/// Verifies a package signature using the embedded production release public key.
pub fn verify_release_signature<R: Read + Seek>(
    signature_text: &str,
    package: R,
) -> Result<(), String> {
    verify_signature_with_key(PRODUCTION_PUBLIC_KEY, signature_text, package)
}

/// Verifies a package signature with an explicit public key (test-only).
#[cfg(any(test, feature = "update-test-fixtures"))]
pub fn verify_release_signature_with_key<R: Read + Seek>(
    key_text: &str,
    signature_text: &str,
    package: R,
) -> Result<(), String> {
    verify_signature_with_key(key_text, signature_text, package)
}

/// Production package verifier implementing observer-update's PackageVerifier trait.
pub struct ProductionReleaseVerifier;

impl observer_update::admission::PackageVerifier for ProductionReleaseVerifier {
    fn verify(
        &self,
        package: &mut (dyn observer_update::admission::ReadSeek + '_),
        sidecar_text: &str,
    ) -> Result<(), String> {
        verify_release_signature(sidecar_text, package)
    }
}

/// Test package verifier wrapping an arbitrary public key.
#[cfg(any(test, feature = "update-test-fixtures"))]
pub struct KeyedTestReleaseVerifier<'a>(pub &'a str);

#[cfg(any(test, feature = "update-test-fixtures"))]
impl<'a> observer_update::admission::PackageVerifier for KeyedTestReleaseVerifier<'a> {
    fn verify(
        &self,
        package: &mut (dyn observer_update::admission::ReadSeek + '_),
        sidecar_text: &str,
    ) -> Result<(), String> {
        verify_release_signature_with_key(self.0, sidecar_text, package)
    }
}

/// Production package download, streaming signature verification, and nuspec admission.
fn download_and_verify_entry_impl(
    key_text: &str,
    feed_base: &str,
    asset_type: &str,
    file_name: &str,
    expected_version: &str,
    partial_file: &Path,
    progress_sender: Option<Sender<i16>>,
) -> Result<String, String> {
    // 1. Non-full type -> Err before network
    if !observer_update::admission::is_full_asset_type(asset_type) {
        return Err(format!(
            "asset type '{asset_type}' is not a Full release package"
        ));
    }

    // 2. Plain-name and same-origin query-free URLs
    observer_update::admission::check_plain_filename(file_name)?;
    let pkg_url = observer_update::admission::sibling_url(feed_base, file_name)?;
    let sig_name = format!("{file_name}.minisig");
    let sig_url = observer_update::admission::sibling_url(feed_base, &sig_name)?;

    // 3. GET sidecar with no-redirect ureq agent
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .build()
        .into();

    let sig_req = agent.get(&sig_url);
    let sig_res = sig_req
        .call()
        .map_err(|e| format!("failed to GET sidecar signature from {sig_url}: {e}"))?;

    let sig_status = sig_res.status();
    if !sig_status.is_success() {
        return Err(format!("sidecar GET returned HTTP status {sig_status}"));
    }

    let (_head, body) = sig_res.into_parts();
    let mut sig_reader = body.into_reader();
    let sidecar_text = observer_update::admission::read_sidecar_bounded(&mut sig_reader)
        .map_err(|e| format!("sidecar read failed: {e}"))?;

    // 4. Reject non-ED algorithm before package GET
    let pk = PublicKeyBox::from_string(key_text)
        .map_err(|e| format!("invalid public key box: {e}"))?
        .into_public_key()
        .map_err(|e| format!("invalid public key: {e}"))?;

    let sig_box = parse_signature(&sidecar_text)?;

    // 5. GET package with same no-redirect agent and stream body through verify + file tee
    let pkg_req = agent.get(&pkg_url);
    let pkg_res = match pkg_req.call() {
        Ok(res) => res,
        Err(e) => {
            let _ = std::fs::remove_file(partial_file);
            return Err(format!("failed to GET package from {pkg_url}: {e}"));
        }
    };

    let pkg_status = pkg_res.status();
    if !pkg_status.is_success() {
        let _ = std::fs::remove_file(partial_file);
        return Err(format!("package GET returned HTTP status {pkg_status}"));
    }

    let (pkg_head, pkg_body) = pkg_res.into_parts();
    let total_size = pkg_head
        .headers
        .get("Content-Length")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    if let Some(parent) = partial_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let sidecar_final = partial_file
        .parent()
        .ok_or("partial package has no parent")?
        .join(&sig_name);
    let sidecar_partial = sidecar_final.with_file_name(format!("{sig_name}.partial"));
    let result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial_file)
            .map_err(|e| format!("partial package create failed: {e}"))?;
        let mut reader = pkg_body.into_reader();
        let mut downloaded = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let size = reader
                .read(&mut buffer)
                .map_err(|e| format!("package read failed: {e}"))?;
            if size == 0 {
                break;
            }
            file.write_all(&buffer[..size])
                .map_err(|e| format!("package write failed: {e}"))?;
            downloaded += size as u64;
            if let (Some(total), Some(sender)) = (total_size.filter(|v| *v > 0), &progress_sender) {
                let percent = (u128::from(downloaded) * 100 / u128::from(total)).min(100) as i16;
                let _ = sender.send(percent);
            }
        }
        file.sync_all()
            .map_err(|e| format!("package sync failed: {e}"))?;
        drop(file);
        let mut package = observer_update::admission::open_regular_package(partial_file)?;
        minisign::verify(&pk, &sig_box, &mut package.file, true, false, false)
            .map_err(|e| format!("package signature verification failed: {e}"))?;
        observer_update::admission::verify_nuspec(&mut package.file, expected_version)?;
        drop(package);
        let mut sidecar = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sidecar_partial)
            .map_err(|e| format!("sidecar create failed: {e}"))?;
        sidecar
            .write_all(sidecar_text.as_bytes())
            .map_err(|e| format!("sidecar write failed: {e}"))?;
        sidecar
            .sync_all()
            .map_err(|e| format!("sidecar sync failed: {e}"))?;
        drop(sidecar);
        std::fs::rename(&sidecar_partial, &sidecar_final)
            .map_err(|e| format!("sidecar promotion failed: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(partial_file);
        let _ = std::fs::remove_file(&sidecar_partial);
    }
    result?;

    // 8. Return sidecar text on success
    Ok(sidecar_text)
}

/// Production package download entry point used by R2FeedSource.
pub fn download_and_verify_release_entry(
    feed_base: &str,
    asset_type: &str,
    file_name: &str,
    expected_version: &str,
    partial_file: &Path,
    progress_sender: Option<Sender<i16>>,
) -> Result<String, String> {
    download_and_verify_entry_impl(
        PRODUCTION_PUBLIC_KEY,
        feed_base,
        asset_type,
        file_name,
        expected_version,
        partial_file,
        progress_sender,
    )
}

/// Test-only package download entry point with key injection.
#[cfg(any(test, feature = "update-test-fixtures"))]
pub fn download_and_verify_release_entry_with_key(
    key_text: &str,
    feed_base: &str,
    asset_type: &str,
    file_name: &str,
    expected_version: &str,
    partial_file: &Path,
    progress_sender: Option<Sender<i16>>,
) -> Result<String, String> {
    download_and_verify_entry_impl(
        key_text,
        feed_base,
        asset_type,
        file_name,
        expected_version,
        partial_file,
        progress_sender,
    )
}

/// Returns the Windows volume serial number and 64-bit file index for an open file.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn windows_file_id(file: &std::fs::File) -> Result<(u32, u64), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: GetFileInformationByHandle is called with a valid file handle and pointer to out struct.
    unsafe {
        GetFileInformationByHandle(handle, &mut info)
            .map_err(|e| format!("GetFileInformationByHandle failed: {e}"))?;
    }
    let index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    Ok((info.dwVolumeSerialNumber, index))
}

/// Checks that a path exists as a regular file and has the identical volume serial number
/// and 64-bit file index as the held open file descriptor.
#[cfg(windows)]
pub fn windows_path_matches_held(path: &std::path::Path, held: &std::fs::File) -> bool {
    let Ok(sym_meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !sym_meta.file_type().is_file() {
        return false;
    }
    let Ok(fresh_file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(held_id) = windows_file_id(held) else {
        return false;
    };
    let Ok(fresh_id) = windows_file_id(&fresh_file) else {
        return false;
    };
    held_id == fresh_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, SeekFrom};
    #[cfg(feature = "transport-tests")]
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FIXTURE_PUB: &str = include_str!("../tests/fixtures/update/fixture-pub.pub");
    const FIXTURE_NUPKG: &[u8] =
        include_bytes!("../tests/fixtures/update/fixture-2.0.34-full.nupkg");
    const FIXTURE_SIG: &str =
        include_str!("../tests/fixtures/update/fixture-2.0.34-full.nupkg.minisig");

    fn make_test_nupkg(id: &str, version: &str) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Solstone.nuspec", options).unwrap();
        let nuspec = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2010/07/nuspec.xsd">
<metadata>
<id>{id}</id>
<version>{version}</version>
<channel>win</channel>
<os>win</os>
<rid>win-x64</rid>
<machineArchitecture>x64</machineArchitecture>
<mainExe>solstone-windows-app.exe</mainExe>
</metadata>
</package>"#
        );
        zip.write_all(nuspec.as_bytes()).unwrap();
        zip.start_file("lib/net45/solstone-windows-app.exe", options)
            .unwrap();
        zip.write_all(b"exe payload").unwrap();
        zip.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn malformed_comment_signature_refuses_without_panicking() {
        for global in [
            "",
            "AA==",
            &base64::engine::general_purpose::STANDARD.encode([0u8; 65]),
        ] {
            let mut lines: Vec<_> = FIXTURE_SIG.lines().collect();
            lines[3] = global;
            let signature = lines.join("\n") + "\n";
            let result = std::panic::catch_unwind(|| {
                verify_release_signature_with_key(
                    FIXTURE_PUB,
                    &signature,
                    Cursor::new(FIXTURE_NUPKG),
                )
            });
            assert!(result
                .expect("malformed signatures must not panic")
                .is_err());
        }
    }

    #[test]
    fn test_ephemeral_key_signing_and_admission() {
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pub_text = keypair.pk.to_box().unwrap().to_string();

        let nupkg_bytes = make_test_nupkg("Solstone", "2.0.34");
        let sig_box = minisign::sign(
            Some(&keypair.pk),
            &keypair.sk,
            Cursor::new(&nupkg_bytes),
            Some("trusted comment"),
            None,
        )
        .unwrap();
        let sig_text = sig_box.to_string();

        // Positive case: admits
        let mut reader = Cursor::new(&nupkg_bytes);
        assert!(verify_release_signature_with_key(&pub_text, &sig_text, &mut reader).is_ok());

        let verifier = KeyedTestReleaseVerifier(&pub_text);
        let admission = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&nupkg_bytes),
            Some(&sig_text),
            "2.0.34",
            &verifier,
        );
        assert_eq!(admission, observer_update::admission::Admission::Ready);

        // Missing signature -> NotReady
        let not_ready = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&nupkg_bytes),
            None,
            "2.0.34",
            &verifier,
        );
        assert_eq!(not_ready, observer_update::admission::Admission::NotReady);

        // Malformed signature -> Refuse
        let malformed = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&nupkg_bytes),
            Some("not a minisign signature"),
            "2.0.34",
            &verifier,
        );
        assert_eq!(malformed, observer_update::admission::Admission::Refuse);

        // Another key -> Refuse
        let other_keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other_pub = other_keypair.pk.to_box().unwrap().to_string();
        let other_verifier = KeyedTestReleaseVerifier(&other_pub);
        let wrong_key = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&nupkg_bytes),
            Some(&sig_text),
            "2.0.34",
            &other_verifier,
        );
        assert_eq!(wrong_key, observer_update::admission::Admission::Refuse);

        // One flipped package byte -> Refuse
        let mut tampered_bytes = nupkg_bytes.clone();
        tampered_bytes[20] ^= 0xff;
        let tampered = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&tampered_bytes),
            Some(&sig_text),
            "2.0.34",
            &verifier,
        );
        assert_eq!(tampered, observer_update::admission::Admission::Refuse);

        // Signed but wrong id -> Refuse
        let wrong_id_bytes = make_test_nupkg("DifferentApp", "2.0.34");
        let wrong_id_sig = minisign::sign(
            Some(&keypair.pk),
            &keypair.sk,
            Cursor::new(&wrong_id_bytes),
            Some("trusted"),
            None,
        )
        .unwrap()
        .to_string();
        let wrong_id_admission = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&wrong_id_bytes),
            Some(&wrong_id_sig),
            "2.0.34",
            &verifier,
        );
        assert_eq!(
            wrong_id_admission,
            observer_update::admission::Admission::Refuse
        );

        // Signed but wrong version -> Refuse
        let wrong_ver_bytes = make_test_nupkg("Solstone", "2.0.35");
        let wrong_ver_sig = minisign::sign(
            Some(&keypair.pk),
            &keypair.sk,
            Cursor::new(&wrong_ver_bytes),
            Some("trusted"),
            None,
        )
        .unwrap()
        .to_string();
        let wrong_ver_admission = observer_update::admission::admit_package_reader(
            &mut Cursor::new(&wrong_ver_bytes),
            Some(&wrong_ver_sig),
            "2.0.34", // Expecting 2.0.34
            &verifier,
        );
        assert_eq!(
            wrong_ver_admission,
            observer_update::admission::Admission::Refuse
        );
    }

    struct CountingReader<R> {
        inner: R,
        read_bytes: AtomicUsize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read_bytes.fetch_add(n, Ordering::SeqCst);
            Ok(n)
        }
    }

    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn test_committed_stock_minisign_fixture_and_legacy_ed() {
        let mut reader = Cursor::new(FIXTURE_NUPKG);
        assert!(verify_release_signature_with_key(FIXTURE_PUB, FIXTURE_SIG, &mut reader).is_ok());

        let verifier = KeyedTestReleaseVerifier(FIXTURE_PUB);
        let admission = observer_update::admission::admit_package_reader(
            &mut Cursor::new(FIXTURE_NUPKG),
            Some(FIXTURE_SIG),
            "2.0.34",
            &verifier,
        );
        assert_eq!(admission, observer_update::admission::Admission::Ready);

        // Structurally valid legacy Ed box (sig_alg starts with Ed)
        // In base64, Ed begins with 'R' 'W' or similar, whereas ED begins with 'R' 'U'
        // Construct a signature line whose first decoded bytes are "Ed"
        let mut ed_sig_bytes = Vec::new();
        ed_sig_bytes.extend_from_slice(b"Ed");
        ed_sig_bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // keynum
        ed_sig_bytes.extend_from_slice(&[0u8; 64]); // signature

        use base64::prelude::*;
        let ed_b64 = BASE64_STANDARD.encode(&ed_sig_bytes);
        let dummy_global = BASE64_STANDARD.encode([0u8; 64]);
        let legacy_ed_sig = format!(
            "untrusted comment: legacy sig\n{ed_b64}\ntrusted comment: test\n{dummy_global}\n"
        );

        let mut counting_reader = CountingReader {
            inner: Cursor::new(FIXTURE_NUPKG),
            read_bytes: AtomicUsize::new(0),
        };

        let res =
            verify_release_signature_with_key(FIXTURE_PUB, &legacy_ed_sig, &mut counting_reader);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("legacy Ed signature rejected"));
        // Assert package reader was never read
        assert_eq!(counting_reader.read_bytes.load(Ordering::SeqCst), 0);
    }

    // Simple scripted single-connection HTTP server for 127.0.0.1 tests
    #[cfg(feature = "transport-tests")]
    fn run_single_test_server<F>(handler: F) -> (String, std::thread::JoinHandle<()>)
    where
        F: Fn(&str, &str) -> (u16, Vec<(&'static str, String)>, Vec<u8>) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 4096];
            let n = stream.read(&mut req_buf).unwrap();
            let req_str = String::from_utf8_lossy(&req_buf[..n]);
            let first_line = req_str.lines().next().unwrap_or("");
            let parts: Vec<&str> = first_line.split_whitespace().collect();
            if parts.len() >= 2 {
                let (status, headers, body) = handler(parts[0], parts[1]);
                let status_text = match status {
                    200 => "OK",
                    302 => "Found",
                    404 => "Not Found",
                    _ => "Response",
                };
                let mut resp = format!(
                    "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\n",
                    body.len()
                );
                for (h, v) in headers {
                    resp.push_str(&format!("{h}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[cfg(feature = "transport-tests")]
    fn run_multi_test_server<F>(count: usize, handler: F) -> (String, std::thread::JoinHandle<()>)
    where
        F: Fn(&str, &str) -> (u16, Vec<(&'static str, String)>, Vec<u8>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = std::sync::Arc::new(handler);
        let handle = std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut req_buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut req_buf) else {
                    break;
                };
                if n == 0 {
                    continue;
                }
                let req_str = String::from_utf8_lossy(&req_buf[..n]);
                let first_line = req_str.lines().next().unwrap_or("");
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let (status, headers, body) = handler(parts[0], parts[1]);
                    let status_text = match status {
                        200 => "OK",
                        302 => "Found",
                        404 => "Not Found",
                        _ => "Response",
                    };
                    let mut resp = format!("HTTP/1.1 {status} {status_text}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
                    for (h, v) in headers {
                        resp.push_str(&format!("{h}: {v}\r\n"));
                    }
                    resp.push_str("\r\n");
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(&body);
                }
            }
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn existing_partial_links_never_truncate_their_targets() {
        for sidecar in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let victim = dir.path().join("Update.exe");
            std::fs::write(&victim, b"incumbent helper").unwrap();
            let partial = dir.path().join("package.partial");
            let target = if sidecar {
                dir.path()
                    .join("Solstone-2.0.34-full.nupkg.minisig.partial")
            } else {
                partial.clone()
            };
            std::fs::hard_link(&victim, &target).unwrap();
            let (base, worker) = run_multi_test_server(2, |_, path| {
                (
                    200,
                    vec![],
                    if path.ends_with(".minisig") {
                        FIXTURE_SIG.as_bytes().to_vec()
                    } else {
                        FIXTURE_NUPKG.to_vec()
                    },
                )
            });
            assert!(download_and_verify_release_entry_with_key(
                FIXTURE_PUB,
                &base,
                "Full",
                "Solstone-2.0.34-full.nupkg",
                "2.0.34",
                &partial,
                None
            )
            .is_err());
            worker.join().unwrap();
            assert_eq!(std::fs::read(&victim).unwrap(), b"incumbent helper");
            assert!(!target.exists());
        }
    }

    #[cfg(all(unix, feature = "transport-tests"))]
    #[test]
    fn existing_partial_symlink_never_truncates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("Update.exe");
        std::fs::write(&victim, b"incumbent helper").unwrap();
        let partial = dir.path().join("package.partial");
        std::os::unix::fs::symlink(&victim, &partial).unwrap();
        let (base, worker) = run_multi_test_server(2, |_, path| {
            (
                200,
                vec![],
                if path.ends_with(".minisig") {
                    FIXTURE_SIG.as_bytes().to_vec()
                } else {
                    FIXTURE_NUPKG.to_vec()
                },
            )
        });
        assert!(download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None
        )
        .is_err());
        worker.join().unwrap();
        assert_eq!(std::fs::read(victim).unwrap(), b"incumbent helper");
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_production_download_delta_type_refuses_before_socket() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        // Delta type rejected before network
        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            "http://127.0.0.1:9999",
            "Delta",
            "Solstone-2.0.34-1.2.2-delta.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("not a Full release package"));
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_redirect_refusal_on_localhost() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        let (base_url, server) = run_single_test_server(|_method, _path| {
            (
                302,
                vec![("Location", "http://127.0.0.1/redirected".to_string())],
                b"".to_vec(),
            )
        });

        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base_url,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_err());
        server.join().unwrap();
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_oversize_sidecar_refused_before_package_get() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        let oversize_bytes = vec![b'x'; 4097];
        let (base_url, server) = run_single_test_server(move |_method, path| {
            if path.ends_with(".minisig") {
                (200, vec![], oversize_bytes.clone())
            } else {
                panic!("package route should not be requested when sidecar is oversize");
            }
        });

        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base_url,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_err());
        server.join().unwrap();
    }

    #[test]
    fn test_existing_final_file_admission() {
        let temp_dir = tempfile::tempdir().unwrap();
        let final_file = temp_dir.path().join("Solstone-2.0.34-full.nupkg");
        let sig_file = temp_dir.path().join("Solstone-2.0.34-full.nupkg.minisig");

        // Unsigned cache -> NotReady (no sig file)
        std::fs::write(&final_file, FIXTURE_NUPKG).unwrap();
        let verifier = KeyedTestReleaseVerifier(FIXTURE_PUB);

        let mut f = File::open(&final_file).unwrap();
        let admission =
            observer_update::admission::admit_package_reader(&mut f, None, "2.0.34", &verifier);
        assert_eq!(admission, observer_update::admission::Admission::NotReady);

        // Valid pair is Ready
        std::fs::write(&sig_file, FIXTURE_SIG).unwrap();
        let mut f2 = File::open(&final_file).unwrap();
        let admission2 = observer_update::admission::admit_package_reader(
            &mut f2,
            Some(FIXTURE_SIG),
            "2.0.34",
            &verifier,
        );
        assert_eq!(admission2, observer_update::admission::Admission::Ready);
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_download_unsigned_package_refuses_on_localhost() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        // 404 for sidecar
        let (base_url, server) = run_multi_test_server(1, |_method, path| {
            if path.ends_with(".minisig") {
                (404, vec![], b"Not found".to_vec())
            } else {
                (200, vec![], FIXTURE_NUPKG.to_vec())
            }
        });

        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base_url,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_err());
        server.join().unwrap();
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_download_wrong_key_package_refuses_on_localhost() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        // Sign fixture nupkg with a different key
        let other_keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other_sig = minisign::sign(
            Some(&other_keypair.pk),
            &other_keypair.sk,
            Cursor::new(FIXTURE_NUPKG),
            Some("trusted"),
            None,
        )
        .unwrap()
        .to_string();

        let other_sig_bytes = other_sig.into_bytes();
        let (base_url, server) = run_multi_test_server(2, move |_method, path| {
            if path.ends_with(".minisig") {
                (200, vec![], other_sig_bytes.clone())
            } else {
                (200, vec![], FIXTURE_NUPKG.to_vec())
            }
        });

        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base_url,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_err());
        server.join().unwrap();
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn test_download_valid_signed_package_succeeds_on_localhost() {
        let temp_dir = tempfile::tempdir().unwrap();
        let partial = temp_dir.path().join("test.partial");

        let (base_url, server) = run_multi_test_server(2, |_method, path| {
            if path.ends_with(".minisig") {
                (200, vec![], FIXTURE_SIG.as_bytes().to_vec())
            } else {
                (200, vec![], FIXTURE_NUPKG.to_vec())
            }
        });

        let res = download_and_verify_release_entry_with_key(
            FIXTURE_PUB,
            &base_url,
            "Full",
            "Solstone-2.0.34-full.nupkg",
            "2.0.34",
            &partial,
            None,
        );
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), FIXTURE_SIG);
        server.join().unwrap();
    }
}
