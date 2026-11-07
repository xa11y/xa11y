//! Real browser evidence for the explicit native IA2 provider.
#[cfg(target_os = "windows")]
#[path = "../tests/support/ia2.rs"]
mod support;
#[cfg(target_os = "windows")]
fn main() {
    use std::sync::Arc;
    use xa11y_core::text_api::{TextRange, TextUnit};
    use xa11y_core::{Element, ElementData, Error, Provider, Selector};
    fn one(p: &dyn Provider, root: &ElementData, selector: &str) -> Result<ElementData, Error> {
        let matches = p
            .find_elements(root, &Selector::parse(selector)?, Some(2), Some(32))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("find {selector}: {e}"),
            })?;
        if matches.len() != 1 {
            return Err(Error::Platform {
                code: -1,
                message: format!("IA2 fixture expected one {selector}, got {}", matches.len()),
            });
        }
        Ok(matches[0].clone())
    }
    fn run() -> support::ProbeResult<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 8 {
            return Err("expected owned-browser probe arguments".into());
        }
        let hwnd = windows::Win32::Foundation::HWND(args[1].parse::<usize>()? as _);
        let pid = args[3].parse()?;
        let mut report = {
            let _apartment = support::Apartment::new()?;
            support::probe(hwnd, pid, &args[5], &args[7])?
        };
        let provider: Arc<dyn Provider> = Arc::new(xa11y_windows::Ia2Provider::new()?);
        let app = provider.app_by_pid(pid)?;
        let doc = one(
            provider.as_ref(),
            &app,
            &format!(r#"web_area[name*="{}"]"#, args[5]),
        )?;
        assert_eq!(doc.raw["accessibility_backend"], "ia2");
        assert!(provider
            .find_elements(
                &app,
                &Selector::parse(&format!(r#"web_area[name*="{}"]"#, args[7]))?,
                Some(1),
                Some(32)
            )?
            .is_empty());
        let edit = one(provider.as_ref(), &doc, r#"text_field[name="IA2 editor"]"#)?;
        let edit = Element::new(edit, Arc::clone(&provider));
        let text = edit.text()?;
        let all = text.read_all()?;
        assert_eq!(
            all,
            "Hello \u{1f600} world\nSecond line"
                .replace("\\u{1f600}", "\u{1f600}")
                .replace("\\n", "\n")
        );
        assert_eq!(text.read(TextRange { start: 6, end: 8 })?, "\u{1f600}");
        assert_eq!(text.find("world")?, Some(TextRange { start: 9, end: 14 }));
        assert_eq!(
            text.range_at(7, TextUnit::Character)?,
            TextRange { start: 6, end: 8 }
        );
        assert!(matches!(
            text.read(TextRange { start: 7, end: 8 }),
            Err(Error::InvalidTextRange { .. })
        ));
        let preceding = text.range_at(5, TextUnit::Word)?;
        assert_eq!(
            text.read(preceding)?.trim(),
            "Hello",
            "native Word at whitespace"
        );
        edit.focus()?;
        edit.select_text(6, 8)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while text.selections()? != vec![TextRange { start: 6, end: 8 }] {
            if std::time::Instant::now() >= deadline {
                return Err("IA2 selection did not arrive".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let len = text.length()?;
        edit.select_text(len, len)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while text.caret()? != Some(len) {
            if std::time::Instant::now() >= deadline {
                return Err("IA2 end caret did not arrive".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(text.selections()?.is_empty());
        let lines = text.lines(TextRange { start: 0, end: len }, 10)?;
        assert!(!lines.truncated);
        assert_eq!(
            lines.lines,
            vec![
                TextRange { start: 0, end: 15 },
                TextRange { start: 15, end: 26 }
            ]
        );
        assert!(!text.bounds(TextRange { start: 0, end: 5 })?.is_empty());
        assert!(text
            .bounds(TextRange {
                start: len,
                end: len
            })?
            .iter()
            .any(|r| r.height > 0));
        let rich = Element::new(
            one(
                provider.as_ref(),
                &doc,
                r#"text_field[name="IA2 rich editor"]"#,
            )?,
            Arc::clone(&provider),
        );
        let rich_text = rich.text()?;
        let value = rich_text.read_all()?;
        assert!(value.starts_with("Hello bold italic red"), "{value:?}");
        let runs = rich_text.runs(
            TextRange {
                start: 0,
                end: rich_text.length()?,
            },
            30,
        )?;
        assert!(!runs.truncated);
        assert!(runs
            .runs
            .iter()
            .any(|r| r.attributes.font_weight == Some(700)));
        let at = |offset| {
            &runs
                .runs
                .iter()
                .find(|r| r.range.start <= offset && offset < r.range.end)
                .expect("formatting covers offset")
                .attributes
        };
        assert_eq!(at(6).font_weight, Some(700));
        assert_eq!(at(11).italic, Some(true));
        assert_eq!(at(18).underline, Some(true));
        assert_eq!(
            at(18).foreground.map(|c| c.to_hex()).as_deref(),
            Some("#ff0000")
        );
        let table = Element::new(
            one(
                provider.as_ref(),
                &doc,
                r#"table[name="IA2 fixture table"]"#,
            )?,
            Arc::clone(&provider),
        );
        assert_eq!(
            table
                .table
                .expect("table info")
                .row_count
                .and_then(|c| c.known()),
            Some(3)
        );
        let merged = table.cell_at(1, 1)?.expect("merged cell");
        assert_eq!(merged.cell.expect("cell info").column_span, Some(2));
        assert!(table.cell_at(3, 0)?.is_none());
        assert!(!merged.column_header_cells()?.is_empty());
        assert!(!merged.row_header_cells()?.is_empty());
        let parent = merged.parent()?.expect("native cell parent");
        assert_eq!(parent.role, xa11y_core::Role::TableRow);
        for header in merged.column_header_cells()? {
            assert!(header.raw["ia2_unique_id"].as_i64().is_some());
            assert_ne!(
                header
                    .parent()?
                    .expect("native header parent")
                    .raw
                    .get("ia2_unique_id"),
                merged.raw.get("ia2_unique_id")
            );
        }
        assert!(!table.column_headers()?.is_empty());
        assert!(!table.row_headers()?.is_empty());
        let button = Element::new(
            one(provider.as_ref(), &doc, r#"button[name="Fixture action"]"#)?,
            Arc::clone(&provider),
        );
        assert!(matches!(
            button.cell_at(0, 0),
            Err(Error::ActionNotSupported { .. })
        ));
        button.press()?;
        let removed = Element::new(
            one(provider.as_ref(), &doc, r#"button[name="Remove once"]"#)?,
            Arc::clone(&provider),
        );
        removed.press()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut logged_pending = false;
        loop {
            // Chrome can return E_FAIL between DOM removal and publication
            // of the new AX parent list. Poll only a read-only witness;
            // never retry an action whose delivery could be unknown.
            match provider.get_parent(removed.data()) {
                Err(Error::ElementStale { .. }) => break,
                Ok(_) if std::time::Instant::now() < deadline => {}
                Err(Error::Platform { code, .. })
                    if code == 0x80004005u32 as i32 as i64
                        && std::time::Instant::now() < deadline =>
                {
                    if !logged_pending {
                        eprintln!("waiting for Chrome to publish the controlled DOM removal");
                        logged_pending = true;
                    }
                }
                result => {
                    return Err(
                        format!("removed IA2 object did not become stale: {result:?}").into(),
                    )
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            matches!(removed.press(), Err(Error::ElementStale { .. })),
            "captured removed object is refused before native dispatch"
        );
        let moving = Element::new(
            one(provider.as_ref(), &doc, r#"button[name="Moving control"]"#)?,
            Arc::clone(&provider),
        );
        assert_eq!(
            moving.parent()?.expect("original parent").name.as_deref(),
            Some("Original parent")
        );
        Element::new(
            one(
                provider.as_ref(),
                &doc,
                r#"button[name="Reparent control"]"#,
            )?,
            Arc::clone(&provider),
        )
        .press()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while moving.parent()?.expect("current parent").name.as_deref() != Some("New parent") {
            if std::time::Instant::now() >= deadline {
                return Err("IA2 parent did not reflect DOM reparenting".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if std::env::var_os("XA11Y_IA2_CONSUMERS").is_some() {
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("workspace");
            let output = std::process::Command::new("python")
                .arg(workspace.join("tests/harness/windows_ia2_consumers.py"))
                .arg(pid.to_string())
                .output()?;
            if !output.status.success() {
                return Err(format!(
                    "IA2 consumer source checks: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }
            report["consumer_sources"] = serde_json::from_slice(&output.stdout)?;
        }
        let clear_deep = Element::new(
            one(
                provider.as_ref(),
                &doc,
                r#"button[name="Remove deep tree"]"#,
            )?,
            Arc::clone(&provider),
        );
        Element::new(
            one(provider.as_ref(), &doc, r#"button[name="Add deep tree"]"#)?,
            Arc::clone(&provider),
        )
        .press()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match provider.find_elements(
                &doc,
                &Selector::parse(r#"button[name="Deep target"]"#)?,
                Some(1),
                None,
            ) {
                Err(Error::Unsupported { feature }) if feature.contains("depth 32") => break,
                Ok(matches) if matches.is_empty() && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                result => {
                    return Err(format!("IA2 deep tree must report its budget: {result:?}").into())
                }
            }
        }
        assert_eq!(text.length()?, 26, "budget errors leave the worker usable");
        clear_deep.press()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match provider.find_elements(
                &doc,
                &Selector::parse(r#"button[name="Deep target"]"#)?,
                Some(1),
                None,
            ) {
                Ok(matches) if matches.is_empty() => break,
                Err(Error::Unsupported { .. } | Error::ElementStale { .. })
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                result => {
                    return Err(
                        format!("IA2 deep fixture cleanup did not publish: {result:?}").into(),
                    )
                }
            }
        }
        report["rich_text_table"] = serde_json::json!({"source":"ia2","utf16":true,"format_runs":runs.runs.len(),"lines":lines.lines.len(),"merged_columns":2,"removed_stale":true,"reparented":true,"depth_budget":true,"end_caret_geometry":true,"text2":edit.raw.get("ia2_text2")});
        println!("{report}");
        Ok(())
    }
    if let Err(error) = run() {
        eprintln!("IA2 rich provider: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("IA2 rich provider requires Windows");
    std::process::exit(1);
}

#[cfg(all(test, target_os = "windows"))]
mod native_tests {
    #[test]
    #[ignore = "launches owned Chromium tabs; run via cargo xtask test-integ and Windows CI"]
    fn owned_browser_ia2_text_table() {
        let probe = std::env::var_os("XA11Y_IA2_RICH_PROBE")
            .expect("set XA11Y_IA2_RICH_PROBE to built probe executable");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace");
        let status = std::process::Command::new("python")
            .arg(root.join("tests/harness/windows_ia2_browser.py"))
            .arg("--probe")
            .arg(probe)
            .current_dir(root)
            .status()
            .expect("owned browser harness");
        assert!(status.success(), "IA2 rich fixture failed: {status}");
    }
}
