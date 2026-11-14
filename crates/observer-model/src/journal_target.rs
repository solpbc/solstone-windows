// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Opaque journal-link ingress. The journal alone interprets the sealed text.

#[derive(Debug, PartialEq, Eq)]
pub enum Ingress<'a> {
    Other,
    Invalid,
    Target(&'a str),
}

pub fn valid_target(target: &str) -> bool {
    let (sealed, moment) = match target.split_once('~') {
        Some((sealed, moment)) => (sealed, Some(moment)),
        None => (target, None),
    };
    !sealed.is_empty()
        && sealed.len() <= 512
        && sealed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && moment.is_none_or(|m| m.len() == 6 && m.bytes().all(|b| b.is_ascii_digit()))
}

pub fn classify(url: &str) -> Ingress<'_> {
    let Some(scheme) = url.get(..11) else {
        return Ingress::Other;
    };
    if !scheme.eq_ignore_ascii_case("solstone://") {
        return Ingress::Other;
    }
    let rest = &url[11..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if !host.eq_ignore_ascii_case("open")
        && !rest
            .get(..4)
            .is_some_and(|s| s.eq_ignore_ascii_case("open"))
    {
        return Ingress::Other;
    }
    match rest
        .get(..4)
        .filter(|s| s.eq_ignore_ascii_case("open"))
        .and_then(|_| rest[4..].strip_prefix('#'))
    {
        Some(target) if valid_target(target) => Ingress::Target(target),
        _ => Ingress::Invalid,
    }
}

pub fn path(target: &str) -> String {
    format!("/app/open/{target}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_preserves_opaque_bytes_and_checks_only_grammar() {
        for target in [
            "a-_Z09".to_string(),
            "A".repeat(512),
            format!("{}~256199", "A".repeat(512)),
        ] {
            let url = format!("solstone://open#{target}");
            assert_eq!(classify(&url), Ingress::Target(target.as_str()));
            assert_eq!(path(&target), format!("/app/open/{target}"));
        }
        assert_eq!(classify("SOLSTONE://OPEN#a"), Ingress::Target("a"));
        assert_eq!(classify("solstone://pair#abc"), Ingress::Other);
    }

    #[test]
    fn malformed_open_never_enters_pairing() {
        for suffix in [
            "",
            "#",
            "/#a",
            "?x#a",
            "@evil#a",
            "#a=",
            "#a%2Db",
            "#a/b",
            "#a?b",
            "#a#b",
            "#a~1234",
            "#a~1234567",
            "#a~12345é",
            "#a~123456~123456",
            "#a ",
            "#é",
        ] {
            assert_eq!(
                classify(&format!("solstone://open{suffix}")),
                Ingress::Invalid
            );
        }
        assert_eq!(
            classify(&format!("solstone://open#{}", "A".repeat(513))),
            Ingress::Invalid
        );
    }

    #[test]
    fn malformed_open_authorities_request_the_notice() {
        for url in [
            "solstone://user@open#Aa",
            "solstone://user:password@open#Aa",
            "solstone://open:80#Aa",
            "solstone://open@elsewhere#Aa",
        ] {
            assert_eq!(classify(url), Ingress::Invalid);
        }
    }
}
