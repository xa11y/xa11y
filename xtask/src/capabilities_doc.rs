//! `cargo xtask sync-capabilities`: render `xa11y_core::capabilities` into
//! the platform-details reference page.
//!
//! The table is generated rather than hand-written because the hand-written
//! tables on that page had drifted from the code (Blur was documented as
//! unsupported on Linux and Windows while both implement it). The page holds
//! the rendered table between two markers; everything outside them stays
//! hand-written.

use std::fs;
use std::path::Path;

use xa11y_core::{Capabilities, Capability, Platform, Support};

const PAGE: &str = "docs/site/src/content/docs/reference/platform-details.mdx";
const BEGIN: &str =
    "{/* BEGIN GENERATED: capabilities. Edit xa11y-core/src/capabilities.rs, then run `cargo xtask sync-capabilities`. */}";
const END: &str = "{/* END GENERATED: capabilities */}";

/// Escape a cell for a GFM table inside MDX: pipes would split the cell, and
/// braces and angle brackets would be read as JSX.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn answer(c: &Capability) -> String {
    let word = match c.support {
        Support::Supported => "Supported",
        Support::Partial => "Partial",
        Support::Unsupported => "Unsupported",
        // `Support` is non_exhaustive; a new answer must be named here
        // before the docs can render it, so fail the render rather than
        // guess a word for it.
        _ => "UNRENDERED",
    };
    // The note goes in parentheses, not after a colon: the prose linter
    // reads "Supported: Note" as a capitalised word after a colon, and the
    // parenthesis keeps the answer word scannable down the column.
    let mut extra: Vec<String> = Vec::new();
    if let Some(note) = &c.note {
        extra.push(cell(note));
    }
    if let Some(since) = c.since {
        extra.push(format!("since {since}"));
    }
    if extra.is_empty() {
        word.to_string()
    } else {
        // A comma, not a semicolon: the prose linter flags semicolons.
        format!("{word} ({})", extra.join(", "))
    }
}

/// The generated block, markers included.
pub fn render() -> Result<String, String> {
    let mut tables: Vec<Capabilities> = Platform::ALL
        .iter()
        .map(|p| Capabilities::for_platform(*p, None).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    tables.push(
        Capabilities::for_backend(Platform::Windows, None, "ia2").map_err(|e| e.to_string())?,
    );
    let mut out = String::new();
    out.push_str(BEGIN);
    out.push_str(
        "\n\n| Feature | What it is | Linux (AT-SPI) | macOS | Windows (UIA) | Windows (IA2) |\n",
    );
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    let first = &tables[0];
    for (i, feature) in first.entries().iter().enumerate() {
        out.push_str(&format!(
            "| `{}` | {}",
            feature.name,
            cell(feature.description)
        ));
        for table in &tables {
            let c = &table.entries()[i];
            if c.name != feature.name {
                return Err(format!(
                    "platform tables disagree on row order at {}: {}",
                    feature.name, c.name
                ));
            }
            let rendered = answer(c);
            if rendered.contains("UNRENDERED") {
                return Err(format!(
                    "{} on {}: a Support value has no rendering in xtask/src/capabilities_doc.rs",
                    c.name, table.platform
                ));
            }
            out.push_str(&format!(" | {rendered}"));
        }
        out.push_str(" |\n");
    }
    out.push('\n');
    out.push_str(END);
    Ok(out)
}

/// Rewrite (or with `check`, compare) the generated block in the page.
pub fn sync(root: &Path, check: bool) -> bool {
    let path = root.join(PAGE);
    let page = match fs::read_to_string(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("!! could not read {PAGE}: {e}");
            return false;
        }
    };
    let (Some(start), Some(end)) = (page.find(BEGIN), page.find(END)) else {
        eprintln!(
            "!! {PAGE} has no generated capabilities block; add the two markers:\n{BEGIN}\n{END}"
        );
        return false;
    };
    if end < start {
        eprintln!("!! {PAGE}: the END marker comes before the BEGIN marker");
        return false;
    }
    let block = match render() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("!! {e}");
            return false;
        }
    };
    let updated = format!("{}{}{}", &page[..start], block, &page[end + END.len()..]);
    if check {
        if updated == page {
            eprintln!("{PAGE}: capability table is up to date.");
            true
        } else {
            eprintln!(
                "!! {PAGE}: capability table is out of date. Run `cargo xtask sync-capabilities`."
            );
            false
        }
    } else if updated == page {
        eprintln!("{PAGE}: capability table already up to date.");
        true
    } else {
        match fs::write(&path, updated) {
            Ok(()) => {
                eprintln!("Wrote the capability table into {PAGE}");
                true
            }
            Err(e) => {
                eprintln!("!! could not write {PAGE}: {e}");
                false
            }
        }
    }
}
