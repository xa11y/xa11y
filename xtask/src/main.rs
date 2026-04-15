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
    test-js             Build and unit-test JS (Node) bindings
    test-js-integ       Run JS integration tests against the AccessKit test app
    test-integ          Run integration tests (delegates to scripts/)
    test-integ-container  Run Linux integration tests in container
    test-qt             Run Qt (PySide6) integration tests
    test-gtk            Run GTK4 integration tests
    test-cocoa          Run Cocoa/AppKit integration tests (macOS only)
    test-tauri          Run Tauri integration tests
    test-apps           Run all app integration test suites (qt, gtk, cocoa, tauri)
    docs                Build documentation
    coverage            Generate code coverage report
    fuzz [ARGS..]       Run provider fuzzer (pass-through args)
    sync-readmes [--check]  Generate crates.io/PyPI READMEs from root README.md
    check               Run ALL pre-PR checks (fmt, lint, test, test-python, test-js, docs)
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
        "test-js" => do_test_js(),
        "test-js-integ" => do_test_js_integ(),
        "test-integ" => do_test_integ(rest),
        "test-integ-container" => do_test_integ_container(rest),
        "test-qt" => do_test_qt(),
        "test-gtk" => do_test_gtk(),
        "test-cocoa" => do_test_cocoa(),
        "test-tauri" => do_test_tauri(),
        "test-apps" => do_test_apps(),
        "docs" => do_docs(),
        "coverage" => do_coverage(),
        "fuzz" => do_fuzz(rest),
        "sync-readmes" => do_sync_readmes(rest),
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
    run_with_env_in(cmd, args, &project_root(), key, val)
}

fn run_with_env_in(cmd: &str, args: &[&str], dir: &std::path::Path, key: &str, val: &str) -> bool {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
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

    heading("JS bindings: cargo check");
    let js_dir = project_root().join("xa11y-js");
    let js_cargo_ok = run_with_env_in("cargo", &["check"], &js_dir, "RUSTFLAGS", "-Dwarnings");

    heading("JS bindings: cargo fmt --check");
    let js_fmt_ok = run_in("cargo", &["fmt", "--", "--check"], &js_dir);

    clippy_ok && ruff_ok && py_cargo_ok && py_fmt_ok && js_cargo_ok && js_fmt_ok
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

fn do_test_js() -> bool {
    let js_dir = project_root().join("xa11y-js");

    heading("JS bindings: install dev deps");
    if !js_dir.join("node_modules").exists() && !run_in("npm", &["ci"], &js_dir) {
        return false;
    }

    heading("JS bindings: build (debug)");
    if !run_in(
        "npx",
        &[
            "napi",
            "build",
            "--platform",
            "--js",
            "native.js",
            "--dts",
            "native.d.ts",
        ],
        &js_dir,
    ) {
        return false;
    }

    heading("JS bindings: patch native.d.ts");
    if !run_in("node", &["scripts/patch-native-dts.mjs"], &js_dir) {
        return false;
    }

    heading("JS bindings: tsc --noEmit");
    if !run_in("npx", &["tsc", "--noEmit"], &js_dir) {
        return false;
    }

    heading("JS bindings: unit tests");
    run_in("npm", &["test"], &js_dir)
}

fn do_test_js_integ() -> bool {
    heading("JS bindings: integration tests");
    let root = project_root();
    if env::consts::OS == "windows" {
        eprintln!("JS integration tests on Windows: run scripts/run_js_tests.sh from a PowerShell that mirrors the Linux flow, or run on CI.");
        return false;
    }
    run_in("bash", &["scripts/run_js_tests.sh"], &root)
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

fn do_test_qt() -> bool {
    heading("Qt integration tests (PySide6)");
    let root = project_root();
    run_in("bash", &["scripts/run_qt_tests.sh"], &root)
}

fn do_test_gtk() -> bool {
    heading("GTK4 integration tests");
    let root = project_root();
    run_in("bash", &["scripts/run_gtk_tests.sh"], &root)
}

fn do_test_cocoa() -> bool {
    heading("Cocoa/AppKit integration tests");
    let root = project_root();
    run_in("bash", &["scripts/run_cocoa_tests.sh"], &root)
}

fn do_test_tauri() -> bool {
    heading("Tauri integration tests");
    let root = project_root();
    run_in("bash", &["scripts/run_tauri_tests.sh"], &root)
}

fn do_test_apps() -> bool {
    heading("All app integration tests");
    let mut ok = true;
    if !do_test_qt() {
        ok = false;
    }
    if !do_test_gtk() {
        ok = false;
    }
    if env::consts::OS == "macos" && !do_test_cocoa() {
        ok = false;
    }
    if !do_test_tauri() {
        ok = false;
    }
    ok
}

fn do_docs() -> bool {
    heading("Check doc links");
    let root = project_root();
    let links_ok = run_in("python", &["docs/check_links.py"], &root);
    if !links_ok {
        return false;
    }

    heading("Generate Python API docs");
    let gen_ok = run_in("python", &["docs/generate_python_api.py"], &root);
    if !gen_ok {
        return false;
    }

    heading("Generate JavaScript API docs");
    let gen_js_ok = run_in("python", &["docs/generate_js_api.py"], &root);
    if !gen_js_ok {
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

fn do_sync_readmes(args: &[String]) -> bool {
    let check = args.iter().any(|a| a == "--check");
    heading(if check {
        "Check READMEs are in sync"
    } else {
        "Sync READMEs"
    });
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
        let expected = strip_lang_blocks(&source, keep, remove);
        let path = root.join(dest);

        if check {
            let actual = fs::read_to_string(&path).unwrap_or_default();
            if actual != expected {
                eprintln!("{dest} is out of date. Run `cargo xtask sync-readmes` to fix.");
                ok = false;
            } else {
                eprintln!("{dest} is up to date.");
            }
        } else if let Err(e) = fs::write(&path, &expected) {
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

fn do_check() -> bool {
    let mut ok = true;

    heading("PRE-PR CHECK: sync-readmes");
    if !do_sync_readmes(&["--check".to_string()]) {
        eprintln!("!! READMEs out of date. Run `cargo xtask sync-readmes` to fix.");
        ok = false;
    }

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

    heading("PRE-PR CHECK: test-js");
    if !do_test_js() {
        eprintln!("!! JS unit tests failed.");
        ok = false;
    }

    if ok {
        heading("All checks passed!");
    } else {
        heading("Some checks failed");
    }
    ok
}
