// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Relay access payload validation and JWT-v2 claim verification.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelayAccessValidationError {
    #[error("unsupported protocol version")]
    UnsupportedProtocolVersion,
    #[error("instance ID mismatch")]
    InstanceMismatch,
    #[error("invalid relay origin")]
    InvalidRelayOrigin,
    #[error("malformed RFC3339 timestamp")]
    MalformedTimestamp,
    #[error("malformed JWT token")]
    MalformedJwt,
    #[error("invalid JWT claims")]
    InvalidJwtClaims,
    #[error("token expired or unusable")]
    TokenExpired,
    #[error("expires_at does not match JWT exp claim")]
    ExpiryMismatch,
}

/// Strict JWT v2 claims payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayJwtClaimsV2 {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub scope: String,
    pub ver: u32,
    pub instance_id: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

/// Response from `GET /app/network/api/relay/access`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status")]
pub enum RelayAccessResponse {
    #[serde(rename = "ready")]
    Ready {
        protocol_version: u32,
        relay_origin: String,
        instance_id: String,
        device_token: String,
        expires_at: String,
    },
    #[serde(rename = "not_configured")]
    NotConfigured { protocol_version: u32 },
}

/// Validated ready access credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReadyAccess {
    pub relay_origin: String,
    pub instance_id: String,
    pub device_token: String,
    pub expires_at: i64,
}

/// Parse RFC3339 date-time string to unix seconds.
///
/// Supports UTC 'Z' and numeric offsets '+HH:MM' / '-HH:MM', with optional fractional seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 20 {
        return None;
    }
    // Expected format: YYYY-MM-DDTHH:MM:SS...
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4)? != &b'-' {
        return None;
    }
    let month: usize = s.get(5..7)?.parse().ok()?;
    if s.as_bytes().get(7)? != &b'-' {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let sep = s.as_bytes().get(10)?;
    if *sep != b'T' && *sep != b't' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if s.as_bytes().get(13)? != &b':' {
        return None;
    }
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    if s.as_bytes().get(16)? != &b':' {
        return None;
    }
    let sec: i64 = s.get(17..19)?.parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&sec)
    {
        return None;
    }

    let rest = &s[19..];
    // Rest might have fractional seconds (.123456) followed by Z or +HH:MM / -HH:MM
    let rest_without_frac = if let Some(stripped) = rest.strip_prefix('.') {
        let end_idx = stripped
            .find(['Z', 'z', '+', '-'])
            .unwrap_or(stripped.len());
        &stripped[end_idx..]
    } else {
        rest
    };

    let offset_secs: i64 = if rest_without_frac == "Z" || rest_without_frac == "z" {
        0
    } else if (rest_without_frac.starts_with('+') || rest_without_frac.starts_with('-'))
        && rest_without_frac.len() == 6
        && rest_without_frac.as_bytes()[3] == b':'
    {
        let sign = if rest_without_frac.starts_with('+') {
            1
        } else {
            -1
        };
        let off_hour: i64 = rest_without_frac[1..3].parse().ok()?;
        let off_min: i64 = rest_without_frac[4..6].parse().ok()?;
        if off_hour > 23 || off_min > 59 {
            return None;
        }
        sign * (off_hour * 3600 + off_min * 60)
    } else {
        return None;
    };

    // Calculate days since 1970-01-01
    fn is_leap_year(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
    }

    fn days_in_month(y: i64, m: usize) -> i64 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap_year(y) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    }

    if day > days_in_month(year, month) {
        return None;
    }

    // Days before this year from 1970
    let mut total_days = 0i64;
    if year >= 1970 {
        for y in 1970..year {
            total_days += if is_leap_year(y) { 366 } else { 365 };
        }
    } else {
        for y in year..1970 {
            total_days -= if is_leap_year(y) { 366 } else { 365 };
        }
    }

    for m in 1..month {
        total_days += days_in_month(year, m);
    }
    total_days += day - 1;

    let total_secs = total_days * 86400 + hour * 3600 + minute * 60 + sec - offset_secs;
    Some(total_secs)
}

/// Decode and strictly validate JWT v2 claims for relay access.
pub fn validate_jwt_v2(
    token: &str,
    paired_instance_id: &str,
    now_secs: i64,
) -> Result<RelayJwtClaimsV2, RelayAccessValidationError> {
    let mut parts = token.split('.');
    let _header = parts
        .next()
        .ok_or(RelayAccessValidationError::MalformedJwt)?;
    let payload = parts
        .next()
        .ok_or(RelayAccessValidationError::MalformedJwt)?;
    let _sig = parts
        .next()
        .ok_or(RelayAccessValidationError::MalformedJwt)?;
    if parts.next().is_some() {
        return Err(RelayAccessValidationError::MalformedJwt);
    }

    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| RelayAccessValidationError::MalformedJwt)?;

    let claims: RelayJwtClaimsV2 = serde_json::from_slice(&decoded)
        .map_err(|_| RelayAccessValidationError::InvalidJwtClaims)?;

    if claims.iss.trim().is_empty() {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    if claims.ver != 2 {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    if claims.aud != "spl-relay" {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    if claims.scope != "session.dial" {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    let expected_sub = format!("instance:{paired_instance_id}");
    if claims.sub != expected_sub {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    if claims.instance_id != paired_instance_id {
        return Err(RelayAccessValidationError::InstanceMismatch);
    }
    if claims.exp <= claims.iat {
        return Err(RelayAccessValidationError::InvalidJwtClaims);
    }
    if claims.exp <= now_secs {
        return Err(RelayAccessValidationError::TokenExpired);
    }

    Ok(claims)
}

/// Validate a relay access response.
pub fn validate_relay_access_response(
    response: &RelayAccessResponse,
    paired_instance_id: &str,
    now_secs: i64,
) -> Result<Option<ValidatedReadyAccess>, RelayAccessValidationError> {
    match response {
        RelayAccessResponse::NotConfigured { protocol_version } => {
            if *protocol_version != 2 {
                return Err(RelayAccessValidationError::UnsupportedProtocolVersion);
            }
            Ok(None)
        }
        RelayAccessResponse::Ready {
            protocol_version,
            relay_origin,
            instance_id,
            device_token,
            expires_at,
        } => {
            if *protocol_version != 2 {
                return Err(RelayAccessValidationError::UnsupportedProtocolVersion);
            }
            if instance_id != paired_instance_id {
                return Err(RelayAccessValidationError::InstanceMismatch);
            }

            // Origin check: must be a bare http/https origin supported by relay dial logic.
            if crate::relay_http::parse_relay_origin(relay_origin).is_err()
                || observer_pl::relay::pair_dial_url(relay_origin).is_err()
            {
                return Err(RelayAccessValidationError::InvalidRelayOrigin);
            }

            let exp_ts =
                parse_rfc3339(expires_at).ok_or(RelayAccessValidationError::MalformedTimestamp)?;

            let claims = validate_jwt_v2(device_token, paired_instance_id, now_secs)?;
            if exp_ts != claims.exp {
                return Err(RelayAccessValidationError::ExpiryMismatch);
            }

            Ok(Some(ValidatedReadyAccess {
                relay_origin: relay_origin.clone(),
                instance_id: instance_id.clone(),
                device_token: device_token.clone(),
                expires_at: exp_ts,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint_test_jwt(claims: &RelayJwtClaimsV2) -> String {
        let header = "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCJ9";
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let sig = "dummy_signature";
        format!("{header}.{payload}.{sig}")
    }

    #[test]
    fn rfc3339_parser_cases() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339("1969-12-31T19:00:00-05:00"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-09-07T12:00:00.123456Z"),
            Some(1788782400)
        );
        assert_eq!(parse_rfc3339("2026-09-07T14:00:00+02:00"), Some(1788782400));
        assert_eq!(parse_rfc3339("invalid"), None);
        assert_eq!(parse_rfc3339("2026-02-29T12:00:00Z"), None); // 2026 not leap
        assert_eq!(parse_rfc3339("2024-02-29T00:00:00Z"), Some(1709164800)); // 2024 leap
    }

    #[test]
    fn validate_jwt_v2_exact_claims() {
        let now = 1788782400;
        let claims = RelayJwtClaimsV2 {
            iss: "https://relay.solstone.app".to_string(),
            sub: "instance:inst-123".to_string(),
            aud: "spl-relay".to_string(),
            scope: "session.dial".to_string(),
            ver: 2,
            instance_id: "inst-123".to_string(),
            iat: now - 100,
            exp: now + 3600,
            jti: "jti-1".to_string(),
        };
        let token = mint_test_jwt(&claims);

        let validated = validate_jwt_v2(&token, "inst-123", now).unwrap();
        assert_eq!(validated, claims);

        // Instance mismatch
        assert_eq!(
            validate_jwt_v2(&token, "inst-456", now),
            Err(RelayAccessValidationError::InvalidJwtClaims)
        );

        // Expired
        assert_eq!(
            validate_jwt_v2(&token, "inst-123", now + 4000),
            Err(RelayAccessValidationError::TokenExpired)
        );

        // Extra unknown field -> rejected
        let raw_with_extra = serde_json::json!({
            "iss": "https://relay.solstone.app",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": now - 100,
            "exp": now + 3600,
            "jti": "jti-1",
            "extra_field": "forbidden"
        });
        let extra_token = format!(
            "header.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&raw_with_extra).unwrap())
        );
        assert_eq!(
            validate_jwt_v2(&extra_token, "inst-123", now),
            Err(RelayAccessValidationError::InvalidJwtClaims)
        );
    }

    #[test]
    fn validate_relay_access_response_ready_and_not_configured() {
        let now = 1788782400;
        let claims = RelayJwtClaimsV2 {
            iss: "https://relay.solstone.app".to_string(),
            sub: "instance:inst-123".to_string(),
            aud: "spl-relay".to_string(),
            scope: "session.dial".to_string(),
            ver: 2,
            instance_id: "inst-123".to_string(),
            iat: now - 100,
            exp: now + 3600,
            jti: "jti-1".to_string(),
        };
        let token = mint_test_jwt(&claims);

        let ready_resp = RelayAccessResponse::Ready {
            protocol_version: 2,
            relay_origin: "https://relay.solstone.app".to_string(),
            instance_id: "inst-123".to_string(),
            device_token: token.clone(),
            expires_at: "2026-09-07T13:00:00Z".to_string(), // now + 3600
        };

        let res = validate_relay_access_response(&ready_resp, "inst-123", now).unwrap();
        assert_eq!(
            res,
            Some(ValidatedReadyAccess {
                relay_origin: "https://relay.solstone.app".to_string(),
                instance_id: "inst-123".to_string(),
                device_token: token,
                expires_at: now + 3600,
            })
        );

        let not_config = RelayAccessResponse::NotConfigured {
            protocol_version: 2,
        };
        assert_eq!(
            validate_relay_access_response(&not_config, "inst-123", now).unwrap(),
            None
        );
    }
}
