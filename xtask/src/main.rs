use std::env;
use std::fs;
use std::process::{Command, ExitCode};

mod api;
mod binding_api;
mod parity;
mod rustdoc_api;

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
    test-integ-container  Run Linux X11 integration tests in container
    test-integ-wayland-container  Run Linux Wayland portal screenshot tests in container
    test-integ-wayland-uinput-container  Run Linux Wayland uinput input-sim e2e tests in container
    test-integ-input-smoke-container  Run Linux XTest input smoke in container
    test-qt [SUITE..]   Run Qt (PySide6) integration tests (default suites: python js cli)
    test-gtk [SUITE..]  Run GTK4 integration tests
    test-cocoa [SUITE..]  Run Cocoa/AppKit integration tests (macOS only)
    test-tauri [SUITE..]  Run Tauri integration tests
    test-electron [SUITE..]  Run Electron integration tests (Linux only)
    test-egui [SUITE..]  Run egui (eframe) integration tests
    test-winforms [SUITE..]  Run WinForms integration tests (Windows only)
    test-wpf [SUITE..]  Run WPF integration tests (Windows only)
    test-apps           Run the Python suite for every app (qt, gtk, cocoa, tauri, electron, egui, winforms, wpf)
    test-compat [APP]   Run shared harness (python + js + cli suites) against APP (default: tauri)
    test-matrix-check   Validate the tests/matrix.yaml coverage index
    test-harness        Unit-test the shared integ harness (tests/harness/)
    test-pytest-plugin  Install and test the pytest-xa11y plugin package
    docs                Build documentation
    lint-docs           Lint README + docs prose with Vale (requires the `vale` binary)
    coverage            Generate code coverage report
    fuzz [ARGS..]       Run provider fuzzer (pass-through args)
    sync-readmes [--check]  Generate crates.io/PyPI READMEs from root README.md
    check-macos-ffi     Verify xa11y-macos/src/ax.rs only uses safe_* CF/AX wrappers
    check-bindings-parity  Verify Python/JS bindings mirror xa11y-core's public API
    check               Run ALL pre-PR checks (fmt, lint, test, test-python, test-js, test-harness, test-pytest-plugin)
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
        "test-integ-wayland-container" => do_test_integ_wayland_container(),
        "test-integ-wayland-uinput-container" => do_test_integ_wayland_uinput_container(),
        "test-integ-input-smoke-container" => do_test_integ_input_smoke_container(),
        "test-qt" => do_test_qt(rest),
        "test-gtk" => do_test_gtk(rest),
        "test-cocoa" => do_test_cocoa(rest),
        "test-tauri" => do_test_tauri(rest),
        "test-electron" => do_test_electron(rest),
        "test-egui" => do_test_egui(rest),
        "test-winforms" => do_test_winforms(rest),
        "test-wpf" => do_test_wpf(rest),
        "test-apps" => do_test_apps(),
        "test-compat" => do_test_compat(rest),
        "test-matrix-check" => do_test_matrix_check(),
        "test-harness" => do_test_harness(),
        "test-pytest-plugin" => do_test_pytest_plugin(),
        "docs" => do_docs(),
        "lint-docs" => do_lint_docs(),
        "coverage" => do_coverage(),
        "fuzz" => do_fuzz(rest),
        "sync-readmes" => do_sync_readmes(rest),
        "check-macos-ffi" => do_check_macos_ffi(),
        "check-bindings-parity" => parity::check(&project_root()),
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

    heading("pytest-xa11y format (ruff)");
    let plugin_dir = project_root().join("pytest-xa11y");
    let plugin_ok = if check {
        run_in(
            "ruff",
            &["format", "--check", "src/", "tests/"],
            &plugin_dir,
        )
    } else {
        run_in("ruff", &["format", "src/", "tests/"], &plugin_dir)
    };

    rust_ok && python_ok && plugin_ok
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

    heading("pytest-xa11y: ruff check");
    let plugin_dir = project_root().join("pytest-xa11y");
    let plugin_ok = run_in("ruff", &["check", "src/", "tests/"], &plugin_dir);

    clippy_ok && ruff_ok && py_cargo_ok && py_fmt_ok && js_cargo_ok && js_fmt_ok && plugin_ok
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

fn do_test_integ_wayland_container() -> bool {
    heading("Wayland portal screenshot tests (container)");
    let root = project_root();
    // Build the Wayland container image (extends xa11y-base with sway +
    // xdg-desktop-portal + pipewire) if it isn't already present.
    let img_exists = std::process::Command::new("docker")
        .args(["image", "inspect", "xa11y-wayland"])
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !img_exists
        && !run_in(
            "docker",
            &[
                "build",
                "-t",
                "xa11y-wayland",
                "-f",
                "Containerfile.wayland",
                ".",
            ],
            &root,
        )
    {
        return false;
    }
    run_in(
        "docker",
        &[
            "run",
            "--rm",
            "-v",
            &format!("{}:/xa11y", root.display()),
            "-v",
            "xa11y-cargo-cache:/xa11y/target",
            "xa11y-wayland",
            "bash",
            "/xa11y/scripts/run_wayland_portal.sh",
        ],
        &root,
    )
}

fn do_test_integ_wayland_uinput_container() -> bool {
    heading("Wayland uinput input-sim e2e (container)");
    let root = project_root();
    // Build the uinput container image (extends xa11y-base with libevdev
    // + libxkbcommon) if it isn't already present.
    let img_exists = std::process::Command::new("docker")
        .args(["image", "inspect", "xa11y-wayland-uinput"])
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !img_exists
        && !run_in(
            "docker",
            &[
                "build",
                "-t",
                "xa11y-wayland-uinput",
                "-f",
                "Containerfile.wayland-uinput",
                ".",
            ],
            &root,
        )
    {
        return false;
    }
    run_in(
        "docker",
        &[
            "run",
            "--rm",
            "--device",
            "/dev/uinput",
            // Bind-mount /dev/input so the host-side udev-created
            // event nodes for our new uinput device are visible.
            "-v",
            "/dev/input:/dev/input",
            "-v",
            &format!("{}:/xa11y", root.display()),
            "-v",
            "xa11y-cargo-cache:/xa11y/target",
            "xa11y-wayland-uinput",
            "bash",
            "/xa11y/scripts/run_wayland_uinput.sh",
        ],
        &root,
    )
}

fn do_test_integ_input_smoke_container() -> bool {
    heading("Linux X11 input smoke (container)");
    let root = project_root();
    run_in(
        "docker",
        &[
            "run",
            "--rm",
            "-v",
            &format!("{}:/xa11y", root.display()),
            "-v",
            "xa11y-cargo-cache:/xa11y/target",
            "xa11y-base",
            "bash",
            "/xa11y/scripts/run_input_smoke.sh",
        ],
        &root,
    )
}

// The per-app test commands all funnel through the same shared runner
// (scripts/run_app_suite.sh), which sets up the environment, builds the app,
// and hands off to tests/harness/launch.py — the same harness CI invokes. Any
// trailing args are passed through as the suite list (default: python js cli).
fn do_test_app_suite(app: &str, suites: &[String], label: &str) -> bool {
    heading(label);
    let root = project_root();
    let mut args = vec!["scripts/run_app_suite.sh", app];
    args.extend(suites.iter().map(|s| s.as_str()));
    run_in("bash", &args, &root)
}

fn do_test_qt(rest: &[String]) -> bool {
    do_test_app_suite("qt", rest, "Qt integration tests (PySide6)")
}

fn do_test_gtk(rest: &[String]) -> bool {
    do_test_app_suite("gtk", rest, "GTK4 integration tests")
}

fn do_test_cocoa(rest: &[String]) -> bool {
    do_test_app_suite("cocoa", rest, "Cocoa/AppKit integration tests")
}

fn do_test_tauri(rest: &[String]) -> bool {
    do_test_app_suite("tauri", rest, "Tauri integration tests")
}

fn do_test_electron(rest: &[String]) -> bool {
    do_test_app_suite("electron", rest, "Electron integration tests")
}

fn do_test_egui(rest: &[String]) -> bool {
    do_test_app_suite("egui", rest, "egui integration tests")
}

fn do_test_winforms(rest: &[String]) -> bool {
    do_test_app_suite("winforms", rest, "WinForms integration tests")
}

fn do_test_wpf(rest: &[String]) -> bool {
    do_test_app_suite("wpf", rest, "WPF integration tests")
}

fn do_test_apps() -> bool {
    heading("All app integration tests");
    // Run the Python suite for each app (the historical `test-apps` scope).
    // Use the per-app commands directly for js/cli coverage on one app.
    let py = [String::from("python")];
    let mut ok = true;
    if !do_test_qt(&py) {
        ok = false;
    }
    if !do_test_gtk(&py) {
        ok = false;
    }
    if env::consts::OS == "macos" && !do_test_cocoa(&py) {
        ok = false;
    }
    if !do_test_tauri(&py) {
        ok = false;
    }
    if env::consts::OS == "linux" && !do_test_electron(&py) {
        ok = false;
    }
    if !do_test_egui(&py) {
        ok = false;
    }
    if env::consts::OS == "windows" && !do_test_winforms(&py) {
        ok = false;
    }
    if env::consts::OS == "windows" && !do_test_wpf(&py) {
        ok = false;
    }
    ok
}

fn do_test_compat(args: &[String]) -> bool {
    heading("Compat harness (shared python + js + cli suites)");
    let app = args.first().cloned().unwrap_or_else(|| "tauri".to_string());
    let root = project_root();
    let status = Command::new("python")
        .args(["tests/harness/launch.py", &app])
        .current_dir(&root)
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("Failed to run tests/harness/launch.py: {e}");
            false
        }
    }
}

fn do_test_matrix_check() -> bool {
    heading("Test coverage matrix check");
    let root = project_root();
    run_in("python", &["tests/matrix_check.py"], &root)
}

/// Install and test the pytest-xa11y plugin package.
///
/// The package is pure Python and lives outside the Cargo workspace (like
/// xa11y-python), so workspace-wide commands do not reach it. Its own tests
/// launch no app: they cover launcher validation, capability probing, the
/// event recorder, diagnostics rendering, and the plugin's pytest hooks via
/// pytest's `pytester`. They do need `xa11y` importable — run
/// `cargo xtask test-python` first if the bindings are not installed.
fn do_test_pytest_plugin() -> bool {
    heading("pytest-xa11y: install");
    let root = project_root();
    let plugin_dir = root.join("pytest-xa11y");
    if !run_in("pip", &["install", "-e", "."], &plugin_dir) {
        return false;
    }

    heading("pytest-xa11y: test");
    run_in("python", &["-m", "pytest", "tests/", "-v"], &plugin_dir)
}

/// Unit-test the shared integration harness and the coverage-index checker.
///
/// The harness decides which suites run in every CI matrix cell and
/// `matrix_check.py` decides whether the coverage index is believed, so a bug
/// in either is invisible: it just stops covering something and the cell stays
/// green (issues #327 and #348). These tests never launch an app — plain
/// pytest, no venv, no bindings required.
fn do_test_harness() -> bool {
    heading("Integ harness + coverage-index self-tests");
    let root = project_root();
    run_in(
        "python",
        &[
            "-m",
            "pytest",
            "tests/harness",
            "tests/test_matrix_check.py",
            "-v",
        ],
        &root,
    )
}

/// Paths the prose linter covers: the root README and every hand-written
/// page of the docs site. Generated API reference pages live under
/// `docs/site/src/content/docs/api/` and are excluded by `.vale.ini`.
const VALE_TARGETS: &[&str] = &["README.md", "docs/site/src/content/docs"];

/// Lint README and docs prose with Vale (`.vale.ini` at the repo root).
///
/// Vale is a Go binary rather than a Cargo dependency, so this reports a
/// missing install as a failure with instructions rather than skipping the
/// check: a doc-lint that quietly passes when the linter is absent is worse
/// than no check at all.
fn do_lint_docs() -> bool {
    heading("Lint docs prose (vale)");
    let root = project_root();

    if Command::new("vale").arg("--version").output().is_err() {
        eprintln!("`vale` was not found on PATH.");
        eprintln!("Install it from https://vale.sh/docs/install, then re-run.");
        eprintln!("  macOS:  brew install vale");
        eprintln!("  Linux:  download the release tarball for your arch");
        return false;
    }

    // Downloads the pinned `ai-tells` package into .github/styles/. A no-op
    // once the styles are present, so it is safe to run on every invocation.
    if !run_in("vale", &["sync"], &root) {
        eprintln!("`vale sync` failed: could not fetch the styles named in .vale.ini.");
        return false;
    }

    run_in("vale", VALE_TARGETS, &root)
}

fn do_docs() -> bool {
    let root = project_root();

    // Before anything else: a page that doesn't declare its Diátaxis type is
    // a structural problem, and reporting it takes no toolchain. The Astro
    // content schema enforces the frontmatter key again at build time; this
    // additionally checks the banner comment and the page's directory.
    heading("Check doc page types");
    let page_types_ok = run_in("python", &["docs/check_page_types.py"], &root);
    if !page_types_ok {
        return false;
    }

    heading("Check doc tables");
    let tables_ok = run_in("python", &["docs/check_tables.py"], &root);
    if !tables_ok {
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

    // After generation, so links into the generated API pages resolve on a
    // fresh checkout (e.g. /api/python/ from quick-start.mdx).
    heading("Check doc links");
    let links_ok = run_in("python", &["docs/check_links.py"], &root);
    if !links_ok {
        return false;
    }

    heading("Build docs site");
    let site_dir = root.join("docs/site");
    let install_ok = run_in("npm", &["ci"], &site_dir);
    if !install_ok {
        return false;
    }
    let build_ok = run_in("npm", &["run", "build"], &site_dir);
    if !build_ok {
        return false;
    }

    // Source well-formedness can't catch a toolchain regression that
    // drops every table from the rendered HTML (issue #247), so verify
    // the built pages actually contain the <table> elements.
    heading("Check rendered doc tables");
    run_in(
        "python",
        &["docs/check_tables.py", "--rendered", "docs/site/dist"],
        &root,
    )
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
        let expected = render_readme(&source, keep);
        let path = root.join(dest);

        // A marker surviving the render is a typo'd language name: it would
        // ship to crates.io or PyPI as literal text, and the block it guards
        // would land in the wrong README.
        for residue in ["-only -->", "-only-hidden"] {
            if expected.contains(residue) {
                eprintln!(
                    "{dest}: unresolved `{residue}` marker — language names must be one of {README_LANGS:?}"
                );
                ok = false;
            }
        }

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

/// Languages a README block can be tagged for. The root README renders every
/// visible block, so it is the one page that shows all three install steps;
/// each package README keeps its own language's blocks and drops the rest.
const README_LANGS: &[&str] = &["rust", "python", "js"];

/// Render the package README for `keep` from the root README: keep that
/// language's blocks, drop every other language's, and collapse the blank
/// lines the dropped blocks leave behind.
fn render_readme(source: &str, keep: &str) -> String {
    let mut result = source.to_string();
    for lang in README_LANGS {
        result = resolve_lang_blocks(&result, lang, *lang == keep);
    }

    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result
}

/// Resolve every block tagged for `lang`, keeping its content without the
/// markers when `keep` and dropping the whole block otherwise.
///
/// Two spellings are recognised. A visible block renders as ordinary Markdown
/// in the root README:
///
/// ```text
/// <!-- python-only -->
/// ...content...
/// <!-- /python-only -->
/// ```
///
/// A hidden block puts the content *inside* the comment, so the root README
/// renders nothing while that language's package README still gets it. This is
/// how the crates.io README keeps a Rust example that the root README, which
/// leads with Python, doesn't show:
///
/// ```text
/// <!-- rust-only-hidden
/// ...content...
/// -->
/// ```
///
/// Hidden content must not itself contain `-->`, which would end the comment
/// early and leak the remainder into the rendered root README.
///
/// An unclosed marker leaves the rest of the document untouched, so a
/// malformed README fails the `--check` diff instead of being silently
/// truncated.
fn resolve_lang_blocks(source: &str, lang: &str, keep: bool) -> String {
    let visible_open = format!("<!-- {lang}-only -->\n");
    let visible_close = format!("<!-- /{lang}-only -->\n");
    let hidden_open = format!("<!-- {lang}-only-hidden\n");
    let hidden_close = "-->\n";

    let mut result = String::with_capacity(source.len());
    let mut rest = source;
    loop {
        // Whichever spelling comes first wins. The two openers cannot match
        // the same marker: one ends in `-only -->`, the other in `-only-hidden`.
        let hidden = rest.find(&hidden_open).map(|at| (at, true));
        let visible = rest.find(&visible_open).map(|at| (at, false));
        let (start, is_hidden) = match (hidden, visible) {
            (Some(h), Some(v)) if v.0 < h.0 => v,
            (Some(h), _) => h,
            (None, Some(v)) => v,
            (None, None) => break,
        };
        let (open, close) = if is_hidden {
            (hidden_open.as_str(), hidden_close)
        } else {
            (visible_open.as_str(), visible_close.as_str())
        };

        result.push_str(&rest[..start]);
        let body = &rest[start + open.len()..];
        let Some(end) = body.find(close) else {
            result.push_str(&rest[start..]);
            return result;
        };
        if keep {
            result.push_str(&body[..end]);
        }
        rest = &body[end + close.len()..];
    }

    result.push_str(rest);
    result
}

/// Verify that `xa11y-macos/src/ax.rs` only uses the `safe_*` wrappers from
/// `exception_safe.m` for CoreFoundation / AX interop. A misbehaving AX
/// value's `-release` / `-getTypeID` can throw an `NSException` that unwinds
/// through `extern "C"` -> process abort, so every raw CF/AX call must go
/// through an Objective-C `@try`/`@catch` wrapper.
///
/// This is a simple token check over `ax.rs`. If a new raw symbol is needed,
/// add a `safe_*` wrapper to `exception_safe.m` first and call that instead.
/// References in `//` line comments are ignored so documentation can still
/// mention the forbidden symbols by name.
fn do_check_macos_ffi() -> bool {
    heading("macOS FFI exception-safety check");

    // Symbols that MUST be called through a `safe_*` wrapper, not directly.
    // Matching is on a whole-identifier token followed by `(`, so `CFRelease,`
    // in prose passes but `CFRelease(...)` / `CFRelease (...)` do not.
    const FORBIDDEN_CALLS: &[&str] = &[
        "CFRelease",
        "CFRetain",
        "CFGetTypeID",
        "CFStringGetTypeID",
        "CFNumberGetTypeID",
        "CFBooleanGetTypeID",
        "CFArrayGetTypeID",
        "CFArrayGetCount",
        "CFArrayGetValueAtIndex",
        "CFBooleanGetValue",
        "CFNumberGetValue",
        "CFDictionaryGetValue",
        "CFArrayCreate",
        "AXIsProcessTrusted",
    ];
    // Statics don't use `(`; match as whole identifiers.
    const FORBIDDEN_STATICS: &[&str] = &["kCFTypeArrayCallBacks"];

    let path = project_root().join("xa11y-macos/src/ax.rs");
    let src = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", path.display());
            return false;
        }
    };

    let mut violations: Vec<(usize, String, String)> = Vec::new();

    for (lineno, line) in src.lines().enumerate() {
        let code = strip_line_comment(line);
        if code.trim().is_empty() {
            continue;
        }

        for &sym in FORBIDDEN_CALLS {
            if contains_ident_followed_by(code, sym, b'(') {
                violations.push((lineno + 1, sym.to_string(), line.to_string()));
            }
        }
        for &sym in FORBIDDEN_STATICS {
            if contains_ident(code, sym) {
                violations.push((lineno + 1, sym.to_string(), line.to_string()));
            }
        }
    }

    if violations.is_empty() {
        eprintln!(
            "OK: xa11y-macos/src/ax.rs uses only safe_* CF/AX wrappers ({} forbidden symbols checked).",
            FORBIDDEN_CALLS.len() + FORBIDDEN_STATICS.len(),
        );
        return true;
    }

    eprintln!(
        "!! {} raw CF/AX call site(s) found in xa11y-macos/src/ax.rs:",
        violations.len()
    );
    for (lineno, sym, line) in &violations {
        eprintln!(
            "  {}:{}: {}  ->  {}",
            path.display(),
            lineno,
            sym,
            line.trim()
        );
    }
    eprintln!(
        "\n  Each of these must go through a safe_* wrapper defined in\n  \
         xa11y-macos/src/exception_safe.m. If the wrapper does not yet exist,\n  \
         add one following the @try/@catch pattern of the existing wrappers."
    );
    false
}

/// Strip a trailing `// ...` line comment from a Rust source line. Approximate
/// (doesn't handle `/* */` blocks or raw strings) but good enough to skip
/// documentation comments in the ax.rs header block.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i + 1 < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'/' && bytes[i + 1] == b'/' {
            return &line[..i];
        }
        i += 1;
    }
    line
}

fn contains_ident_followed_by(haystack: &str, needle: &str, next: u8) -> bool {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            let left_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let right_idx = i + needle_bytes.len();
            let right_ok = right_idx >= bytes.len() || !is_ident_byte(bytes[right_idx]);
            if left_ok && right_ok {
                let mut j = right_idx;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == next {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn contains_ident(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            let left_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let right_idx = i + needle_bytes.len();
            let right_ok = right_idx >= bytes.len() || !is_ident_byte(bytes[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn do_check() -> bool {
    let mut ok = true;

    heading("PRE-PR CHECK: sync-readmes");
    if !do_sync_readmes(&["--check".to_string()]) {
        eprintln!("!! READMEs out of date. Run `cargo xtask sync-readmes` to fix.");
        ok = false;
    }

    heading("PRE-PR CHECK: macos-ffi");
    if !do_check_macos_ffi() {
        eprintln!("!! macOS FFI check failed. See above for details.");
        ok = false;
    }

    heading("PRE-PR CHECK: bindings-parity");
    if !parity::check(&project_root()) {
        eprintln!("!! Bindings parity check failed. See above for details.");
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

    heading("PRE-PR CHECK: test-harness");
    if !do_test_harness() {
        eprintln!("!! Integ harness self-tests failed.");
        ok = false;
    }

    heading("PRE-PR CHECK: test-pytest-plugin");
    if !do_test_pytest_plugin() {
        eprintln!("!! pytest-xa11y tests failed.");
        ok = false;
    }

    if ok {
        heading("All checks passed!");
    } else {
        heading("Some checks failed");
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::{render_readme, README_LANGS};

    #[test]
    fn visible_block_survives_only_in_its_own_readme() {
        let source = "\
# Title

<!-- rust-only -->
cargo
<!-- /rust-only -->

<!-- python-only -->
pip
<!-- /python-only -->

<!-- js-only -->
npm
<!-- /js-only -->
";
        // The trailing blank line is what the dropped blocks leave behind:
        // blank-line collapsing folds runs of three or more newlines only.
        assert_eq!(render_readme(source, "rust"), "# Title\n\ncargo\n\n");
        assert_eq!(render_readme(source, "python"), "# Title\n\npip\n\n");
        assert_eq!(render_readme(source, "js"), "# Title\n\nnpm\n");
    }

    #[test]
    fn hidden_block_is_unwrapped_for_its_own_readme() {
        let source = "\
## Quick Example

<!-- rust-only-hidden
fn main() -> Result<()> { Ok(()) }
-->

<!-- python-only -->
import xa11y
<!-- /python-only -->
";
        assert_eq!(
            render_readme(source, "rust"),
            "## Quick Example\n\nfn main() -> Result<()> { Ok(()) }\n\n"
        );
        assert_eq!(
            render_readme(source, "python"),
            "## Quick Example\n\nimport xa11y\n"
        );
    }

    #[test]
    fn unclosed_marker_leaves_the_document_intact() {
        // No close marker: the text is preserved verbatim so `--check` reports
        // a diff rather than the generator silently truncating the README.
        let source = "# Title\n\n<!-- rust-only -->\ncargo\n";
        assert_eq!(render_readme(source, "rust"), source);
        assert_eq!(render_readme(source, "python"), source);
    }

    #[test]
    fn the_real_readme_resolves_every_marker() {
        let source = std::fs::read_to_string(super::project_root().join("README.md"))
            .expect("root README.md is readable");
        for keep in README_LANGS {
            let rendered = render_readme(&source, keep);
            for residue in ["-only -->", "-only-hidden"] {
                assert!(
                    !rendered.contains(residue),
                    "`{residue}` survived rendering README.md for {keep}"
                );
            }
        }
    }
}
