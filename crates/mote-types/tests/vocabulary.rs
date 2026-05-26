//! Black-box tests for the `mote-types` public vocabulary.
//!
//! These exercise only the public API, exactly as a downstream crate would.

use mote_types::{
    Checksum, Glob, GlobSet, IdentityId, Match, Origin, PluginName, SchemaVersion, TabId,
    WorkspaceId,
};

// --- SchemaVersion ----------------------------------------------------------

#[test]
fn schema_version_parses_and_renders_v1() {
    let v: SchemaVersion = "v1".parse().expect("v1 parses");
    assert_eq!(v, SchemaVersion::V1);
    assert_eq!(v.to_string(), "v1");
    assert_eq!(v.as_str(), "v1");
}

#[test]
fn schema_version_rejects_unknown() {
    assert!("v2".parse::<SchemaVersion>().is_err());
    assert!("1".parse::<SchemaVersion>().is_err());
    assert!("".parse::<SchemaVersion>().is_err());
    assert!("V1".parse::<SchemaVersion>().is_err());
}

// --- PluginName -------------------------------------------------------------

#[test]
fn plugin_name_accepts_valid_identifiers() {
    for name in [
        "history",
        "dark-mode",
        "password-manager-1password",
        "a",
        "plugin0",
    ] {
        let parsed = PluginName::new(name).unwrap_or_else(|e| panic!("{name} should parse: {e}"));
        assert_eq!(parsed.as_str(), name);
        assert_eq!(parsed.to_string(), name);
    }
}

#[test]
fn plugin_name_rejects_invalid() {
    for bad in [
        "",          // empty
        "-leading",  // leading hyphen
        "trailing-", // trailing hyphen
        "double--hyphen",
        "Upper",       // uppercase
        "has space",   // space
        "under_score", // underscore not allowed
        "dot.name",    // dot
        "slash/name",  // path separator
    ] {
        assert!(PluginName::new(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn plugin_name_equality_and_ordering() {
    let a = PluginName::new("alpha").unwrap();
    let a2 = PluginName::new("alpha").unwrap();
    let b = PluginName::new("beta").unwrap();
    assert_eq!(a, a2);
    assert_ne!(a, b);
    assert!(a < b);
}

// --- Origin -----------------------------------------------------------------

#[test]
fn origin_roundtrips() {
    let o = Origin::new("https://github.com");
    assert_eq!(o.as_str(), "https://github.com");
    assert_eq!(o.to_string(), "https://github.com");
    assert_eq!(o, Origin::new("https://github.com"));
}

// --- Glob matching ----------------------------------------------------------

#[test]
fn glob_exact_match() {
    let g: Glob = "https://github.com/*".parse().unwrap();
    assert!(!g.is_negated());
    assert!(g.matches("https://github.com/foo"));
    assert!(g.matches("https://github.com/"));
    assert!(!g.matches("https://gitlab.com/foo"));
}

#[test]
fn glob_bare_star_matches_anything() {
    let g: Glob = "*".parse().unwrap();
    assert!(g.matches("anything-at-all"));
    assert!(g.matches(""));
}

#[test]
fn glob_internal_wildcards() {
    let g: Glob = "https://*.1password.com/*".parse().unwrap();
    assert!(g.matches("https://my.1password.com/vault"));
    assert!(g.matches("https://a.1password.com/"));
    assert!(!g.matches("https://1password.com/")); // requires the dot-subdomain
    assert!(!g.matches("https://evil.com/1password.com"));
}

#[test]
fn glob_wildcard_is_not_greedy_across_required_literals() {
    let g: Glob = "a*c*e".parse().unwrap();
    assert!(g.matches("ace"));
    assert!(g.matches("abcde"));
    assert!(g.matches("axxxcyyye"));
    assert!(!g.matches("ac")); // missing trailing e
    assert!(!g.matches("abcd"));
}

#[test]
fn glob_negation_parses_pattern_without_the_bang() {
    let g: Glob = "!*.banking.com".parse().unwrap();
    assert!(g.is_negated());
    // The negated glob still *matches* on its pattern; precedence is a GlobSet concern.
    assert!(g.matches("secure.banking.com"));
    assert!(!g.matches("github.com"));
}

#[test]
fn glob_literal_star_only_via_wildcard() {
    // A pattern is a sequence of literals separated by '*'; there is no escape,
    // so matching a literal asterisk is not expressible — documented behavior.
    let g: Glob = "a*b".parse().unwrap();
    assert!(g.matches("a*b")); // the '*' in input is just a literal char here
    assert!(g.matches("aXb"));
}

#[test]
fn glob_rejects_empty_pattern() {
    assert!("".parse::<Glob>().is_err());
    assert!("!".parse::<Glob>().is_err()); // negation with no body
}

// --- GlobSet deny-precedence ------------------------------------------------

#[test]
fn globset_allow_only() {
    let set = GlobSet::parse(["https://github.com/*", "https://*.linear.app/*"]).unwrap();
    assert_eq!(set.evaluate("https://github.com/x"), Match::Allow);
    assert_eq!(set.evaluate("https://team.linear.app/y"), Match::Allow);
    assert_eq!(set.evaluate("https://gitlab.com/x"), Match::Unmatched);
}

#[test]
fn globset_deny_beats_overlapping_allow() {
    // The DESIGN example: intercept everything but never banking.
    let set = GlobSet::parse(["*", "!*.banking.com"]).unwrap();
    assert_eq!(set.evaluate("ads.example.com"), Match::Allow);
    assert_eq!(set.evaluate("secure.banking.com"), Match::Deny);
}

#[test]
fn globset_deny_precedence_regardless_of_order() {
    // Deny wins even when the allow pattern is listed *after* the deny.
    let denied_first = GlobSet::parse(["!*.banking.com", "*"]).unwrap();
    assert_eq!(denied_first.evaluate("secure.banking.com"), Match::Deny);
    assert_eq!(denied_first.evaluate("example.com"), Match::Allow);
}

#[test]
fn globset_unmatched_when_nothing_applies() {
    let set = GlobSet::parse(["https://github.com/*"]).unwrap();
    assert_eq!(set.evaluate("https://elsewhere.com/"), Match::Unmatched);
}

#[test]
fn globset_deny_without_matching_allow_is_still_deny() {
    let set = GlobSet::parse(["!*.banking.com"]).unwrap();
    assert_eq!(set.evaluate("secure.banking.com"), Match::Deny);
    assert_eq!(set.evaluate("example.com"), Match::Unmatched);
}

#[test]
fn globset_is_allowed_convenience() {
    let set = GlobSet::parse(["*", "!*.banking.com"]).unwrap();
    assert!(set.is_allowed("example.com"));
    assert!(!set.is_allowed("secure.banking.com"));
    let narrow = GlobSet::parse(["https://github.com/*"]).unwrap();
    assert!(!narrow.is_allowed("https://other.com/")); // unmatched is not allowed
}

// --- Checksum ---------------------------------------------------------------

#[test]
fn checksum_hashes_bytes_deterministically() {
    let a = Checksum::hash(b"hello world");
    let b = Checksum::hash(b"hello world");
    let c = Checksum::hash(b"different");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn checksum_renders_with_blake3_prefix() {
    let cs = Checksum::hash(b"mote");
    let rendered = cs.to_string();
    assert!(rendered.starts_with("blake3:"), "got {rendered}");
    // 32-byte digest => 64 hex chars after the prefix.
    assert_eq!(rendered.len(), "blake3:".len() + 64);
}

#[test]
fn checksum_roundtrips_through_string() {
    let cs = Checksum::hash(b"roundtrip");
    let s = cs.to_string();
    let parsed: Checksum = s.parse().unwrap();
    assert_eq!(cs, parsed);
}

#[test]
fn checksum_matches_known_blake3_vector() {
    // BLAKE3 of the empty input is a fixed, well-known digest.
    let cs = Checksum::hash(b"");
    assert_eq!(
        cs.to_string(),
        "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}

#[test]
fn checksum_rejects_bad_input() {
    assert!("sha256:abc".parse::<Checksum>().is_err()); // wrong algorithm
    assert!("blake3:".parse::<Checksum>().is_err()); // empty hex
    assert!("blake3:xyz".parse::<Checksum>().is_err()); // non-hex
    assert!("deadbeef".parse::<Checksum>().is_err()); // missing prefix
    assert!("blake3:abcd".parse::<Checksum>().is_err()); // wrong length
}

// --- Id newtypes ------------------------------------------------------------

#[test]
fn ids_are_distinct_types_but_share_behavior() {
    let i = IdentityId::new(1);
    let w = WorkspaceId::new(1);
    let t = TabId::new(1);
    assert_eq!(i.get(), 1);
    assert_eq!(w.get(), 1);
    assert_eq!(t.get(), 1);
    assert_eq!(i, IdentityId::new(1));
    assert_ne!(i, IdentityId::new(2));
    // Distinct types: this is a compile-time guarantee, asserted by usage above.
}
