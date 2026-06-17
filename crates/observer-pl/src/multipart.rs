// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The ingest `multipart/form-data` body builder.
//!
//! Byte-identical to the macOS / iOS / Android uploaders: text fields first
//! (`segment`, `day`, `platform`, optional `meta`), then one or more file parts
//! under the field name **`files`** — the journal reads
//! `request.files.getlist("files")`, so the field name is load-bearing. CRLF
//! line endings throughout. The boundary is supplied by the caller (the
//! transport mints a unique one per request); this crate stays pure.

/// One file part of the upload.
pub struct FilePart {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The `Content-Type` header value for a multipart body with `boundary`.
pub fn content_type(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

/// Build the multipart body. `fields` are ordered text fields; `files` are the
/// `files`-named parts. Mirrors the reference uploaders exactly.
pub fn build(boundary: &str, fields: &[(&str, &str)], files: &[FilePart]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in fields {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    for file in files {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"files\"; filename=\"{}\"\r\n",
                file.filename
            )
            .as_bytes(),
        );
        out.extend_from_slice(format!("Content-Type: {}\r\n\r\n", file.content_type).as_bytes());
        out.extend_from_slice(&file.bytes);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ordered_fields_then_files() {
        let body = build(
            "BREAD",
            &[
                ("segment", "143000_300"),
                ("day", "20260617"),
                ("platform", "windows"),
            ],
            &[FilePart {
                filename: "screen.bin".into(),
                content_type: "application/octet-stream".into(),
                bytes: b"RGBA".to_vec(),
            }],
        );
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains(
            "--BREAD\r\nContent-Disposition: form-data; name=\"segment\"\r\n\r\n143000_300\r\n"
        ));
        assert!(text.contains(
            "--BREAD\r\nContent-Disposition: form-data; name=\"day\"\r\n\r\n20260617\r\n"
        ));
        assert!(text.contains(
            "Content-Disposition: form-data; name=\"files\"; filename=\"screen.bin\"\r\nContent-Type: application/octet-stream\r\n\r\nRGBA\r\n"
        ));
        assert!(text.ends_with("--BREAD--\r\n"));
        // The file field name is exactly `files` (server reads getlist("files")).
        assert!(text.contains("name=\"files\""));
    }

    #[test]
    fn content_type_carries_boundary() {
        assert_eq!(content_type("xyz"), "multipart/form-data; boundary=xyz");
    }

    #[test]
    fn segment_field_precedes_day_precedes_files() {
        let body = build(
            "B",
            &[("segment", "s"), ("day", "d")],
            &[FilePart {
                filename: "f".into(),
                content_type: "x".into(),
                bytes: b"z".to_vec(),
            }],
        );
        let text = String::from_utf8(body).unwrap();
        let seg = text.find("name=\"segment\"").unwrap();
        let day = text.find("name=\"day\"").unwrap();
        let files = text.find("name=\"files\"").unwrap();
        assert!(seg < day && day < files);
    }
}
