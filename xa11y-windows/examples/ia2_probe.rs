//! Stage-one #483 native conformance probe; no public provider or raw-tree API.
#[cfg(target_os = "windows")]
#[path = "../tests/support/ia2.rs"]
mod ia2;

#[cfg(target_os = "windows")]
fn main() {
    fn run() -> ia2::ProbeResult<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 8
            || args[0] != "--hwnd"
            || args[2] != "--pid"
            || args[4] != "--active-marker"
            || args[6] != "--inactive-marker"
        {
            return Err("usage: ia2_probe --hwnd DECIMAL --pid PID --active-marker TITLE --inactive-marker TITLE".into());
        }
        // All interface owners are dropped before this apartment is uninitialized.
        let _apartment = ia2::Apartment::new()?;
        let hwnd =
            windows::Win32::Foundation::HWND(args[1].parse::<usize>()? as *mut std::ffi::c_void);
        let report = ia2::probe(hwnd, args[3].parse()?, &args[5], &args[7])?;
        println!("{report}");
        Ok(())
    }
    if let Err(error) = run() {
        eprintln!("IA2 conformance probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod native_tests {
    #[test]
    #[ignore = "launches two controlled browser tabs; run via the Windows integration CI step"]
    fn owned_browser_ia2_identity() {
        let probe = std::env::var_os("XA11Y_IA2_PROBE")
            .expect("set XA11Y_IA2_PROBE to the separately built ia2_probe.exe");
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("Windows crate lives in workspace");
        let status = std::process::Command::new("python")
            .arg(repository.join("tests/harness/windows_ia2_browser.py"))
            .arg("--probe")
            .arg(probe)
            .current_dir(repository)
            .status()
            .expect("start bounded owned-browser harness");
        assert!(
            status.success(),
            "owned-browser IA2 harness failed: {status}"
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("IA2 conformance probe requires Windows; no alternative backend is selected");
    std::process::exit(1);
}
