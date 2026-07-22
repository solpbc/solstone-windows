// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Narrow object transport for the release-transparency publisher.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::release_exec::CommandRunner;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TransparencyPlane {
    S3,
    Public,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TransparencyObjectDestination {
    pub plane: TransparencyPlane,
    pub key: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TransparencyListDestination {
    pub prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransparencyCachePolicy {
    Immutable,
    NoCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransparencyFetchPolicy {
    Bypass,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransparencyTransportError {
    InvalidDestination,
    ScratchUnavailable,
    InvocationFailed,
    ResponseUnavailable,
}

impl fmt::Display for TransparencyTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDestination => "transparency object destination is invalid",
            Self::ScratchUnavailable => "transparency curl scratch storage is unavailable",
            Self::InvocationFailed => "transparency curl invocation could not be observed",
            Self::ResponseUnavailable => "transparency HTTP response status is unavailable",
        })
    }
}

impl std::error::Error for TransparencyTransportError {}

pub trait TransparencyObjectTransport {
    fn create_only_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError>;

    fn mutable_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
        if_match: Option<&str>,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError>;

    fn get(
        &self,
        destination: &TransparencyObjectDestination,
        cache: TransparencyFetchPolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError>;

    fn list(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError>;
}

pub struct TransparencyS3Credentials {
    access_key_id: String,
    secret_access_key: String,
}

impl TransparencyS3Credentials {
    pub fn new(access_key_id: String, secret_access_key: String) -> Self {
        Self {
            access_key_id,
            secret_access_key,
        }
    }
}

pub struct CurlTransparencyTransport<'a, R: CommandRunner + ?Sized> {
    runner: &'a R,
    curl_program: PathBuf,
    scratch: PathBuf,
    s3_endpoint: String,
    public_base_url: String,
    bucket: String,
    credentials: TransparencyS3Credentials,
    next_exchange: AtomicU64,
}

impl<'a, R: CommandRunner + ?Sized> CurlTransparencyTransport<'a, R> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runner: &'a R,
        curl_program: PathBuf,
        scratch: PathBuf,
        s3_endpoint: String,
        public_base_url: String,
        bucket: String,
        credentials: TransparencyS3Credentials,
    ) -> Result<Self, TransparencyTransportError> {
        if !curl_program.is_absolute()
            || !scratch.is_absolute()
            || !is_https_base(&s3_endpoint)
            || !is_https_base(&public_base_url)
            || !is_safe_bucket(&bucket)
        {
            return Err(TransparencyTransportError::InvalidDestination);
        }
        fs::create_dir_all(&scratch).map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        Ok(Self {
            runner,
            curl_program,
            scratch,
            s3_endpoint,
            public_base_url,
            bucket,
            credentials,
            next_exchange: AtomicU64::new(0),
        })
    }

    fn object_url(
        &self,
        destination: &TransparencyObjectDestination,
    ) -> Result<String, TransparencyTransportError> {
        validate_object_key(&destination.key)?;
        let base = match destination.plane {
            TransparencyPlane::S3 => format!(
                "{}/{}",
                self.s3_endpoint.trim_end_matches('/'),
                percent_encode(&self.bucket)
            ),
            TransparencyPlane::Public => self.public_base_url.trim_end_matches('/').to_owned(),
        };
        Ok(format!(
            "{base}/{}",
            destination
                .key
                .split('/')
                .map(percent_encode)
                .collect::<Vec<_>>()
                .join("/")
        ))
    }

    fn list_url(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<String, TransparencyTransportError> {
        validate_object_key(&destination.prefix)?;
        Ok(format!(
            "{}/{}?list-type=2&prefix={}",
            self.s3_endpoint.trim_end_matches('/'),
            percent_encode(&self.bucket),
            percent_encode(&destination.prefix)
        ))
    }

    fn execute(
        &self,
        plane: TransparencyPlane,
        method: &str,
        url: String,
        body: Option<&[u8]>,
        request_headers: &[String],
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        let mut exchange = self.next_exchange.fetch_add(1, Ordering::Relaxed);
        let exchange_root = loop {
            let candidate = self
                .scratch
                .join(format!("exchange-{}-{exchange}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    exchange = self.next_exchange.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Err(_) => return Err(TransparencyTransportError::ScratchUnavailable),
            }
        };
        let result = (|| {
            let request_path = exchange_root.join("request");
            let response_path = exchange_root.join("response");
            let headers_path = exchange_root.join("headers");
            if let Some(body) = body {
                fs::write(&request_path, body)
                    .map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
            }
            let mut args = Vec::new();
            let stdin = if plane == TransparencyPlane::S3 {
                args.extend(["-K".to_owned(), "-".to_owned()]);
                Some(self.curl_config()?)
            } else {
                None
            };
            args.extend([
                "--silent".to_owned(),
                "--show-error".to_owned(),
                "--request".to_owned(),
                method.to_owned(),
                "--url".to_owned(),
                url,
            ]);
            for header in request_headers {
                args.extend(["--header".to_owned(), header.clone()]);
            }
            if body.is_some() {
                args.extend(["--upload-file".to_owned(), path_text(&request_path)?]);
            }
            args.extend([
                "--dump-header".to_owned(),
                path_text(&headers_path)?,
                "--output".to_owned(),
                path_text(&response_path)?,
            ]);
            let output = self
                .runner
                .run(&self.curl_program, &args, stdin.as_deref(), None)
                .map_err(|_| TransparencyTransportError::InvocationFailed)?;
            let headers = fs::read(&headers_path)
                .map_err(|_| TransparencyTransportError::ResponseUnavailable)?;
            let response_body = fs::read(&response_path)
                .map_err(|_| TransparencyTransportError::ResponseUnavailable)?;
            let (status, etag) = parse_response_headers(&headers)?;
            let _observed_process_status = output.status;
            Ok(ObservedHttpResponse {
                status,
                body: response_body,
                etag,
            })
        })();
        let _ = fs::remove_dir_all(exchange_root);
        result
    }

    fn curl_config(&self) -> Result<Vec<u8>, TransparencyTransportError> {
        let access = quote_curl_config(&self.credentials.access_key_id)?;
        let secret = quote_curl_config(&self.credentials.secret_access_key)?;
        Ok(format!("aws-sigv4 = \"aws:amz:auto:s3\"\nuser = \"{access}:{secret}\"\n").into_bytes())
    }
}

impl<R: CommandRunner + ?Sized> TransparencyObjectTransport for CurlTransparencyTransport<'_, R> {
    fn create_only_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        if destination.plane != TransparencyPlane::S3 {
            return Err(TransparencyTransportError::InvalidDestination);
        }
        self.execute(
            destination.plane,
            "PUT",
            self.object_url(destination)?,
            Some(body),
            &[
                "If-None-Match: *".to_owned(),
                cache_header(cache).to_owned(),
            ],
        )
    }

    fn mutable_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
        if_match: Option<&str>,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        if destination.plane != TransparencyPlane::S3 {
            return Err(TransparencyTransportError::InvalidDestination);
        }
        let mut headers = vec![cache_header(cache).to_owned()];
        if let Some(etag) = if_match {
            if etag.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(TransparencyTransportError::InvalidDestination);
            }
            headers.push(format!("If-Match: {etag}"));
        }
        self.execute(
            destination.plane,
            "PUT",
            self.object_url(destination)?,
            Some(body),
            &headers,
        )
    }

    fn get(
        &self,
        destination: &TransparencyObjectDestination,
        cache: TransparencyFetchPolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        let headers = match cache {
            TransparencyFetchPolicy::Bypass => vec!["Cache-Control: no-cache".to_owned()],
        };
        self.execute(
            destination.plane,
            "GET",
            self.object_url(destination)?,
            None,
            &headers,
        )
    }

    fn list(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.execute(
            TransparencyPlane::S3,
            "GET",
            self.list_url(destination)?,
            None,
            &["Cache-Control: no-cache".to_owned()],
        )
    }
}

fn cache_header(policy: TransparencyCachePolicy) -> &'static str {
    match policy {
        TransparencyCachePolicy::Immutable => "Cache-Control: public,max-age=31536000,immutable",
        TransparencyCachePolicy::NoCache => "Cache-Control: no-cache",
    }
}

fn parse_response_headers(
    bytes: &[u8],
) -> Result<(u16, Option<String>), TransparencyTransportError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| TransparencyTransportError::ResponseUnavailable)?;
    let mut status = None;
    let mut etag = None;
    for line in text.lines() {
        if line.starts_with("HTTP/") {
            status = line
                .split_ascii_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u16>().ok());
            etag = None;
        } else if let Some(value) = line
            .strip_prefix("ETag:")
            .or_else(|| line.strip_prefix("etag:"))
        {
            etag = Some(value.trim().to_owned());
        }
    }
    status
        .map(|status| (status, etag))
        .ok_or(TransparencyTransportError::ResponseUnavailable)
}

fn quote_curl_config(value: &str) -> Result<String, TransparencyTransportError> {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TransparencyTransportError::InvalidDestination);
    }
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    Ok(output)
}

fn is_https_base(value: &str) -> bool {
    value.starts_with("https://")
        && !value[8..].is_empty()
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && !value.contains(['@', '#', '?'])
}

fn is_safe_bucket(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn validate_object_key(value: &str) -> Result<(), TransparencyTransportError> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(TransparencyTransportError::InvalidDestination);
    }
    Ok(())
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

fn path_text(path: &Path) -> Result<String, TransparencyTransportError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(TransparencyTransportError::ScratchUnavailable)
}
