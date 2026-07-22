// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Byte-exact public transparency JSON formats.

use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use jsonschema::{PatternOptions, Validator};
use serde::ser::{Impossible, SerializeMap, SerializeSeq, SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::release_clock::UtcTimestamp;
use crate::rust_release_manifest::{Manifest, PRODUCT};

pub const TRANSPARENCY_ENTRY_SCHEMA_SHA256: &str =
    "b4889cc7195e13a32a76041349103c3829b19a363d49f27e0df62cbf65fb9476";
pub const TRANSPARENCY_ENTRY_SCHEMA_ID: &str =
    "https://solpbc.org/schemas/transparency-ledger-entry/v1.json";
pub const TRANSPARENCY_ENTRY_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
pub const TRANSPARENCY_LATEST_SCHEMA_SHA256: &str =
    "46e655f17170105f73c5f1183e976d2100198bbeb16818d2e666bd6e4630b9a2";
pub const TRANSPARENCY_LATEST_SCHEMA_ID: &str =
    "https://solpbc.org/schemas/transparency-latest/v1.json";
pub const TRANSPARENCY_LATEST_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
pub const TRANSPARENCY_PUBLIC_KEY_FILENAME: &str = "solpbc-transparency-1.pub";
pub const TRANSPARENCY_PUBLIC_KEY_PATH: &str = "releases/keys/solpbc-transparency-1.pub";

const ENTRY_SCHEMA_BYTES: &[u8] = include_bytes!("../../schemas/transparency-ledger-entry/v1.json");
const LATEST_SCHEMA_BYTES: &[u8] = include_bytes!("../../schemas/transparency-latest/v1.json");

static COMPILED_ENTRY_SCHEMA: OnceLock<Result<Validator, TransparencyFormatError>> =
    OnceLock::new();
static COMPILED_LATEST_SCHEMA: OnceLock<Result<Validator, TransparencyFormatError>> =
    OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransparencyFormatError {
    SchemaDigestMismatch,
    SchemaIdentityMismatch,
    SchemaFileMismatch,
    SchemaCompile,
    SchemaViolation,
    Canonical(CanonicalTransparencyJsonError),
    Serialization,
    ProductMismatch,
    DirtySource,
    InvalidArtifactName,
    InvalidNamedDigest,
    DuplicateName,
    SequenceOverflow,
    InvalidPreviousTime,
    PublicationTimeNotLater,
    ValidityTimeOutOfRange,
}

impl fmt::Display for TransparencyFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::SchemaDigestMismatch => "transparency schema digest mismatch",
            Self::SchemaIdentityMismatch => "transparency schema identity mismatch",
            Self::SchemaFileMismatch => "transparency schema file differs from embedded bytes",
            Self::SchemaCompile => "transparency schema failed to compile",
            Self::SchemaViolation => "transparency value violates its runtime schema",
            Self::Canonical(error) => return error.fmt(formatter),
            Self::Serialization => "transparency value could not be projected for validation",
            Self::ProductMismatch => "transparency product does not match the compiled product",
            Self::DirtySource => "transparency entry source state is dirty",
            Self::InvalidArtifactName => "transparency artifact name is not a bare basename",
            Self::InvalidNamedDigest => "transparency named digest is invalid",
            Self::DuplicateName => "transparency inventory contains a duplicate name",
            Self::SequenceOverflow => "transparency sequence is outside the supported range",
            Self::InvalidPreviousTime => "transparency previous publication time is invalid",
            Self::PublicationTimeNotLater => {
                "transparency publication time is not later than the current tip"
            }
            Self::ValidityTimeOutOfRange => {
                "transparency pointer validity time is outside the supported range"
            }
        };
        formatter.write_str(message)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyArtifactRecord {
    pub bytes: u64,
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyNamedDigest {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyLedgerEntryV1 {
    pub artifacts: Vec<TransparencyArtifactRecord>,
    pub manifests: Vec<TransparencyNamedDigest>,
    pub prev_sha256: String,
    pub prev_version: String,
    pub product: String,
    pub proofs: Vec<TransparencyNamedDigest>,
    pub published_utc: String,
    pub schema: String,
    pub seq: u64,
    pub source_commit: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyLatestV1 {
    pub chain_length: u64,
    pub product: String,
    pub schema: String,
    pub signed_at: String,
    pub tip_sha256: String,
    pub valid_until: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparencyTipIdentity {
    pub seq: u64,
    pub version: String,
    pub sha256: String,
    pub published_utc: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyHeadLogRow {
    pub entry_sha256: String,
    pub product: String,
    pub published_utc: String,
    pub seq: u64,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustedCommentError {
    Missing,
    MalformedPrefix,
    WrongKind,
    ProductMismatch,
    SequenceMismatch,
    VersionMismatch,
    BodySha256Mismatch,
    PreviousSha256Mismatch,
    ChainLengthMismatch,
    TipSha256Mismatch,
    ValidUntilMismatch,
    NonCanonical,
}

impl fmt::Display for TrustedCommentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "minisign trusted comment is missing",
            Self::MalformedPrefix => "minisign trusted comment prefix is malformed",
            Self::WrongKind => "minisign trusted comment kind is wrong",
            Self::ProductMismatch => "minisign trusted comment product differs from the body",
            Self::SequenceMismatch => "minisign trusted comment sequence differs from the body",
            Self::VersionMismatch => "minisign trusted comment version differs from the body",
            Self::BodySha256Mismatch => "minisign trusted comment digest differs from the body",
            Self::PreviousSha256Mismatch => {
                "minisign trusted comment previous digest differs from the body"
            }
            Self::ChainLengthMismatch => {
                "minisign trusted comment chain length differs from the body"
            }
            Self::TipSha256Mismatch => "minisign trusted comment tip digest differs from the body",
            Self::ValidUntilMismatch => "minisign trusted comment validity differs from the body",
            Self::NonCanonical => "minisign trusted comment is not in canonical field order",
        })
    }
}

impl std::error::Error for TrustedCommentError {}

impl std::error::Error for TransparencyFormatError {}

impl From<CanonicalTransparencyJsonError> for TransparencyFormatError {
    fn from(error: CanonicalTransparencyJsonError) -> Self {
        Self::Canonical(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalTransparencyJsonError {
    NonAscii,
    DuplicateKey,
    MapValueWithoutKey,
    UnsupportedType,
}

impl fmt::Display for CanonicalTransparencyJsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NonAscii => "transparency JSON contains a non-ASCII string",
            Self::DuplicateKey => "transparency JSON contains a duplicate object key",
            Self::MapValueWithoutKey => "transparency JSON contains a map value without a key",
            Self::UnsupportedType => "transparency JSON contains an unsupported value type",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CanonicalTransparencyJsonError {}

impl serde::ser::Error for CanonicalTransparencyJsonError {
    fn custom<T: fmt::Display>(_message: T) -> Self {
        Self::UnsupportedType
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CanonicalNode {
    Unsigned(u64),
    String(String),
    Array(Vec<CanonicalNode>),
    Object(Vec<(String, CanonicalNode)>),
}

/// Render one transparency JSON value with bytewise-sorted keys and one final newline.
///
/// This deliberately does not use `serde_json::Map` or its feature-dependent iteration order.
pub fn canonicalize_transparency_json<T: Serialize + ?Sized>(
    value: &T,
) -> Result<Vec<u8>, CanonicalTransparencyJsonError> {
    let node = value.serialize(CanonicalNodeSerializer)?;
    let mut output = Vec::new();
    render_node(&node, &mut output)?;
    output.push(b'\n');
    Ok(output)
}

pub fn build_transparency_entry(
    manifest: &Manifest,
    companion: &TransparencyNamedDigest,
    proofs: &[TransparencyNamedDigest],
    previous: Option<&TransparencyTipIdentity>,
    published_utc: &UtcTimestamp,
) -> Result<TransparencyLedgerEntryV1, TransparencyFormatError> {
    if manifest.product != PRODUCT {
        return Err(TransparencyFormatError::ProductMismatch);
    }
    if manifest.source_dirty {
        return Err(TransparencyFormatError::DirtySource);
    }
    validate_named_digest(companion)?;
    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        if artifact.path.is_empty() || artifact.path.contains('/') || artifact.path.contains('\\') {
            return Err(TransparencyFormatError::InvalidArtifactName);
        }
        artifacts.push(TransparencyArtifactRecord {
            bytes: artifact.bytes,
            name: artifact.path.clone(),
            sha256: artifact.sha256.clone(),
        });
    }
    sort_unique_artifacts(&mut artifacts)?;

    let mut proofs = proofs.to_vec();
    for proof in &proofs {
        validate_named_digest(proof)?;
    }
    sort_unique_named_digests(&mut proofs)?;

    let (seq, prev_sha256, prev_version) = match previous {
        Some(previous) => {
            let previous_time = UtcTimestamp::parse(&previous.published_utc)
                .map_err(|_| TransparencyFormatError::InvalidPreviousTime)?;
            if published_utc.system_time() <= previous_time.system_time() {
                return Err(TransparencyFormatError::PublicationTimeNotLater);
            }
            (
                previous
                    .seq
                    .checked_add(1)
                    .ok_or(TransparencyFormatError::SequenceOverflow)?,
                previous.sha256.clone(),
                previous.version.clone(),
            )
        }
        None => (1, "0".repeat(64), String::new()),
    };

    let entry = TransparencyLedgerEntryV1 {
        artifacts,
        manifests: vec![companion.clone()],
        prev_sha256,
        prev_version,
        product: PRODUCT.to_owned(),
        proofs,
        published_utc: published_utc.as_str().to_owned(),
        schema: TRANSPARENCY_ENTRY_SCHEMA_ID.to_owned(),
        seq,
        source_commit: manifest.source_commit.clone(),
        version: manifest.version.clone(),
    };
    validate_transparency_entry_value(
        &serde_json::to_value(&entry).map_err(|_| TransparencyFormatError::Serialization)?,
    )?;
    Ok(entry)
}

pub fn build_transparency_pointer(
    tip: &TransparencyTipIdentity,
    signed_at: &UtcTimestamp,
) -> Result<TransparencyLatestV1, TransparencyFormatError> {
    let valid_until = signed_at
        .system_time()
        .checked_add(Duration::from_secs(14 * 24 * 60 * 60))
        .ok_or(TransparencyFormatError::ValidityTimeOutOfRange)?;
    let valid_until = UtcTimestamp::from_system_time(valid_until)
        .map_err(|_| TransparencyFormatError::ValidityTimeOutOfRange)?;
    let pointer = TransparencyLatestV1 {
        chain_length: tip.seq,
        product: PRODUCT.to_owned(),
        schema: TRANSPARENCY_LATEST_SCHEMA_ID.to_owned(),
        signed_at: signed_at.as_str().to_owned(),
        tip_sha256: tip.sha256.clone(),
        valid_until: valid_until.as_str().to_owned(),
        version: tip.version.clone(),
    };
    validate_transparency_latest_value(
        &serde_json::to_value(&pointer).map_err(|_| TransparencyFormatError::Serialization)?,
    )?;
    Ok(pointer)
}

pub fn render_transparency_entry(
    entry: &TransparencyLedgerEntryV1,
) -> Result<Vec<u8>, TransparencyFormatError> {
    let value = serde_json::to_value(entry).map_err(|_| TransparencyFormatError::Serialization)?;
    validate_transparency_entry_value(&value)?;
    canonicalize_transparency_json(entry).map_err(Into::into)
}

pub fn render_transparency_latest(
    pointer: &TransparencyLatestV1,
) -> Result<Vec<u8>, TransparencyFormatError> {
    let value =
        serde_json::to_value(pointer).map_err(|_| TransparencyFormatError::Serialization)?;
    validate_transparency_latest_value(&value)?;
    canonicalize_transparency_json(pointer).map_err(Into::into)
}

pub fn format_entry_trusted_comment(
    entry: &TransparencyLedgerEntryV1,
    canonical_body: &[u8],
) -> String {
    format!(
        "solpbc-transparency-v1 entry product={} seq={} version={} sha256={} prev={}",
        entry.product,
        entry.seq,
        entry.version,
        transparency_sha256_hex(canonical_body),
        entry.prev_sha256
    )
}

pub fn format_latest_trusted_comment(pointer: &TransparencyLatestV1) -> String {
    format!(
        "solpbc-transparency-v1 latest product={} chain_length={} tip={} valid_until={}",
        pointer.product, pointer.chain_length, pointer.tip_sha256, pointer.valid_until
    )
}

pub fn require_entry_trusted_comment_matches_body(
    entry: &TransparencyLedgerEntryV1,
    canonical_body: &[u8],
    trusted_comment: &str,
) -> Result<(), TrustedCommentError> {
    if trusted_comment.is_empty() {
        return Err(TrustedCommentError::Missing);
    }
    let fields: Vec<&str> = trusted_comment.split(' ').collect();
    if fields.first() != Some(&"solpbc-transparency-v1") {
        return Err(TrustedCommentError::MalformedPrefix);
    }
    if fields.get(1) != Some(&"entry") {
        return Err(TrustedCommentError::WrongKind);
    }
    if fields.len() != 7 || fields.iter().any(|field| field.is_empty()) {
        return Err(TrustedCommentError::NonCanonical);
    }
    let product = required_comment_value(fields[2], "product=")?;
    let seq = required_comment_value(fields[3], "seq=")?;
    let version = required_comment_value(fields[4], "version=")?;
    let sha256 = required_comment_value(fields[5], "sha256=")?;
    let previous = required_comment_value(fields[6], "prev=")?;
    if product != entry.product {
        return Err(TrustedCommentError::ProductMismatch);
    }
    if seq.parse::<u64>().ok() != Some(entry.seq) {
        return Err(TrustedCommentError::SequenceMismatch);
    }
    if version != entry.version {
        return Err(TrustedCommentError::VersionMismatch);
    }
    if sha256 != transparency_sha256_hex(canonical_body) {
        return Err(TrustedCommentError::BodySha256Mismatch);
    }
    if previous != entry.prev_sha256 {
        return Err(TrustedCommentError::PreviousSha256Mismatch);
    }
    if trusted_comment != format_entry_trusted_comment(entry, canonical_body) {
        return Err(TrustedCommentError::NonCanonical);
    }
    Ok(())
}

pub fn require_latest_trusted_comment_matches_body(
    pointer: &TransparencyLatestV1,
    trusted_comment: &str,
) -> Result<(), TrustedCommentError> {
    if trusted_comment.is_empty() {
        return Err(TrustedCommentError::Missing);
    }
    let fields: Vec<&str> = trusted_comment.split(' ').collect();
    if fields.first() != Some(&"solpbc-transparency-v1") {
        return Err(TrustedCommentError::MalformedPrefix);
    }
    if fields.get(1) != Some(&"latest") {
        return Err(TrustedCommentError::WrongKind);
    }
    if fields.len() != 6 || fields.iter().any(|field| field.is_empty()) {
        return Err(TrustedCommentError::NonCanonical);
    }
    let product = required_comment_value(fields[2], "product=")?;
    let chain_length = required_comment_value(fields[3], "chain_length=")?;
    let tip = required_comment_value(fields[4], "tip=")?;
    let valid_until = required_comment_value(fields[5], "valid_until=")?;
    if product != pointer.product {
        return Err(TrustedCommentError::ProductMismatch);
    }
    if chain_length.parse::<u64>().ok() != Some(pointer.chain_length) {
        return Err(TrustedCommentError::ChainLengthMismatch);
    }
    if tip != pointer.tip_sha256 {
        return Err(TrustedCommentError::TipSha256Mismatch);
    }
    if valid_until != pointer.valid_until {
        return Err(TrustedCommentError::ValidUntilMismatch);
    }
    if trusted_comment != format_latest_trusted_comment(pointer) {
        return Err(TrustedCommentError::NonCanonical);
    }
    Ok(())
}

fn required_comment_value<'a>(
    field: &'a str,
    prefix: &str,
) -> Result<&'a str, TrustedCommentError> {
    field
        .strip_prefix(prefix)
        .filter(|value| !value.is_empty())
        .ok_or(TrustedCommentError::NonCanonical)
}

fn validate_named_digest(digest: &TransparencyNamedDigest) -> Result<(), TransparencyFormatError> {
    if digest.name.is_empty()
        || digest.name.contains('/')
        || digest.name.contains('\\')
        || digest.sha256.len() != 64
        || !digest
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TransparencyFormatError::InvalidNamedDigest);
    }
    Ok(())
}

fn sort_unique_artifacts(
    artifacts: &mut [TransparencyArtifactRecord],
) -> Result<(), TransparencyFormatError> {
    artifacts.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    if artifacts
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err(TransparencyFormatError::DuplicateName);
    }
    Ok(())
}

fn sort_unique_named_digests(
    digests: &mut [TransparencyNamedDigest],
) -> Result<(), TransparencyFormatError> {
    digests.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    if digests.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(TransparencyFormatError::DuplicateName);
    }
    Ok(())
}

pub fn verify_vendored_transparency_entry_schema(
    root: &Path,
) -> Result<(), TransparencyFormatError> {
    verify_vendored_schema(
        &root.join("schemas/transparency-ledger-entry/v1.json"),
        ENTRY_SCHEMA_BYTES,
        TRANSPARENCY_ENTRY_SCHEMA_SHA256,
        compiled_entry_schema,
    )
}

pub fn verify_vendored_transparency_latest_schema(
    root: &Path,
) -> Result<(), TransparencyFormatError> {
    verify_vendored_schema(
        &root.join("schemas/transparency-latest/v1.json"),
        LATEST_SCHEMA_BYTES,
        TRANSPARENCY_LATEST_SCHEMA_SHA256,
        compiled_latest_schema,
    )
}

pub fn validate_transparency_entry_value(value: &Value) -> Result<(), TransparencyFormatError> {
    compiled_entry_schema()?
        .validate(value)
        .map_err(|_| TransparencyFormatError::SchemaViolation)
}

pub fn validate_transparency_latest_value(value: &Value) -> Result<(), TransparencyFormatError> {
    compiled_latest_schema()?
        .validate(value)
        .map_err(|_| TransparencyFormatError::SchemaViolation)
}

pub fn transparency_sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn verify_vendored_schema(
    path: &Path,
    embedded: &[u8],
    digest: &str,
    compile: fn() -> Result<&'static Validator, TransparencyFormatError>,
) -> Result<(), TransparencyFormatError> {
    let bytes = fs::read(path).map_err(|_| TransparencyFormatError::SchemaFileMismatch)?;
    if bytes != embedded || transparency_sha256_hex(&bytes) != digest {
        return Err(TransparencyFormatError::SchemaFileMismatch);
    }
    compile().map(|_| ())
}

fn compiled_entry_schema() -> Result<&'static Validator, TransparencyFormatError> {
    COMPILED_ENTRY_SCHEMA
        .get_or_init(|| {
            compile_schema(
                ENTRY_SCHEMA_BYTES,
                TRANSPARENCY_ENTRY_SCHEMA_SHA256,
                TRANSPARENCY_ENTRY_SCHEMA_ID,
                TRANSPARENCY_ENTRY_SCHEMA_DIALECT,
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn compiled_latest_schema() -> Result<&'static Validator, TransparencyFormatError> {
    COMPILED_LATEST_SCHEMA
        .get_or_init(|| {
            compile_schema(
                LATEST_SCHEMA_BYTES,
                TRANSPARENCY_LATEST_SCHEMA_SHA256,
                TRANSPARENCY_LATEST_SCHEMA_ID,
                TRANSPARENCY_LATEST_SCHEMA_DIALECT,
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn compile_schema(
    bytes: &[u8],
    digest: &str,
    id: &str,
    dialect: &str,
) -> Result<Validator, TransparencyFormatError> {
    if transparency_sha256_hex(bytes) != digest {
        return Err(TransparencyFormatError::SchemaDigestMismatch);
    }
    let schema: Value = serde_json::from_slice(bytes)
        .map_err(|_| TransparencyFormatError::SchemaIdentityMismatch)?;
    if schema.get("$id").and_then(Value::as_str) != Some(id)
        || schema.get("$schema").and_then(Value::as_str) != Some(dialect)
    {
        return Err(TransparencyFormatError::SchemaIdentityMismatch);
    }
    jsonschema::draft202012::options()
        .with_pattern_options(PatternOptions::fancy_regex())
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(&schema)
        .map_err(|_| TransparencyFormatError::SchemaCompile)
}

fn render_node(
    node: &CanonicalNode,
    output: &mut Vec<u8>,
) -> Result<(), CanonicalTransparencyJsonError> {
    match node {
        CanonicalNode::Unsigned(value) => output.extend_from_slice(value.to_string().as_bytes()),
        CanonicalNode::String(value) => render_ascii_string(value, output),
        CanonicalNode::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                render_node(value, output)?;
            }
            output.push(b']');
        }
        CanonicalNode::Object(entries) => {
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            for pair in sorted.windows(2) {
                if pair[0].0 == pair[1].0 {
                    return Err(CanonicalTransparencyJsonError::DuplicateKey);
                }
            }
            output.push(b'{');
            for (index, (key, value)) in sorted.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                render_ascii_string(key, output);
                output.push(b':');
                render_node(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn render_ascii_string(value: &str, output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push(b'"');
    for byte in value.bytes() {
        match byte {
            b'"' => output.extend_from_slice(br#"\""#),
            b'\\' => output.extend_from_slice(br#"\\"#),
            0x08 => output.extend_from_slice(br"\b"),
            0x09 => output.extend_from_slice(br"\t"),
            0x0a => output.extend_from_slice(br"\n"),
            0x0c => output.extend_from_slice(br"\f"),
            0x0d => output.extend_from_slice(br"\r"),
            0x00..=0x1f => {
                output.extend_from_slice(br"\u00");
                output.push(HEX[usize::from(byte >> 4)]);
                output.push(HEX[usize::from(byte & 0x0f)]);
            }
            _ => output.push(byte),
        }
    }
    output.push(b'"');
}

#[derive(Clone, Copy, Debug)]
struct CanonicalNodeSerializer;

impl Serializer for CanonicalNodeSerializer {
    type Ok = CanonicalNode;
    type Error = CanonicalTransparencyJsonError;
    type SerializeSeq = ArraySerializer;
    type SerializeTuple = Impossible<CanonicalNode, CanonicalTransparencyJsonError>;
    type SerializeTupleStruct = Impossible<CanonicalNode, CanonicalTransparencyJsonError>;
    type SerializeTupleVariant = Impossible<CanonicalNode, CanonicalTransparencyJsonError>;
    type SerializeMap = ObjectSerializer;
    type SerializeStruct = ObjectSerializer;
    type SerializeStructVariant = Impossible<CanonicalNode, CanonicalTransparencyJsonError>;

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(CanonicalNode::Unsigned(value))
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        require_ascii(value)?;
        Ok(CanonicalNode::String(value.to_owned()))
    }

    fn serialize_seq(self, length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(ArraySerializer {
            values: Vec::with_capacity(length.unwrap_or(0)),
        })
    }

    fn serialize_map(self, length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(ObjectSerializer::new(length))
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(ObjectSerializer::new(Some(length)))
    }

    fn serialize_bool(self, _value: bool) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_i8(self, _value: i8) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_i16(self, _value: i16) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_i32(self, _value: i32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_i64(self, _value: i64) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_u8(self, _value: u8) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_u16(self, _value: u16) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_u32(self, _value: u32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_char(self, _value: char) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        unsupported()
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        unsupported()
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        unsupported()
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        unsupported()
    }
}

fn unsupported<T>() -> Result<T, CanonicalTransparencyJsonError> {
    Err(CanonicalTransparencyJsonError::UnsupportedType)
}

fn require_ascii(value: &str) -> Result<(), CanonicalTransparencyJsonError> {
    if value.is_ascii() {
        Ok(())
    } else {
        Err(CanonicalTransparencyJsonError::NonAscii)
    }
}

#[derive(Debug)]
struct ArraySerializer {
    values: Vec<CanonicalNode>,
}

impl SerializeSeq for ArraySerializer {
    type Ok = CanonicalNode;
    type Error = CanonicalTransparencyJsonError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.values.push(value.serialize(CanonicalNodeSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(CanonicalNode::Array(self.values))
    }
}

#[derive(Debug)]
struct ObjectSerializer {
    entries: Vec<(String, CanonicalNode)>,
    pending_key: Option<String>,
}

impl ObjectSerializer {
    fn new(length: Option<usize>) -> Self {
        Self {
            entries: Vec::with_capacity(length.unwrap_or(0)),
            pending_key: None,
        }
    }

    fn push(
        &mut self,
        key: &str,
        value: CanonicalNode,
    ) -> Result<(), CanonicalTransparencyJsonError> {
        require_ascii(key)?;
        self.entries.push((key.to_owned(), value));
        Ok(())
    }

    fn finish(self) -> Result<CanonicalNode, CanonicalTransparencyJsonError> {
        if self.pending_key.is_some() {
            return Err(CanonicalTransparencyJsonError::MapValueWithoutKey);
        }
        Ok(CanonicalNode::Object(self.entries))
    }
}

impl SerializeMap for ObjectSerializer {
    type Ok = CanonicalNode;
    type Error = CanonicalTransparencyJsonError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        if self.pending_key.is_some() {
            return Err(CanonicalTransparencyJsonError::MapValueWithoutKey);
        }
        self.pending_key = Some(key.serialize(ObjectKeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let key = self
            .pending_key
            .take()
            .ok_or(CanonicalTransparencyJsonError::MapValueWithoutKey)?;
        let value = value.serialize(CanonicalNodeSerializer)?;
        self.push(&key, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

impl SerializeStruct for ObjectSerializer {
    type Ok = CanonicalNode;
    type Error = CanonicalTransparencyJsonError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.push(key, value.serialize(CanonicalNodeSerializer)?)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish()
    }
}

#[derive(Clone, Copy, Debug)]
struct ObjectKeySerializer;

impl Serializer for ObjectKeySerializer {
    type Ok = String;
    type Error = CanonicalTransparencyJsonError;
    type SerializeSeq = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeTuple = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeTupleStruct = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeTupleVariant = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeMap = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeStruct = Impossible<String, CanonicalTransparencyJsonError>;
    type SerializeStructVariant = Impossible<String, CanonicalTransparencyJsonError>;

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        require_ascii(value)?;
        Ok(value.to_owned())
    }

    fn serialize_bool(self, _value: bool) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_i8(self, _value: i8) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_i16(self, _value: i16) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_i32(self, _value: i32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_i64(self, _value: i64) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_u8(self, _value: u8) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_u16(self, _value: u16) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_u32(self, _value: u32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_u64(self, _value: u64) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_char(self, _value: char) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        unsupported()
    }
    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        unsupported()
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        unsupported()
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        unsupported()
    }
    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        unsupported()
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        unsupported()
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        unsupported()
    }
}
