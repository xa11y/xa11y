//! Debug tool: dumps the UIA tree for the xa11y test app.

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Accessibility::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT(0x0)); // MTA (COINIT_MULTITHREADED)

        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_ALL)
            .expect("CoCreateInstance failed");

        let root = automation.GetRootElement().expect("GetRootElement failed");
        let true_cond = automation.CreateTrueCondition().expect("TrueCondition");

        // Find xa11y window
        let windows = root
            .FindAll(TreeScope_Children, &true_cond)
            .expect("FindAll");
        let count = windows.Length().unwrap_or(0);

        println!("Top-level elements: {}", count);

        for i in 0..count {
            let el = windows.GetElement(i).unwrap();
            let name = el.CurrentName().unwrap_or_default().to_string();
            if !name.to_lowercase().contains("xa11y") {
                continue;
            }

            let pid = el.CurrentProcessId().unwrap_or(0);
            let ct = el.CurrentControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
            let hwnd = el.CurrentNativeWindowHandle().ok();
            println!(
                "Window: '{}' PID={} ControlType={} HWND={:?}",
                name, pid, ct.0, hwnd
            );

            // Try ElementFromHandle
            if let Some(h) = hwnd {
                if !h.0.is_null() {
                    match automation.ElementFromHandle(h) {
                        Ok(el2) => {
                            let name2 = el2.CurrentName().unwrap_or_default().to_string();
                            println!("  ElementFromHandle: '{}'", name2);

                            // Try FindAll children
                            match el2.FindAll(TreeScope_Children, &true_cond) {
                                Ok(children) => {
                                    let cc = children.Length().unwrap_or(0);
                                    println!("  FindAll children: {}", cc);
                                    for j in 0..cc.min(10) {
                                        if let Ok(c) = children.GetElement(j) {
                                            let cn = c.CurrentName().unwrap_or_default().to_string();
                                            let cct = c.CurrentControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
                                            println!("    [{}] {} '{}'", j, cct.0, cn);
                                        }
                                    }
                                }
                                Err(e) => println!("  FindAll error: {}", e),
                            }

                            // Try RawViewWalker
                            match automation.RawViewWalker() {
                                Ok(walker) => {
                                    println!("  RawViewWalker children:");
                                    let mut child = walker.GetFirstChildElement(&el2).ok();
                                    let mut idx = 0;
                                    while let Some(ref c) = child {
                                        let cn = c.CurrentName().unwrap_or_default().to_string();
                                        let cct = c.CurrentControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
                                        println!("    [{}] {} '{}'", idx, cct.0, cn);
                                        idx += 1;
                                        if idx > 10 {
                                            println!("    ... (truncated)");
                                            break;
                                        }
                                        child = walker.GetNextSiblingElement(c).ok();
                                    }
                                    println!("  Total walker children: {}", idx);
                                }
                                Err(e) => println!("  RawViewWalker error: {}", e),
                            }

                            // Try ControlViewWalker
                            match automation.ControlViewWalker() {
                                Ok(walker) => {
                                    println!("  ControlViewWalker children:");
                                    let mut child = walker.GetFirstChildElement(&el2).ok();
                                    let mut idx = 0;
                                    while let Some(ref c) = child {
                                        let cn = c.CurrentName().unwrap_or_default().to_string();
                                        let cct = c.CurrentControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
                                        println!("    [{}] {} '{}'", idx, cct.0, cn);
                                        idx += 1;
                                        if idx > 10 {
                                            break;
                                        }
                                        child = walker.GetNextSiblingElement(c).ok();
                                    }
                                    println!("  Total control children: {}", idx);
                                }
                                Err(e) => println!("  ControlViewWalker error: {}", e),
                            }
                        }
                        Err(e) => println!("  ElementFromHandle error: {}", e),
                    }
                }
            }

            // Also try direct children on original element
            println!("  Original element FindAll children:");
            match el.FindAll(TreeScope_Children, &true_cond) {
                Ok(children) => {
                    let cc = children.Length().unwrap_or(0);
                    println!("    Count: {}", cc);
                    for j in 0..cc.min(10) {
                        if let Ok(c) = children.GetElement(j) {
                            let cn = c.CurrentName().unwrap_or_default().to_string();
                            let cct = c.CurrentControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
                            println!("    [{}] {} '{}'", j, cct.0, cn);
                        }
                    }
                }
                Err(e) => println!("    Error: {}", e),
            }
        }

        CoUninitialize();
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This tool only works on Windows");
    std::process::exit(1);
}
