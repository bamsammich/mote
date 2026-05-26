//! The [`Permission`] type — a parsed `domain:action[:resource]` triple.

use std::fmt;
use std::str::FromStr;

use mote_types::Glob;

use crate::error::PermissionParseError;

/// A parsed permission in `domain:action[:resource]` form.
///
/// # Grammar
///
/// ```text
/// permission    = domain ":" action [":" resource]
/// domain        = identifier
/// action        = identifier
/// resource      = glob           # may be negated with a leading "!"
/// identifier    = [a-z] [a-z0-9_]*
/// glob          = ["!"] body
/// body          = segment ("*" segment)*
/// segment       = *<any char except "!">
/// ```
///
/// When the resource segment is absent, the permission is treated as applying to
/// **all resources** (equivalent to `*`).  The resource *itself* may contain
/// further `:` characters — everything after the second `:` is the resource glob
/// (e.g. `mcp:client:my-server-name`, `secret:read:anthropic_api_key`).
///
/// # Examples
///
/// ```
/// use mote_permissions::Permission;
///
/// // Implicit resource — all requests
/// let p: Permission = "net:intercept_request".parse().unwrap();
/// assert_eq!(p.domain(), "net");
/// assert_eq!(p.action(), "intercept_request");
/// assert!(p.resource().is_none());
///
/// // Explicit origin scope
/// let p: Permission = "page:inject_script:https://*.1password.com/*".parse().unwrap();
/// assert_eq!(p.domain(), "page");
/// assert_eq!(p.action(), "inject_script");
/// assert_eq!(p.resource().unwrap().to_string(), "https://*.1password.com/*");
///
/// // Deny / negation
/// let p: Permission = "net:intercept_request:!*.banking.com".parse().unwrap();
/// assert!(p.resource().unwrap().is_negated());
///
/// // Dynamic-resource forms
/// let p: Permission = "mcp:client:my-mcp-server".parse().unwrap();
/// let p: Permission = "secret:read:anthropic_api_key".parse().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Permission {
    domain: String,
    action: String,
    /// `None` means "all resources" (equivalent to `*`).
    resource: Option<Glob>,
}

impl Permission {
    /// Returns the domain segment (e.g. `"net"`, `"page"`, `"mcp"`).
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Returns the action segment (e.g. `"intercept_request"`, `"client"`).
    #[must_use]
    pub fn action(&self) -> &str {
        &self.action
    }

    /// Returns the optional resource glob.
    ///
    /// `None` means all resources are in scope (equivalent to `*`).
    #[must_use]
    pub const fn resource(&self) -> Option<&Glob> {
        self.resource.as_ref()
    }

    /// Returns `true` if this is a deny (negated-resource) permission.
    ///
    /// A permission with no resource is never negated (it is an allow-all).
    #[must_use]
    pub fn is_deny(&self) -> bool {
        self.resource.as_ref().is_some_and(Glob::is_negated)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.domain, self.action)?;
        if let Some(res) = &self.resource {
            write!(f, ":{res}")?;
        }
        Ok(())
    }
}

impl FromStr for Permission {
    type Err = PermissionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on the first two `:` only; everything after the second belongs
        // to the resource glob (which may itself contain `:`).
        let mut parts = s.splitn(3, ':');

        let domain_raw = parts.next().unwrap_or("");
        let action_raw = parts
            .next()
            .ok_or_else(|| PermissionParseError::MissingSeparator {
                input: s.to_owned(),
            })?;
        let resource_raw = parts.next(); // may be absent

        if domain_raw.is_empty() {
            return Err(PermissionParseError::EmptyDomain {
                input: s.to_owned(),
            });
        }
        if action_raw.is_empty() {
            return Err(PermissionParseError::EmptyAction {
                input: s.to_owned(),
            });
        }

        validate_identifier(domain_raw, "domain", s)?;
        validate_identifier(action_raw, "action", s)?;

        let resource = resource_raw
            .map(str::parse::<Glob>)
            .transpose()
            .map_err(|source| PermissionParseError::InvalidResourceGlob {
                input: s.to_owned(),
                source,
            })?;

        Ok(Self {
            domain: domain_raw.to_owned(),
            action: action_raw.to_owned(),
            resource,
        })
    }
}

/// Validates that `seg` is a lowercase ASCII identifier `[a-z][a-z0-9_]*`.
fn validate_identifier(
    seg: &str,
    which: &'static str,
    full_input: &str,
) -> Result<(), PermissionParseError> {
    let mut chars = seg.chars();
    let first = chars.next().unwrap(); // caller ensures non-empty
    if !first.is_ascii_lowercase() {
        return Err(PermissionParseError::InvalidIdentifier {
            input: full_input.to_owned(),
            segment: which,
            reason: format!("must start with a lowercase ASCII letter, got {first:?}"),
        });
    }
    for ch in chars {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '_') {
            return Err(PermissionParseError::InvalidIdentifier {
                input: full_input.to_owned(),
                segment: which,
                reason: format!(
                    "contains invalid character {ch:?}: only [a-z0-9_] allowed after the first"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parsing happy paths ------------------------------------------------

    #[test]
    fn parse_two_segment_no_resource() {
        let p: Permission = "net:intercept_request".parse().unwrap();
        assert_eq!(p.domain(), "net");
        assert_eq!(p.action(), "intercept_request");
        assert!(p.resource().is_none());
        assert!(!p.is_deny());
    }

    #[test]
    fn parse_explicit_wildcard_resource() {
        let p: Permission = "net:intercept_request:*".parse().unwrap();
        assert_eq!(p.domain(), "net");
        assert_eq!(p.action(), "intercept_request");
        assert_eq!(p.resource().unwrap().to_string(), "*");
        assert!(!p.is_deny());
    }

    #[test]
    fn parse_url_resource() {
        let p: Permission = "page:inject_script:https://*.1password.com/*"
            .parse()
            .unwrap();
        assert_eq!(p.domain(), "page");
        assert_eq!(p.action(), "inject_script");
        assert_eq!(
            p.resource().unwrap().to_string(),
            "https://*.1password.com/*"
        );
        assert!(!p.is_deny());
    }

    #[test]
    fn parse_wss_resource() {
        let p: Permission = "http:fetch:wss://localhost:6263".parse().unwrap();
        assert_eq!(p.domain(), "http");
        assert_eq!(p.action(), "fetch");
        // resource contains a colon — everything after second colon is the glob
        assert_eq!(p.resource().unwrap().to_string(), "wss://localhost:6263");
    }

    #[test]
    fn parse_negated_resource() {
        let p: Permission = "net:intercept_request:!*.banking.com".parse().unwrap();
        assert!(p.resource().unwrap().is_negated());
        assert!(p.is_deny());
        assert_eq!(p.resource().unwrap().to_string(), "!*.banking.com");
    }

    #[test]
    fn parse_mcp_dynamic_resource() {
        let p: Permission = "mcp:client:my-mcp-server".parse().unwrap();
        assert_eq!(p.domain(), "mcp");
        assert_eq!(p.action(), "client");
        assert_eq!(p.resource().unwrap().to_string(), "my-mcp-server");
    }

    #[test]
    fn parse_secret_dynamic_resource() {
        let p: Permission = "secret:read:anthropic_api_key".parse().unwrap();
        assert_eq!(p.domain(), "secret");
        assert_eq!(p.action(), "read");
        assert_eq!(p.resource().unwrap().to_string(), "anthropic_api_key");
    }

    #[test]
    fn parse_storage_persistent() {
        let p: Permission = "storage:persistent".parse().unwrap();
        assert_eq!(p.domain(), "storage");
        assert_eq!(p.action(), "persistent");
        assert!(p.resource().is_none());
    }

    #[test]
    fn parse_crypto_seal() {
        let p: Permission = "crypto:seal_to_plugin".parse().unwrap();
        assert_eq!(p.domain(), "crypto");
        assert_eq!(p.action(), "seal_to_plugin");
    }

    #[test]
    fn display_round_trip_no_resource() {
        let original = "net:intercept_request";
        let p: Permission = original.parse().unwrap();
        assert_eq!(p.to_string(), original);
    }

    #[test]
    fn display_round_trip_with_resource() {
        let original = "page:inject_script:https://*.1password.com/*";
        let p: Permission = original.parse().unwrap();
        assert_eq!(p.to_string(), original);
    }

    #[test]
    fn display_round_trip_negated() {
        let original = "net:intercept_request:!*.banking.com";
        let p: Permission = original.parse().unwrap();
        assert_eq!(p.to_string(), original);
    }

    // --- parsing error paths ------------------------------------------------

    #[test]
    fn parse_missing_separator() {
        let e = "nodomain".parse::<Permission>().unwrap_err();
        assert!(matches!(e, PermissionParseError::MissingSeparator { .. }));
    }

    #[test]
    fn parse_empty_domain() {
        let e = ":action".parse::<Permission>().unwrap_err();
        assert!(matches!(e, PermissionParseError::EmptyDomain { .. }));
    }

    #[test]
    fn parse_empty_action() {
        let e = "net:".parse::<Permission>().unwrap_err();
        assert!(matches!(e, PermissionParseError::EmptyAction { .. }));
    }

    #[test]
    fn parse_domain_starts_with_digit() {
        let e = "1net:intercept".parse::<Permission>().unwrap_err();
        assert!(matches!(
            e,
            PermissionParseError::InvalidIdentifier {
                segment: "domain",
                ..
            }
        ));
    }

    #[test]
    fn parse_action_invalid_char() {
        let e = "net:intercept-request".parse::<Permission>().unwrap_err();
        assert!(matches!(
            e,
            PermissionParseError::InvalidIdentifier {
                segment: "action",
                ..
            }
        ));
    }

    #[test]
    fn parse_uppercase_domain_rejected() {
        let e = "Net:intercept_request".parse::<Permission>().unwrap_err();
        assert!(matches!(
            e,
            PermissionParseError::InvalidIdentifier {
                segment: "domain",
                ..
            }
        ));
    }

    #[test]
    fn parse_empty_resource_glob() {
        // Two colons but nothing after the second — the empty string is an
        // invalid glob per mote-types.
        let e = "net:intercept_request:".parse::<Permission>().unwrap_err();
        assert!(matches!(
            e,
            PermissionParseError::InvalidResourceGlob { .. }
        ));
    }

    // --- known DESIGN examples parse clean ----------------------------------

    #[test]
    fn design_examples_parse() {
        let examples = [
            "net:intercept_request",
            "storage:persistent",
            "net:intercept_request:*",
            "page:inject_script:*",
            "page:inject_script:https://*.1password.com/*",
            "http:fetch:https://api.bitwarden.com/*",
            "http:fetch:wss://localhost:6263",
            "net:intercept_request:!*.banking.com",
            "mcp:client:my-server",
            "secret:read:anthropic_api_key",
            "identity:read_current",
            "crypto:seal_to_plugin",
        ];
        for ex in examples {
            assert!(ex.parse::<Permission>().is_ok(), "failed to parse {ex:?}");
        }
    }
}
