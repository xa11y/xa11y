//! `cargo xtask check-versions` — every place the release version is written
//! must agree, and (with `--expect`) must agree with the tag being cut.
//!
//! The release is one version (`release.toml`: `shared-version = true`), but
//! it is spelled in more places than `cargo-release` knows about. The two
//! bindings are excluded from the Cargo workspace, so their `Cargo.toml`,
//! `package.json` / `package-lock.json`, and their own `Cargo.lock` files are
//! invisible to it. The publish workflow used to sync those in follow-up
//! commits *after* `cargo-release` had already tagged and pushed, and never
//! refreshed the out-of-workspace lockfiles at all — so `v0.15.2` pointed at a
//! tree whose `xa11y-js` said `0.15.1`, and whose `xa11y-js/Cargo.lock` still
//! recorded `xa11y-core 0.15.0`, which makes `cargo build --locked` fail from
//! the tag (issue #463).
//!
//! The checks:
//!
//! * `[workspace.package].version` is the release version `V`.
//! * Every workspace member either inherits it (`version.workspace = true`)
//!   or spells `V` literally (`cargo-release` rewrites those in place).
//! * `xa11y-python/Cargo.toml` and `xa11y-js/Cargo.toml` say `V`.
//! * `xa11y-python/pyproject.toml` takes its version from `Cargo.toml`
//!   (`dynamic = ["version"]`) or says `V`.
//! * `xa11y-js/package.json` and both version fields of
//!   `xa11y-js/package-lock.json` say `V`, as does any committed
//!   `@crowecawcaw/xa11y-*` `optionalDependencies` pin.
//! * In every `Cargo.lock` in the repository, each path package (no `source`)
//!   that is one of the crates above records `V`. This is what `--locked`
//!   checks, and the one nothing else here would catch.
//! * With `--expect X.Y.Z` (or `vX.Y.Z`), `V` is that version.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Crates outside the Cargo workspace that release on the shared version.
const BINDING_CRATES: &[&str] = &["xa11y-python", "xa11y-js"];

/// npm package whose version, and whose platform-package pins, follow `V`.
const JS_DIR: &str = "xa11y-js";
const JS_PLATFORM_PREFIX: &str = "@crowecawcaw/xa11y-";

/// Result of a check: the version found, what was verified, what is wrong.
#[derive(Debug, Default)]
pub struct Report {
    pub version: String,
    pub ok_lines: Vec<String>,
    pub problems: Vec<String>,
}

pub fn check_cmd(root: &Path, args: &[String]) -> bool {
    let mut expect: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--expect" => match it.next() {
                Some(v) => expect = Some(v.clone()),
                None => {
                    eprintln!("!! --expect needs a version, e.g. --expect 0.16.0");
                    return false;
                }
            },
            other => {
                eprintln!("!! unknown argument to check-versions: {other}");
                return false;
            }
        }
    }

    let report = check(root, expect.as_deref());
    for line in &report.ok_lines {
        println!("  {line}");
    }
    for p in &report.problems {
        eprintln!("!! {p}");
    }
    if report.problems.is_empty() {
        println!("  all version fields agree on {}", report.version);
        return true;
    }
    if report.version.is_empty() {
        eprintln!("\n!! could not determine the release version; see above.");
        return false;
    }
    eprintln!(
        "\n!! {} version field(s) disagree with the release version \"{}\".\n   \
         A tag cut from this tree would not build what was published \
         (`cargo build --locked` fails on a stale lockfile).\n   \
         Fix: .github/scripts/sync_release_versions.sh {}",
        report.problems.len(),
        report.version,
        report.version,
    );
    false
}

/// Run every check against the tree at `root`.
pub fn check(root: &Path, expect: Option<&str>) -> Report {
    let mut r = Report::default();

    let ws_path = root.join("Cargo.toml");
    let Some(ws) = read_toml(&ws_path, &mut r) else {
        return r;
    };
    let Some(version) = ws
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    else {
        r.problems
            .push("Cargo.toml: [workspace.package] has no version".into());
        return r;
    };
    r.version = version.to_string();
    r.ok_lines
        .push(format!("Cargo.toml [workspace.package] = \"{version}\""));

    if let Some(expect) = expect {
        let expect = expect.strip_prefix('v').unwrap_or(expect);
        if expect == version {
            r.ok_lines.push(format!("matches expected tag v{expect}"));
        } else {
            r.problems.push(format!(
                "release version is \"{version}\" but the tag being cut is v{expect}"
            ));
        }
    }

    // Names of every crate that releases on `version`; lockfile entries for
    // them must match.
    let mut shared: BTreeSet<String> = BTreeSet::new();

    let members: Vec<String> = ws
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for member in &members {
        if member.contains('*') || member.contains('?') {
            r.problems.push(format!(
                "Cargo.toml: workspace member glob \"{member}\" is not supported by this check; \
                 list members explicitly or teach xtask/src/versions.rs to expand it"
            ));
            continue;
        }
        let path = root.join(member).join("Cargo.toml");
        let rel = format!("{member}/Cargo.toml");
        if let Some(name) = check_manifest(&path, &rel, version, true, &mut r) {
            shared.insert(name);
        }
    }

    for krate in BINDING_CRATES {
        let rel = format!("{krate}/Cargo.toml");
        if let Some(name) = check_manifest(&root.join(&rel), &rel, version, false, &mut r) {
            shared.insert(name);
        }
    }

    check_pyproject(root, version, &mut r);
    check_package_json(root, version, &mut r);

    let mut locks = Vec::new();
    find_lockfiles(root, &mut locks);
    locks.sort();
    for lock in &locks {
        check_lockfile(root, lock, version, &shared, &mut r);
    }
    r
}

/// Check one `[package]` manifest. Returns its package name when it releases
/// on the shared version.
fn check_manifest(
    path: &Path,
    rel: &str,
    version: &str,
    in_workspace: bool,
    r: &mut Report,
) -> Option<String> {
    let doc = read_toml(path, r)?;
    let Some(pkg) = doc.get("package") else {
        r.problems.push(format!("{rel}: no [package] table"));
        return None;
    };
    let Some(name) = pkg.get("name").and_then(|n| n.as_str()) else {
        r.problems.push(format!("{rel}: [package] has no name"));
        return None;
    };
    match pkg.get("version") {
        Some(toml::Value::Table(t))
            if in_workspace && t.get("workspace").and_then(|w| w.as_bool()) == Some(true) =>
        {
            r.ok_lines
                .push(format!("{rel}: {name} inherits the workspace version"));
        }
        Some(toml::Value::String(v)) if v == version => {
            r.ok_lines.push(format!("{rel}: {name} = \"{v}\""));
        }
        Some(toml::Value::String(v)) => {
            r.problems.push(format!(
                "{rel}: {name} is version \"{v}\", expected \"{version}\""
            ));
        }
        Some(other) => {
            r.problems.push(format!(
                "{rel}: {name} has an unrecognised version field `{other:?}`, expected \"{version}\""
            ));
        }
        None => {
            r.problems.push(format!(
                "{rel}: {name} has no version (cargo would default it to 0.0.0), expected \"{version}\""
            ));
        }
    }
    Some(name.to_string())
}

fn check_pyproject(root: &Path, version: &str, r: &mut Report) {
    let rel = "xa11y-python/pyproject.toml";
    let Some(doc) = read_toml(&root.join(rel), r) else {
        return;
    };
    let Some(project) = doc.get("project") else {
        r.problems.push(format!("{rel}: no [project] table"));
        return;
    };
    let dynamic = project
        .get("dynamic")
        .and_then(|d| d.as_array())
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("version")));
    match project.get("version").and_then(|v| v.as_str()) {
        Some(v) if v == version => r.ok_lines.push(format!("{rel}: version = \"{v}\"")),
        Some(v) => r
            .problems
            .push(format!("{rel}: version is \"{v}\", expected \"{version}\"")),
        None if dynamic => r.ok_lines.push(format!(
            "{rel}: version is dynamic (maturin reads xa11y-python/Cargo.toml)"
        )),
        None => r.problems.push(format!(
            "{rel}: [project] has neither a version nor dynamic = [\"version\"]"
        )),
    }
}

fn check_package_json(root: &Path, version: &str, r: &mut Report) {
    let rel = format!("{JS_DIR}/package.json");
    if let Some(pkg) = read_json(&root.join(&rel), r) {
        expect_json_str(&pkg, &["version"], &rel, version, r);
        if let Some(deps) = pkg.get("optionalDependencies").and_then(|d| d.as_object()) {
            for name in deps.keys().filter(|n| n.starts_with(JS_PLATFORM_PREFIX)) {
                expect_json_str(&pkg, &["optionalDependencies", name], &rel, version, r);
            }
        }
    }

    let rel = format!("{JS_DIR}/package-lock.json");
    if let Some(lock) = read_json(&root.join(&rel), r) {
        expect_json_str(&lock, &["version"], &rel, version, r);
        expect_json_str(&lock, &["packages", "", "version"], &rel, version, r);
    }
}

fn expect_json_str(
    doc: &serde_json::Value,
    path: &[&str],
    rel: &str,
    version: &str,
    r: &mut Report,
) {
    let mut cur = doc;
    for key in path {
        match cur.get(*key) {
            Some(v) => cur = v,
            None => {
                r.problems.push(format!(
                    "{rel}: missing {}, expected \"{version}\"",
                    json_path(path)
                ));
                return;
            }
        }
    }
    match cur.as_str() {
        Some(v) if v == version => r
            .ok_lines
            .push(format!("{rel}: {} = \"{v}\"", json_path(path))),
        Some(v) => r.problems.push(format!(
            "{rel}: {} is \"{v}\", expected \"{version}\"",
            json_path(path)
        )),
        None => r.problems.push(format!(
            "{rel}: {} is not a string, expected \"{version}\"",
            json_path(path)
        )),
    }
}

fn json_path(path: &[&str]) -> String {
    path.iter()
        .map(|k| format!("[{k:?}]"))
        .collect::<Vec<_>>()
        .join("")
}

fn check_lockfile(
    root: &Path,
    lock: &Path,
    version: &str,
    shared: &BTreeSet<String>,
    r: &mut Report,
) {
    let rel = lock
        .strip_prefix(root)
        .unwrap_or(lock)
        .display()
        .to_string();
    let Some(doc) = read_toml(lock, r) else {
        return;
    };
    let packages = doc
        .get("package")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut checked = 0usize;
    for pkg in &packages {
        // Registry and git packages carry a `source`; only path packages are
        // built from this tree, and only those `--locked` compares against
        // the manifests here.
        if pkg.get("source").is_some() {
            continue;
        }
        let Some(name) = pkg.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if !shared.contains(name) {
            continue;
        }
        checked += 1;
        let found = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("");
        if found != version {
            r.problems.push(format!(
                "{rel}: records {name} {found}, expected {version} \
                 (`cargo build --locked` fails on this lockfile)"
            ));
        }
    }
    if checked > 0 {
        r.ok_lines
            .push(format!("{rel}: {checked} path crate(s) checked"));
    }
}

/// Every `Cargo.lock` under `dir`, skipping build output, dependencies and
/// dot-directories (`.git`, and the agent worktrees under `.claude`).
fn find_lockfiles(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            find_lockfiles(&path, out);
        } else if ft.is_file() && name == "Cargo.lock" {
            out.push(path);
        }
    }
}

fn read_toml(path: &Path, r: &mut Report) -> Option<toml::Value> {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            r.problems
                .push(format!("could not read {}: {e}", path.display()));
            return None;
        }
    };
    match toml::from_str::<toml::Value>(&src) {
        Ok(v) => Some(v),
        Err(e) => {
            r.problems
                .push(format!("could not parse {}: {e}", path.display()));
            None
        }
    }
}

fn read_json(path: &Path, r: &mut Report) -> Option<serde_json::Value> {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            r.problems
                .push(format!("could not read {}: {e}", path.display()));
            return None;
        }
    };
    match serde_json::from_str(&src) {
        Ok(v) => Some(v),
        Err(e) => {
            r.problems
                .push(format!("could not parse {}: {e}", path.display()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A minimal tree shaped like the real one, all at `v`.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str, v: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("xtask-versions-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir); // leftover from an aborted run, if any
            let f = Fixture { dir };
            f.write(
                "Cargo.toml",
                &format!(
                    "[workspace]\nmembers = [\"xa11y-core\", \"examples/rust\"]\n\
                     exclude = [\"xa11y-js\", \"xa11y-python\"]\n\
                     [workspace.package]\nversion = \"{v}\"\n"
                ),
            );
            f.write(
                "xa11y-core/Cargo.toml",
                "[package]\nname = \"xa11y-core\"\nversion.workspace = true\n",
            );
            f.write(
                "examples/rust/Cargo.toml",
                &format!("[package]\nname = \"xa11y-example\"\nversion = \"{v}\"\n"),
            );
            f.write(
                "xa11y-js/Cargo.toml",
                &format!("[package]\nname = \"xa11y-js\"\nversion = \"{v}\"\n"),
            );
            f.write(
                "xa11y-python/Cargo.toml",
                &format!("[package]\nname = \"xa11y-python\"\nversion = \"{v}\"\n"),
            );
            f.write(
                "xa11y-python/pyproject.toml",
                "[project]\nname = \"xa11y\"\ndynamic = [\"version\"]\n",
            );
            f.write(
                "xa11y-js/package.json",
                &format!("{{\"name\": \"@crowecawcaw/xa11y\", \"version\": \"{v}\"}}"),
            );
            f.write(
                "xa11y-js/package-lock.json",
                &format!(
                    "{{\"version\": \"{v}\", \"packages\": {{\"\": {{\"version\": \"{v}\"}}}}}}"
                ),
            );
            f.write(
                "Cargo.lock",
                &lock(&[("xa11y-core", v), ("serde", "1.0.0")]),
            );
            f.write(
                "xa11y-js/Cargo.lock",
                &lock(&[("xa11y-core", v), ("xa11y-js", v)]),
            );
            f
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.dir.join(rel);
            fs::create_dir_all(p.parent().expect("fixture path has a parent"))
                .expect("create fixture dir");
            fs::write(p, body).expect("write fixture file");
        }

        fn root(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir); // best-effort temp cleanup
        }
    }

    /// Lockfile body; `serde` gets a registry source so it is not a path crate.
    fn lock(pkgs: &[(&str, &str)]) -> String {
        let mut s = String::from("version = 4\n");
        for (name, v) in pkgs {
            s.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{v}\"\n"
            ));
            if *name == "serde" {
                s.push_str("source = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
            }
        }
        s
    }

    #[test]
    fn consistent_tree_passes() {
        let f = Fixture::new("ok", "0.15.2");
        let r = check(f.root(), Some("v0.15.2"));
        assert!(r.problems.is_empty(), "{:#?}", r.problems);
        assert_eq!(r.version, "0.15.2");
    }

    #[test]
    fn expect_mismatch_fails() {
        let f = Fixture::new("expect", "0.15.2");
        let r = check(f.root(), Some("0.16.0"));
        assert_eq!(r.problems.len(), 1, "{:#?}", r.problems);
        assert!(r.problems[0].contains("v0.16.0"));
    }

    /// The v0.15.2 shape: workspace bumped, bindings and their lock not.
    #[test]
    fn issue_463_tag_shape_fails_on_every_lagging_field() {
        let f = Fixture::new("463", "0.15.2");
        f.write(
            "xa11y-js/Cargo.toml",
            "[package]\nname = \"xa11y-js\"\nversion = \"0.15.1\"\n",
        );
        f.write(
            "xa11y-python/Cargo.toml",
            "[package]\nname = \"xa11y-python\"\nversion = \"0.15.1\"\n",
        );
        f.write(
            "xa11y-js/package.json",
            "{\"version\": \"0.15.1\", \"optionalDependencies\": \
             {\"@crowecawcaw/xa11y-linux-x64-gnu\": \"0.15.1\", \"left-pad\": \"1.0.0\"}}",
        );
        f.write(
            "xa11y-js/package-lock.json",
            "{\"version\": \"0.15.1\", \"packages\": {\"\": {\"version\": \"0.15.1\"}}}",
        );
        f.write(
            "xa11y-js/Cargo.lock",
            &lock(&[("xa11y-core", "0.15.0"), ("xa11y-js", "0.15.1")]),
        );
        let r = check(f.root(), Some("v0.15.2"));
        let all = r.problems.join("\n");
        for needle in [
            "xa11y-js/Cargo.toml: xa11y-js is version \"0.15.1\"",
            "xa11y-python/Cargo.toml: xa11y-python is version \"0.15.1\"",
            "xa11y-js/package.json: [\"version\"]",
            "[\"optionalDependencies\"][\"@crowecawcaw/xa11y-linux-x64-gnu\"]",
            "xa11y-js/package-lock.json: [\"version\"]",
            "xa11y-js/package-lock.json: [\"packages\"][\"\"][\"version\"]",
            "records xa11y-core 0.15.0",
            "records xa11y-js 0.15.1",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
        assert_eq!(r.problems.len(), 8, "{all}");
    }

    #[test]
    fn workspace_member_with_stale_literal_version_fails() {
        let f = Fixture::new("member", "0.15.2");
        f.write(
            "examples/rust/Cargo.toml",
            "[package]\nname = \"xa11y-example\"\nversion = \"0.15.1\"\n",
        );
        let r = check(f.root(), None);
        assert_eq!(r.problems.len(), 1, "{:#?}", r.problems);
        assert!(r.problems[0].contains("examples/rust/Cargo.toml"));
    }

    /// Registry packages and path crates that don't share the version (the
    /// fuzz-target crate, test apps) are not the release's business.
    #[test]
    fn unrelated_lock_entries_are_ignored() {
        let f = Fixture::new("unrelated", "0.15.2");
        f.write(
            "xa11y/fuzz/Cargo.lock",
            &lock(&[
                ("xa11y-core", "0.15.2"),
                ("xa11y-fuzz-targets", "0.0.0"),
                ("serde", "9.9.9"),
            ]),
        );
        let r = check(f.root(), None);
        assert!(r.problems.is_empty(), "{:#?}", r.problems);
    }

    #[test]
    fn lockfiles_under_dot_and_target_dirs_are_skipped() {
        let f = Fixture::new("skip", "0.15.2");
        let stale = lock(&[("xa11y-core", "0.1.0")]);
        f.write(".claude/worktrees/x/Cargo.lock", &stale);
        f.write("target/package/Cargo.lock", &stale);
        f.write("xa11y-js/node_modules/x/Cargo.lock", &stale);
        let r = check(f.root(), None);
        assert!(r.problems.is_empty(), "{:#?}", r.problems);
    }

    #[test]
    fn static_pyproject_version_must_match() {
        let f = Fixture::new("pyproject", "0.15.2");
        f.write(
            "xa11y-python/pyproject.toml",
            "[project]\nname = \"xa11y\"\nversion = \"0.15.1\"\n",
        );
        let r = check(f.root(), None);
        assert_eq!(r.problems.len(), 1, "{:#?}", r.problems);
        assert!(r.problems[0].contains("pyproject.toml"));
    }
}
