// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use crate::{
        CAP_COOKIE_NAME, OBSERVER_HANDLE_HEADER, PROTOCOL_VERSION_HEADER, UPSTREAM_COOKIE_PREFIX,
    };
    use spl_core::bridge::*;

    fn names() -> BridgeNames {
        BridgeNames {
            capability_cookie_name: CAP_COOKIE_NAME.into(),
            upstream_cookie_prefix: UPSTREAM_COOKIE_PREFIX.into(),
            observer_header_name: OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
            protocol_version_header_name: PROTOCOL_VERSION_HEADER.to_ascii_lowercase(),
        }
    }

    fn request_policy() -> RequestHeaderPolicy {
        RequestHeaderPolicy::Allow(
            [
                "accept",
                "accept-language",
                "content-type",
                "cache-control",
                "if-none-match",
                "if-modified-since",
                "range",
                "user-agent",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        )
    }
    fn request(
        method: &str,
        target: &str,
        host: Option<&str>,
        extra: &[(&str, &str)],
    ) -> RequestHead {
        let mut raw = format!("{method} {target} HTTP/1.1\r\n");
        if let Some(host) = host {
            raw.push_str("Host: ");
            raw.push_str(host);
            raw.push_str("\r\n");
        }
        for (name, value) in extra {
            raw.push_str(name);
            raw.push_str(": ");
            raw.push_str(value);
            raw.push_str("\r\n");
        }
        raw.push_str("\r\n");
        parse_request_head(raw.as_bytes()).unwrap().head
    }

    fn authed_request(method: &str, host: Option<&str>, cap: &str) -> RequestHead {
        request(
            method,
            "/",
            host,
            &[("Cookie", &format!("{CAP_COOKIE_NAME}={cap}; sid=journal"))],
        )
    }

    #[test]
    fn parses_request_head_helpers() {
        let head = request(
            "GET",
            "/journal?day=20260701",
            Some("127.0.0.1:49152"),
            &[("Cookie", "a=1; __solstone_journal_cap=secret")],
        );

        assert_eq!(head.method, "GET");
        assert_eq!(head.target, "/journal?day=20260701");
        assert_eq!(head.host(), Some("127.0.0.1:49152"));
        assert_eq!(head.path(), "/journal");
        assert_eq!(head.query(), Some("day=20260701"));
        assert_eq!(head.cookie(CAP_COOKIE_NAME), Some("secret"));
    }

    #[test]
    fn authorize_accepts_valid_local_request() {
        let head = authed_request("GET", Some("127.0.0.1:49152"), "secret");
        assert_eq!(authorize(&head, b"secret", 49152, &names()), Ok(()));
    }

    #[test]
    fn authorize_rejects_bad_or_missing_capability() {
        let wrong = authed_request("GET", Some("127.0.0.1:49152"), "wrong");
        assert_eq!(
            authorize(&wrong, b"secret", 49152, &names()),
            Err(RejectReason::BadCapability)
        );

        let missing = request("GET", "/", Some("127.0.0.1:49152"), &[]);
        assert_eq!(
            authorize(&missing, b"secret", 49152, &names()),
            Err(RejectReason::BadCapability)
        );
    }

    #[test]
    fn authorize_rejects_host_mismatch_before_anything_else() {
        let wrong_port = authed_request("GET", Some("127.0.0.1:49153"), "secret");
        assert_eq!(
            authorize(&wrong_port, b"secret", 49152, &names()),
            Err(RejectReason::BadHost)
        );

        let non_loopback = authed_request("GET", Some("localhost:49152"), "secret");
        assert_eq!(
            authorize(&non_loopback, b"secret", 49152, &names()),
            Err(RejectReason::BadHost)
        );

        let missing = authed_request("GET", None, "secret");
        assert_eq!(
            authorize(&missing, b"secret", 49152, &names()),
            Err(RejectReason::BadHost)
        );
    }

    #[test]
    fn authorize_accepts_current_device_delete_only_on_exact_paths() {
        for path in [
            "/app/network/api/clients/self",
            "/app/link/api/clients/self",
        ] {
            let head = request(
                "DELETE",
                path,
                Some("127.0.0.1:49152"),
                &[("Cookie", "__solstone_journal_cap=secret")],
            );
            assert_eq!(authorize(&head, b"secret", 49152, &names()), Ok(()));
        }

        for path in [
            "/app/network/api/clients/self/",
            "/app/network/api/clients/sha256:other",
            "/app/link/api/clients/selfish",
        ] {
            let head = request(
                "DELETE",
                path,
                Some("127.0.0.1:49152"),
                &[("Cookie", "__solstone_journal_cap=secret")],
            );
            assert_eq!(
                authorize(&head, b"secret", 49152, &names()),
                Err(RejectReason::BadMethod),
                "{path}"
            );
        }
    }

    #[test]
    fn authorize_rejects_unsupported_methods() {
        for method in ["OPTIONS", "PUT", "DELETE"] {
            let head = authed_request(method, Some("127.0.0.1:49152"), "secret");
            assert_eq!(
                authorize(&head, b"secret", 49152, &names()),
                Err(RejectReason::BadMethod)
            );
        }
    }

    #[test]
    fn authorize_rejects_caller_auth_headers() {
        for header in [
            "Authorization",
            "X-Solstone-Observer",
            "X-Solstone-Protocol-Version",
        ] {
            let head = request(
                "GET",
                "/",
                Some("127.0.0.1:49152"),
                &[
                    ("Cookie", "__solstone_journal_cap=secret"),
                    (header, "caller-owned"),
                ],
            );
            assert_eq!(
                authorize(&head, b"secret", 49152, &names()),
                Err(RejectReason::CallerAuth)
            );
        }
    }

    #[test]
    fn constant_time_compare_basics() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreu"));
        assert!(!ct_eq(b"secret", b"secret!"));
    }

    #[test]
    fn upstream_request_headers_keep_only_allowlist() {
        let head = request(
            "POST",
            "/",
            Some("127.0.0.1:49152"),
            &[
                ("Accept", "text/html"),
                ("Accept-Language", "en-US"),
                ("Content-Type", "application/json"),
                ("Cache-Control", "no-cache"),
                ("If-None-Match", "\"abc\""),
                ("If-Modified-Since", "Wed, 01 Jul 2026 00:00:00 GMT"),
                ("Range", "bytes=0-10"),
                ("User-Agent", "WebView2"),
                (
                    "Cookie",
                    "__solstone_journal_cap=secret; __solstone_journal_up_sid=journal; unrelated=drop",
                ),
                ("Origin", "http://127.0.0.1:49152"),
                ("Referer", "http://127.0.0.1:49152/"),
                ("Content-Length", "99"),
                ("Connection", "keep-alive"),
                ("Authorization", "Bearer bad"),
            ],
        );

        let headers = upstream_request_headers(&head, &names(), &request_policy());

        assert!(headers.contains(&("accept".to_string(), "text/html".to_string())));
        assert!(headers.contains(&("accept-language".to_string(), "en-US".to_string())));
        assert!(headers.contains(&("content-type".to_string(), "application/json".to_string())));
        assert!(headers.contains(&("cache-control".to_string(), "no-cache".to_string())));
        assert!(headers.contains(&("if-none-match".to_string(), "\"abc\"".to_string())));
        assert!(headers.contains(&(
            "if-modified-since".to_string(),
            "Wed, 01 Jul 2026 00:00:00 GMT".to_string()
        )));
        assert!(headers.contains(&("range".to_string(), "bytes=0-10".to_string())));
        assert!(headers.contains(&("user-agent".to_string(), "WebView2".to_string())));
        assert!(headers.contains(&("cookie".to_string(), "sid=journal".to_string())));
        assert!(!headers.iter().any(|(name, _)| matches!(
            name.as_str(),
            "host" | "origin" | "referer" | "content-length" | "connection" | "authorization"
        )));
        assert!(!headers
            .iter()
            .any(|(_, value)| value.contains(CAP_COOKIE_NAME)
                || value.contains("secret")
                || value.contains("unrelated")));
    }

    #[test]
    fn upstream_request_headers_omit_cookie_when_only_capability_cookie_present() {
        let head = request(
            "GET",
            "/",
            Some("127.0.0.1:49152"),
            &[("Cookie", "__solstone_journal_cap=secret")],
        );

        let headers = upstream_request_headers(&head, &names(), &request_policy());

        assert!(!headers.iter().any(|(name, _)| name == "cookie"));
    }

    #[test]
    fn rewrite_set_cookie_drops_domain_and_secure() {
        let rewritten = rewrite_set_cookie(
            "sid=abc; Domain=journal.example; Secure; Path=/; HttpOnly; SameSite=Lax; Max-Age=60",
            &names(),
        );

        assert_eq!(
            rewritten,
            "__solstone_journal_up_sid=abc; Path=/; HttpOnly; SameSite=Lax; Max-Age=60"
        );
    }

    #[test]
    fn rewrite_redirect_handles_relative_journal_foreign_and_spl() {
        let loopback = "http://127.0.0.1:49152";
        let journal_hosts = vec![
            "journal.example".to_string(),
            "https://default.example".to_string(),
            "spl.local".to_string(),
        ];

        assert_eq!(
            rewrite_redirect("/app?day=1#top", &journal_hosts, loopback),
            "/app?day=1#top"
        );
        assert_eq!(
            rewrite_redirect(
                "https://journal.example/app?day=1#top",
                &journal_hosts,
                loopback
            ),
            "http://127.0.0.1:49152/app?day=1#top"
        );
        assert_eq!(
            rewrite_redirect("https://foreign.example/app", &journal_hosts, loopback),
            "https://foreign.example/app"
        );
        assert_eq!(
            rewrite_redirect("http://spl.local/sse/events", &journal_hosts, loopback),
            "http://127.0.0.1:49152/sse/events"
        );
        assert_eq!(
            rewrite_redirect("https://default.example:443/x", &journal_hosts, loopback),
            "http://127.0.0.1:49152/x"
        );
    }

    #[test]
    fn response_headers_filter_and_rewrite() {
        let upstream = vec![
            ("content-type".to_string(), "text/html".to_string()),
            ("content-length".to_string(), "10".to_string()),
            ("transfer-encoding".to_string(), "chunked".to_string()),
            ("connection".to_string(), "close".to_string()),
            ("etag".to_string(), "\"abc\"".to_string()),
            ("x-content-type-options".to_string(), "nosniff".to_string()),
            (
                "set-cookie".to_string(),
                "sid=abc; Domain=journal.example; Secure; Path=/; HttpOnly".to_string(),
            ),
            (
                "location".to_string(),
                "https://journal.example/app?x=1#frag".to_string(),
            ),
            ("x-debug".to_string(), "drop".to_string()),
        ];
        let headers = response_headers(
            &upstream,
            &["journal.example".to_string()],
            "http://127.0.0.1:49152",
            &names(),
        );

        assert!(headers.contains(&("content-type".to_string(), "text/html".to_string())));
        assert!(headers.contains(&("etag".to_string(), "\"abc\"".to_string())));
        assert!(headers.contains(&("x-content-type-options".to_string(), "nosniff".to_string())));
        assert!(headers.contains(&(
            "set-cookie".to_string(),
            "__solstone_journal_up_sid=abc; Path=/; HttpOnly".to_string()
        )));
        assert!(headers.contains(&(
            "location".to_string(),
            "http://127.0.0.1:49152/app?x=1#frag".to_string()
        )));
        assert!(!headers.iter().any(|(name, _)| matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "x-debug"
        )));
    }

    #[test]
    fn bootstrap_cap_extracts_only_exact_route_query() {
        assert_eq!(
            bootstrap_cap("/_bridge/bootstrap?cap=secret"),
            Some("secret")
        );
        assert_eq!(
            bootstrap_cap("/_bridge/bootstrap?x=1&cap=secret"),
            Some("secret")
        );
        assert_eq!(bootstrap_cap("/_bridge/bootstrap"), None);
        assert_eq!(bootstrap_cap("/_bridge/bootstrap/extra?cap=secret"), None);
        assert_eq!(bootstrap_cap("/?cap=secret"), None);
        assert_eq!(
            bootstrap_cookie_attributes(),
            "Path=/; HttpOnly; SameSite=Strict"
        );
    }

    #[test]
    fn failure_category_tokens_are_stable() {
        assert_eq!(FailureCategory::LocalBind.token(), "local_bind_fail");
        assert_eq!(
            FailureCategory::LocalCapabilityReject.token(),
            "local_capability_reject"
        );
        assert_eq!(
            FailureCategory::UpstreamUnreachable.token(),
            "upstream_unreachable"
        );
        assert_eq!(
            FailureCategory::UpstreamCredential.token(),
            "upstream_credential"
        );
        assert_eq!(RejectReason::BadHost.token(), "bad_host");
    }
}
