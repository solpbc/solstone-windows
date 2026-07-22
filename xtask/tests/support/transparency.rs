// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![allow(dead_code)]

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xtask::transparency_format::transparency_sha256_hex;
use xtask::transparency_transport::{
    ObservedHttpResponse, TransparencyCachePolicy, TransparencyFetchPolicy,
    TransparencyListDestination, TransparencyObjectDestination, TransparencyObjectTransport,
    TransparencyPlane, TransparencyTransportError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordedTransparencyCall {
    CreateOnlyPut(
        TransparencyObjectDestination,
        Vec<u8>,
        TransparencyCachePolicy,
    ),
    MutablePut(
        TransparencyObjectDestination,
        Vec<u8>,
        TransparencyCachePolicy,
        Option<String>,
    ),
    Get(TransparencyObjectDestination, TransparencyFetchPolicy),
    List(TransparencyListDestination),
}

pub struct DirectoryTransparencyTransport {
    root: PathBuf,
    calls: Mutex<Vec<RecordedTransparencyCall>>,
    injected: Mutex<VecDeque<Result<ObservedHttpResponse, TransparencyTransportError>>>,
}

impl DirectoryTransparencyTransport {
    pub fn new(root: PathBuf) -> Self {
        fs::create_dir_all(root.join("s3")).expect("create fake S3 root");
        fs::create_dir_all(root.join("public")).expect("create fake public root");
        Self {
            root,
            calls: Mutex::new(Vec::new()),
            injected: Mutex::new(VecDeque::new()),
        }
    }

    pub fn calls(&self) -> Vec<RecordedTransparencyCall> {
        self.calls.lock().expect("read fake call log").clone()
    }

    pub fn destinations(&self) -> Vec<TransparencyObjectDestination> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                RecordedTransparencyCall::CreateOnlyPut(destination, _, _)
                | RecordedTransparencyCall::MutablePut(destination, _, _, _)
                | RecordedTransparencyCall::Get(destination, _) => Some(destination),
                RecordedTransparencyCall::List(_) => None,
            })
            .collect()
    }

    pub fn inject(&self, response: Result<ObservedHttpResponse, TransparencyTransportError>) {
        self.injected
            .lock()
            .expect("write fake response script")
            .push_back(response);
    }

    pub fn object_bytes(&self, destination: &TransparencyObjectDestination) -> Option<Vec<u8>> {
        fs::read(self.object_path(destination)).ok()
    }

    fn object_path(&self, destination: &TransparencyObjectDestination) -> PathBuf {
        self.root
            .join(match destination.plane {
                TransparencyPlane::S3 => "s3",
                TransparencyPlane::Public => "public",
            })
            .join(&destination.key)
    }

    fn mirror_public(&self, destination: &TransparencyObjectDestination, body: &[u8]) {
        if destination.plane == TransparencyPlane::S3 {
            let path = self.root.join("public").join(&destination.key);
            fs::create_dir_all(path.parent().expect("fake public object parent"))
                .expect("create fake public parent");
            fs::write(path, body).expect("mirror fake public object");
        }
    }

    fn scripted(&self) -> Option<Result<ObservedHttpResponse, TransparencyTransportError>> {
        self.injected
            .lock()
            .expect("read fake response script")
            .pop_front()
    }

    fn stored_response(path: &Path) -> ObservedHttpResponse {
        match fs::read(path) {
            Ok(body) => ObservedHttpResponse {
                status: 200,
                etag: Some(format!("\"{}\"", transparency_sha256_hex(&body))),
                body,
            },
            Err(_) => ObservedHttpResponse {
                status: 404,
                body: Vec::new(),
                etag: None,
            },
        }
    }
}

impl TransparencyObjectTransport for DirectoryTransparencyTransport {
    fn create_only_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.calls
            .lock()
            .expect("record fake call")
            .push(RecordedTransparencyCall::CreateOnlyPut(
                destination.clone(),
                body.to_vec(),
                cache,
            ));
        if let Some(response) = self.scripted() {
            return response;
        }
        let path = self.object_path(destination);
        if path.exists() {
            return Ok(ObservedHttpResponse {
                status: 412,
                body: fs::read(path).unwrap_or_default(),
                etag: None,
            });
        }
        fs::create_dir_all(path.parent().expect("fake object parent"))
            .map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        fs::write(&path, body).map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        self.mirror_public(destination, body);
        Ok(ObservedHttpResponse {
            status: 201,
            body: Vec::new(),
            etag: Some(format!("\"{}\"", transparency_sha256_hex(body))),
        })
    }

    fn mutable_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
        if_match: Option<&str>,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.calls
            .lock()
            .expect("record fake call")
            .push(RecordedTransparencyCall::MutablePut(
                destination.clone(),
                body.to_vec(),
                cache,
                if_match.map(str::to_owned),
            ));
        if let Some(response) = self.scripted() {
            return response;
        }
        let path = self.object_path(destination);
        let current = Self::stored_response(&path);
        if let Some(expected) = if_match {
            if current.etag.as_deref() != Some(expected) {
                return Ok(ObservedHttpResponse {
                    status: 412,
                    body: Vec::new(),
                    etag: current.etag,
                });
            }
        }
        fs::create_dir_all(path.parent().expect("fake object parent"))
            .map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        fs::write(&path, body).map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        self.mirror_public(destination, body);
        Ok(ObservedHttpResponse {
            status: if current.status == 404 { 201 } else { 200 },
            body: Vec::new(),
            etag: Some(format!("\"{}\"", transparency_sha256_hex(body))),
        })
    }

    fn get(
        &self,
        destination: &TransparencyObjectDestination,
        cache: TransparencyFetchPolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.calls
            .lock()
            .expect("record fake call")
            .push(RecordedTransparencyCall::Get(destination.clone(), cache));
        if let Some(response) = self.scripted() {
            return response;
        }
        Ok(Self::stored_response(&self.object_path(destination)))
    }

    fn list(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.calls
            .lock()
            .expect("record fake call")
            .push(RecordedTransparencyCall::List(destination.clone()));
        if let Some(response) = self.scripted() {
            return response;
        }
        let root = self.root.join("s3");
        let mut keys = Vec::new();
        collect_keys(&root, &root, &mut keys)
            .map_err(|_| TransparencyTransportError::ScratchUnavailable)?;
        keys.sort();
        let body = keys
            .into_iter()
            .filter(|key| key.starts_with(&destination.prefix))
            .fold("<ListBucketResult>".to_owned(), |mut output, key| {
                output.push_str(&format!("<Key>{key}</Key>"));
                output
            })
            + "</ListBucketResult>";
        Ok(ObservedHttpResponse {
            status: 200,
            body: body.into_bytes(),
            etag: None,
        })
    }
}

fn collect_keys(root: &Path, directory: &Path, output: &mut Vec<String>) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_keys(root, &entry.path(), output)?;
        } else if entry.file_type()?.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("fake object below root")
                .to_string_lossy()
                .replace('\\', "/");
            output.push(relative);
        }
    }
    Ok(())
}
