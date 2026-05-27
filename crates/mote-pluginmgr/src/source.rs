//! Plugin [`Source`] types and the `plugins.lua` source-string grammar.
//!
//! A source string is the right-hand side of a `plugins.lua` entry's `source`
//! field (DESIGN §Supported sources). Mote v0.1 understands four forms:
//!
//! | Source        | Syntax                       |
//! |---------------|------------------------------|
//! | GitHub        | `github:<owner>/<repo>`      |
//! | Generic Git   | `git+https://…`              |
//! | Local path    | `path:<local-path>`          |
//! | Bundled       | `bundled`                    |
//!
//! `github:` is sugar over a `git+https://github.com/<owner>/<repo>.git` clone;
//! the [`Source`] keeps the two distinct so the original intent round-trips
//! through [`Display`](std::fmt::Display) into `plugins.lock`.
//!
//! The `bundled:<name>` form (an externally-declared bundle) is reserved for
//! v0.2+ (DESIGN §Supported sources); v0.1 accepts only the bare `bundled`
//! keyword, which resolves to the binary-embedded first-party bundle.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

/// Where a plugin's code comes from.
///
/// Parsed from a `plugins.lua` `source = "…"` string with
/// [`Source::from_str`] and rendered back with [`Display`](std::fmt::Display).
/// The round-trip is stable: `Source::from_str(&s.to_string()) == Ok(s)` for
/// every value this type can produce.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// GitHub shorthand: `github:<owner>/<repo>`.
    Github {
        /// The repository owner (user or organisation).
        owner: String,
        /// The repository name.
        repo: String,
    },
    /// A generic Git URL: `git+https://…`. The stored string is the URL
    /// **without** the `git+` prefix (the prefix is grammar, not part of the
    /// clone URL gix receives).
    Git {
        /// The clone URL, e.g. `https://example.com/team/plugin.git`.
        url: String,
    },
    /// A local directory: `path:<local-path>`. The path is stored verbatim
    /// (including a leading `~`); tilde-expansion and canonicalisation happen
    /// at resolve time, not parse time, so the lock file records what the user
    /// wrote.
    Path(PathBuf),
    /// The binary-embedded first-party bundle (`bundled`).
    Bundled,
}

/// The `github:` prefix.
const GITHUB_PREFIX: &str = "github:";
/// The `git+` prefix that marks a generic Git URL.
const GIT_PREFIX: &str = "git+";
/// The `path:` prefix.
const PATH_PREFIX: &str = "path:";
/// The bare `bundled` keyword.
const BUNDLED: &str = "bundled";

impl Source {
    /// Returns the `https://…` clone URL for Git-backed sources.
    ///
    /// [`Source::Github`] expands to its canonical GitHub HTTPS clone URL;
    /// [`Source::Git`] returns its stored URL. Returns `None` for
    /// [`Source::Path`] and [`Source::Bundled`], which are not cloned.
    #[must_use]
    pub fn clone_url(&self) -> Option<String> {
        match self {
            Self::Github { owner, repo } => Some(format!("https://github.com/{owner}/{repo}.git")),
            Self::Git { url } => Some(url.clone()),
            Self::Path(_) | Self::Bundled => None,
        }
    }

    /// Whether this source is fetched over the network (Git-backed).
    ///
    /// `path:` and `bundled` are local and never touch the network.
    #[must_use]
    pub const fn is_git(&self) -> bool {
        matches!(self, Self::Github { .. } | Self::Git { .. })
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Github { owner, repo } => write!(f, "{GITHUB_PREFIX}{owner}/{repo}"),
            Self::Git { url } => write!(f, "{GIT_PREFIX}{url}"),
            Self::Path(path) => write!(f, "{PATH_PREFIX}{}", path.display()),
            Self::Bundled => f.write_str(BUNDLED),
        }
    }
}

/// Error returned when a `plugins.lua` source string is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SourceParseError {
    /// The string matched none of the known prefixes/keywords.
    #[error(
        "unrecognized source {0:?}: expected one of \
         `github:owner/repo`, `git+https://…`, `path:…`, or `bundled`"
    )]
    UnknownScheme(String),
    /// A `github:` source was not exactly `owner/repo`.
    #[error("github source must be `github:owner/repo`, got {0:?}")]
    MalformedGithub(String),
    /// A `git+` source had an empty URL after the prefix.
    #[error("git source must be `git+<url>` with a non-empty URL")]
    EmptyGitUrl,
    /// A `path:` source had an empty path after the prefix.
    #[error("path source must be `path:<local-path>` with a non-empty path")]
    EmptyPath,
    /// The reserved `bundled:<name>` form is not supported in v0.1.
    #[error("external bundles (`bundled:{0}`) are reserved for a future release")]
    NamedBundleUnsupported(String),
}

impl FromStr for Source {
    type Err = SourceParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == BUNDLED {
            return Ok(Self::Bundled);
        }
        if let Some(name) = s.strip_prefix("bundled:") {
            // Reserved grammar; not wired in v0.1.
            return Err(SourceParseError::NamedBundleUnsupported(name.to_owned()));
        }
        if let Some(rest) = s.strip_prefix(GITHUB_PREFIX) {
            return parse_github(rest);
        }
        if let Some(rest) = s.strip_prefix(GIT_PREFIX) {
            if rest.is_empty() {
                return Err(SourceParseError::EmptyGitUrl);
            }
            return Ok(Self::Git {
                url: rest.to_owned(),
            });
        }
        if let Some(rest) = s.strip_prefix(PATH_PREFIX) {
            if rest.is_empty() {
                return Err(SourceParseError::EmptyPath);
            }
            return Ok(Self::Path(PathBuf::from(rest)));
        }
        Err(SourceParseError::UnknownScheme(s.to_owned()))
    }
}

/// Parses the `owner/repo` body of a `github:` source.
fn parse_github(body: &str) -> Result<Source, SourceParseError> {
    let malformed = || SourceParseError::MalformedGithub(body.to_owned());
    let (owner, repo) = body.split_once('/').ok_or_else(malformed)?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(malformed());
    }
    Ok(Source::Github {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github() {
        let s: Source = "github:mote-browser/adblock".parse().unwrap();
        assert_eq!(
            s,
            Source::Github {
                owner: "mote-browser".into(),
                repo: "adblock".into(),
            }
        );
        assert_eq!(
            s.clone_url().unwrap(),
            "https://github.com/mote-browser/adblock.git"
        );
        assert!(s.is_git());
    }

    #[test]
    fn parses_generic_git() {
        let s: Source = "git+https://example.com/team/plugin.git".parse().unwrap();
        assert_eq!(
            s,
            Source::Git {
                url: "https://example.com/team/plugin.git".into(),
            }
        );
        assert_eq!(
            s.clone_url().unwrap(),
            "https://example.com/team/plugin.git"
        );
    }

    #[test]
    fn parses_path() {
        let s: Source = "path:~/code/my-plugin".parse().unwrap();
        assert_eq!(s, Source::Path(PathBuf::from("~/code/my-plugin")));
        assert!(s.clone_url().is_none());
        assert!(!s.is_git());
    }

    #[test]
    fn parses_bundled() {
        let s: Source = "bundled".parse().unwrap();
        assert_eq!(s, Source::Bundled);
        assert!(s.clone_url().is_none());
        assert!(!s.is_git());
    }

    #[test]
    fn display_round_trips_every_form() {
        for original in [
            "github:owner/repo",
            "git+https://example.com/a.git",
            "path:/abs/path",
            "path:~/rel",
            "bundled",
        ] {
            let parsed: Source = original.parse().unwrap();
            let rendered = parsed.to_string();
            assert_eq!(rendered, original, "round-trip failed for {original:?}");
            // And the rendered string re-parses to the same value.
            assert_eq!(rendered.parse::<Source>().unwrap(), parsed);
        }
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(matches!(
            "ftp://nope".parse::<Source>(),
            Err(SourceParseError::UnknownScheme(_))
        ));
        assert!(matches!(
            "".parse::<Source>(),
            Err(SourceParseError::UnknownScheme(_))
        ));
    }

    #[test]
    fn rejects_malformed_github() {
        for bad in [
            "github:noslash",
            "github:/repo",
            "github:owner/",
            "github:a/b/c",
        ] {
            assert!(
                matches!(
                    bad.parse::<Source>(),
                    Err(SourceParseError::MalformedGithub(_))
                ),
                "{bad:?} should be MalformedGithub"
            );
        }
    }

    #[test]
    fn rejects_empty_git_and_path() {
        assert_eq!("git+".parse::<Source>(), Err(SourceParseError::EmptyGitUrl));
        assert_eq!("path:".parse::<Source>(), Err(SourceParseError::EmptyPath));
    }

    #[test]
    fn rejects_named_bundle() {
        assert!(matches!(
            "bundled:extra".parse::<Source>(),
            Err(SourceParseError::NamedBundleUnsupported(_))
        ));
    }
}
