use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

static REGEX: LazyLock<RwLock<HashMap<String, Option<regex_lite::Regex>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn regex_match(pattern: &str, s: &str) -> bool {
    if let Some(r) = REGEX.read().ok().and_then(|m| m.get(pattern).cloned()) {
        return r.is_some_and(|r| r.is_match(s));
    }
    let compiled = regex_lite::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .ok();
    let hit = compiled.as_ref().is_some_and(|r| r.is_match(s));
    if let Ok(mut m) = REGEX.write() {
        m.insert(pattern.to_string(), compiled);
    }
    hit
}

fn host_matches(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
    } else if let Some(prefix) = pattern.strip_suffix(".*") {
        host.starts_with(prefix) && host.as_bytes().get(prefix.len()) == Some(&b'.')
    } else {
        pattern.eq_ignore_ascii_case(host)
    }
}

pub fn is_valid(rules: &[String], referer: Option<&str>, server_names: &[String]) -> bool {
    let Some(referer) = referer.filter(|r| !r.is_empty()) else {
        return rules.iter().any(|r| r == "none");
    };
    let lower = referer.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return rules.iter().any(|r| r == "blocked");
    };
    let (hostport, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = hostport.split(':').next().unwrap_or("");

    rules.iter().any(|rule| match rule.as_str() {
        "none" | "blocked" => false,
        "server_names" => server_names.iter().any(|n| host_matches(n, host)),
        r if r.starts_with('~') => regex_match(&r[1..], rest),
        r => match r.split_once('/') {
            Some((h, p)) => host_matches(h, host) && path.starts_with(&format!("/{p}")),
            None => host_matches(r, host),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nginx_semantics() {
        let names = rules(&["ex.com"]);
        let r = rules(&["none", "blocked", "server_names", "*.cdn.net", "~google\\."]);
        assert!(is_valid(&r, None, &names));
        assert!(is_valid(&r, Some("ex.com/x"), &names));
        assert!(is_valid(&r, Some("https://ex.com/page"), &names));
        assert!(is_valid(&r, Some("http://a.cdn.net:8080/"), &names));
        assert!(!is_valid(&r, Some("http://cdn.net/"), &names));
        assert!(is_valid(&r, Some("https://www.google.com/"), &names));
        assert!(!is_valid(&r, Some("https://evil.com/"), &names));
        assert!(!is_valid(&rules(&["ex.com"]), None, &names));
        assert!(is_valid(
            &rules(&["ex.com/img"]),
            Some("http://ex.com/img/a"),
            &names
        ));
        assert!(!is_valid(
            &rules(&["ex.com/img"]),
            Some("http://ex.com/b"),
            &names
        ));
    }
}
