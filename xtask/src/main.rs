use std::env;
use std::fs;
use std::process::{Command, ExitCode};

const HELP: &str = "\
cargo xtask — development workflow commands

USAGE:
    cargo xtask <COMMAND>

COMMANDS:
    fmt [--check]       Format Rust (cargo fmt) and Python (ruff format)
    lint                Run clippy and ruff check
    test                Run Rust unit tests (cargo test --workspace)
    test-python         Build and test Python bindings
    test-integ          Run integration tests (delegates to scripts/)
    test-integ-container  Run Linux integration tests in container
    docs                Build documentation
    coverage            Generate code coverage report
    fuzz [ARGS..]       Run provider fuzzer (pass-through args)
    sync-readmes        Generate crates.io/PyPI READMEs from root README.md
    sync-doc-versions   Update version in docs from Cargo.toml
    check               Run ALL pre-PR checks (fmt, lint, test, test-python, docs)
    help                Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1..];

    let ok = match cmd {
        "fmt" => do_fmt(rest),
        "lint" => do_lint(),
        "test" => do_test(),
        "test-python" => do_test_python(),
        "test-integ" => do_test_integ(rest),
        "test-integ-container" => do_test_integ_container(rest),
        "docs" => do_docs(),
        "coverage" => do_coverage(),
        "fuzz" => do_fuzz(rest),
        "sync-readmes" => do_sync_readmes(),
        "sync-doc-versions" => sync_doc_versions(),
        "check" => do_check(),
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            true
        }
        other => {
            eprintln!("Unknown command: {other}\n");
            print!("{HELP}");
            false
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn project_root() -> std::path::PathBuf {
    let dir = env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap());
    // xtask/Cargo.toml -> repo root
    dir.parent().unwrap_or(&dir).to_path_buf()
}

fn run(cmd: &str, args: &[&str]) -> bool {
    run_in(cmd, args, &project_root())
}

fn run_in(cmd: &str, args: &[&str], dir: &std::path::Path) -> bool {
    let status = Command::new(cmd).args(args).current_dir(dir).status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("Failed to run {cmd}: {e}");
            false
        }
    }
}

fn run_with_env(cmd: &str, args: &[&str], key: &str, val: &str) -> bool {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(project_root())
        .env(key, val)
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("Failed to run {cmd}: {e}");
            false
        }
    }
}

fn heading(msg: &str) {
    eprintln!("\n=== {msg} ===\n");
}

// ── Commands ────────────────────────────────────────────────────────────────

fn do_fmt(args: &[String]) -> bool {
    let check = args.iter().any(|a| a == "--check");

    heading("Rust format");
    let rust_ok = if check {
        run("cargo", &["fmt", "--all", "--", "--check"])
    } else {
        run("cargo", &["fmt", "--all"])
    };

    heading("Python format (ruff)");
    let python_dir = project_root().join("xa11y-python");
    let python_ok = if check {
        run_in(
            "ruff",
            &["format", "--check", "python/", "tests/"],
            &python_dir,
        )
    } else {
        run_in("ruff", &["format", "python/", "tests/"], &python_dir)
    };

    rust_ok && python_ok
}

fn do_lint() -> bool {
    heading("Clippy");
    let clippy_ok = run_with_env(
        "cargo",
        &["clippy", "--workspace", "--all-targets"],
        "RUSTFLAGS",
        "-Dwarnings",
    );

    heading("Ruff check");
    let python_dir = project_root().join("xa11y-python");
    let ruff_ok = run_in("ruff", &["check", "python/", "tests/"], &python_dir);

    heading("Python Rust check");
    let py_cargo_ok = run_in("cargo", &["check"], &python_dir);

    heading("Python Rust format check");
    let py_fmt_ok = run_in("cargo", &["fmt", "--", "--check"], &python_dir);

    clippy_ok && ruff_ok && py_cargo_ok && py_fmt_ok
}

fn do_test() -> bool {
    heading("Rust unit tests");
    run("cargo", &["test", "--workspace"])
}

fn do_test_python() -> bool {
    heading("Python bindings: build");
    let python_dir = project_root().join("xa11y-python");
    let build_ok = run_in("pip", &["install", "-e", "."], &python_dir);
    if !build_ok {
        return false;
    }

    heading("Python bindings: test");
    run_in("python", &["-m", "pytest", "tests/", "-v"], &python_dir)
}

fn do_test_integ(args: &[String]) -> bool {
    heading("Integration tests");
    let root = project_root();
    let os = env::consts::OS;
    let script = match os {
        "macos" => "scripts/run_integ_tests_macos.sh",
        "linux" => "scripts/run_integ_tests.sh",
        _ => {
            eprintln!("Integration tests not supported on {os}");
            return false;
        }
    };
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd_args = vec![script];
    cmd_args.extend(&str_args);
    run_in("bash", &cmd_args, &root)
}

fn do_test_integ_container(args: &[String]) -> bool {
    heading("Integration tests (container)");
    let root = project_root();
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd_args = vec!["scripts/run_integ_container.sh"];
    cmd_args.extend(&str_args);
    run_in("bash", &cmd_args, &root)
}

fn do_docs() -> bool {
    heading("Check doc links");
    let root = project_root();
    let links_ok = run_in("python", &["docs/check_links.py"], &root);
    if !links_ok {
        return false;
    }

    heading("Sync doc versions");
    if !sync_doc_versions() {
        return false;
    }

    heading("Generate Python API docs");
    let gen_ok = run_in("python", &["docs/generate_python_api.py"], &root);
    if !gen_ok {
        return false;
    }

    heading("Build docs site");
    let site_dir = root.join("docs/site");
    let install_ok = run_in("npm", &["ci"], &site_dir);
    if !install_ok {
        return false;
    }
    run_in("npm", &["run", "build"], &site_dir)
}

fn do_coverage() -> bool {
    heading("Code coverage");
    run("bash", &["scripts/coverage.sh"])
}

fn do_fuzz(args: &[String]) -> bool {
    heading("Provider fuzzer");
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd_args = vec!["scripts/run_provider_fuzz.sh"];
    cmd_args.extend(&str_args);
    run("bash", &cmd_args)
}

fn do_sync_readmes() -> bool {
    heading("Sync READMEs");
    let root = project_root();
    let source = match fs::read_to_string(root.join("README.md")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read README.md: {e}");
            return false;
        }
    };

    let targets: &[(&str, &str)] = &[
        ("rust", "xa11y/README.md"),
        ("python", "xa11y-python/README.md"),
    ];

    let mut ok = true;
    for &(keep, dest) in targets {
        let remove = if keep == "rust" { "python" } else { "rust" };
        let text = strip_lang_blocks(&source, keep, remove);

        let path = root.join(dest);
        if let Err(e) = fs::write(&path, &text) {
            eprintln!("Failed to write {dest}: {e}");
            ok = false;
        } else {
            eprintln!("Wrote {dest}");
        }
    }
    ok
}

/// Remove `<!-- {remove}-only -->...<!-- /{remove}-only -->` blocks entirely,
/// and unwrap `<!-- {keep}-only -->...<!-- /{keep}-only -->` markers (keeping content).
fn strip_lang_blocks(source: &str, keep: &str, remove: &str) -> String {
    let open_remove = format!("<!-- {remove}-only -->\n");
    let close_remove = format!("<!-- /{remove}-only -->\n");
    let open_keep = format!("<!-- {keep}-only -->\n");
    let close_keep = format!("<!-- /{keep}-only -->\n");

    // Remove the other language's blocks
    let mut result = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find(&open_remove) {
        result.push_str(&rest[..start]);
        rest = &rest[start + open_remove.len()..];
        if let Some(end) = rest.find(&close_remove) {
            rest = &rest[end + close_remove.len()..];
        } else {
            // Unclosed marker — keep the rest as-is
            break;
        }
    }
    result.push_str(rest);

    // Unwrap the kept language's markers
    result = result.replace(&open_keep, "");
    result = result.replace(&close_keep, "");

    // Collapse triple+ blank lines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result
}

/// Read the workspace version from Cargo.toml and update the Rust install
/// snippet in the quick-start guide so it always shows the latest major.minor.
fn sync_doc_versions() -> bool {
    let root = project_root();

    let cargo_toml = match fs::read_to_string(root.join("Cargo.toml")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read Cargo.toml: {e}");
            return false;
        }
    };

    let version = match workspace_major_minor(&cargo_toml) {
        Some(v) => v,
        None => {
            eprintln!("Failed to parse workspace version from Cargo.toml");
            return false;
        }
    };

    let quick_start = root.join("docs/site/src/content/docs/guides/quick-start.mdx");
    let content = match fs::read_to_string(&quick_start) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read quick-start.mdx: {e}");
            return false;
        }
    };

    // Replace `xa11y = "X.Y"` with the current version.
    // The snippet appears inside a ```toml block as the only xa11y dependency line.
    let needle = find_xa11y_dep_line(&content);
    let target = format!(r#"xa11y = "{version}""#);

    match needle {
        Some(old) if old != target => {
            let updated = content.replace(&old, &target);
            if let Err(e) = fs::write(&quick_start, &updated) {
                eprintln!("Failed to write quick-start.mdx: {e}");
                return false;
            }
            eprintln!("Updated quick-start.mdx: {old} -> {target}");
        }
        Some(_) => {
            eprintln!("quick-start.mdx already up to date ({target})");
        }
        None => {
            eprintln!("Warning: could not find xa11y dependency line in quick-start.mdx");
        }
    }

    true
}

/// Find the `xa11y = "X.Y"` dependency line in the content.
fn find_xa11y_dep_line(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("xa11y = \"") && trimmed.ends_with('"') {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Extract "X.Y" from the `[workspace.package]` version in Cargo.toml.
fn workspace_major_minor(cargo_toml: &str) -> Option<String> {
    let mut in_workspace_pkg = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[workspace.package]" {
            in_workspace_pkg = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_workspace_pkg = false;
            continue;
        }
        if in_workspace_pkg && trimmed.starts_with("version") {
            let full = trimmed.split('"').nth(1)?;
            let parts: Vec<&str> = full.split('.').collect();
            if parts.len() >= 2 {
                return Some(format!("{}.{}", parts[0], parts[1]));
            }
        }
    }
    None
}

fn do_check() -> bool {
    let mut ok = true;

    heading("PRE-PR CHECK: format");
    if !do_fmt(&["--check".to_string()]) {
        eprintln!("!! Format check failed. Run `cargo xtask fmt` to fix.");
        ok = false;
    }

    heading("PRE-PR CHECK: lint");
    if !do_lint() {
        eprintln!("!! Lint check failed.");
        ok = false;
    }

    heading("PRE-PR CHECK: test");
    if !do_test() {
        eprintln!("!! Unit tests failed.");
        ok = false;
    }

    heading("PRE-PR CHECK: test-python");
    if !do_test_python() {
        eprintln!("!! Python tests failed.");
        ok = false;
    }

    if ok {
        heading("All checks passed!");
    } else {
        heading("Some checks failed");
    }
    ok
}
