//! What each platform backend can report, as data.
//!
//! A `None` field, an event that never arrives, or an `Unsupported` error can
//! mean two different things: *this element or app* has nothing to report, or
//! *this backend* can never report it. [`Capabilities`] answers the second
//! question without probing an app, so the two can be told apart.
//!
//! Each feature has one of three answers ([`Support`]):
//!
//! - [`Supported`](Support::Supported): the backend reads it whenever the app
//!   exposes it. A `None` on an element means that element has none.
//! - [`Partial`](Support::Partial): the backend reads it for some toolkits,
//!   apps or display servers and not others. The note says which.
//! - [`Unsupported`](Support::Unsupported): never. The method fails with an
//!   `Unsupported`-class error, the event is never delivered, or the field is
//!   always `None`.
//!
//! The table is static per platform, so [`Capabilities::for_platform`] can
//! describe a platform other than the one running. An entry can name the
//! first OS version that supports it (`since`); when an OS version is known,
//! entries that need a later one read `Unsupported`.
//!
//! Feature names are `<area>:<name>` in `snake_case`, the spellings used
//! everywhere else (`event:menu_opened`, `window:maximize`,
//! `text:range_at:word`). [`Capabilities::names`] lists them.
//!
//! The rows live in one list with one column per platform, so a new feature
//! cannot be added without deciding all three, and a new platform is a
//! compile error on every row.

use std::fmt;

use crate::error::{Error, Result};
#[cfg(test)]
use crate::event::EventKind;

/// A desktop platform xa11y has a backend for.
///
/// `#[non_exhaustive]`: a new backend is a new variant. The capability table
/// stores one column per variant, so adding one fails the table's build until
/// every feature has an answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Platform {
    /// Linux, through AT-SPI2.
    Linux,
    /// macOS, through the AX API.
    Macos,
    /// Windows, through UI Automation.
    Windows,
}

impl Platform {
    /// Every platform, in the order the docs list them.
    pub const ALL: [Platform; 3] = [Platform::Linux, Platform::Macos, Platform::Windows];

    /// The platform this build runs on, or `None` on one xa11y has no
    /// backend for.
    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Macos)
        } else if cfg!(target_os = "windows") {
            Some(Platform::Windows)
        } else {
            None
        }
    }

    /// The name this platform crosses the bindings, the CLI and MCP as:
    /// `"linux"`, `"macos"` or `"windows"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::Macos => "macos",
            Platform::Windows => "windows",
        }
    }

    /// Parse a platform name as [`to_snake_case`](Self::to_snake_case)
    /// spells it. Fails with [`Error::InvalidActionData`] naming the valid
    /// names.
    pub fn parse(name: &str) -> Result<Platform> {
        Platform::ALL
            .into_iter()
            .find(|p| p.to_snake_case() == name)
            .ok_or_else(|| Error::InvalidActionData {
                message: format!(
                    "unknown platform {name:?}; expected one of \"linux\", \"macos\", \"windows\""
                ),
            })
    }

    fn display_name(self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::Macos => "macOS",
            Platform::Windows => "Windows",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Whether a backend can report a feature.
///
/// `#[non_exhaustive]`: consumers compare against the values they act on, and
/// a finer answer (for example one for deprecated features) must not break
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Support {
    /// Reported whenever the app exposes it.
    Supported,
    /// Reported for some toolkits, apps or display servers only; the
    /// capability's note says which.
    Partial,
    /// Never reported by this backend.
    Unsupported,
}

impl Support {
    /// The name this answer crosses the bindings, the CLI and MCP as:
    /// `"supported"`, `"partial"` or `"unsupported"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Support::Supported => "supported",
            Support::Partial => "partial",
            Support::Unsupported => "unsupported",
        }
    }
}

/// One feature's answer on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capability {
    /// Feature name, `<area>:<name>` (`event:menu_opened`).
    pub name: &'static str,
    /// What the feature is, in one sentence.
    pub description: &'static str,
    /// Whether this platform reports it.
    pub support: Support,
    /// How the platform provides it, or why it does not. `None` when there is
    /// nothing to add.
    pub note: Option<String>,
    /// The first OS version that supports it, dotted (`10.0.17134`), when
    /// older versions do not.
    pub since: Option<&'static str>,
}

/// Every feature's answer on one platform. Build with
/// [`Capabilities::current`] or [`Capabilities::for_platform`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capabilities {
    /// The platform described.
    pub platform: Platform,
    /// The OS version the answers were narrowed to, or `None` when no version
    /// was given or detected. Linux has none: what it can report depends on
    /// the toolkit and the display server, which the notes name.
    pub os_version: Option<String>,
    entries: Vec<Capability>,
}

impl Capabilities {
    /// The platform this build runs on, narrowed to the running OS version
    /// where one is detected (macOS and Windows).
    ///
    /// Fails with [`Error::Unsupported`] on an OS xa11y has no backend for.
    /// Reads no accessibility data and connects to no app.
    pub fn current() -> Result<Capabilities> {
        let platform = Platform::current().ok_or_else(|| Error::Unsupported {
            feature: "capabilities on this operating system: xa11y has no backend for it"
                .to_string(),
        })?;
        Ok(Self::build(platform, detected_os_version()))
    }

    /// The answers for `platform`, which need not be the one running.
    ///
    /// With `os_version` (dotted numbers, `"14.5"`, `"10.0.19045"`), entries
    /// that need a later version read [`Support::Unsupported`]. Without it,
    /// every entry is answered for a current OS, and `since` says which need
    /// one. Fails with [`Error::InvalidActionData`] for an unparsable
    /// version, or any version on Linux.
    pub fn for_platform(platform: Platform, os_version: Option<&str>) -> Result<Capabilities> {
        if let Some(v) = os_version {
            if platform == Platform::Linux {
                return Err(Error::InvalidActionData {
                    message: format!(
                        "os_version {v:?} given for linux: Linux has no single platform version; \
                         what it reports depends on the toolkit and display server, which the \
                         notes name"
                    ),
                });
            }
            parse_version(v)?;
        }
        Ok(Self::build(platform, os_version.map(str::to_string)))
    }

    fn build(platform: Platform, os_version: Option<String>) -> Capabilities {
        let entries = ROWS
            .iter()
            .map(|row| {
                let (support, note) = resolve(row.entry(platform), platform, os_version.as_deref());
                Capability {
                    name: row.name,
                    description: row.description,
                    support,
                    note,
                    since: row.entry(platform).since,
                }
            })
            .collect();
        Capabilities {
            platform,
            os_version,
            entries,
        }
    }

    /// Every feature, in table order.
    pub fn entries(&self) -> &[Capability] {
        &self.entries
    }

    /// The capability named `name`. Fails with [`Error::InvalidActionData`]
    /// for an unknown name, so a typo is not read as "unsupported".
    pub fn get(&self, name: &str) -> Result<&Capability> {
        self.entries
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| unknown_feature(name))
    }

    /// Shorthand for `get(name)?.support`.
    pub fn support(&self, name: &str) -> Result<Support> {
        self.get(name).map(|c| c.support)
    }

    /// Shorthand for `get(name)?.note`: how the platform provides the
    /// feature, or why it does not.
    pub fn note(&self, name: &str) -> Result<Option<&str>> {
        self.get(name).map(|c| c.note.as_deref())
    }

    /// Whether `name` is a feature the table knows.
    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|c| c.name == name)
    }

    /// Every feature name the table knows, in table order.
    pub fn names() -> impl Iterator<Item = &'static str> {
        ROWS.iter().map(|r| r.name)
    }
}

/// One entry's answer for an OS version: `Unsupported`, with the reason
/// prepended to the note, when the version is known and older than `since`.
fn resolve(
    entry: Entry,
    platform: Platform,
    os_version: Option<&str>,
) -> (Support, Option<String>) {
    let note = entry.note.map(str::to_string);
    let (Some(since), Some(have)) = (entry.since, os_version) else {
        return (entry.support, note);
    };
    // Both strings are validated: `since` by a unit test, `have` by
    // `for_platform` or the probe.
    let (Ok(need), Ok(have_v)) = (parse_version(since), parse_version(have)) else {
        return (entry.support, note);
    };
    if !version_lt(&have_v, &need) {
        return (entry.support, note);
    }
    let why = format!("requires {platform} {since} or later; {have} is older");
    let note = match note {
        Some(n) => format!("{why}. {n}"),
        None => why,
    };
    (Support::Unsupported, Some(note))
}

fn unknown_feature(name: &str) -> Error {
    // Suggest names in the same area, closest first, so `event:menu_open`
    // points at `event:menu_opened`. Bounded: at most ten.
    let area = name.split(':').next().unwrap_or_default();
    let shared = |n: &str| {
        n.chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let mut near: Vec<&str> = ROWS
        .iter()
        .map(|r| r.name)
        .filter(|n| n.split(':').next() == Some(area))
        .collect();
    // Closest first: the longest shared prefix with what was asked for.
    near.sort_by_key(|n| std::cmp::Reverse(shared(n)));
    near.truncate(10);
    let hint = if near.is_empty() {
        let areas: std::collections::BTreeSet<&str> = ROWS
            .iter()
            .filter_map(|r| r.name.split(':').next())
            .collect();
        format!(
            "known areas: {}",
            areas.into_iter().collect::<Vec<_>>().join(", ")
        )
    } else {
        format!("names in {area:?}: {}", near.join(", "))
    };
    Error::InvalidActionData {
        message: format!("unknown capability {name:?}; {hint}"),
    }
}

fn parse_version(v: &str) -> Result<Vec<u32>> {
    let parts: std::result::Result<Vec<u32>, _> = v.trim().split('.').map(str::parse).collect();
    match parts {
        Ok(p) if !p.is_empty() => Ok(p),
        _ => Err(Error::InvalidActionData {
            message: format!(
                "os_version {v:?} is not a dotted version such as \"14.5\" or \"10.0.19045\""
            ),
        }),
    }
}

/// `a < b`, comparing component-wise with missing components as 0.
fn version_lt(a: &[u32], b: &[u32]) -> bool {
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x < y;
        }
    }
    false
}

/// The running OS version, read once. `None` on Linux (no single platform
/// version) and when the probe fails; [`Capabilities::os_version`] then
/// reads `None`, so the caller can see no version was applied.
fn detected_os_version() -> Option<String> {
    static VERSION: std::sync::LazyLock<Option<String>> =
        std::sync::LazyLock::new(probe_os_version);
    VERSION.clone()
}

#[cfg(target_os = "macos")]
fn probe_os_version() -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.osproductversion"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_version(&v).ok().map(|_| v)
}

#[cfg(target_os = "windows")]
fn probe_os_version() -> Option<String> {
    // `ver` prints "Microsoft Windows [Version 10.0.22631.4317]". Unlike
    // GetVersionEx it is not subject to the manifest-based version lie.
    let out = std::process::Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_windows_ver(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_ver(text: &str) -> Option<String> {
    let start = text.find("Version ")? + "Version ".len();
    let rest = &text[start..];
    let end = rest.find(']').unwrap_or(rest.len());
    let v = rest[..end].trim().to_string();
    parse_version(&v).ok().map(|_| v)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn probe_os_version() -> Option<String> {
    None
}

/// The feature name of an event kind: `event:` plus the snake_case spelling
/// the bindings use. Exhaustive (core may match its own `#[non_exhaustive]`
/// enum without a wildcard), so a new kind fails this build until it has a
/// name, and a unit test then fails until it has a row. Test-only: the guard
/// is the exhaustive match, which a test build compiles.
#[cfg(test)]
fn event_feature(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::FocusChanged => "event:focus_changed",
        EventKind::ValueChanged => "event:value_changed",
        EventKind::NameChanged => "event:name_changed",
        EventKind::StateChanged { .. } => "event:state_changed",
        EventKind::StructureChanged => "event:structure_changed",
        EventKind::WindowOpened => "event:window_opened",
        EventKind::WindowClosed => "event:window_closed",
        EventKind::WindowActivated => "event:window_activated",
        EventKind::WindowDeactivated => "event:window_deactivated",
        EventKind::SelectionChanged => "event:selection_changed",
        EventKind::MenuOpened => "event:menu_opened",
        EventKind::MenuClosed => "event:menu_closed",
        EventKind::TextChanged => "event:text_changed",
        EventKind::TextSelectionChanged => "event:text_selection_changed",
        EventKind::Announcement => "event:announcement",
    }
}

// ── The table ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Entry {
    support: Support,
    note: Option<&'static str>,
    since: Option<&'static str>,
}

const fn yes() -> Entry {
    Entry {
        support: Support::Supported,
        note: None,
        since: None,
    }
}
const fn yes_by(note: &'static str) -> Entry {
    Entry {
        support: Support::Supported,
        note: Some(note),
        since: None,
    }
}
const fn partial(note: &'static str) -> Entry {
    Entry {
        support: Support::Partial,
        note: Some(note),
        since: None,
    }
}
const fn no(note: &'static str) -> Entry {
    Entry {
        support: Support::Unsupported,
        note: Some(note),
        since: None,
    }
}

impl Entry {
    /// The first OS version that supports this entry. No row uses it yet; the
    /// first will be UIA properties that arrived in Windows 10 updates.
    #[allow(
        dead_code,
        reason = "Version-gated rows arrive with #476 / #480; tested now."
    )]
    const fn since(mut self, version: &'static str) -> Entry {
        self.since = Some(version);
        self
    }
}

struct Row {
    name: &'static str,
    description: &'static str,
    linux: Entry,
    macos: Entry,
    windows: Entry,
}

impl Row {
    fn entry(&self, platform: Platform) -> Entry {
        match platform {
            Platform::Linux => self.linux,
            Platform::Macos => self.macos,
            Platform::Windows => self.windows,
        }
    }
}

const fn event(name: &'static str, description: &'static str) -> Row {
    Row {
        name,
        description,
        linux: yes(),
        macos: yes(),
        windows: yes(),
    }
}

const X11_SWAY: &str = "X11, and Sway on Wayland; other Wayland compositors: unsupported";
const X11_ONLY: &str = "X11 only; unsupported on Wayland";
const X11_SWAY_FLOATING: &str =
    "X11, and floating windows on Sway; other Wayland compositors: unsupported";

const ROWS: &[Row] = &[
    // ── Events ──
    event("event:focus_changed", "Keyboard focus moved to another element."),
    event("event:value_changed", "An element's value changed."),
    event("event:name_changed", "An element's name changed."),
    event("event:state_changed", "A state flag changed (checked, expanded, enabled, ...)."),
    event("event:structure_changed", "Children were added to or removed from an element."),
    event("event:window_opened", "A window opened."),
    event("event:window_closed", "A window closed."),
    Row {
        name: "event:window_activated",
        description: "A window became the active window.",
        linux: yes_by("Window:Activate and Window:Restore"),
        macos: yes_by("AXFocusedWindowChanged"),
        windows: no("no UIA handler is registered for window activation"),
    },
    Row {
        name: "event:window_deactivated",
        description: "A window stopped being the active window.",
        linux: yes_by("Window:Deactivate and Window:Minimize"),
        macos: no("AX reports the newly focused window, not the one that lost focus"),
        windows: no("no UIA handler is registered for window activation"),
    },
    event("event:selection_changed", "The selected items of a list, table or other container changed."),
    Row {
        name: "event:menu_opened",
        description: "A menu opened.",
        linux: no("the AT-SPI backend maps no menu event"),
        macos: yes_by("AXMenuOpened"),
        windows: yes_by("UIA MenuOpened"),
    },
    Row {
        name: "event:menu_closed",
        description: "A menu closed.",
        linux: no("the AT-SPI backend maps no menu event"),
        macos: yes_by("AXMenuClosed"),
        windows: yes_by("UIA MenuClosed"),
    },
    event("event:text_changed", "Text in an editable element changed."),
    event("event:text_selection_changed", "The caret moved or the text selection changed."),
    Row {
        name: "event:announcement",
        description: "The app asked for an announcement, or a live region or alert changed.",
        linux: yes_by("Object:Announcement; the text is not carried"),
        macos: yes_by("AXAnnouncementRequested; the text is not carried"),
        windows: yes_by("UIA Notification, LiveRegionChanged and SystemAlert; no text is carried"),
    },
    Row {
        name: "event:text_change",
        description: "A text_changed event says what was inserted or deleted (Event.text_change).",
        linux: partial("inserted or deleted text from Object:TextChanged; the UTF-16 range only for Qt, which counts UTF-16 natively"),
        macos: no("AXValueChanged carries no delta"),
        windows: no("UIA text-change events carry no delta"),
    },
    // ── Element actions ──
    Row {
        name: "action:blur",
        description: "Remove keyboard focus from an element (Element.blur).",
        linux: yes_by("focuses the parent with Component.GrabFocus; fails if the parent cannot take focus"),
        macos: yes_by("sets AXFocused to false"),
        windows: yes_by("focuses the desktop root"),
    },
    Row {
        name: "app:enable_web_accessibility",
        description: "Ask a Chromium or Electron app to build its web accessibility tree (App.enable_web_accessibility).",
        linux: no("a launch-time switch on Linux; relaunch the app with --force-renderer-accessibility"),
        macos: yes_by("sets AXManualAccessibility, then AXEnhancedUserInterface"),
        windows: no("not needed; Chromium builds the tree when a UI Automation client connects"),
    },
    // ── Window verbs ──
    Row {
        name: "window:activate",
        description: "Bring a window to the foreground and focus it.",
        linux: partial(X11_SWAY),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "window:minimize",
        description: "Minimize a window.",
        linux: partial(X11_ONLY),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "window:maximize",
        description: "Maximize a window.",
        linux: partial(X11_ONLY),
        macos: no("AX has no zoom state to read or set"),
        windows: yes(),
    },
    Row {
        name: "window:enter_fullscreen",
        description: "Put a window in native fullscreen.",
        linux: partial(X11_SWAY),
        macos: yes_by("AXFullScreen"),
        windows: no("UIA has no fullscreen API"),
    },
    Row {
        name: "window:restore",
        description: "Restore a window from minimized, maximized or fullscreen.",
        linux: partial(X11_SWAY),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "window:close",
        description: "Close a window.",
        linux: partial(X11_SWAY),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "window:move_to",
        description: "Move a window.",
        linux: partial(X11_SWAY_FLOATING),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "window:resize_to",
        description: "Resize a window.",
        linux: partial(X11_SWAY_FLOATING),
        macos: yes(),
        windows: yes(),
    },
    // ── Window states ──
    Row {
        name: "state:minimized",
        description: "Read whether a window is minimized.",
        linux: partial("X11, and the AT-SPI iconified state on Wayland compositors other than Sway; unknown on Sway"),
        macos: yes_by("AXMinimized"),
        windows: yes_by("WindowPattern visual state"),
    },
    Row {
        name: "state:maximized",
        description: "Read whether a window is maximized.",
        linux: partial(X11_ONLY),
        macos: no("AX has no zoom state; always unknown"),
        windows: yes_by("WindowPattern visual state"),
    },
    Row {
        name: "state:fullscreen",
        description: "Read whether a window is fullscreen.",
        linux: partial(X11_SWAY),
        macos: yes_by("AXFullScreen"),
        windows: no("UIA has no fullscreen state; always unknown"),
    },
    // ── Tables and collections ──
    Row {
        name: "table:row_index",
        description: "The index of a table row element (Element.table_row).",
        linux: no("AT-SPI has no row-index call for a row object; read the row from a cell"),
        macos: yes_by("AXIndex"),
        windows: partial("when the row object reports a GridItem row"),
    },
    Row {
        name: "table:cell_header",
        description: "Whether a cell is a column or row header (CellInfo.header).",
        linux: yes_by("the column header and row header roles"),
        macos: no("AX marks no cell as a header"),
        windows: yes(),
    },
    Row {
        name: "table:row_headers",
        description: "The row header cells of a table or cell.",
        linux: yes(),
        macos: partial("AppKit tables report no row headers"),
        windows: yes(),
    },
    Row {
        name: "table:collection",
        description: "Position and size of an item in a list, tree, tab list or radio group (Element.collection).",
        linux: partial("toolkits that set posinset, setsize and level (GTK, Chromium); Qt sets none"),
        macos: partial("rows, tabs and radio buttons; web list items are groups in AX and report none"),
        windows: yes(),
    },
    // ── Text ──
    Row {
        name: "text:range_at:character",
        description: "Text.range_at with the character unit.",
        linux: yes(),
        macos: yes(),
        windows: yes(),
    },
    Row {
        name: "text:range_at:word",
        description: "Text.range_at with the word unit.",
        linux: partial("where the toolkit reports word boundaries"),
        macos: no("AX has no word boundary API"),
        windows: yes(),
    },
    Row {
        name: "text:range_at:line",
        description: "Text.range_at with the line unit.",
        linux: yes(),
        macos: yes_by("AXLineForIndex and AXRangeForLine"),
        windows: yes(),
    },
    Row {
        name: "text:range_at:paragraph",
        description: "Text.range_at with the paragraph unit.",
        linux: partial("where the toolkit reports paragraph boundaries; the GTK 3 text view does not"),
        macos: no("AX has no paragraph boundary API"),
        windows: yes(),
    },
    Row {
        name: "text:attribute:script",
        description: "Superscript and subscript in text runs (TextAttributes.script).",
        linux: no("AT-SPI text attributes carry no script position"),
        macos: yes(),
        windows: yes(),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::StateFlag;

    #[test]
    fn names_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            assert!(seen.insert(row.name), "duplicate capability {}", row.name);
            let mut parts = row.name.split(':');
            let area = parts.next().unwrap_or_default();
            assert!(
                !area.is_empty() && parts.next().is_some(),
                "{} needs <area>:<name>",
                row.name
            );
            assert!(
                row.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ':'),
                "{} must be snake_case",
                row.name
            );
            assert!(
                row.description.ends_with('.'),
                "{}: description is a sentence",
                row.name
            );
        }
    }

    #[test]
    fn every_non_supported_entry_says_why() {
        for row in ROWS {
            for p in Platform::ALL {
                let e = row.entry(p);
                if e.support != Support::Supported {
                    assert!(
                        e.note.is_some(),
                        "{} on {p}: say why it is not supported",
                        row.name
                    );
                }
                if let Some(since) = e.since {
                    assert!(
                        parse_version(since).is_ok(),
                        "{}: bad since {since}",
                        row.name
                    );
                    assert_ne!(
                        p,
                        Platform::Linux,
                        "{}: Linux has no platform version",
                        row.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_event_kind_has_a_row() {
        let kinds = [
            EventKind::FocusChanged,
            EventKind::ValueChanged,
            EventKind::NameChanged,
            EventKind::StateChanged {
                flag: StateFlag::Checked,
                value: true,
            },
            EventKind::StructureChanged,
            EventKind::WindowOpened,
            EventKind::WindowClosed,
            EventKind::WindowActivated,
            EventKind::WindowDeactivated,
            EventKind::SelectionChanged,
            EventKind::MenuOpened,
            EventKind::MenuClosed,
            EventKind::TextChanged,
            EventKind::TextSelectionChanged,
            EventKind::Announcement,
        ];
        // The list above must name every kind: `event_feature` is exhaustive,
        // so count its distinct names against the rows instead of trusting it.
        for kind in &kinds {
            let name = event_feature(kind);
            assert!(ROWS.iter().any(|r| r.name == name), "no row for {name}");
        }
        let event_rows = ROWS
            .iter()
            .filter(|r| r.name.starts_with("event:") && r.name != "event:text_change")
            .count();
        assert_eq!(
            event_rows,
            kinds.len(),
            "an event row has no EventKind, or the kind list is short"
        );
    }

    #[test]
    fn get_rejects_unknown_names_with_suggestions() {
        let caps = Capabilities::for_platform(Platform::Linux, None).expect("linux table");
        let err = caps.get("event:menu_open").expect_err("unknown name");
        let msg = err.to_string();
        assert!(msg.contains("event:menu_opened"), "{msg}");
        let err = caps.get("nope").expect_err("unknown area");
        assert!(err.to_string().contains("known areas"), "{err}");
    }

    #[test]
    fn platform_answers_differ_where_the_table_says() {
        let linux = Capabilities::for_platform(Platform::Linux, None).expect("linux");
        let mac = Capabilities::for_platform(Platform::Macos, None).expect("macos");
        let win = Capabilities::for_platform(Platform::Windows, None).expect("windows");
        assert_eq!(
            linux.support("event:menu_opened").ok(),
            Some(Support::Unsupported)
        );
        assert_eq!(
            mac.support("event:menu_opened").ok(),
            Some(Support::Supported)
        );
        assert_eq!(
            win.support("window:enter_fullscreen").ok(),
            Some(Support::Unsupported)
        );
        assert_eq!(
            linux.support("window:maximize").ok(),
            Some(Support::Partial)
        );
        assert_eq!(linux.entries().len(), ROWS.len());
        assert_eq!(Capabilities::names().count(), ROWS.len());
    }

    #[test]
    fn linux_takes_no_version_and_bad_versions_fail() {
        assert!(Capabilities::for_platform(Platform::Linux, Some("6.1")).is_err());
        assert!(Capabilities::for_platform(Platform::Windows, Some("ten")).is_err());
        assert!(Capabilities::for_platform(Platform::Windows, Some("")).is_err());
        let caps = Capabilities::for_platform(Platform::Macos, Some("14.5")).expect("valid");
        assert_eq!(caps.os_version.as_deref(), Some("14.5"));
    }

    #[test]
    fn versions_compare_numerically() {
        let v = |s| parse_version(s).expect("valid");
        assert!(version_lt(&v("10.0.16299"), &v("10.0.17134")));
        assert!(!version_lt(&v("10.0.17134"), &v("10.0.17134")));
        assert!(version_lt(&v("9"), &v("10")));
        assert!(!version_lt(&v("14.0"), &v("14")));
        assert!(version_lt(&v("14"), &v("14.0.1")));
    }

    #[test]
    fn since_narrows_support_when_a_version_is_known() {
        // No production row is version-gated yet, so exercise `resolve`, the
        // function `build` calls, on an entry built here.
        let gated = yes_by("UIA HeadingLevel").since("10.0.17134");
        let (support, note) = resolve(gated, Platform::Windows, Some("10.0.16299"));
        assert_eq!(support, Support::Unsupported);
        let note = note.expect("reason");
        assert!(
            note.starts_with("requires Windows 10.0.17134 or later; 10.0.16299 is older"),
            "{note}"
        );
        assert!(note.ends_with("UIA HeadingLevel"), "{note}");
        assert_eq!(
            resolve(gated, Platform::Windows, Some("10.0.19045")).0,
            Support::Supported
        );
        assert_eq!(
            resolve(gated, Platform::Windows, None).0,
            Support::Supported
        );
    }

    #[test]
    fn windows_ver_output_parses() {
        assert_eq!(
            parse_windows_ver("\r\nMicrosoft Windows [Version 10.0.22631.4317]\r\n").as_deref(),
            Some("10.0.22631.4317")
        );
        assert_eq!(parse_windows_ver("garbage"), None);
    }

    #[test]
    fn platform_names_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::parse(p.to_snake_case()).ok(), Some(p));
        }
        assert!(Platform::parse("darwin").is_err());
    }
}
