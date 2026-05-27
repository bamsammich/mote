//! THROWAWAY spike: can gix 0.84 fetch a specific commit from a (local) git URL
//! and materialize that commit's working tree into an arbitrary directory,
//! without a system-git dependency?
//!
//! Strategy under test (the production plan's contract):
//!   1. create a fixture repo on disk with N commits
//!   2. point a "URL" at it (file:// path)
//!   3. clone/fetch into a fresh temp dir
//!   4. check out the working tree AT A KNOWN OLDER COMMIT (not just HEAD)
//!   5. assert the tree contents match that commit, and we got the SHA back

use std::fs;
use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main() {
    let tmp = std::env::temp_dir().join(format!("gix-spike-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let fixture = tmp.join("fixture");
    fs::create_dir_all(&fixture).unwrap();

    // Build a fixture repo with two commits. We use the system git ONLY to
    // build the fixture (production builds it with gix in tests, or ships a
    // tarball); the point of the spike is the FETCH side, done by gix.
    run(&fixture, &["init", "-q", "-b", "main"]);
    fs::write(fixture.join("init.lua"), b"-- v1\n").unwrap();
    run(&fixture, &["add", "."]);
    run(&fixture, &["commit", "-q", "-m", "v1"]);
    let commit1 = run(&fixture, &["rev-parse", "HEAD"]);

    fs::write(fixture.join("init.lua"), b"-- v2\n").unwrap();
    fs::write(fixture.join("extra.txt"), b"new file\n").unwrap();
    run(&fixture, &["add", "."]);
    run(&fixture, &["commit", "-q", "-m", "v2"]);
    let commit2 = run(&fixture, &["rev-parse", "HEAD"]);

    println!("fixture: commit1={commit1} commit2={commit2}");

    let url = format!("file://{}", fixture.display());
    let dest = tmp.join("checkout");

    // ---- gix: clone + checkout at commit1 (the OLDER commit, not HEAD) ----
    match fetch_at_commit(&url, &commit1, &dest) {
        Ok(sha) => {
            let contents = fs::read_to_string(dest.join("init.lua")).unwrap();
            let has_extra = dest.join("extra.txt").exists();
            println!("VERDICT: gix fetch-at-commit OK");
            println!("  returned sha: {sha}");
            println!("  init.lua = {contents:?} (expect '-- v1')");
            println!("  extra.txt present = {has_extra} (expect false at commit1)");
            assert_eq!(sha, commit1, "returned sha must equal requested commit");
            assert_eq!(contents.trim(), "-- v1", "must be v1 tree, not HEAD");
            assert!(!has_extra, "extra.txt must NOT exist at commit1");
            println!("ALL ASSERTIONS PASSED");
        }
        Err(e) => {
            println!("VERDICT: gix fetch-at-commit FAILED: {e:#}");
            std::process::exit(1);
        }
    }
    let _ = fs::remove_dir_all(&tmp);
}

/// Fetch `commit` from `url` and check out its working tree into `dest`.
/// Returns the resolved commit SHA on success.
fn fetch_at_commit(url: &str, commit: &str, dest: &Path) -> Result<String, Box<dyn std::error::Error>> {
    fs::create_dir_all(dest)?;

    // prepare_clone sets up the remote; we then fetch and check out.
    let mut prepare = gix::prepare_clone(url, dest)?;

    // Fetch refs+objects (blocking transport). For a local file:// remote this
    // copies the relevant pack. shallow could be added but local is cheap.
    let (mut checkout, _outcome) = prepare
        .fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)?;

    // checkout currently points at HEAD. We need the tree AT `commit`.
    // Resolve the requested commit object, then detach HEAD onto it so the
    // worktree checkout materializes THAT commit's tree, not the fetched HEAD.
    let oid = gix::ObjectId::from_hex(commit.as_bytes())?;
    {
        let repo = checkout.repo();
        // Validate the object exists and is a commit before we point HEAD at it.
        let object = repo.find_object(oid)?;
        let _commit_obj = object.try_into_commit()?;

        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange::default(),
                expected: gix::refs::transaction::PreviousValue::Any,
                new: gix::refs::Target::Object(oid),
            },
            name: "HEAD".try_into()?,
            deref: false,
        })?;
    }

    let (_outcome, _) = checkout.main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)?;

    Ok(oid.to_string())
}
