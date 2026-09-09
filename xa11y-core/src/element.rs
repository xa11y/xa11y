use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::provider::Provider;
use crate::role::Role;

/// Declare a reader/writer struct pair from one field list.
///
/// `#[non_exhaustive]` gives *readers* a stability guarantee: adding a field
/// does not break a consumer that only reads one. *Writers* need the opposite
/// — a new field should stop their build until they have decided what it
/// means. Two types satisfy both, but only if their field sets cannot drift,
/// which is what this macro enforces: there is one list, so a field cannot be
/// added to one type and defaulted in the other.
///
/// The reader gets the doc comments, serde attributes, and `#[non_exhaustive]`.
/// The writer gets the bare fields and stays exhaustive, so a struct literal
/// in another crate fails to compile the moment the list grows.
macro_rules! reader_writer_pair {
    (
        $(#[$reader_meta:meta])*
        pub struct $reader:ident;

        $(#[$writer_meta:meta])*
        pub struct $writer:ident;

        fields {
            $(
                $(#[$field_meta:meta])*
                pub $field:ident : $ty:ty,
            )*
        }
    ) => {
        $(#[$reader_meta])*
        #[non_exhaustive]
        pub struct $reader {
            $(
                $(#[$field_meta])*
                pub $field: $ty,
            )*
        }

        $(#[$writer_meta])*
        #[doc(hidden)]
        pub struct $writer {
            $(pub $field: $ty,)*
        }

        impl From<$writer> for $reader {
            fn from(parts: $writer) -> Self {
                // Destructured, not field-by-field: both halves come from the
                // macro's single field list, so neither can gain a field the
                // other silently defaults.
                let $writer { $($field,)* } = parts;
                Self { $($field,)* }
            }
        }
    };
}

// Importable by path from sibling modules (`Event` uses it too).
pub(crate) use reader_writer_pair;

reader_writer_pair! {
    /// The raw data for a single element in an accessibility tree.
    ///
    /// This is the underlying data struct. Most consumers should use
    /// [`Element`], which wraps `ElementData` with a provider reference for
    /// lazy navigation. `ElementData` is used directly by provider
    /// implementors.
    ///
    /// `#[non_exhaustive]`: this is the type that grows every time the
    /// normalized element model learns a new property, so adding a field must
    /// not break the consumers that only ever *read* one. Providers, which
    /// *write* one, get the opposite guarantee from [`ElementParts`].
    ///
    /// Build a partial element (an event target, a test fixture) with
    /// [`ElementData::for_role`] and assign what you have:
    ///
    /// ```
    /// # use xa11y_core::{ElementData, Role};
    /// let mut data = ElementData::for_role(Role::Button);
    /// data.name = Some("Submit".to_string());
    /// ```
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ElementData;

    /// Every field a provider must decide on when it builds a *complete*
    /// element from a platform node.
    ///
    /// Deliberately exhaustive: a struct literal in `xa11y-linux`,
    /// `xa11y-macos`, or `xa11y-windows` stops compiling the moment a field
    /// is added, which is the only thing that forces a per-platform decision
    /// instead of a silent `None` on every backend.
    ///
    /// Paths that are partial by nature — an event target with no bounds, a
    /// test fixture — should use [`ElementData::for_role`] instead and accept
    /// that a new field arrives there as its default.
    ///
    /// Not public API (`#[doc(hidden)]`). Because a new field here is a
    /// compile error in the sibling provider crates, they pin `xa11y-core`
    /// with `=` rather than a caret requirement — see the workspace
    /// `Cargo.toml`.
    #[allow(
        clippy::exhaustive_structs,
        reason = "This type IS the completeness guard. Literal construction \
                  from the provider crates is exactly what makes a new \
                  ElementData field fail their build until each platform maps \
                  it; #[non_exhaustive] here would delete the property it \
                  exists for."
    )]
    #[derive(Debug, Clone)]
    pub struct ElementParts;

    fields {
        /// Element role
        pub role: Role,

        /// Human-readable name (title, label).
        ///
        /// Stripped of Unicode bidi format controls (LRM, RLM, embeddings,
        /// overrides, isolates) so equality assertions match the logical text.
        /// The unstripped platform string is preserved in [`Self::raw`] under the
        /// platform-native key (e.g. `AXTitle` on macOS, `atspi_name` on Linux,
        /// `uia_name` on Windows). See [`crate::text::strip_bidi`].
        pub name: Option<String>,

        /// Current value (text content, slider position, etc.).
        ///
        /// Stripped of Unicode bidi format controls. The unstripped platform
        /// string is preserved in [`Self::raw`] (`AXValue` on macOS, `atspi_value`
        /// on Linux, `uia_value` on Windows). See [`crate::text::strip_bidi`].
        pub value: Option<String>,

        /// Supplementary description (tooltip, help text).
        ///
        /// Stripped of Unicode bidi format controls. The unstripped platform
        /// string is preserved in [`Self::raw`] (`AXDescription`/`AXHelp` on
        /// macOS, `atspi_description` on Linux, `uia_help_text` on Windows).
        /// See [`crate::text::strip_bidi`].
        pub description: Option<String>,

        /// Bounding rectangle in **logical** screen coordinates
        /// (device-independent points), origin at the top-left of the primary
        /// display. This is the same coordinate space accepted by
        /// [`crate::ScreenshotProvider::capture_region`] and by the input layer's
        /// [`crate::input::Point`], so bounds can be fed directly to
        /// `screenshot_element` / `click` without conversion.
        ///
        /// To map to physical device pixels (e.g. to index into a captured image),
        /// multiply by the [`crate::Screenshot::scale`] reported for that display:
        /// `physical = logical × scale`. See [`Rect::to_physical`] /
        /// [`Rect::to_logical`].
        pub bounds: Option<Rect>,

        /// Available actions reported by the platform.
        ///
        /// Names are `snake_case` strings — well-known actions use their standard
        /// names (`"press"`, `"toggle"`, `"expand"`, etc.) and platform-specific
        /// actions use their converted names (e.g. macOS `AXCustomThing` →
        /// `"custom_thing"`).
        pub actions: Vec<String>,

        /// Current state flags
        pub states: StateSet,

        /// Numeric value for range controls (sliders, progress bars, spinners).
        pub numeric_value: Option<f64>,

        /// Minimum value for range controls.
        pub min_value: Option<f64>,

        /// Maximum value for range controls.
        pub max_value: Option<f64>,

        /// Platform-assigned stable identifier for cross-snapshot correlation.
        /// - macOS: `AXIdentifier`
        /// - Windows: native window handle (`hwnd:0x…`) for top-level windows,
        ///   `AutomationId` for the elements that have one — UIA excludes
        ///   top-level windows from the AutomationId contract, and the handle
        ///   is session-scoped (like the Linux object path; HWNDs are reused
        ///   after a window closes)
        /// - Linux: D-Bus `object_path`
        ///
        /// Not all elements have one.
        pub stable_id: Option<String>,

        /// Process ID of the application that owns this element.
        pub pid: Option<u32>,

        /// Platform-specific raw data
        pub raw: RawPlatformData,

        /// Opaque handle for the provider to look up the platform object.
        /// Not serialized — only valid within the provider that created it.
        #[serde(skip, default)]
        pub handle: u64,
    }
}

impl ElementData {
    /// An element with the given role and every other field empty.
    ///
    /// `states` starts at [`StateSet::default`] (enabled and visible, nothing
    /// else), and `handle` at `0` — providers assign their own.
    ///
    /// This is the *partial* construction path. A provider translating a real
    /// platform node should use [`ElementParts`] instead, so that a new field
    /// fails its build rather than arriving as a default.
    ///
    /// Named `for_role` rather than `new` because `ElementData` is flattened
    /// onto `Element` for the bindings-parity check, where a member called
    /// `new` would collide with the existing [`Element::new`].
    pub fn for_role(role: Role) -> Self {
        // Struct literal, not a builder: this lives in the defining crate, so
        // the compiler still checks it for completeness when a field is added.
        Self {
            role,
            name: None,
            value: None,
            description: None,
            bounds: None,
            actions: Vec::new(),
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: RawPlatformData::new(),
            handle: 0,
        }
    }
}

impl Default for ElementData {
    /// A [`Role::Unknown`] element with no properties.
    fn default() -> Self {
        Self::for_role(Role::Unknown)
    }
}

/// A live element with lazy navigation via a provider reference.
///
/// `Element` dereferences to [`ElementData`], so all properties (`role`, `name`,
/// `value`, `states`, etc.) are accessible via field access. Navigation
/// methods (`parent()`, `children()`) call the provider on demand.
///
/// Elements are cheap to clone (they share the provider via `Arc`).
#[derive(Clone)]
pub struct Element {
    data: ElementData,
    provider: Arc<dyn Provider>,
}

impl Deref for Element {
    type Target = ElementData;

    fn deref(&self) -> &ElementData {
        &self.data
    }
}

impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.data, f)
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name_part = self
            .data
            .name
            .as_ref()
            .map(|n| format!(" \"{}\"", n))
            .unwrap_or_default();
        let value_part = self
            .data
            .value
            .as_ref()
            .map(|v| format!(" value=\"{}\"", v))
            .unwrap_or_default();
        write!(
            f,
            "{}{}{}",
            self.data.role.to_snake_case(),
            name_part,
            value_part,
        )
    }
}

impl Serialize for Element {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.data.serialize(serializer)
    }
}

impl Element {
    /// Create an Element from raw data and a provider reference.
    pub fn new(data: ElementData, provider: Arc<dyn Provider>) -> Self {
        Self { data, provider }
    }

    /// Get the underlying ElementData.
    pub fn data(&self) -> &ElementData {
        &self.data
    }

    /// Get the provider reference.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Get direct children of this element.
    ///
    /// Each call queries the provider — results are not cached.
    pub fn children(&self) -> crate::error::Result<Vec<Element>> {
        let children = self.provider.get_children(Some(&self.data))?;
        Ok(children
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Get the parent element, if any (root-level elements have no parent).
    ///
    /// Each call queries the provider — results are not cached.
    pub fn parent(&self) -> crate::error::Result<Option<Element>> {
        let parent = self.provider.get_parent(&self.data)?;
        Ok(parent.map(|d| Element::new(d, Arc::clone(&self.provider))))
    }

    /// Get the process ID from the element data.
    pub fn pid(&self) -> Option<u32> {
        self.data.pid
    }

    /// Capture the subtree rooted at this element as a recursive snapshot.
    ///
    /// `max_depth` limits traversal depth: `0` = only this node (no children),
    /// `1` = node + direct children, and so on. `None` traverses the full subtree.
    pub fn tree(&self, max_depth: Option<usize>) -> crate::error::Result<TreeNode> {
        build_tree_node(self, max_depth, 0)
    }

    /// Render the subtree rooted at this element as an indented string.
    ///
    /// Each line is `{indent}{role} "{name}" [value="{value}"]`. Returns the
    /// string without printing it. Same depth semantics as [`Element::tree`].
    pub fn dump(&self, max_depth: Option<usize>) -> crate::error::Result<String> {
        let node = self.tree(max_depth)?;
        let mut out = String::new();
        write_tree_node(&node, 0, &mut out);
        Ok(out)
    }

    // ── Actions ─────────────────────────────────────────────────────
    //
    // Element actions invoke the platform via the captured provider handle —
    // they do **not** re-resolve the selector. If the underlying element has
    // been destroyed since this snapshot was taken, the provider returns a
    // platform-specific "gone" error. For resilient retry-on-change semantics,
    // use the equivalent method on [`crate::Locator`] instead.

    /// Click / invoke this element via the accessibility action layer.
    pub fn press(&self) -> crate::error::Result<()> {
        self.provider.press(&self.data)
    }

    /// Set keyboard focus to this element.
    pub fn focus(&self) -> crate::error::Result<()> {
        self.provider.focus(&self.data)
    }

    /// Remove keyboard focus from this element.
    pub fn blur(&self) -> crate::error::Result<()> {
        self.provider.blur(&self.data)
    }

    /// Toggle a two- or three-state control (checkbox, switch).
    pub fn toggle(&self) -> crate::error::Result<()> {
        self.provider.toggle(&self.data)
    }

    /// Select this element (list item, tab, row).
    pub fn select(&self) -> crate::error::Result<()> {
        self.provider.select(&self.data)
    }

    /// Expand a disclosure, menu, combo box, or tree item.
    pub fn expand(&self) -> crate::error::Result<()> {
        self.provider.expand(&self.data)
    }

    /// Collapse an expanded element.
    pub fn collapse(&self) -> crate::error::Result<()> {
        self.provider.collapse(&self.data)
    }

    /// Open this element's context menu or dropdown.
    pub fn show_menu(&self) -> crate::error::Result<()> {
        self.provider.show_menu(&self.data)
    }

    /// Increment a numeric control (slider, spinner) by its platform step.
    pub fn increment(&self) -> crate::error::Result<()> {
        self.provider.increment(&self.data)
    }

    /// Decrement a numeric control (slider, spinner) by its platform step.
    pub fn decrement(&self) -> crate::error::Result<()> {
        self.provider.decrement(&self.data)
    }

    /// Scroll this element into the visible area.
    ///
    /// No-op on macOS — the macOS accessibility API has no equivalent.
    pub fn scroll_into_view(&self) -> crate::error::Result<()> {
        self.provider.scroll_into_view(&self.data)
    }

    /// Set the text value of this element. Replaces the entire value rather
    /// than inserting at the caret — use [`Element::type_text`] for insertion.
    pub fn set_value(&self, value: &str) -> crate::error::Result<()> {
        self.provider.set_value(&self.data, value)
    }

    /// Set the numeric value of this element (slider, spinner).
    ///
    /// Returns [`Error::InvalidActionData`] if `value` is NaN or infinite.
    pub fn set_numeric_value(&self, value: f64) -> crate::error::Result<()> {
        if !value.is_finite() {
            return Err(Error::InvalidActionData {
                message: format!("set_numeric_value requires a finite value, got {}", value),
            });
        }
        self.provider.set_numeric_value(&self.data, value)
    }

    /// Insert text at the current cursor position.
    ///
    /// Uses the platform accessibility API — never simulates keyboard events.
    pub fn type_text(&self, text: &str) -> crate::error::Result<()> {
        self.provider.type_text(&self.data, text)
    }

    /// Select the text range from `start` to `end` (0-based character offsets).
    ///
    /// Returns [`Error::InvalidActionData`] if `start > end`.
    pub fn select_text(&self, start: u32, end: u32) -> crate::error::Result<()> {
        if start > end {
            return Err(Error::InvalidActionData {
                message: format!("select_text start ({}) must be <= end ({})", start, end),
            });
        }
        self.provider.set_text_selection(&self.data, start, end)
    }

    /// Perform an action by its `snake_case` name.
    ///
    /// Use this for actions the element advertises in its [`actions`](ElementData::actions)
    /// list that don't have a dedicated method. Well-known names (`"press"`,
    /// `"focus"`, etc.) also work — providers delegate to the named methods.
    /// The nullary window verbs are routed through the typed methods so the
    /// shared role guard applies on this path too: `perform_action("raise")`
    /// on a non-window element must fail exactly like `raise()` does, however
    /// the generic escape hatch is reached.
    pub fn perform_action(&self, action: &str) -> crate::error::Result<()> {
        match action {
            "raise" => self.raise(),
            "minimize" => self.minimize(),
            "maximize" => self.maximize(),
            "restore" => self.restore(),
            "close" => self.close(),
            // "move_to"/"resize_to" need payloads the generic path cannot
            // carry; the providers reject them surfaceably (InvalidActionData),
            // before any OS call.
            _ => self.provider.perform_action(&self.data, action),
        }
    }

    // ── Window management ──────────────────────────────────────────
    //
    // These verbs operate on top-level window targets. The shared layer keeps
    // the obvious non-window roles out before delegation: inside a provider
    // the same call on a button has platform-dependent semantics (Linux
    // `raise` would GrabFocus it and report success; the mock would accept any
    // live node). Providers then enforce the stronger identity check for the
    // window-like roles whose meaning is broader than a real OS window (for
    // example `Role::Dialog` can be an in-page ARIA dialog).
    //
    // Multiple windows can be managed from any window element, not just the
    // app root. The platform semantics are:
    // - `minimize`/`maximize`/`restore`/`close`: window state operations.
    // - `raise`: bring the window to the foreground (activation).
    // - `move_to`/`resize_to`: geometry operations in logical coordinates.

    /// Reject a window verb whose target is not a plausible top-level window.
    ///
    /// The shared check is intentionally stricter than role alone: web/ARIA
    /// descendants can map to `Role::Dialog` without being an OS window. A
    /// candidate must therefore carry at least one window-only signal
    /// (advertised window action or readable window state). This is a
    /// first-pass filter, not the authoritative boundary: providers make the
    /// final platform-specific top-level check.
    fn require_window_like(&self, action: &str) -> crate::error::Result<()> {
        let has_window_signal = self.data.actions.iter().any(|a| {
            matches!(
                a.as_str(),
                "raise" | "minimize" | "maximize" | "restore" | "close" | "move_to" | "resize_to"
            )
        }) || self.data.states.minimized.is_some()
            || self.data.states.maximized.is_some()
            || self.data.states.fullscreen.is_some();
        if matches!(self.data.role, Role::Window)
            || (self.data.role == Role::Dialog && has_window_signal)
        {
            Ok(())
        } else {
            Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: self.data.role,
            })
        }
    }

    /// Raise this window to the foreground.
    pub fn raise(&self) -> crate::error::Result<()> {
        self.require_window_like("raise")?;
        self.provider.raise(&self.data)
    }

    /// Minimize this window.
    pub fn minimize(&self) -> crate::error::Result<()> {
        self.require_window_like("minimize")?;
        self.provider.minimize(&self.data)
    }

    /// Maximize this window.
    pub fn maximize(&self) -> crate::error::Result<()> {
        self.require_window_like("maximize")?;
        self.provider.maximize(&self.data)
    }

    /// Restore this window to its normal state (from minimized/maximized).
    pub fn restore(&self) -> crate::error::Result<()> {
        self.require_window_like("restore")?;
        self.provider.restore(&self.data)
    }

    /// Close this window.
    pub fn close(&self) -> crate::error::Result<()> {
        self.require_window_like("close")?;
        self.provider.close(&self.data)
    }

    /// Move this window to the given **logical** screen coordinates (top-left
    /// origin, same space as [`ElementData::bounds`]).
    pub fn move_to(&self, x: i32, y: i32) -> crate::error::Result<()> {
        self.require_window_like("move_to")?;
        self.provider.move_to(&self.data, x, y)
    }

    /// Resize this window to the given **logical** width and height.
    ///
    /// Returns [`Error::InvalidActionData`] if either dimension is 0.
    pub fn resize_to(&self, width: u32, height: u32) -> crate::error::Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidActionData {
                message: format!(
                    "resize_to requires positive width and height, got {width}x{height}"
                ),
            });
        }
        self.require_window_like("resize_to")?;
        self.provider.resize_to(&self.data, width, height)
    }
}

fn build_tree_node(
    element: &Element,
    max_depth: Option<usize>,
    depth: usize,
) -> crate::error::Result<TreeNode> {
    let children = if max_depth.is_none_or(|d| depth < d) {
        element
            .children()?
            .into_iter()
            .map(|child| build_tree_node(&child, max_depth, depth + 1))
            .collect::<crate::error::Result<Vec<_>>>()?
    } else {
        vec![]
    };
    Ok(TreeNode {
        role: element.data.role.to_snake_case().to_string(),
        name: element.data.name.clone(),
        value: element.data.value.clone(),
        children,
    })
}

fn write_tree_node(node: &TreeNode, depth: usize, out: &mut String) {
    use fmt::Write as _;
    let indent = "  ".repeat(depth);
    write!(out, "{}{}", indent, node.role).unwrap();
    if let Some(ref n) = node.name {
        write!(out, " \"{}\"", n).unwrap();
    }
    if let Some(ref v) = node.value {
        write!(out, " value=\"{}\"", v).unwrap();
    }
    out.push('\n');
    for child in &node.children {
        write_tree_node(child, depth + 1, out);
    }
}

reader_writer_pair! {
    /// Boolean state flags for an element.
    ///
    /// **Semantics for non-applicable states:** When a state doesn't apply to
    /// an element's role, the backend uses the platform's reported value or
    /// defaults:
    /// - `enabled`: `true` (elements are enabled unless explicitly disabled)
    /// - `visible`: `true` (elements are visible unless explicitly hidden/offscreen)
    /// - `focused`, `active`, `focusable`, `modal`, `selected`, `editable`, `required`, `busy`: `false`
    ///
    /// States that are inherently inapplicable use `Option`: `checked` is
    /// `None` for non-checkable elements, `expanded` is `None` for
    /// non-expandable elements. The window states (`minimized`, `maximized`,
    /// `fullscreen`) are `Option` for a second reason: `None` also means
    /// *unknown* — a platform that cannot read the state reports `None`
    /// rather than a guessed `false` (Linux cannot report maximized or
    /// fullscreen at all, and Windows cannot report fullscreen).
    ///
    /// `#[non_exhaustive]`: more states arrive in compatible releases and must
    /// not break readers. Providers building a complete state set use
    /// [`StateParts`], which is exhaustive — the documented defaults above are
    /// what a *partial* construction falls back to, not a licence for a
    /// backend to skip deciding.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct StateSet;

    /// Every state a provider must decide on when it translates a platform
    /// node's state bits.
    ///
    /// Deliberately exhaustive, for the same reason as [`ElementParts`]: a new
    /// state must fail each backend's build rather than silently inherit
    /// [`StateSet::default`]. That matters more here than the defaults
    /// suggest — the parity check requires every state to surface as a binding
    /// getter, so a silently-defaulted state ships as a documented API that no
    /// platform populates.
    ///
    /// Not public API (`#[doc(hidden)]`).
    #[allow(
        clippy::exhaustive_structs,
        reason = "This type IS the completeness guard for element state. See \
                  ElementParts; the same reasoning applies."
    )]
    #[derive(Debug, Clone)]
    pub struct StateParts;

    fields {
        pub enabled: bool,
        pub visible: bool,
        pub focused: bool,
        /// Whether this element is the active (foreground) window — the window that
        /// currently receives the user's input. Only meaningful for window-like
        /// elements (windows, dialogs); `false` elsewhere. Distinct from `focused`,
        /// which is element-level keyboard focus. Platform mappings: the AT-SPI
        /// `ACTIVE` state (Linux), `AXMain` (macOS), and the foreground `HWND`
        /// (Windows).
        #[serde(default)]
        pub active: bool,
        /// Whether the window is minimized (iconified).
        ///
        /// `None` = unknown or not a window. Unlike `enabled`/`visible`, the
        /// three window states are `Option<bool>` because `None` also means
        /// "this platform cannot report the state": Linux cannot read
        /// maximized or fullscreen at all, and Windows cannot read fullscreen
        /// (it reads `minimized` and `maximized` from
        /// `WindowPattern.CurrentWindowVisualState`), so a hard-coded `false`
        /// would be a silent guess.
        pub minimized: Option<bool>,
        /// Whether the window is maximized. `None` = unknown / not a window.
        pub maximized: Option<bool>,
        /// Whether the window is in fullscreen. `None` = unknown / not a window.
        pub fullscreen: Option<bool>,
        /// None = not checkable
        pub checked: Option<Toggled>,
        pub selected: bool,
        /// None = not expandable
        pub expanded: Option<bool>,
        pub editable: bool,
        /// Whether the element can receive keyboard focus
        pub focusable: bool,
        /// Whether the element is a modal dialog
        pub modal: bool,
        /// Form field required
        pub required: bool,
        /// Async operation in progress
        pub busy: bool,
    }
}

impl Default for StateSet {
    fn default() -> Self {
        Self {
            enabled: true,
            visible: true,
            focused: false,
            active: false,
            minimized: None,
            maximized: None,
            fullscreen: None,
            checked: None,
            selected: false,
            expanded: None,
            editable: false,
            focusable: false,
            modal: false,
            required: false,
            busy: false,
        }
    }
}

/// Tri-state toggle value.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a toggle is off, on, or indeterminate. Every \
              platform's tri-state checkbox is exactly these three values, \
              and a fourth would not be a toggle."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Toggled {
    Off,
    On,
    /// Indeterminate / tri-state
    Mixed,
}

/// Screen-pixel bounding rectangle (origin + size).
/// `x`/`y` are signed to support negative multi-monitor coordinates.
/// `width`/`height` are unsigned (always non-negative).
#[allow(
    clippy::exhaustive_structs,
    reason = "Closed domain: an axis-aligned rectangle is fully described by \
              an origin and a size. Literal construction is the point of the \
              type, and it will not gain a fifth field."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Convert a **logical** rectangle to **physical** device pixels by
    /// multiplying every component by `scale` (the physical-to-logical ratio,
    /// e.g. `1.5` at 150% or `2.0` on a typical Retina display).
    ///
    /// This is the inverse of [`Rect::to_logical`]. Each field is rounded to
    /// the nearest integer independently; for a single rectangle the position
    /// and size therefore round separately, which can differ by 1px from
    /// scaling the far edge — acceptable for capture/hit-test use where a 1px
    /// slack is expected on fractional scales.
    ///
    /// A non-finite or non-positive `scale` is treated as `1.0` (identity):
    /// callers on platforms without a known scale factor pass `1.0`, and a
    /// bogus value must never produce garbage coordinates.
    #[must_use]
    pub fn to_physical(self, scale: f64) -> Rect {
        let s = sane_scale(scale);
        Rect {
            x: scale_i32(self.x, s),
            y: scale_i32(self.y, s),
            width: scale_u32(self.width, s),
            height: scale_u32(self.height, s),
        }
    }

    /// Convert a **physical** rectangle (device pixels) to **logical**
    /// coordinates by dividing every component by `scale`. Inverse of
    /// [`Rect::to_physical`]. See that method for rounding and `scale`
    /// validity semantics.
    #[must_use]
    pub fn to_logical(self, scale: f64) -> Rect {
        self.to_physical(1.0 / sane_scale(scale))
    }
}

/// Clamp a scale factor to a usable positive, finite value. Non-finite or
/// non-positive inputs collapse to `1.0` so a bad platform reading degrades
/// to identity rather than producing nonsense coordinates.
pub(crate) fn sane_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn scale_i32(v: i32, scale: f64) -> i32 {
    (f64::from(v) * scale).round() as i32
}

fn scale_u32(v: u32, scale: f64) -> u32 {
    let scaled = (f64::from(v) * scale).round();
    if scaled < 0.0 {
        0
    } else {
        scaled as u32
    }
}

#[cfg(test)]
mod rect_scale_tests {
    use super::Rect;

    const R: Rect = Rect {
        x: 100,
        y: 200,
        width: 300,
        height: 40,
    };

    #[test]
    fn scale_one_is_identity() {
        assert_eq!(R.to_physical(1.0), R);
        assert_eq!(R.to_logical(1.0), R);
    }

    #[test]
    fn to_physical_multiplies_all_fields() {
        assert_eq!(
            R.to_physical(2.0),
            Rect {
                x: 200,
                y: 400,
                width: 600,
                height: 80
            }
        );
    }

    #[test]
    fn to_logical_divides_all_fields() {
        // Physical bounds on a 150% display -> logical points.
        let physical = Rect {
            x: 150,
            y: 300,
            width: 450,
            height: 60,
        };
        assert_eq!(
            physical.to_logical(1.5),
            Rect {
                x: 100,
                y: 200,
                width: 300,
                height: 40
            }
        );
    }

    #[test]
    fn round_trip_preserves_within_one_px() {
        for &scale in &[1.25_f64, 1.5, 1.75, 2.0] {
            let back = R.to_physical(scale).to_logical(scale);
            assert!((back.x - R.x).abs() <= 1, "x drift at {scale}");
            assert!((back.y - R.y).abs() <= 1, "y drift at {scale}");
            assert!(
                (back.width as i64 - R.width as i64).abs() <= 1,
                "w drift at {scale}"
            );
            assert!(
                (back.height as i64 - R.height as i64).abs() <= 1,
                "h drift at {scale}"
            );
        }
    }

    #[test]
    fn negative_origin_scales_correctly() {
        // Multi-monitor: a window on a display left of the primary.
        let r = Rect {
            x: -1920,
            y: -100,
            width: 200,
            height: 100,
        };
        assert_eq!(
            r.to_physical(2.0),
            Rect {
                x: -3840,
                y: -200,
                width: 400,
                height: 200
            }
        );
    }

    #[test]
    fn fractional_scale_rounds_to_nearest() {
        let r = Rect {
            x: 3,
            y: 3,
            width: 5,
            height: 5,
        };
        // 3 * 1.5 = 4.5 -> 5 (round half away from zero via f64::round);
        // 5 * 1.5 = 7.5 -> 8.
        assert_eq!(
            r.to_physical(1.5),
            Rect {
                x: 5,
                y: 5,
                width: 8,
                height: 8
            }
        );
    }

    #[test]
    fn bad_scale_degrades_to_identity() {
        assert_eq!(R.to_physical(0.0), R);
        assert_eq!(R.to_physical(-2.0), R);
        assert_eq!(R.to_physical(f64::NAN), R);
        assert_eq!(R.to_physical(f64::INFINITY), R);
        assert_eq!(R.to_logical(0.0), R);
    }
}

/// Platform-specific raw data attached to every element.
///
/// An untyped key-value map containing the original platform-specific data
/// exactly as the platform reported it. Keys use `snake_case` naming. This is
/// the escape hatch for consumers who need full platform fidelity.
pub type RawPlatformData = HashMap<String, serde_json::Value>;

/// A node in a recursive snapshot of the accessibility subtree.
///
/// Returned by [`Element::tree`] and [`Locator::tree`]. Each node carries the
/// role, display name, and value of one element, plus its children recursively.
/// `children` is empty when `max_depth` was reached or the element is a leaf.
///
/// `#[non_exhaustive]`: a dump node grows alongside [`ElementData`] — bounds
/// and stable ids are both plausible additions. Build one with
/// [`TreeNode::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TreeNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// A leaf node with the given role and no name, value, or children.
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            name: None,
            value: None,
            children: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `Element` action methods. Verifies each action records the
    //! expected entry in the mock provider's action log and that validation
    //! errors fire before the provider is ever called.

    use super::*;
    use crate::mock::{build_provider, MockProvider};
    use crate::selector::Selector;

    /// Resolve `selector` against the mock tree and return the first match
    /// wrapped in an `Element`. Panics on no match — these are unit tests, not
    /// production paths.
    fn find_element(provider: &Arc<MockProvider>, selector: &str) -> Element {
        let parsed = Selector::parse(selector).expect("selector must parse");
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let root = provider_dyn
            .list_apps()
            .expect("list_apps must succeed")
            .into_iter()
            .next()
            .expect("mock provider must expose an application root");
        let mut matches = provider_dyn
            .find_elements(&root, &parsed, Some(1), None)
            .expect("find_elements must succeed");
        let data = matches.pop().expect("selector matched no elements");
        Element::new(data, provider_dyn)
    }

    fn last_action(provider: &Arc<MockProvider>) -> (u64, String, Option<String>) {
        provider
            .actions()
            .last()
            .cloned()
            .expect("expected at least one recorded action")
    }

    #[test]
    fn nullary_actions_record_correct_name() {
        let provider = build_provider();
        let cases = [
            (r#"button[name="Back"]"#, "press" as &str),
            (r#"button[name="Back"]"#, "focus"),
            (r#"button[name="Back"]"#, "blur"),
            (r#"check_box[name="Agree"]"#, "toggle"),
            (r#"list_item[name="Item 1"]"#, "select"),
            (r#"list[name="Items"]"#, "expand"),
            (r#"list[name="Items"]"#, "collapse"),
            (r#"button[name="Back"]"#, "show_menu"),
            (r#"slider[name="Volume"]"#, "increment"),
            (r#"slider[name="Volume"]"#, "decrement"),
            (r#"button[name="Back"]"#, "scroll_into_view"),
        ];
        for (selector, action) in cases {
            provider.clear_actions();
            let el = find_element(&provider, selector);
            match action {
                "press" => el.press().unwrap(),
                "focus" => el.focus().unwrap(),
                "blur" => el.blur().unwrap(),
                "toggle" => el.toggle().unwrap(),
                "select" => el.select().unwrap(),
                "expand" => el.expand().unwrap(),
                "collapse" => el.collapse().unwrap(),
                "show_menu" => el.show_menu().unwrap(),
                "increment" => el.increment().unwrap(),
                "decrement" => el.decrement().unwrap(),
                "scroll_into_view" => el.scroll_into_view().unwrap(),
                _ => unreachable!(),
            }
            let (handle, name, data) = last_action(&provider);
            assert_eq!(
                name, action,
                "wrong action recorded for selector {selector}"
            );
            assert_eq!(data, None, "nullary action should not carry data");
            assert_eq!(handle, el.data.handle);
        }
    }

    #[test]
    fn set_value_records_text_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.set_value("world").unwrap();
        let (handle, name, data) = last_action(&provider);
        assert_eq!(handle, el.data.handle);
        assert_eq!(name, "set_value");
        assert_eq!(data.as_deref(), Some("world"));
    }

    #[test]
    fn set_numeric_value_records_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"slider[name="Volume"]"#);
        el.set_numeric_value(42.0).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "set_numeric_value");
        assert_eq!(data.as_deref(), Some("42"));
    }

    #[test]
    fn set_numeric_value_rejects_non_finite() {
        let provider = build_provider();
        let el = find_element(&provider, r#"slider[name="Volume"]"#);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                el.set_numeric_value(bad),
                Err(Error::InvalidActionData { .. })
            ));
        }
        // None of the validation failures should have reached the provider.
        assert!(provider.actions().is_empty());
    }

    #[test]
    fn type_text_records_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.type_text("abc").unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "type_text");
        assert_eq!(data.as_deref(), Some("abc"));
    }

    #[test]
    fn select_text_records_range() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.select_text(1, 4).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "set_text_selection");
        assert_eq!(data.as_deref(), Some("1..4"));
    }

    #[test]
    fn select_text_rejects_inverted_range() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        assert!(matches!(
            el.select_text(5, 2),
            Err(Error::InvalidActionData { .. })
        ));
        assert!(provider.actions().is_empty());
    }

    #[test]
    fn perform_action_records_arbitrary_name() {
        let provider = build_provider();
        let el = find_element(&provider, r#"button[name="Back"]"#);
        el.perform_action("custom_swipe").unwrap();
        let (_, name, _) = last_action(&provider);
        assert_eq!(name, "custom_swipe");
    }

    // ── Window management verbs ───────────────────────────────────

    #[test]
    fn window_verbs_record_correct_name() {
        let provider = build_provider();
        let cases = [
            ("window", "raise" as &str),
            ("window", "minimize"),
            ("window", "maximize"),
            ("window", "restore"),
            ("window", "close"),
        ];
        for (selector, action) in cases {
            provider.clear_actions();
            let el = find_element(&provider, selector);
            match action {
                "raise" => el.raise().unwrap(),
                "minimize" => el.minimize().unwrap(),
                "maximize" => el.maximize().unwrap(),
                "restore" => el.restore().unwrap(),
                "close" => el.close().unwrap(),
                _ => unreachable!(),
            }
            let (handle, name, data) = last_action(&provider);
            assert_eq!(name, action, "wrong action recorded for {selector}");
            assert_eq!(data, None, "nullary action should not carry data");
            assert_eq!(handle, el.data.handle);
        }
    }

    #[test]
    fn move_to_records_coordinate_payload() {
        let provider = build_provider();
        let el = find_element(&provider, "window");
        el.move_to(10, 20).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "move_to");
        assert_eq!(data.as_deref(), Some("10,20"));
    }

    #[test]
    fn resize_to_records_size_payload() {
        let provider = build_provider();
        let el = find_element(&provider, "window");
        el.resize_to(640, 480).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "resize_to");
        assert_eq!(data.as_deref(), Some("640x480"));
    }

    #[test]
    fn window_verbs_reject_non_window_targets() {
        // The role guard lives in the shared Element layer so behavior is
        // identical across platforms: Linux `raise` would otherwise reach a
        // button's GrabFocus and report success, and the mock would accept
        // any live node — a test that passed here could fail everywhere.
        let provider = build_provider();
        let button = find_element(&provider, r#"button[name="Back"]"#);
        let errors = [
            button.raise(),
            button.minimize(),
            button.maximize(),
            button.restore(),
            button.close(),
            button.move_to(0, 0),
            button.resize_to(100, 100),
        ];
        for err in errors {
            assert!(
                matches!(err, Err(Error::ActionNotSupported { .. })),
                "a window verb on a button must be ActionNotSupported, got {err:?}"
            );
        }
        // The generic escape hatch routes the nullary window verbs through
        // the typed methods, so it cannot dodge the role guard.
        assert!(
            matches!(
                button.perform_action("raise"),
                Err(Error::ActionNotSupported { .. })
            ),
            "perform_action(\"raise\") on a button must fail like raise() does"
        );
        assert!(
            matches!(
                button.perform_action("close"),
                Err(Error::ActionNotSupported { .. })
            ),
            "perform_action(\"close\") on a button must fail like close() does"
        );
        assert!(
            provider.actions().is_empty(),
            "rejected window verbs must not reach the provider"
        );
    }

    #[test]
    fn window_verbs_reject_dialogs_without_window_signals() {
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let dialog = Element::new(ElementData::for_role(Role::Dialog), provider_dyn);
        let errors = [
            dialog.raise(),
            dialog.minimize(),
            dialog.maximize(),
            dialog.restore(),
            dialog.close(),
            dialog.move_to(0, 0),
            dialog.resize_to(100, 100),
        ];
        for err in errors {
            assert!(
                matches!(err, Err(Error::ActionNotSupported { .. })),
                "a dialog with no window evidence must be rejected, got {err:?}"
            );
        }
        assert!(
            matches!(
                dialog.perform_action("raise"),
                Err(Error::ActionNotSupported { .. })
            ),
            "perform_action(\"raise\") on a non-window dialog must fail like raise() does"
        );
        assert!(
            provider.actions().is_empty(),
            "rejected dialog window verbs must not reach the provider"
        );
    }

    #[test]
    fn resize_to_rejects_zero_dimensions() {
        let provider = build_provider();
        let el = find_element(&provider, "window");
        for (w, h) in [(0u32, 100u32), (100, 0), (0, 0)] {
            assert!(matches!(
                el.resize_to(w, h),
                Err(Error::InvalidActionData { .. })
            ));
        }
        assert!(
            provider.actions().is_empty(),
            "validation failures must not reach the provider"
        );
    }

    #[test]
    fn minimize_then_restore_roundtrip_updates_state() {
        let provider = build_provider();
        let el = find_element(&provider, "window");
        el.minimize().unwrap();
        // Re-resolve: the window must now report minimized and be off-screen.
        let after = find_element(&provider, "window");
        assert_eq!(after.states.minimized, Some(true));
        // The mock decided the window is not maximized, so the real-provider
        // tri-state (UIA WindowVisualState_Minimized → (true, false)) must be
        // reported; `None` would mean "unknown", which it is not.
        assert_eq!(after.states.maximized, Some(false));
        assert!(!after.states.visible);
        el.restore().unwrap();
        let restored = find_element(&provider, "window");
        assert_eq!(restored.states.minimized, Some(false));
        assert_eq!(restored.states.maximized, Some(false));
        assert!(restored.states.visible);
    }

    #[test]
    fn locator_actions_desugar_to_element_actions() {
        // Locator's auto-wait wraps the resolved data in an Element and calls
        // its action — no duplication at the provider call site. This test
        // pins that behavior: pressing via the Locator should record exactly
        // the same entry as pressing via the Element it resolves to.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let locator = crate::locator::Locator::new(provider_dyn, None, r#"button[name="Back"]"#);
        locator.press().unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "press");
        assert_eq!(data, None);
    }

    #[test]
    fn locator_validation_runs_before_auto_wait() {
        // Locator validates payloads before entering its 5s auto-wait poll.
        // We verify by passing invalid input against a never-matching selector:
        // if validation fired first we get InvalidActionData immediately, not
        // a Timeout 5 seconds later.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let locator =
            crate::locator::Locator::new(provider_dyn, None, r#"button[name="never-matches"]"#);
        let started = std::time::Instant::now();
        let err = locator.set_numeric_value(f64::NAN).unwrap_err();
        assert!(matches!(err, Error::InvalidActionData { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "validation must short-circuit auto-wait",
        );
    }

    #[test]
    fn state_set_payload_without_window_fields_still_deserializes() {
        // A `states` payload serialized before the window-management release
        // has no `minimized` / `maximized` / `fullscreen` keys at all. Those
        // are `Option<bool>`, and serde's derived `Deserialize` treats a
        // missing `Option` field as `None` implicitly — unlike a plain field
        // such as `active`, which is why `active` carries `#[serde(default)]`
        // while the three window fields need nothing. This test pins that
        // backward-compat guarantee so a payload from a previous release stays
        // readable.
        let json = r#"{
            "enabled": true, "visible": true, "focused": false, "active": false,
            "checked": null, "selected": false, "expanded": null, "editable": false,
            "focusable": false, "modal": false, "required": false, "busy": false
        }"#;
        let states: StateSet = serde_json::from_str(json)
            .expect("payloads without the window state fields must still deserialize");
        assert_eq!(states.minimized, None);
        assert_eq!(states.maximized, None);
        assert_eq!(states.fullscreen, None);

        // And a payload that does carry a window state round-trips untouched.
        let mut data = ElementData::for_role(Role::Window);
        data.states.minimized = Some(true);
        let json = serde_json::to_string(&data.states).unwrap();
        assert!(json.contains("\"minimized\":true"));
        let round: StateSet = serde_json::from_str(&json).unwrap();
        assert_eq!(round.minimized, Some(true));
        assert_eq!(round.maximized, None);
    }
}
