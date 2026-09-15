// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use serde_json::json;
    use spl_core::relay_access::*;

    fn claims() -> serde_json::Value {
        json!({"iss":"independent-issuer","sub":"instance:home","aud":"spl-relay",
            "scope":"session.dial","ver":2,"instance_id":"home","iat":100,"exp":200,"jti":"fresh"})
    }

    fn token(payload: &serde_json::Value) -> String {
        format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(payload.to_string()))
    }

    #[test]
    fn negotiated_access_checks_identity_shape_and_exact_expiry() {
        let valid = token(&claims());
        assert!(negotiated_claims(2, &valid, "1970-01-01T00:03:20Z", "home", 150).is_some());
        assert!(negotiated_claims(2, &valid, "1970-01-01T00:03:20.001Z", "home", 150).is_none());
        assert!(negotiated_claims(2, &valid, "1970-01-01T00:03:21Z", "home", 150).is_none());
        assert!(negotiated_claims(1, &valid, "1970-01-01T00:03:20Z", "home", 150).is_none());
        assert!(instance_claims(&valid, "other-home", 150).is_none());
        assert!(instance_claims(&valid, "home", 200).is_none());
        for (field, value) in [
            ("ver", json!(3)),
            ("device_fp", json!("private")),
            ("ca_fp", json!("private")),
            ("previous_jti", json!("old")),
            ("iat", json!(211)),
            ("exp", json!(100)),
            ("exp", json!(200.5)),
            ("sub", json!("device:old")),
            ("scope", json!("session.listen")),
            ("aud", json!("other")),
            ("iss", json!("")),
            ("jti", json!("")),
        ] {
            let mut bad = claims();
            bad[field] = value;
            assert!(
                instance_claims(&token(&bad), "home", 150).is_none(),
                "{field}"
            );
        }
        for bad in ["a.b.c.d", ".e30.sig", "e30.e30.", "e30.!!!!.sig"] {
            assert!(instance_claims(bad, "home", 150).is_none());
        }
    }
}
