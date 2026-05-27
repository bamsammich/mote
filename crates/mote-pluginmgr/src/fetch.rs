//! Git fetching via [`gix`] — the pure-Rust git client (DESIGN §Implementation
//! Language: no libgit2 FFI, matching `unsafe_code = "deny"`).
//!
//! The fetch contract (DESIGN §Fetching): given a [`Source`] and an optional
//! version (a commit SHA, tag, or branch), produce the working tree at that
//! commit in a directory plus the resolved commit SHA. The caller hashes the
//! tree and moves it into the cache under `<name>/<sha>/`.
//!
//! # gix capability (spike result)
//!
//! gix 0.84 supports the contract cleanly: `gix::prepare_clone(url, dest)`
//! followed by `fetch_then_checkout` populates the object database and a
//! working tree, and the working tree can then be re-materialised **at an
//! arbitrary commit** (not just the fetched HEAD) by detaching `HEAD` onto the
//! requested object and re-running `main_worktree`. This was verified against a
//! local fixture repo: fetching an older commit yields that commit's tree, not
//! HEAD's. The one build-time gotcha is that gix's `sha1` feature must be
//! enabled or `gix-hash` fails to compile under `default-features = false`.
//!
//! Network failures (offline, host down) are recoverable [`FetchError`]s, never
//! panics — `sync`/`update` report per-plugin failures and leave the existing
//! cache untouched (DESIGN §Offline behaviour).

use std::path::Path;

use thiserror::Error;

use crate::source::Source;

/// The outcome of a successful fetch: the resolved commit and its working tree.
#[derive(Debug)]
pub struct Fetched {
    /// The resolved commit SHA (40 lowercase hex chars).
    pub commit: String,
    /// A temporary directory holding the working tree at `commit`. The caller
    /// hashes it and moves it into the cache; dropping it cleans up.
    pub tree: tempfile::TempDir,
}

/// Error returned while fetching a Git source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FetchError {
    /// The source is not Git-backed (`path:`/`bundled` are not fetched here).
    #[error("source {0} is not a git source and cannot be fetched")]
    NotGit(Source),
    /// A temporary working directory could not be created.
    #[error("could not create a temporary fetch directory: {0}")]
    TempDir(#[source] std::io::Error),
    /// Setting up the clone (remote/url parsing) failed.
    #[error("failed to prepare clone of {url}: {source}")]
    Prepare {
        /// The clone URL.
        url: String,
        /// The underlying gix error, rendered.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The network fetch failed (offline, host down, auth). Recoverable.
    #[error("failed to fetch {url}: {source}")]
    Network {
        /// The clone URL.
        url: String,
        /// The underlying gix error, rendered.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The requested version/commit could not be resolved in the fetched repo.
    #[error("could not resolve version {version:?} in {url}: {source}")]
    Resolve {
        /// The clone URL.
        url: String,
        /// The requested version (commit/tag/branch).
        version: String,
        /// The underlying gix error, rendered.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Checking out the working tree at the resolved commit failed.
    #[error("failed to check out {commit} from {url}: {source}")]
    Checkout {
        /// The clone URL.
        url: String,
        /// The resolved commit.
        commit: String,
        /// The underlying gix error, rendered.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Fetches a Git [`Source`] at `version` into a temporary working tree.
///
/// `version` is a commit SHA, tag, or branch name; `None` resolves the remote's
/// default-branch HEAD. Returns the resolved commit SHA and the materialised
/// working tree (DESIGN §Fetching).
///
/// # Errors
///
/// Returns [`FetchError::NotGit`] for non-Git sources, and the network/resolve/
/// checkout variants on the corresponding failure. All are recoverable: the
/// caller's cache and links are left untouched.
pub fn fetch(source: &Source, version: Option<&str>) -> Result<Fetched, FetchError> {
    let url = source
        .clone_url()
        .ok_or_else(|| FetchError::NotGit(source.clone()))?;

    let tmp = tempfile::tempdir().map_err(FetchError::TempDir)?;
    let dest = tmp.path().join("checkout");
    std::fs::create_dir_all(&dest).map_err(FetchError::TempDir)?;

    let commit = fetch_into(&url, version, &dest)?;
    Ok(Fetched { commit, tree: tmp })
}

/// Performs the gix clone + checkout-at-commit into `dest`, returning the SHA.
fn fetch_into(url: &str, version: Option<&str>, dest: &Path) -> Result<String, FetchError> {
    let boxed = |e: gix::clone::Error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) };

    let mut prepare = gix::prepare_clone(url, dest).map_err(|source| FetchError::Prepare {
        url: url.to_owned(),
        source: boxed(source),
    })?;

    let (mut checkout, _outcome) = prepare
        .fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|source| FetchError::Network {
            url: url.to_owned(),
            source: Box::new(source),
        })?;

    // Resolve the requested version (or HEAD) to a concrete commit, detach HEAD
    // onto it, then re-materialise the working tree at THAT commit.
    let oid = {
        let repo = checkout.repo();
        let oid = resolve_commit(repo, url, version)?;
        let edit = gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange::default(),
                expected: gix::refs::transaction::PreviousValue::Any,
                new: gix::refs::Target::Object(oid),
            },
            name: head_name(url)?,
            deref: false,
        };
        repo.edit_reference(edit)
            .map_err(|source| FetchError::Checkout {
                url: url.to_owned(),
                commit: oid.to_string(),
                source: Box::new(source),
            })?;
        oid
    };

    checkout
        .main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|source| FetchError::Checkout {
            url: url.to_owned(),
            commit: oid.to_string(),
            source: Box::new(source),
        })?;

    Ok(oid.to_string())
}

/// Resolves `version` (or HEAD when `None`) to a commit object id.
fn resolve_commit(
    repo: &gix::Repository,
    url: &str,
    version: Option<&str>,
) -> Result<gix::ObjectId, FetchError> {
    let resolve_err = |source: Box<dyn std::error::Error + Send + Sync>| FetchError::Resolve {
        url: url.to_owned(),
        version: version.unwrap_or("HEAD").to_owned(),
        source,
    };

    let spec = version.unwrap_or("HEAD");
    let object = repo
        .rev_parse_single(spec)
        .map_err(|e| resolve_err(Box::new(e)))?
        .object()
        .map_err(|e| resolve_err(Box::new(e)))?;
    // Peel tags/etc. down to the commit, then its id.
    let commit = object
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|e| resolve_err(Box::new(e)))?;
    Ok(commit.id)
}

/// Returns a `HEAD` reference name for the ref transaction.
fn head_name(url: &str) -> Result<gix::refs::FullName, FetchError> {
    "HEAD"
        .try_into()
        .map_err(|e: gix::refs::name::Error| FetchError::Checkout {
            url: url.to_owned(),
            commit: "HEAD".to_owned(),
            source: Box::new(e),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    /// Builds a fixture git repo on disk with two commits, returning
    /// `(repo_dir, commit1, commit2)`. Uses system git ONLY to construct the
    /// fixture; the production fetch path is gix.
    fn fixture_repo(dir: &Path) -> (String, String) {
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .expect("git available");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("init.lua"), b"-- v1\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "v1"]);
        let c1 = git(&["rev-parse", "HEAD"]);

        std::fs::write(dir.join("init.lua"), b"-- v2\n").unwrap();
        std::fs::write(dir.join("extra.txt"), b"new\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "v2"]);
        let c2 = git(&["rev-parse", "HEAD"]);

        (c1, c2)
    }

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn file_url(repo: &Path) -> Source {
        Source::Git {
            url: format!("file://{}", repo.display()),
        }
    }

    #[test]
    fn fetch_at_head_returns_latest_tree() {
        if !git_available() {
            eprintln!("skipping: system git unavailable for fixture construction");
            return;
        }
        let repo = tempfile::tempdir().unwrap();
        let (_c1, c2) = fixture_repo(repo.path());

        let fetched = fetch(&file_url(repo.path()), None).unwrap();
        assert_eq!(fetched.commit, c2, "HEAD resolves to the latest commit");
        let init = fetched.tree.path().join("checkout").join("init.lua");
        assert_eq!(std::fs::read_to_string(init).unwrap().trim(), "-- v2");
        assert!(
            fetched
                .tree
                .path()
                .join("checkout")
                .join("extra.txt")
                .exists(),
            "HEAD tree includes the v2 file"
        );
    }

    #[test]
    fn fetch_at_specific_commit_yields_that_tree() {
        if !git_available() {
            eprintln!("skipping: system git unavailable for fixture construction");
            return;
        }
        let repo = tempfile::tempdir().unwrap();
        let (c1, _c2) = fixture_repo(repo.path());

        // Fetch the OLDER commit explicitly — proves fetch-at-commit, not HEAD.
        let fetched = fetch(&file_url(repo.path()), Some(&c1)).unwrap();
        assert_eq!(fetched.commit, c1);
        let checkout = fetched.tree.path().join("checkout");
        assert_eq!(
            std::fs::read_to_string(checkout.join("init.lua"))
                .unwrap()
                .trim(),
            "-- v1",
            "must be the v1 tree, not HEAD"
        );
        assert!(
            !checkout.join("extra.txt").exists(),
            "extra.txt must NOT exist at the older commit"
        );
    }

    #[test]
    fn fetch_non_git_source_errors() {
        let err = fetch(&Source::Bundled, None).unwrap_err();
        assert!(matches!(err, FetchError::NotGit(_)));
        let err = fetch(&Source::Path(PathBuf::from("/tmp")), None).unwrap_err();
        assert!(matches!(err, FetchError::NotGit(_)));
    }

    #[test]
    fn fetch_offline_url_is_recoverable_error() {
        // A bogus file:// path: gix prepare/fetch fails; we must return an
        // error (not panic), proving the recoverable-error posture.
        let src = Source::Git {
            url: "file:///nonexistent/repo/that/does/not/exist".to_owned(),
        };
        let result = fetch(&src, None);
        assert!(
            result.is_err(),
            "fetching a missing repo must error, not panic"
        );
    }
}
