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
    test-electron       Run Electron integration tests (Linux only)
    test-apps           Run all app integration test suites (qt, gtk, cocoa, tauri, electron)
    docs                Build documentation
    coverage            Generate code coverage report
    fuzz [ARGS..]       Run provider fuzzer (pass-through args)
    sync-readmes [--check]  Generate crates.io/PyPI READMEs from root README.md
    check-macos-ffi     Verify xa11y-macos/src/ax.rs only uses safe_* CF/AX wrappers
    check-bindings-parity  Verify Python/JS bindings mirror xa11y-core's public API
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
        "test-electron" => do_test_electron(),
        "test-apps" => do_test_apps(),
        "docs" => do_docs(),
        "coverage" => do_coverage(),
        "fuzz" => do_fuzz(rest),
        "sync-readmes" => do_sync_readmes(rest),
        "check-macos-ffi" => do_check_macos_ffi(),
        "check-bindings-parity" => do_check_bindings_parity(),
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

fn do_test_electron() -> bool {
    heading("Electron integration tests");
    let root = project_root();
    run_in("bash", &["scripts/run_electron_tests.sh"], &root)
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
    if env::consts::OS == "linux" && !do_test_electron() {
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

// ── Bindings parity check ────────────────────────────────────────────────────

/// One method extracted from a Rust source file.
#[derive(Debug, Clone)]
struct ExtractedFn {
    name: String,
    line: usize,
}

/// Python dunder / built-in methods that never need to mirror a Rust method.
fn is_python_idiomatic_extra(name: &str) -> bool {
    // Any __dunder__ pattern is filtered; these are Python-specific idioms.
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

/// JS-side method names that don't need a Rust counterpart.
fn is_js_idiomatic_extra(name: &str) -> bool {
    name == "constructor"
}

/// Scan `src` for the inherent impl block whose header line starts with
/// `impl_header` (e.g. `"impl Locator"`), and collect every `pub fn NAME(`
/// declaration inside it. The impl ends at the next line that is exactly
/// `}` (a column-zero closing brace) — all xa11y-core impl blocks are at
/// the top level, so this is reliable.
fn extract_pub_fns_in_impl(src: &str, impl_header: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let mut in_impl = false;
    for (idx, line) in src.lines().enumerate() {
        let lineno = idx + 1;
        if !in_impl {
            let trimmed = line.trim_start();
            // Match e.g. `impl Locator {` but not `impl Locator<T>`-style or
            // trait impls like `impl Clone for Locator`.
            if let Some(after) = trimmed.strip_prefix(impl_header) {
                // Must be followed by ` {` or just `{` to be an inherent impl.
                let after_trim = after.trim_start();
                if after_trim.starts_with('{') {
                    in_impl = true;
                }
            }
            continue;
        }
        // Column-zero `}` closes the top-level impl.
        if line == "}" {
            in_impl = false;
            continue;
        }
        if let Some(name) = parse_fn_decl(line, "pub fn ") {
            out.push(ExtractedFn { name, line: lineno });
        }
    }
    out
}

/// Scan `src` for a `pub struct TY {` block and collect the names of every
/// `pub FIELD: ...` field inside. Stops at the column-zero closing `}`.
///
/// Used to treat public struct fields as part of the API surface. In Rust,
/// `Element` derefs to `ElementData`, so `element.role` is field access on
/// `ElementData` — but the Python / JS bindings expose it as a getter
/// `role()`. For parity accounting we treat the core field as a mirrored
/// "method".
fn extract_pub_fields_in_struct(src: &str, ty: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let mut in_struct = false;
    let header = format!("pub struct {ty} {{");
    for (idx, line) in src.lines().enumerate() {
        let lineno = idx + 1;
        if !in_struct {
            if line.trim_start().starts_with(&header) {
                in_struct = true;
            }
            continue;
        }
        if line == "}" {
            in_struct = false;
            continue;
        }
        if let Some(name) = parse_pub_field(line) {
            out.push(ExtractedFn { name, line: lineno });
        }
    }
    out
}

/// Extract the field name from a `    pub FIELD: TY,` line. Returns `None`
/// for non-field lines (docs, attributes, nested types, etc.).
fn parse_pub_field(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // `pub(crate)` / `pub(super)` are non-public to outside users — skip.
    let rest = trimmed.strip_prefix("pub ")?;
    // Field names end at `:` (typed field). Anything without `:` on this line
    // is a `pub fn`, `pub struct`, `pub use`, etc.
    let colon = rest.find(':')?;
    let name = rest[..colon].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// Scan `src` for `#[pyclass]` / `#[pyclass(...)]` struct TY { ... } and
/// collect the names of every field marked with `#[pyo3(get)]` (or
/// `#[pyo3(get, set)]`, etc.). These are Python-visible attributes that are
/// morally equivalent to getter methods, so they count toward parity.
fn extract_pyo3_get_fields(src: &str, ty: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let struct_header = format!("struct {ty} {{");
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let is_pyclass_attr =
            trimmed.starts_with("#[pyclass]") || trimmed.starts_with("#[pyclass(");
        if is_pyclass_attr {
            // Walk forward to the `struct TY {` header, skipping any further
            // attributes / derives.
            let mut j = i + 1;
            while j < lines.len() {
                let t = lines[j].trim();
                if t.is_empty() || t.starts_with("#[") || t.starts_with("//") {
                    j += 1;
                    continue;
                }
                break;
            }
            if j < lines.len() && lines[j].trim().starts_with(&struct_header) {
                // Track whether the most recent attribute line was
                // `#[pyo3(get*)]`. Cleared on any non-attribute/non-blank line
                // (so the getter marker applies only to the immediately
                // following field).
                let mut has_get_attr = false;
                let mut k = j + 1;
                while k < lines.len() {
                    if lines[k] == "}" {
                        break;
                    }
                    let t = lines[k].trim();
                    if t.starts_with("#[pyo3(")
                        && (t.contains("get)") || t.contains("get,") || t.contains("get "))
                    {
                        has_get_attr = true;
                        k += 1;
                        continue;
                    }
                    if t.starts_with("#[") || t.starts_with("//") || t.is_empty() {
                        k += 1;
                        continue;
                    }
                    // Non-attribute line — either a field or something else.
                    if has_get_attr {
                        if let Some(name) = parse_field_name(t) {
                            out.push(ExtractedFn { name, line: k + 1 });
                        }
                        has_get_attr = false;
                    } else {
                        has_get_attr = false;
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Parse a field name from a struct-body line. Handles both `pub NAME: TY`
/// and private `NAME: TY` (since #[pyo3(get)] works on private fields too).
fn parse_field_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
    let colon = rest.find(':')?;
    let name = rest[..colon].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// Scan `src` for `#[pymethods] impl TY { ... }` and collect every
/// `fn NAME(` declaration inside.
fn extract_pymethods_fns(src: &str, ty: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let target = format!("impl {ty} {{");
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line == "#[pymethods]" {
            // Look for the impl header on subsequent lines (skipping blanks
            // and intervening attributes, though in practice it's the next
            // line).
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() && lines[j].trim() == target {
                // Collect fn decls until column-zero `}`.
                let mut k = j + 1;
                while k < lines.len() {
                    if lines[k] == "}" {
                        break;
                    }
                    if let Some(name) = parse_fn_decl(lines[k], "fn ") {
                        out.push(ExtractedFn { name, line: k + 1 });
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Scan `src` for `#[napi]` / `#[napi(...)]` followed by `impl TY {` and
/// collect `fn NAME(` declarations inside. napi also decorates individual
/// methods with `#[napi(...)]` — we still collect every top-level `fn` inside
/// the impl, since nothing else lives there.
///
/// If a method is tagged with `#[napi(... js_name = "X" ...)]`, the comparison
/// uses `X` instead of the Rust fn name; this absorbs the `select_` → `select`
/// rename without treating it as divergence.
fn extract_napi_fns(src: &str, ty: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let target = format!("impl {ty} {{");
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Outer `#[napi]` / `#[napi(...)]` attribute on the impl block.
        let is_napi_attr = trimmed.starts_with("#[napi]") || trimmed.starts_with("#[napi(");
        if is_napi_attr {
            // Some #[napi(js_name = "...")] attributes span multiple lines;
            // consume lines until we hit a non-attribute line (for impl
            // headers they're normally on a single line, but be defensive).
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() && lines[j].trim() == target {
                let mut k = j + 1;
                // Track js_name overrides that precede each fn inside the impl.
                let mut pending_js_name: Option<String> = None;
                while k < lines.len() {
                    if lines[k] == "}" {
                        break;
                    }
                    let inner_trim = lines[k].trim();
                    if let Some(js_name) = parse_js_name(inner_trim) {
                        pending_js_name = Some(js_name);
                    }
                    // napi methods are declared `pub fn NAME`; the impl can
                    // also contain private helpers (`fn from_core`). We only
                    // want the public ones exposed to JS.
                    if let Some(rust_name) = parse_fn_decl(lines[k], "pub fn ") {
                        let effective = pending_js_name
                            .take()
                            .unwrap_or_else(|| rust_name.trim_end_matches('_').to_string());
                        out.push(ExtractedFn {
                            name: effective,
                            line: k + 1,
                        });
                    }
                    // A blank line or another fn clears the pending js_name.
                    if inner_trim.is_empty() {
                        pending_js_name = None;
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Scan `index.d.ts` for pure-TypeScript method declarations on a class or
/// interface. These are JS-layer augmentations that aren't visible from the
/// napi-decorated Rust source but are real user-visible API.
///
/// Two forms are recognised:
///   1. `export class TY extends ... {` / `export class TY {` — direct class
///      declaration (e.g. `Subscription`).
///   2. `interface TY {` — inside a `declare module './native.js' { ... }`
///      augmentation (e.g. the `Locator.waitUntil` extension).
///
/// Inside either block, we pick up lines that match a simple method decl
/// pattern: optional `static`, a camelCase identifier, optional generics,
/// then `(`. The identifier is converted from camelCase to snake_case so it
/// matches napi's Rust-side `pub fn wait_until` etc.
fn extract_index_dts_methods(src: &str, ty: &str) -> Vec<ExtractedFn> {
    let mut out = Vec::new();
    let class_header = format!("class {ty}");
    let iface_header = format!("interface {ty}");
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Class or interface opening. We don't distinguish blocks further —
        // just track brace depth until the opening `{` is closed.
        let is_open = (trimmed.starts_with(&class_header) || trimmed.contains(&class_header))
            && (trimmed.contains("class ") || trimmed.starts_with(&class_header))
            || trimmed.starts_with(&iface_header);
        // Tighten: require the exact header token followed by a space, `{`,
        // `<` or `extends`. This avoids false positives like `Subscriber`
        // when looking for `Subscription`.
        let is_open = is_open
            && (contains_header_token(trimmed, &class_header)
                || contains_header_token(trimmed, &iface_header));
        if !is_open {
            i += 1;
            continue;
        }
        // Advance to the opening `{` (may be on the same line or a following line).
        let mut j = i;
        let mut found_open = false;
        while j < lines.len() {
            if lines[j].contains('{') {
                found_open = true;
                break;
            }
            j += 1;
        }
        if !found_open {
            i += 1;
            continue;
        }
        // Walk forward, tracking brace + paren depth; collect method decls at
        // brace depth 1 (direct members of the class/interface body) and
        // paren depth 0 (not inside a multi-line method signature).
        let mut brace_depth = 0i32;
        let mut paren_depth = 0i32;
        let mut k = j;
        // Count braces/parens on the opening line itself so we start at the
        // right depth.
        for ch in lines[k].chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                '(' => paren_depth += 1,
                ')' => paren_depth -= 1,
                _ => {}
            }
        }
        k += 1;
        while k < lines.len() && brace_depth > 0 {
            let line = lines[k];
            let line_brace_before = brace_depth;
            let line_paren_before = paren_depth;
            for ch in line.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => brace_depth -= 1,
                    '(' => paren_depth += 1,
                    ')' => paren_depth -= 1,
                    _ => {}
                }
            }
            // A direct member is a line inside the class body (brace depth 1)
            // that isn't sitting inside a wrapped method signature (paren
            // depth 0). Multi-line decls like
            //     waitForEvent(
            //       type: EventTypeName,
            //       opts?: Options,
            //     ): Promise<Event>;
            // would otherwise be misread as members named `type`/`opts` etc.
            if line_brace_before == 1 && line_paren_before == 0 {
                if let Some(name) = parse_ts_method_decl(line) {
                    out.push(ExtractedFn {
                        name: camel_to_snake(&name),
                        line: k + 1,
                    });
                }
            }
            k += 1;
        }
        i = k.max(i + 1);
    }
    out
}

/// Whole-word token match: does `haystack` contain `token` with non-ident
/// boundaries on both sides?
fn contains_header_token(haystack: &str, token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let token_bytes = token.as_bytes();
    let mut i = 0;
    while i + token_bytes.len() <= bytes.len() {
        if &bytes[i..i + token_bytes.len()] == token_bytes {
            let left_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let right_idx = i + token_bytes.len();
            // Header token is a type name, optionally followed by whitespace,
            // `{`, `<`, `extends`. The important thing is that it isn't a
            // prefix of a longer identifier (e.g. `Subscription` ≠ `Subscriber`).
            let right_ok = right_idx >= bytes.len() || !is_ident_byte(bytes[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Try to parse a TypeScript method declaration line. Recognises the common
/// patterns used in our `index.d.ts`:
///   `  methodName(args): RetType;`
///   `  methodName<T>(args): RetType;`
///   `  static methodName(args): RetType;`
///   `  readonly methodName: Type;` — treated as a property; included for parity.
/// Returns the method / property name (camelCase preserved; caller normalises).
///
/// Deliberately skips:
///   `  on(type: ..., listener: ...): this;` — these are EventEmitter overloads.
///     They ARE real methods but they come from EventEmitter and would require
///     the core to also expose `on/once/off`, which doesn't make sense. We
///     detect and skip them by name.
///   `  // comment` / `/** ... */` blocks.
fn parse_ts_method_decl(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // Strip common leading keywords: static, readonly, get, set, async.
    // (We don't care about their semantics — we just need the identifier.)
    let rest = trimmed
        .strip_prefix("static ")
        .or_else(|| trimmed.strip_prefix("readonly "))
        .or_else(|| trimmed.strip_prefix("get "))
        .or_else(|| trimmed.strip_prefix("set "))
        .or_else(|| trimmed.strip_prefix("async "))
        .unwrap_or(trimmed);

    // Accept only an identifier-start char; this filters out comments,
    // blank lines, and closing braces.
    let first = rest.chars().next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    // Identifier ends at the first non-ident char.
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let name = &rest[..end];
    // Must be followed by `(` (method) or `<` (generic method) or `:` (prop)
    // or `?(` (optional method). We DON'T accept `=` (assignments) or `,`
    // (type unions).
    let after = rest[end..].trim_start();
    let is_method = after.starts_with('(')
        || after.starts_with('<')
        || after.starts_with("?(")
        || after.starts_with("?<");
    let is_prop = after.starts_with(':') || after.starts_with("?:");
    if !is_method && !is_prop {
        return None;
    }
    // Filter out EventEmitter overloads + other things that aren't part of
    // the real API surface.
    const SKIP: &[&str] = &["on", "once", "off", "emit", "addListener", "removeListener"];
    if SKIP.contains(&name) {
        return None;
    }
    Some(name.to_string())
}

/// Convert `waitUntil` → `wait_until`, `URLPath` → `u_r_l_path` (naive but
/// matches the napi convention for the surface we need).
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Extract a function name from a source line. Expects the prefix (e.g.
/// `"pub fn "` or `"fn "`) after whitespace. Stops at `(` or `<`.
fn parse_fn_decl(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // Skip `pub(crate)`/`pub(super)` — only exact `pub fn ` and `fn `.
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let rest = &trimmed[prefix.len()..];
    let end = rest
        .find(|c: char| c == '(' || c == '<' || c.is_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// If the line contains `js_name = "..."`, return the value.
fn parse_js_name(line: &str) -> Option<String> {
    let needle = "js_name";
    let i = line.find(needle)?;
    let rest = &line[i + needle.len()..];
    // Skip whitespace / `=` / whitespace to the opening quote.
    let eq = rest.find('=')?;
    let after_eq = &rest[eq + 1..];
    let quote = after_eq.find('"')?;
    let tail = &after_eq[quote + 1..];
    let close = tail.find('"')?;
    Some(tail[..close].to_string())
}

/// Parsed allowlist entries for one language.
#[derive(Default)]
struct AllowlistSide {
    /// Rust `Type::method` keys that are allowed to be absent from the binding.
    rust_only: std::collections::HashSet<String>,
    /// Binding-only methods allowed to have no Rust counterpart (plain name).
    extra: std::collections::HashSet<String>,
}

struct Allowlist {
    python: AllowlistSide,
    js: AllowlistSide,
}

fn parse_allowlist(path: &std::path::Path) -> Result<Allowlist, String> {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let value: toml::Value = toml::from_str(&src).map_err(|e| format!("parse error: {e}"))?;

    fn side(v: &toml::Value, key: &str, only_key: &str, other_key: &str) -> AllowlistSide {
        let mut out = AllowlistSide::default();
        let Some(table) = v.get(key).and_then(|t| t.as_table()) else {
            return out;
        };
        if let Some(arr) = table.get(only_key).and_then(|a| a.as_array()) {
            for entry in arr {
                // Accept either a bare string or `{ method = "..." }` inline.
                if let Some(s) = entry.as_str() {
                    out.rust_only.insert(s.to_string());
                } else if let Some(t) = entry.as_table() {
                    if let Some(m) = t.get("method").and_then(|m| m.as_str()) {
                        out.rust_only.insert(m.to_string());
                    }
                }
            }
        }
        if let Some(arr) = table.get(other_key).and_then(|a| a.as_array()) {
            for entry in arr {
                if let Some(s) = entry.as_str() {
                    out.extra.insert(s.to_string());
                } else if let Some(t) = entry.as_table() {
                    if let Some(m) = t.get("method").and_then(|m| m.as_str()) {
                        out.extra.insert(m.to_string());
                    }
                }
            }
        }
        out
    }

    Ok(Allowlist {
        python: side(&value, "python", "rust_only", "python_only"),
        js: side(&value, "js", "rust_only", "js_only"),
    })
}

fn do_check_bindings_parity() -> bool {
    heading("Bindings parity check (xa11y-core vs Python & JS)");

    let root = project_root();

    // ── Load core methods per type ──────────────────────────────────────
    let core_files: &[(&str, &str)] = &[
        ("App", "xa11y-core/src/app.rs"),
        ("Locator", "xa11y-core/src/locator.rs"),
        ("Element", "xa11y-core/src/element.rs"),
        ("Subscription", "xa11y-core/src/event_provider.rs"),
    ];
    let mut core: Vec<(&str, std::path::PathBuf, Vec<ExtractedFn>)> = Vec::new();
    for &(ty, rel) in core_files {
        let path = root.join(rel);
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("!! Failed to read {}: {e}", path.display());
                return false;
            }
        };
        let header = format!("impl {ty}");
        let mut fns = extract_pub_fns_in_impl(&src, &header);
        // Public struct fields are also part of the surface — `app.name`
        // in Rust is a field, mirrored as a getter in Python/JS. For the
        // `Element` type, the fields live on `ElementData` (Element derefs
        // to it), so read them from the same element.rs source.
        let extra_fields = match ty {
            "App" => extract_pub_fields_in_struct(&src, "App"),
            "Element" => extract_pub_fields_in_struct(&src, "ElementData"),
            _ => Vec::new(),
        };
        // Dedup against impl fns (shouldn't overlap in practice, but be safe).
        let have: std::collections::HashSet<String> = fns.iter().map(|f| f.name.clone()).collect();
        for f in extra_fields {
            if !have.contains(&f.name) {
                fns.push(f);
            }
        }
        core.push((ty, path, fns));
    }

    // ── Load Python bindings ────────────────────────────────────────────
    let python_path = root.join("xa11y-python/src/lib.rs");
    let python_src = match fs::read_to_string(&python_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("!! Failed to read {}: {e}", python_path.display());
            return false;
        }
    };

    // ── Load JS bindings (one file per type) ────────────────────────────
    let js_files: &[(&str, &str)] = &[
        ("App", "xa11y-js/src/app.rs"),
        ("Locator", "xa11y-js/src/locator.rs"),
        ("Element", "xa11y-js/src/element.rs"),
        ("Subscription", "xa11y-js/src/subscription.rs"),
    ];

    // ── Load JS layer augmentations from index.d.ts ─────────────────────
    // `index.d.ts` declares methods added purely in TypeScript (not via
    // napi). Parse it once and union its methods with the napi-derived set.
    let dts_path = root.join("xa11y-js/index.d.ts");
    let dts_src = fs::read_to_string(&dts_path).unwrap_or_default();

    // ── Load allowlist ──────────────────────────────────────────────────
    let allow_path = root.join("bindings/parity_allowlist.toml");
    let allow = match parse_allowlist(&allow_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("!! {}: {e}", allow_path.display());
            return false;
        }
    };

    // Subscription in JS is wrapped by `NativeSubscription` (Rust type),
    // exposed to JS as `_NativeSubscription`. The source-of-truth Rust fn
    // name is `NativeSubscription`.
    fn js_impl_name_for(ty: &str) -> &'static str {
        match ty {
            "App" => "App",
            "Locator" => "Locator",
            "Element" => "Element",
            "Subscription" => "NativeSubscription",
            _ => "",
        }
    }

    let mut ok = true;
    let mut total_core = 0;
    let mut total_py = 0;
    let mut total_js = 0;

    for (ty, core_path, core_fns) in &core {
        total_core += core_fns.len();
        let core_names: std::collections::BTreeMap<&str, usize> =
            core_fns.iter().map(|f| (f.name.as_str(), f.line)).collect();

        // ── Python side ──
        let mut py_fns = extract_pymethods_fns(&python_src, ty);
        // `#[pyo3(get)]` fields expose Python-visible attributes that behave
        // like getter methods. Treat them as part of the API surface.
        py_fns.extend(extract_pyo3_get_fields(&python_src, ty));
        let py_names: std::collections::BTreeSet<String> = py_fns
            .iter()
            .map(|f| f.name.clone())
            .filter(|n| !is_python_idiomatic_extra(n))
            .collect();
        total_py += py_names.len();

        // Core -> Python: every core name should appear in Python.
        let mut py_missing: Vec<&ExtractedFn> = Vec::new();
        for f in core_fns {
            let key = format!("{ty}::{}", f.name);
            if py_names.contains(&f.name) {
                continue;
            }
            if allow.python.rust_only.contains(&key) {
                continue;
            }
            py_missing.push(f);
        }
        // Python -> Core: any extra Python method must be in the allowlist.
        // Entries may be listed as bare names (`close`) or fully qualified
        // (`Subscription::close`); accept either for backwards compatibility.
        let mut py_extra: Vec<String> = Vec::new();
        for name in &py_names {
            if core_names.contains_key(name.as_str()) {
                continue;
            }
            let qualified = format!("{ty}::{name}");
            if allow.python.extra.contains(name) || allow.python.extra.contains(&qualified) {
                continue;
            }
            py_extra.push(name.clone());
        }

        // ── JS side ──
        let js_ty = js_impl_name_for(ty);
        let js_rel = js_files.iter().find(|(t, _)| *t == *ty).map(|(_, p)| *p);
        let js_src_opt: Option<String> = match js_rel {
            Some(rel) => {
                let p = root.join(rel);
                match fs::read_to_string(&p) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!("!! Failed to read {}: {e}", p.display());
                        return false;
                    }
                }
            }
            None => None,
        };
        let mut js_fns: Vec<ExtractedFn> = match &js_src_opt {
            Some(src) => extract_napi_fns(src, js_ty),
            None => Vec::new(),
        };
        // Pick up any index.d.ts augmentations for this type. `Locator` is
        // declared via `declare module './native.js' { interface Locator }`;
        // `Subscription` is declared as its own class. Either way the
        // extraction logic is the same. Names come out in camelCase and are
        // converted to snake_case by the extractor, so they match the napi
        // side (`wait_until`, `wait_for`, etc.).
        js_fns.extend(extract_index_dts_methods(&dts_src, ty));
        let js_names: std::collections::BTreeSet<String> = js_fns
            .iter()
            .map(|f| f.name.clone())
            .filter(|n| !is_js_idiomatic_extra(n))
            .collect();
        total_js += js_names.len();

        let mut js_missing: Vec<&ExtractedFn> = Vec::new();
        for f in core_fns {
            let key = format!("{ty}::{}", f.name);
            if js_names.contains(&f.name) {
                continue;
            }
            if allow.js.rust_only.contains(&key) {
                continue;
            }
            js_missing.push(f);
        }
        let mut js_extra: Vec<String> = Vec::new();
        for name in &js_names {
            if core_names.contains_key(name.as_str()) {
                continue;
            }
            let qualified = format!("{ty}::{name}");
            if allow.js.extra.contains(name) || allow.js.extra.contains(&qualified) {
                continue;
            }
            js_extra.push(name.clone());
        }

        // ── Report ──
        let py_ok = py_missing.is_empty() && py_extra.is_empty();
        let js_ok = js_missing.is_empty() && js_extra.is_empty();

        let py_allow_count = core_fns
            .iter()
            .filter(|f| {
                allow
                    .python
                    .rust_only
                    .contains(&format!("{ty}::{}", f.name))
            })
            .count()
            + allow.python.extra.len();
        let js_allow_count = core_fns
            .iter()
            .filter(|f| allow.js.rust_only.contains(&format!("{ty}::{}", f.name)))
            .count()
            + allow.js.extra.len();

        eprintln!(
            "{ty}: {} core methods | Python: {} ({}) | JS: {} ({})",
            core_fns.len(),
            if py_ok { "all mirrored" } else { "DRIFT" },
            format_allow_count(py_allow_count),
            if js_ok { "all mirrored" } else { "DRIFT" },
            format_allow_count(js_allow_count),
        );

        if !py_ok {
            ok = false;
            for f in &py_missing {
                eprintln!("!! Python binding missing: {ty}::{}", f.name);
                eprintln!(
                    "     Rust:   {}:{}  pub fn {}(...)",
                    core_path.display(),
                    f.line,
                    f.name,
                );
                eprintln!(
                    "     Python: expected #[pymethods] impl {ty} {{ fn {}(...) }}",
                    f.name,
                );
                eprintln!(
                    "     Fix:    either add the Python binding, or list {ty}::{} in",
                    f.name,
                );
                eprintln!(
                    "             bindings/parity_allowlist.toml [python.rust_only] with a reason."
                );
            }
            for name in &py_extra {
                eprintln!(
                    "!! Python method with no Rust counterpart: {ty}::{name}\n   \
                     Fix: add `{name}` to bindings/parity_allowlist.toml [python.python_only] \
                     with a reason (or mirror it in xa11y-core)."
                );
            }
        }
        if !js_ok {
            ok = false;
            for f in &js_missing {
                eprintln!("!! JS binding missing: {ty}::{}", f.name);
                eprintln!(
                    "     Rust: {}:{}  pub fn {}(...)",
                    core_path.display(),
                    f.line,
                    f.name,
                );
                let js_file = js_rel.unwrap_or("xa11y-js/src/<type>.rs");
                eprintln!(
                    "     JS:   expected #[napi] impl {js_ty} {{ fn {}(...) }} in {}",
                    f.name, js_file,
                );
                eprintln!(
                    "     Fix:  either add the JS binding, or list {ty}::{} in",
                    f.name,
                );
                eprintln!(
                    "           bindings/parity_allowlist.toml [js.rust_only] with a reason."
                );
            }
            for name in &js_extra {
                let js_file = js_rel.unwrap_or("xa11y-js/src/<type>.rs");
                eprintln!(
                    "!! JS method with no Rust counterpart: {ty}::{name} (in {js_file})\n   \
                     Fix: add `{name}` to bindings/parity_allowlist.toml [js.js_only] with a \
                     reason (or mirror it in xa11y-core)."
                );
            }
        }
    }

    let py_extra_total = allow.python.extra.len();
    let js_extra_total = allow.js.extra.len();
    eprintln!(
        "\nTotals: core={total_core} python={total_py} js={total_js} \
         (allowlist: python_only={py_extra_total}, js_only={js_extra_total})"
    );

    if ok {
        eprintln!("OK: all core methods have Python and JS bindings.");
    } else {
        eprintln!(
            "!! Bindings parity drift. See bindings/parity_allowlist.toml for allowlist entries."
        );
    }
    ok
}

fn format_allow_count(n: usize) -> String {
    if n == 0 {
        "allow:0".to_string()
    } else {
        format!("allow:{n}")
    }
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
    if !do_check_bindings_parity() {
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

    if ok {
        heading("All checks passed!");
    } else {
        heading("Some checks failed");
    }
    ok
}
