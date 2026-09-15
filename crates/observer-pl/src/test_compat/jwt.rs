// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use spl_core::jwt::*;
    fn token_with_payload(payload: &[u8]) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(b"{}"),
            URL_SAFE_NO_PAD.encode(payload),
            "sig"
        )
    }

    #[test]
    fn decodes_valid_claims() {
        let token = token_with_payload(br#"{"iat":100,"exp":200}"#);
        assert_eq!(
            decode_unverified_claims(&token),
            Some(JwtClaims { iat: 100, exp: 200 })
        );
    }

    #[test]
    fn malformed_tokens_decode_to_none() {
        assert_eq!(decode_unverified_claims("two.parts"), None);
        assert_eq!(decode_unverified_claims("too.many.parts.here"), None);
        assert_eq!(decode_unverified_claims("header.!!!!.sig"), None);
        assert_eq!(
            decode_unverified_claims(&token_with_payload(b"not json")),
            None
        );
        assert_eq!(
            decode_unverified_claims(&token_with_payload(br#"{"iat":100}"#)),
            None
        );
        assert_eq!(
            decode_unverified_claims(&token_with_payload(br#"{"iat":"100","exp":200}"#)),
            None
        );
    }

    #[test]
    fn refresh_boundary_is_strictly_greater_than_eighty_percent() {
        let claims = JwtClaims { iat: 100, exp: 200 };
        assert!(!should_refresh(&claims, 180));
        assert!(should_refresh(&claims, 181));
    }

    #[test]
    fn expired_positive_ttl_refreshes() {
        let claims = JwtClaims { iat: 100, exp: 200 };
        assert!(should_refresh(&claims, 250));
    }

    #[test]
    fn non_positive_ttl_does_not_refresh() {
        assert!(!should_refresh(&JwtClaims { iat: 200, exp: 200 }, 300));
        assert!(!should_refresh(&JwtClaims { iat: 201, exp: 200 }, 300));
    }
}
