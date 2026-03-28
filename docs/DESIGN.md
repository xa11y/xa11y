# xa11y — Cross-Platform Accessibility Client Library

## Overview

**xa11y** is a Rust library for reading and interacting with accessibility trees across desktop (and eventually mobile) platforms. It provides a unified API over platform-specific accessibility APIs, letting consumers query UI structure and perform actions without writing platform-specific code.

This library is designed to replace the cross-platform accessibility layer in [agent-desktop](https://github.com/crowecawcaw/agent-desktop), with first-class support for FFI bindings (Python, JavaScript) and future mobile platforms (iOS, Android).

### Design Principles

1. **Client-side focus** — xa11y *reads* accessibility trees exposed by other applications. It is not a toolkit for *providing* accessibility (that's what accesskit does). However, we borrow accesskit's data model where it makes sense.
2. **Platform concepts mapped, not hidden** — Each platform has unique behaviors. The abstraction normalizes them but exposes raw/platform-specific data when needed.
3. **Snapshot-based** — Trees are captured as point-in-time snapshots. Platform handles are always cached internally for action dispatch and are not exposed to the consumer.
4. **FFI-first** — Core types are designed to be representable in C, Python, and JavaScript. No lifetimes or complex generics in the public API.

---

## Core Abstractions

### 1. `Role` — What an element *is*

A normalized enum covering UI element types across all platforms. Derived from ARIA roles (like accesskit), but scoped to the roles commonly surfaced by real desktop applications.

```rust
#[repr(u8)]
pub enum Role {
    Unknown,
    Window,
    Application,
    Button,
    CheckBox,
    RadioButton,
    TextField,         // Single-line text input
    TextArea,          // Multi-line text input
    StaticText,        // Non-editable text / label
    ComboBox,
    List,
    ListItem,
    Menu,
    MenuItem,
    MenuBar,
    Tab,
    TabGroup,
    Table,
    TableRow,
    TableCell,
    Toolbar,
    ScrollBar,
    Slider,
    Image,
    Link,
    Group,             // Generic container
    Dialog,
    Alert,
    ProgressBar,
    TreeItem,
    WebArea,           // Web content area / document
    Heading,
    Separator,
    SplitGroup,
    // Future expansion for mobile
    // Switch,
    // NavigationBar,
    // PageIndicator,
}
```

#### Platform Mapping

| xa11y Role | macOS (AX) | Windows (UIA) | Linux (AT-SPI) |
|---|---|---|---|
| `Window` | AXWindow, AXSheet | UIA_WindowControlTypeId | Frame, Window |
| `Button` | AXButton | UIA_ButtonControlTypeId | PushButton, PushButtonMenu |
| `TextField` | AXTextField, AXSearchField, AXSecureTextField | UIA_EditControlTypeId (single-line) | Entry, PasswordText, SpinButton |
| `TextArea` | AXTextArea | UIA_EditControlTypeId (multi-line) | Text (multi-line) |
| `StaticText` | AXStaticText | UIA_TextControlTypeId | Label, Static, Caption |
| `CheckBox` | AXCheckBox | UIA_CheckBoxControlTypeId | CheckBox, CheckMenuItem |
| `RadioButton` | AXRadioButton | UIA_RadioButtonControlTypeId | RadioButton, RadioMenuItem |
| `ComboBox` | AXComboBox, AXPopUpButton | UIA_ComboBoxControlTypeId | ComboBox |
| `List` | AXList, AXOutline | UIA_ListControlTypeId | List, ListBox |
| `ListItem` | AXRow | UIA_ListItemControlTypeId | ListItem |
| `Menu` | AXMenu | UIA_MenuControlTypeId | Menu |
| `MenuItem` | AXMenuItem | UIA_MenuItemControlTypeId | MenuItem, TearoffMenuItem |
| `MenuBar` | AXMenuBar, AXMenuBarItem | UIA_MenuBarControlTypeId | MenuBar |
| `Tab` | *(via TabGroup children)* | UIA_TabItemControlTypeId | PageTab |
| `TabGroup` | AXTabGroup | UIA_TabControlTypeId | PageTabList |
| `Table` | AXTable | UIA_TableControlTypeId, UIA_DataGridControlTypeId | Table, TreeTable |
| `TableRow` | *(via Table children)* | UIA_DataItemControlTypeId | TableRow |
| `TableCell` | AXCell | UIA_HeaderControlTypeId, UIA_HeaderItemControlTypeId | TableCell, TableColumnHeader, TableRowHeader |
| `Toolbar` | AXToolbar | UIA_ToolBarControlTypeId | ToolBar |
| `ScrollBar` | AXScrollBar | UIA_ScrollBarControlTypeId | ScrollBar |
| `Slider` | AXSlider | UIA_SliderControlTypeId | Slider |
| `Image` | AXImage | UIA_ImageControlTypeId | Image, Icon, DesktopIcon |
| `Link` | AXLink | UIA_HyperlinkControlTypeId | Link |
| `Group` | AXGroup, AXLayoutArea, AXScrollArea | UIA_GroupControlTypeId, UIA_PaneControlTypeId | Panel, Section, Form, Filler |
| `Dialog` | AXDialog | UIA_WindowControlTypeId with `IsDialog` property | Dialog, FileChooser |
| `Alert` | *(via AXDialog subrole)* | *(via UIA alert pattern)* | Alert, Notification |
| `ProgressBar` | AXProgressIndicator, AXBusyIndicator | UIA_ProgressBarControlTypeId | ProgressBar |
| `TreeItem` | AXDisclosureTriangle | UIA_TreeItemControlTypeId | TreeItem |
| `WebArea` | AXWebArea | UIA_DocumentControlTypeId | DocumentWeb, DocumentFrame |
| `Heading` | AXHeading | *(via landmark/heading pattern)* | Heading |
| `Separator` | AXSplitter | UIA_SeparatorControlTypeId | Separator |
| `SplitGroup` | AXSplitGroup | UIA_PaneControlTypeId (split pane) | SplitPane |
| `Application` | AXApplication | *(root element)* | Application |

**Edge cases:**
- macOS `AXSheet` maps to `Window` (modal sheet is conceptually a window).
- macOS `AXMenuBarItem` maps to `MenuBar` (it's a child of the menu bar, but acts as a menu trigger — could revisit).
- Windows `UIA_PaneControlTypeId` maps to `Group` — panes are generic containers in UIA.
- Linux AT-SPI has `PushButtonMenu` which is a button that opens a menu — maps to `Button`.
- Some platforms expose roles xa11y doesn't model (e.g., AXLayoutArea) — these map to `Group` or `Unknown`.

### 2. `Action` — What you can *do* to an element

A normalized enum of interactions. Inspired by accesskit's `Action` enum but scoped to client-observable actions.

```rust
#[repr(u8)]
pub enum Action {
    Press,          // Click / tap / invoke
    Focus,
    /// Set text content or numeric value.
    ///
    /// Accepts `ActionData::Value(String)` for text or
    /// `ActionData::NumericValue(f64)` for numeric values.
    ///
    /// **Platform note:** On Linux AT-SPI, the Value interface only supports
    /// numeric values (`f64`). Setting text requires the Text interface. The
    /// backend will attempt text input via the Text interface if the element
    /// is editable and `ActionData::Value` is provided. If text input is not
    /// supported, returns `Error::TextValueNotSupported`.
    SetValue,
    Toggle,         // CheckBox, Switch
    Expand,
    Collapse,
    Select,         // Selection in a list/table
    ShowMenu,       // Context menu / dropdown
    ScrollIntoView,
    Increment,      // Slider / spinner
    Decrement,
}
```

#### Platform Mapping

| xa11y Action | macOS | Windows (UIA Pattern) | Linux (AT-SPI) |
|---|---|---|---|
| `Press` | AXPress | InvokePattern.Invoke() | Action "click"/"activate"/"press" |
| `Focus` | Set AXFocused=true | Element.SetFocus() | Component.GrabFocus() |
| `SetValue` | Set AXValue attribute | ValuePattern.SetValue() | Value.SetCurrentValue() (numeric only, text via AT-SPI Text interface) |
| `Toggle` | AXPress (on checkbox) | TogglePattern.Toggle() | Action "toggle"/"check"/"uncheck" |
| `Expand` | AXShowMenu or AXPress | ExpandCollapsePattern.Expand() | Action "expand"/"open" |
| `Collapse` | AXCancel or AXPress | ExpandCollapsePattern.Collapse() | Action "collapse"/"close" |
| `Select` | AXPress | SelectionItemPattern.Select() | Action "select" |
| `ShowMenu` | AXShowMenu | *(no direct equivalent, use Expand or right-click)* | Action "menu"/"showmenu"/"popup" |
| `ScrollIntoView` | AXScrollToVisible | ScrollItemPattern.ScrollIntoView() | Component.ScrollTo() |
| `Increment` | AXIncrement | RangeValuePattern (adjust) | Action "increment" |
| `Decrement` | AXDecrement | RangeValuePattern (adjust) | Action "decrement" |

**Edge cases:**
- **SetValue on Linux AT-SPI:** The Value interface only supports numeric values. For text input, the Text interface must be used. xa11y will try Value first, then fall back to the Text interface if the element is editable. Returns `Error::TextValueNotSupported` if neither works.
- **Toggle on macOS:** There's no dedicated toggle — AXPress on a checkbox toggles it. xa11y maps both `Press` and `Toggle` to `AXPress` for checkboxes.
- **ShowMenu on Windows:** No direct pattern. Can be accomplished via keyboard simulation (Shift+F10) or by expanding a combo box. xa11y will attempt `ExpandCollapse.Expand()` as fallback.
- **Action discovery:** macOS reports actions via `AXUIElementCopyActionNames()`. Windows uses UIA patterns (each pattern implies certain actions). Linux AT-SPI reports actions via the Action interface with indexed names. xa11y normalizes all these into the `Action` enum.

### 3. `Node` — A snapshot handle for a single element

A `Node` is a handle into an accessibility tree snapshot. It wraps an `Arc<Tree>` and an index, so cloning is cheap and navigation (parent/children/query) uses the shared snapshot without any platform refetch.

```rust
/// The raw data for a single element (used by providers building trees).
pub struct NodeData {
    pub role: Role,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub bounds: Option<Rect>,
    pub actions: Vec<Action>,

    /// Current state flags
    pub states: StateSet,

    /// Child node indices (direct children only, internal use)
    pub(crate) children_indices: Vec<NodeIndex>,

    /// Parent node index (None for root, internal use)
    pub(crate) parent_index: Option<NodeIndex>,

    /// Depth in the tree (0 = root)
    pub depth: u32,

    /// Application name (useful when querying all apps)
    pub app_name: Option<String>,

    /// Platform-specific raw data (opt-in, for debugging)
    pub raw: Option<RawPlatformData>,
}

/// Internal index type, not part of the public API.
#[doc(hidden)]
pub type NodeIndex = u32;
```

#### `StateSet` — Boolean state flags

```rust
/// Boolean state flags for a node.
///
/// **Semantics for non-applicable states:** When a state doesn't apply to an
/// element's role (e.g., `enabled` on a `StaticText`), the backend uses the
/// platform's reported value or the following defaults:
/// - `enabled`: `true` (elements are enabled unless explicitly disabled)
/// - `visible`: `true` (elements are visible unless explicitly hidden/offscreen)
/// - `focused`, `selected`, `editable`, `required`, `busy`: `false`
///
/// States that are inherently inapplicable use `Option`: `checked` is `None`
/// for non-checkable elements, `expanded` is `None` for non-expandable elements.
pub struct StateSet {
    pub enabled: bool,
    pub visible: bool,
    pub focused: bool,
    pub checked: Option<Toggled>,  // None = not checkable
    pub selected: bool,
    pub expanded: Option<bool>,    // None = not expandable
    pub editable: bool,
    pub required: bool,            // form field required
    pub busy: bool,                // async operation in progress
}

pub enum Toggled {
    Off,
    On,
    Mixed,   // indeterminate / tri-state
}
```

#### Platform Mapping for States

| xa11y State | macOS | Windows (UIA) | Linux (AT-SPI) |
|---|---|---|---|
| `enabled` | AXEnabled attribute | CurrentIsEnabled | State::Enabled |
| `visible` | Has position + non-zero size | !CurrentIsOffscreen | State::Visible \|\| State::Showing |
| `focused` | AXFocused attribute | CurrentHasKeyboardFocus | State::Focused |
| `checked` | AXValue ("0"/"1") on checkbox/radio | TogglePattern.CurrentToggleState | State::Checked |
| `selected` | AXSelected attribute | SelectionItemPattern.IsSelected | State::Selected |
| `expanded` | AXExpanded attribute | ExpandCollapsePattern state | State::Expanded |
| `editable` | Role is TextField/TextArea | ValuePattern.CurrentIsReadOnly | State::Editable |

**Edge cases:**
- **Visibility on macOS:** There's no explicit "visible" attribute. An element is considered visible if it has a position with non-zero size. Offscreen elements with valid bounds are technically "visible" but may be clipped.
- **Checked on macOS:** Reported via AXValue as "0"/"1" strings, not a boolean. Must be parsed.
- **Toggled::Mixed:** Windows supports tri-state via ToggleState_Indeterminate. macOS uses AXValue "2". Linux uses a separate Mixed state. xa11y normalizes these.

### 4. `Rect` — Geometry

```rust
/// Screen-pixel bounding rectangle (origin + size).
/// `x`/`y` are signed to support negative multi-monitor coordinates.
/// `width`/`height` are unsigned (always non-negative).
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
```

Geometry uses accesskit-style `Rect` for pixel coordinates.

**Edge cases:**
- **Multi-monitor:** Coordinates are in the global screen space. On macOS, the origin (0,0) is top-left of the primary display. On Windows, coordinates can be negative for monitors left of or above the primary. On Linux/X11, coordinates are in the X screen space.
- **High-DPI / Retina:** macOS reports bounds in points (not pixels). Windows reports in physical pixels. Linux depends on the toolkit. xa11y reports whatever the platform API returns and documents the coordinate space.
- **Coordinate origin:** macOS accessibility uses top-left origin (unlike AppKit which uses bottom-left). Windows and Linux use top-left. xa11y uses top-left consistently.

### 5. `Node` handle — Snapshot navigation

The `Node` handle wraps `Arc<Tree>` + index, giving cheap cloning and snapshot navigation:

```rust
pub struct Node {
    snapshot: Arc<Tree>,
    index: u32,
}

impl Deref for Node {
    type Target = NodeData;
    // Access all NodeData fields directly: node.role, node.name, etc.
}

impl Node {
    pub fn parent(&self) -> Option<Node>;
    pub fn children(&self) -> Vec<Node>;
    pub fn subtree(&self) -> Vec<Node>;
    pub fn query(&self, selector: &str) -> Result<Vec<Node>>;
}
```

All navigation uses the shared snapshot — no platform refetch occurs.

`Tree` is an internal type (public in `xa11y-core` for provider implementors, but not part of the consumer-facing API). It stores nodes in DFS order as `Vec<NodeData>` and provides index-based access.

### 5a. Node vs Locator

xa11y has two main types: **Node** and **Locator**.

#### Node — Snapshot (read-only)

A Node is a point-in-time snapshot of a UI element. When you call `xa11y.app()`,
the library captures the entire accessibility tree and returns the root Node.

- **Navigation**: `node.parent()`, `node.children()` — traverse the snapshot
- **Queries**: `node.query("button")` — search within the snapshot
- **Properties**: `node.role`, `node.name`, `node.value`, `node.states` — read cached data
- **No refetch**: All operations use cached snapshot data. Zero platform calls.
- **Can go stale**: If the UI changes, the snapshot doesn't update. Call `app()` again.

#### Locator — Lazy (actions + fresh reads)

A Locator is a selector that re-evaluates against a fresh tree on every operation.
Inspired by Playwright's Locator pattern.

- **Actions**: `loc.press()`, `loc.set_value("text")` — refetches, finds element, acts
- **Reads**: `loc.name()`, `loc.is_visible()` — refetches, finds element, reads
- **Waits**: `loc.wait_visible(timeout=5)` — polls with fresh snapshots
- **Never stale**: Every call gets the latest state from the platform.
- **No navigation**: Locators don't have parent/children — use Node for that.

#### When to use which

| Need | Use |
|------|-----|
| Inspect UI structure | Node |
| Navigate parent/children | Node |
| Click a button | Locator |
| Wait for element to appear | Locator |
| Read a value that might change | Locator |
| Dump tree for debugging | Node |

#### Which operations refetch?

| Operation | Refetches? |
|-----------|-----------|
| `app()` / `apps()` | Yes — captures fresh snapshot |
| `node.parent()` / `node.children()` / `node.query()` | No — uses snapshot |
| `node.role` / `node.name` / `node.to_string()` | No — uses snapshot |
| `locator.press()` / `locator.set_value()` | Yes — refetches every time |
| `locator.name()` / `locator.is_visible()` | Yes — refetches every time |
| `locator.wait_*()` | Yes — polls with refetch |

### 6. `Selector` — CSS-like Query Language

Ported from agent-desktop's query system. Allows CSS-inspired selectors for finding elements in the tree.

```
button                          — match by role
[name="Submit"]                 — match by attribute
button[name="Submit"]           — role + attribute
[name*="addr"]                  — substring match (case-insensitive)
[name^="addr"]                  — starts-with match
toolbar > text_field             — direct child combinator
toolbar text_field               — descendant combinator
button:nth(2)                   — nth match (1-based)
toolbar > text_field[name*="Address"]  — combined
```

Supported attributes: `name`, `value`, `description`, `role`.

#### Formal Grammar

```
selector      := simple_selector (combinator simple_selector)*
combinator    := " "          // descendant (any depth)
               | " > "       // direct child

simple_selector := role_name? attr_filter* pseudo?

role_name     := [a-z_]+     // snake_case role name (e.g., text_field, menu_item)
                              // Maps to Role enum: text_field → Role::TextField

attr_filter   := "[" attr_name op value "]"
attr_name     := "name" | "value" | "description" | "role"
op            := "="          // exact match (case-sensitive)
               | "*="        // substring match (case-insensitive)
               | "^="        // starts-with match (case-insensitive)
               | "$="        // ends-with match (case-insensitive)
value         := '"' [^"]* '"'   // double-quoted string

pseudo        := ":nth(" integer ")"   // 1-based index among matches
integer       := [1-9][0-9]*
```

**Notes:**
- Role names use `snake_case` in selectors, mapping to `PascalCase` enum variants.
- Attribute presence (`[name]`) is not supported — use `[name*=""]` as a workaround.
- No comma-separated selector lists (union). Use multiple queries instead.

### 7. `Provider` — Platform backend trait

```rust
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree.
    /// Platform handles are cached internally for action dispatch.
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree>;

    /// Snapshot all running applications (shallow).
    fn get_apps(&self, opts: &QueryOptions) -> Result<Tree>;

    /// Perform an action on an element from a specific snapshot.
    ///
    /// The `node` parameter identifies the element to act on. The provider
    /// uses the node's internal index to look up the correct platform handle.
    /// If the handle is stale, the provider re-traverses to rebuild the cache.
    ///
    /// Multiple trees can coexist — calling `get_app_tree` does not
    /// invalidate handles from previous snapshots.
    fn perform_action(
        &self,
        tree: &Tree,
        node: &NodeData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;
}

pub enum AppTarget {
    /// Match by human-readable display name (case-insensitive, substring match).
    ///
    /// Resolution is platform-specific:
    /// - **macOS:** Matches against the localized app name (from NSRunningApplication).
    /// - **Windows:** Matches against window titles. Multiple windows may match;
    ///   the first (foreground) match is used.
    /// - **Linux:** Matches against AT-SPI application name from the registry.
    ///
    /// For precise targeting, use `ByPid` or `ByWindow`.
    ByName(String),
    ByPid(u32),
    /// Target a specific window by platform-specific handle
    ByWindow(WindowHandle),
}

pub struct QueryOptions {
    /// Maximum tree depth to traverse. `None` = unlimited.
    pub max_depth: Option<u32>,
    /// Maximum number of elements to collect. `None` = unlimited.
    pub max_elements: Option<u32>,
    pub visible_only: bool,
    pub roles: Option<Vec<Role>>,   // filter to specific roles
}
// Note: RawPlatformData is always populated on nodes. Platform handles are
// always cached internally for action dispatch.

pub enum ActionData {
    Value(String),
    NumericValue(f64),
    ScrollAmount { direction: ScrollDirection, amount: f64 },
    Point { x: f64, y: f64 },
}

pub enum PermissionStatus {
    Granted,
    Denied { instructions: String },
}

```

### 8. `Error` — Structured error type

All fallible operations return `Result<T, Error>`. Errors are designed to be
informative across FFI boundaries (Python exceptions, JS errors, C error codes).

```rust
#[derive(Debug)]
pub enum Error {
    /// Accessibility permissions not granted.
    /// Contains platform-specific instructions for enabling access.
    PermissionDenied { instructions: String },

    /// The target application was not found or is no longer running.
    AppNotFound { target: String },

    /// No element matched the given selector.
    SelectorNotMatched { selector: String },

    /// The node's platform handle is stale (UI changed since snapshot)
    /// and re-traversal could not relocate the element.
    ElementStale { selector: String },

    /// The requested action is not supported by this element.
    ActionNotSupported { action: Action, role: Role },

    /// Text value input is not supported for this element on this platform.
    /// Common on Linux AT-SPI where the Value interface only supports numerics.
    TextValueNotSupported,

    /// A wait_for or wait_for_event call exceeded its timeout.
    Timeout { elapsed: std::time::Duration },

    /// The selector string could not be parsed.
    InvalidSelector { selector: String, message: String },

    /// A platform-specific error occurred.
    Platform { code: i64, message: String },
}
```

### 9. `RawPlatformData` — Escape hatch

```rust
pub enum RawPlatformData {
    MacOS {
        ax_role: String,
        ax_subrole: Option<String>,
        ax_identifier: Option<String>,
    },
    Windows {
        control_type_id: i32,
        automation_id: Option<String>,
        class_name: Option<String>,
    },
    Linux {
        atspi_role: String,
        bus_name: String,
        object_path: String,
    },
}
```

---

## Architecture

### Crate Structure

```
xa11y/
├── xa11y-core/          # Types, traits, selectors (no platform code)
│   ├── role.rs          # Role enum
│   ├── action.rs        # Action enum
│   ├── node.rs          # Node, StateSet, Rect
│   ├── tree.rs          # Tree snapshot
│   ├── selector.rs      # CSS-like query parser + matcher
│   ├── provider.rs      # Provider trait, QueryOptions, AppTarget
│   └── lib.rs
│
├── xa11y-macos/         # macOS backend (AXUIElement via FFI)
├── xa11y-windows/       # Windows backend (UIA via windows-rs)
├── xa11y-linux/         # Linux backend (AT-SPI2 via atspi-rs / D-Bus)
├── xa11y-ios/           # (future) iOS backend (UIAccessibility)
├── xa11y-android/       # (future) Android backend (AccessibilityService)
│
├── xa11y/               # Umbrella crate — re-exports core + auto-selects platform
│   └── lib.rs           # pub use xa11y_core::*; + create_provider()
│
├── xa11y-ffi/           # C FFI layer (cbindgen)
├── xa11y-python/        # Python bindings (PyO3)
└── xa11y-node/          # Node.js bindings (napi-rs)
```

### Platform Backend Design

Each platform backend implements the `Provider` trait. Internally, each backend:

1. **Traverses** the platform's accessibility tree (DFS)
2. **Maps** platform-specific roles, states, and actions to xa11y types
3. **Builds** a `Tree` snapshot with sequential indices
4. **Caches** live element handles internally (keyed by NodeIndex) for action dispatch
5. **Re-traverses** on action dispatch to ensure handle validity (handles can go stale if the UI changes)

```
┌───────────────────────────────┐
│        Consumer Code          │
│  (Rust / Python / JS / C)     │
└──────────────┬────────────────┘
               │
┌──────────────▼────────────────┐
│         xa11y (umbrella)       │
│  create_provider() → Provider  │
└──────────────┬────────────────┘
               │
┌──────────────▼────────────────┐
│         xa11y-core             │
│  Tree, Node, Role, Action,    │
│  Selector, Provider trait      │
└──────────────┬────────────────┘
               │ impl Provider
    ┌──────────┼──────────┐
    │          │          │
┌───▼───┐ ┌───▼───┐ ┌────▼────┐
│ macOS │ │  Win  │ │  Linux  │
│  AX   │ │  UIA  │ │ AT-SPI  │
└───────┘ └───────┘ └─────────┘
```

### Action Dispatch Model

Actions follow the **re-traversal pattern** from agent-desktop:

1. Consumer calls `provider.perform_action(&tree, &node, Action::Press, None)`
2. Backend uses the node's internal index to look up its cached platform handle
3. If the handle is stale (or missing), backend re-traverses to rebuild the cache
4. Backend maps the xa11y `Action` to platform-specific calls
5. Returns `Ok(())` or `Err(Error::ElementStale)` if the element can't be relocated

Multiple trees can coexist — taking a new snapshot does not invalidate handles
from previous snapshots. The provider manages handle caches internally and evicts
them when the `Tree` is dropped (via a reference-counted guard).

This is necessary because:
- **macOS:** AXUIElementRef handles are process-local and can't be serialized. They may become invalid if the UI updates.
- **Windows:** IUIAutomationElement handles remain valid longer but can still go stale.
- **Linux:** AT-SPI uses D-Bus object paths that are relatively stable, but the element may have been destroyed.

---

## Edge Cases & Platform Quirks

### macOS
- **Permissions:** Requires "Accessibility" permission in System Preferences → Privacy & Security. Must be granted per-terminal-app. Detected via `AXIsProcessTrusted()`.
- **AXValue semantics:** Overloaded — it's the text content for text fields, "0"/"1" for checkboxes, slider position for sliders. xa11y normalizes: checkbox values go to `checked` state, everything else goes to `value`.
- **AXTitle vs AXDescription:** `AXTitle` is the primary label. `AXDescription` is secondary. `AXHelp` is tooltip text. xa11y maps: Title → `name`, Help → `description`, Description → `name` (fallback if no title).
- **Screen coordinates:** Uses points (not physical pixels on Retina). Origin is top-left of primary display.
- **Sheet dialogs:** Mapped to `Window`. They're modal but attached to a parent window.
- **No direct "visible" property:** Visibility inferred from having a position with non-zero dimensions.

### Windows
- **No special permissions needed** for local UIA queries.
- **COM initialization required:** `CoInitializeEx(COINIT_MULTITHREADED)` must be called before using UIA.
- **Pattern-based capability model:** Unlike macOS (action names) and Linux (action interface), Windows uses patterns. Each pattern implies capabilities (InvokePattern → pressable, ValuePattern → has value, etc.). xa11y queries patterns to determine available actions.
- **ContentView vs RawView:** UIA has two tree views. xa11y uses ContentView (skips layout/helper elements) by default, matching agent-desktop's behavior.
- **Negative coordinates:** Multi-monitor setups can have negative x/y values.
- **App by name:** UIA searches by window name, not process name. Finding by app name requires walking children of the desktop root.

### Linux
- **AT-SPI2 over D-Bus:** Async by nature. xa11y's Linux backend uses tokio internally but exposes a sync API (blocking on the runtime).
- **Permissions:** AT-SPI must be enabled. On GNOME: `gsettings set org.gnome.desktop.interface toolkit-accessibility true`. The `at-spi2-core` package must be installed.
- **Action name chaos:** AT-SPI actions have free-form string names. Different toolkits use different names for the same action ("click" vs "activate" vs "press"). xa11y normalizes via a mapping table.
- **Value interface limitations:** The AT-SPI Value interface only supports numeric values (f64). Setting text content requires the Text interface. xa11y will detect editable elements and use the appropriate interface.
- **App discovery:** Uses the AT-SPI registry at `org.a11y.atspi.Registry`. Each application registers as a child of the registry root.
- **Wayland vs X11:** Screen size detection differs. xa11y tries xdpyinfo (X11), then swaymsg (Sway/Wayland), then wlr-randr (wlroots), with a 1920x1080 fallback.
- **Bus name instability:** D-Bus bus names can change across connections. Object paths are more stable but may still be invalidated if the UI is rebuilt.

### Cross-Platform
- **Element index stability:** Internal indices are assigned in DFS order during traversal. If the UI changes between snapshot and action dispatch, indices may no longer match. The re-traversal mechanism mitigates this (using the platform handle cache), but there's an inherent race condition. If re-traversal cannot relocate the element, `Error::ElementStale` is returned.
- **Role granularity mismatch:** macOS has fewer roles (AXGroup covers many things), Windows has more specific control types, and Linux AT-SPI has the most roles. Normalization loses some information — the `raw` field on each node preserves the original platform data.
- **Text input:** Programmatic text input varies wildly:
  - macOS: Set AXValue attribute (works for text fields, not for all editable areas)
  - Windows: ValuePattern.SetValue() or TextPattern
  - Linux: Value interface (numeric only) or synthesized key events
  - xa11y provides `SetValue` as the primary mechanism and will document limitations per platform.
- **Focus model:** Each platform has subtly different focus semantics. macOS uses AXFocused, Windows has keyboard focus vs logical focus, Linux has State::Focused. xa11y normalizes to a single `focused` boolean.

---

## FFI Strategy

### Python (PyO3)

```python
import xa11y

provider = xa11y.create_provider()
provider.check_permissions()

tree = provider.get_app_tree(name="Safari")

# Query
buttons = tree.query("button")
submit = tree.query('button[name="Submit"]')

# Interact — tree + node are passed so the provider can look up handles
tree.press(submit[0])
tree.set_value(text_field, "hello@example.com")
```

### JavaScript/TypeScript (napi-rs)

```typescript
import { createProvider } from 'xa11y';

const provider = createProvider();
const tree = await provider.getAppTree({ name: 'Chrome' });

const buttons = tree.query('button');
await tree.press(buttons[0]);
```

### Design Constraints for FFI

- All public types are `Send + Sync`
- No lifetimes in public API — everything is owned
- `Tree` and `Node` are serializable to JSON
- Node identity is managed internally (`NodeIndex` is `#[doc(hidden)]`); consumers pass `&Node` references
- Error handling uses `Result<T, Error>` in Rust, exceptions in Python/JS
- Async: Linux backend is internally async but the public API is synchronous. Python/JS bindings can offer async wrappers.

---

## Relationship to AccessKit

AccessKit is a library for **providing** accessibility (making your app accessible to screen readers). xa11y is a library for **consuming** accessibility (reading other apps' accessibility trees and interacting with them).

We borrow from accesskit where concepts align:
- **Role enum:** Our role list is a subset of accesskit's ~150 roles, keeping only roles that commonly appear in desktop applications. We use the same names where possible.
- **Action enum:** Similar to accesskit's Action enum but scoped to client-observable actions (e.g., we don't need `SetTextSelection` or `SetSequentialFocusNavigationStartingPoint`).
- **Geometry types:** We use a similar `Rect` type. We don't need `Affine` transforms since we're reading screen coordinates, not widget-local coordinates.
- **Node ID:** AccessKit uses `u64` NodeIds (unique within a tree). We use `u32` sequential indices internally (`NodeIndex`, `#[doc(hidden)]`) that are not part of the public API. Consumers work with `&Node` references. Nodes may optionally carry a `stable_id: Option<String>` for cross-snapshot identification. AccessKit's IDs are assigned by the application; ours are assigned during traversal.

We intentionally **diverge** from accesskit in:
- **Tree model:** AccessKit uses `TreeUpdate` (incremental diffs). We use full snapshots because we're reading external state, not maintaining internal state.
- **Property model:** AccessKit uses a flat struct with ~100 optional properties. We use a simpler `Node` struct with commonly-needed fields and put the rest in `RawPlatformData`.
- **No `no_std`:** AccessKit core is `no_std`. xa11y requires `std` because platform APIs need OS interaction.

---

## Future: Mobile Platforms

### iOS (UIAccessibility)
- Read via `UIAccessibilityElement` and the accessibility hierarchy
- Roles map from `UIAccessibilityTraits` (bitmask-based, unlike macOS's string-based roles)
- Actions: `accessibilityActivate()`, `accessibilityIncrement()`, `accessibilityDecrement()`, `accessibilityScroll()`
- Requires running within the app's process or using XCTest framework for external access

### Android (AccessibilityService)
- Read via `AccessibilityNodeInfo`
- Roles map from `className` + `roleDescription` (less structured than iOS)
- Actions: `AccessibilityNodeInfo.AccessibilityAction` (ACTION_CLICK, ACTION_SET_TEXT, etc.)
- Requires an active AccessibilityService registration
- AccessKit already has an Android adapter — we can reference its role mappings

---

## Event Subscriptions

xa11y supports subscribing to accessibility events from running applications. This enables real-time monitoring, reactive automation, and serves as the foundation for a future Playwright-compatible desktop backend.

### Design Inspirations

- **Playwright** — `page.on(event, handler)`, `page.waitForEvent()`, `locator.waitFor({state})`. The event emitter + wait-for pattern is the primary design target. xa11y's event API should map cleanly to these patterns so a Playwright-compatible desktop adapter can delegate directly.
- **Node.js EventEmitter** — `on`, `off`, `once`, `removeAllListeners`. Familiar pattern for JS consumers via FFI.
- **Rust async streams** — `tokio::sync::broadcast` / `async_channel`. Events are inherently async; the Rust API uses streams with RAII-based unsubscription.
- **macOS AXObserver** — Register for named notifications on specific elements or the app root. Maps naturally to per-target subscriptions.
- **Windows UIA Event Handlers** — `AddAutomationEventHandler`, `AddFocusChangedEventHandler`, `AddPropertyChangedEventHandler`, `AddStructureChangedEventHandler`. Scoped by element + tree scope.
- **Linux AT-SPI2 D-Bus signals** — `object:state-changed:focused`, `object:property-change:accessible-value`, `object:children-changed`, etc. Event-name hierarchy with class:major:minor structure.

### Event Types

```rust
/// Categories of accessibility events, normalized across platforms.
#[repr(u8)]
pub enum EventKind {
    /// An element gained keyboard focus.
    /// Playwright mapping: essential for `locator.waitFor({state: 'visible'})` and focus tracking.
    FocusChanged,

    /// An element's value changed (text content, slider position, etc.).
    ValueChanged,

    /// An element's name/label changed.
    NameChanged,

    /// A boolean state flag changed (enabled, checked, expanded, selected, busy, etc.).
    /// The specific state is captured in `Event::state_flag`.
    StateChanged,

    /// Children were added or removed from an element.
    /// Playwright mapping: maps to element attached/detached detection.
    StructureChanged,

    /// A new window was created.
    /// Playwright mapping: `page.on('popup')` / `browserContext.on('page')`.
    WindowOpened,

    /// A window was closed/destroyed.
    /// Playwright mapping: `page.on('close')`.
    WindowClosed,

    /// A window was activated (brought to front / received focus).
    WindowActivated,

    /// A window was deactivated (lost focus to another window).
    WindowDeactivated,

    /// Selection changed in a list, table, or text.
    SelectionChanged,

    /// A menu was opened.
    MenuOpened,

    /// A menu was closed.
    MenuClosed,

    /// An alert or notification was posted.
    /// Playwright mapping: `page.on('dialog')`.
    Alert,
}
```

#### Platform Mapping

| xa11y EventKind | macOS (AXObserver) | Windows (UIA) | Linux (AT-SPI2 D-Bus) |
|---|---|---|---|
| `FocusChanged` | `AXFocusedUIElementChanged` | `AddFocusChangedEventHandler` | `focus:` / `object:state-changed:focused` |
| `ValueChanged` | `AXValueChanged` | `PropertyChanged(Value)` | `object:property-change:accessible-value` |
| `NameChanged` | `AXTitleChanged` | `PropertyChanged(Name)` | `object:property-change:accessible-name` |
| `StateChanged` | `AXValueChanged` (checkbox), inferred | `PropertyChanged(ToggleState, IsEnabled, ExpandCollapseState, ...)` | `object:state-changed:*` |
| `StructureChanged` | `AXUIElementDestroyed`, `AXCreated` | `AddStructureChangedEventHandler` | `object:children-changed:add/remove` |
| `WindowOpened` | `AXWindowCreated`, `AXSheetCreated` | `StructureChanged(ChildAdded)` on desktop root | `window:create` |
| `WindowClosed` | `AXUIElementDestroyed` (on window) | `StructureChanged(ChildRemoved)` on desktop root | `window:destroy` |
| `WindowActivated` | `AXApplicationActivated`, `AXFocusedWindowChanged` | `PropertyChanged(HasKeyboardFocus)` on window | `window:activate` |
| `WindowDeactivated` | `AXApplicationDeactivated` | `PropertyChanged(HasKeyboardFocus)` on window | `window:deactivate` |
| `SelectionChanged` | `AXSelectedChildrenChanged` | Selection events via `SelectionPattern` | `object:selection-changed` |
| `MenuOpened` | `AXMenuOpened` | `StructureChanged` + role check | `object:state-changed:visible` on Menu |
| `MenuClosed` | `AXMenuClosed` | `StructureChanged` + role check | `object:state-changed:visible` on Menu |
| `Alert` | Inferred from `AXWindowCreated` with Alert role | `AutomationEvent(Notification)` | `object:state-changed:showing` on Alert/Notification |

### Event Payload

```rust
/// An accessibility event delivered to subscribers.
pub struct Event {
    /// What kind of event occurred.
    pub kind: EventKind,

    /// The application that produced this event.
    pub app_name: String,
    pub app_pid: u32,

    /// A snapshot of the element that triggered the event, if available.
    /// None if the element was destroyed or is not capturable.
    pub target: Option<Node>,

    /// For StateChanged events: which state flag changed.
    pub state_flag: Option<StateFlag>,

    /// For StateChanged events: the new value of the flag.
    pub state_value: Option<bool>,

    /// Monotonic timestamp from `Instant::now()` at event receipt.
    /// Uses system monotonic clock so timestamps are comparable across
    /// subscriptions and can be correlated with other monotonic timestamps
    /// in the same process.
    pub timestamp: std::time::Instant,
}

/// Individual state flags, for use in StateChanged events and filters.
#[repr(u8)]
pub enum StateFlag {
    Enabled,
    Visible,
    Focused,
    Checked,
    Selected,
    Expanded,
    Editable,
    Required,
    Busy,
}
```

### Subscription API

The event API is an optional extension to `Provider`, exposed as a separate trait. Backends that don't support events (or where events aren't needed) don't implement it.

```rust
/// Optional trait for backends that support event subscriptions.
/// Extends Provider with reactive capabilities.
pub trait EventProvider: Provider {
    /// Subscribe to events matching the given filter.
    /// Returns a stream of events and a handle to manage the subscription.
    ///
    /// Dropping the `Subscription` unsubscribes automatically (RAII).
    ///
    /// Playwright mapping: `page.on(event, handler)` becomes
    ///   `let sub = provider.subscribe(target, filter)?;`
    ///   `while let Some(event) = sub.stream.recv().await { handler(event); }`
    fn subscribe(
        &self,
        target: &AppTarget,
        filter: EventFilter,
    ) -> Result<Subscription>;

    /// Wait for a single event matching the filter, with timeout.
    /// Returns the first matching event or a timeout error.
    ///
    /// Playwright mapping: `page.waitForEvent('popup')` becomes
    ///   `provider.wait_for_event(target, EventFilter::all(), timeout)?`
    fn wait_for_event(
        &self,
        target: &AppTarget,
        filter: EventFilter,
        timeout: std::time::Duration,
    ) -> Result<Event>;

    /// Wait for an element matching the selector to reach the desired state.
    /// Returns a snapshot of the element once the condition is met.
    ///
    /// Playwright mapping: `locator.waitFor({state: 'visible'})` becomes
    ///   `provider.wait_for(target, "button[name='Submit']", ElementState::Visible, timeout)?`
    fn wait_for(
        &self,
        target: &AppTarget,
        selector: &str,
        state: ElementState,
        timeout: std::time::Duration,
    ) -> Result<Node>;
}
```

### Subscription Handle

```rust
/// A live event subscription. Drop to unsubscribe.
///
/// `Subscription` is `Send` but not `Clone`. It can be moved to another
/// thread but not shared. For multi-consumer patterns, fan out manually
/// from a single subscription.
///
/// The receiver and cancel handle have coupled lifetimes — the receiver
/// cannot outlive the subscription. Access the receiver via methods on
/// `Subscription` rather than taking ownership of it.
pub struct Subscription {
    // Internal: bounded async channel receiver.
    rx: EventReceiver,

    // Internal: dropping this signals the backend to stop delivering events.
    _cancel: CancelHandle,
}

impl Subscription {
    /// Receive the next event (async). Returns `None` when the subscription
    /// is closed (e.g., target app exited).
    pub async fn recv(&self) -> Option<Event>;

    /// Try to receive without blocking (returns None if no event ready).
    pub fn try_recv(&self) -> Option<Event>;
}

/// Platform-agnostic event receiver (internal).
/// Wraps a bounded async channel in the Rust API.
/// FFI bindings expose this as a callback or polling interface.
struct EventReceiver { /* ... */ }
```

### Event Filter

Filters are applied at subscription time so backends can minimize overhead by only registering for relevant platform events.

```rust
/// Filter to narrow which events are delivered.
pub struct EventFilter {
    /// Only deliver events from elements matching this selector.
    /// None = all elements in the target app.
    pub selector: Option<String>,
}

impl EventFilter {
    /// Subscribe to all events from the target (no selector filter).
    pub fn all() -> Self;

    /// Subscribe to events on elements matching a selector.
    pub fn selector(selector: &str) -> Self;
}
```

### Element Wait States

Used by `wait_for` to express what condition to wait for. Directly mirrors Playwright's `locator.waitFor({state})`.

```rust
/// Desired element state for wait_for operations.
pub enum ElementState {
    /// Wait until an element matching the selector exists in the tree.
    /// Playwright: `{state: 'attached'}`
    Attached,

    /// Wait until no element matches the selector.
    /// Playwright: `{state: 'detached'}`
    Detached,

    /// Wait until a matching element exists and is visible.
    /// Playwright: `{state: 'visible'}`
    Visible,

    /// Wait until a matching element is hidden or doesn't exist.
    /// Playwright: `{state: 'hidden'}`
    Hidden,

    /// Wait until a matching element is enabled (not disabled/busy).
    /// Playwright: `:enabled` pseudo-selector in waitForSelector.
    Enabled,
}
```

### FFI Bindings for Events

#### Python

```python
import xa11y

provider = xa11y.create_provider()

# Subscribe — returns an iterable subscription
sub = provider.subscribe(name="Safari", kinds=["FocusChanged", "ValueChanged"])
for event in sub:
    print(f"{event.kind}: {event.target.name}")

# Wait for event (blocking with timeout)
event = provider.wait_for_event(name="Safari", kinds=["WindowOpened"], timeout=5.0)

# Wait for element state
node = provider.wait_for(name="Safari", selector='button[name="Submit"]', state="visible", timeout=10.0)

# Context manager for automatic cleanup
with provider.subscribe(name="Safari", kinds=["FocusChanged"]) as sub:
    for event in sub:
        handle(event)
```

#### JavaScript/TypeScript

```typescript
import { createProvider } from 'xa11y';

const provider = createProvider();

// Event emitter pattern — maps directly to Playwright's page.on()
const sub = provider.subscribe({ name: 'Chrome' }, { kinds: ['FocusChanged', 'ValueChanged'] });
sub.on('event', (event) => {
  console.log(`${event.kind}: ${event.target?.name}`);
});

// One-shot — maps to Playwright's page.waitForEvent()
const event = await provider.waitForEvent(
  { name: 'Chrome' },
  { kinds: ['WindowOpened'] },
  5000
);

// Wait for element state — maps to Playwright's locator.waitFor()
const node = await provider.waitFor(
  { name: 'Chrome' },
  'button[name="Submit"]',
  'visible',
  10000
);

// Cleanup
sub.unsubscribe();
```

### Playwright Compatibility Mapping

The event system is designed so a Playwright-compatible desktop adapter can map directly:

| Playwright API | xa11y equivalent |
|---|---|
| `page.on('close', fn)` | `subscribe(app, EventFilter::all())` |
| `page.on('popup', fn)` | `subscribe(app, EventFilter::all())` |
| `page.on('dialog', fn)` | `subscribe(app, EventFilter::all())` |
| `page.waitForEvent('popup')` | `wait_for_event(app, EventFilter::all(), timeout)` |
| `locator.waitFor({state: 'visible'})` | `wait_for(app, selector, ElementState::Visible, timeout)` |
| `locator.waitFor({state: 'detached'})` | `wait_for(app, selector, ElementState::Detached, timeout)` |
| `locator.waitFor({state: 'hidden'})` | `wait_for(app, selector, ElementState::Hidden, timeout)` |
| `page.waitForSelector(sel)` | `wait_for(app, sel, ElementState::Attached, timeout)` |
| `page.off('close', fn)` | `drop(subscription)` |

### Implementation Strategy

Each platform backend implements `EventProvider` by wrapping the native event mechanism:

**macOS:** Create an `AXObserver` with `AXObserverCreate()` and register notifications via `AXObserverAddNotification()`. The observer is added to the current `CFRunLoop`. Each subscription maps to one or more AX notification registrations on the target app's accessibility element (or its root for global events).

**Windows:** Use `IUIAutomation::AddAutomationEventHandler`, `AddFocusChangedEventHandler`, `AddPropertyChangedEventHandler`, and `AddStructureChangedEventHandler`. Scope subscriptions using `TreeScope` (element, children, subtree). Handler groups (`IUIAutomationEventHandlerGroup`) are preferred to batch registrations and avoid the known ~60s delay per individual handler registration.

**Linux:** Register D-Bus match rules via `atspi_event_listener_register()` for the relevant AT-SPI event classes (`focus:`, `object:state-changed:*`, `object:children-changed:*`, `window:*`, etc.). The async D-Bus listener runs on the internal tokio runtime; events are forwarded to subscriber channels.

### Edge Cases

- **Subscription to dead apps:** If the target app exits, the subscription stream closes (yields `None`). Consumers should handle this gracefully.
- **Event storms:** Bounded channels with configurable capacity (default: 256). Oldest events are dropped when the consumer is slow. This prevents unbounded memory growth.
- **Selector-filtered subscriptions:** The backend receives all events for the target, then filters locally using the selector engine. Platform APIs don't support selector-level filtering, so this is a client-side filter. This is acceptable because event volume per-app is typically low.
- **`wait_for` implementation:** Implemented as poll + subscribe. First checks if the condition is already met (snapshot), then subscribes to relevant events and re-checks after each event. This avoids the race between checking and subscribing.
- **Thread safety:** `Subscription` is `Send` but not `Clone`. The `EventReceiver` can be moved to another thread but not shared. For multi-consumer patterns, users should fan out manually.
- **Multiple subscriptions:** Multiple concurrent subscriptions to the same app are supported. Each gets its own channel. The backend deduplicates platform-level registrations internally.

### Crate Structure Update

Event types and traits live in `xa11y-core`:

```
xa11y-core/
├── ...
├── event.rs           # EventKind, Event, EventFilter, ElementState, StateFlag
├── event_provider.rs  # EventProvider trait, Subscription, EventReceiver
└── lib.rs
```

---

## Resolved Questions

1. **Event/change notification:** ✅ **Yes.** xa11y will support event subscriptions. See the [Event Subscriptions](#event-subscriptions) section above for the full design. This enables real-time monitoring and is essential for a future Playwright-compatible desktop automation backend.

2. **Text interface:** ❌ **No.** Rich text operations (cursor position, selection, text ranges) are out of scope. The `value` field exposes text as a plain string. Fine-grained text manipulation can use `SetValue` or be handled at a higher layer.

3. **Table interface:** ❌ **No.** No dedicated table API. Tables are trees of `TableRow`/`TableCell` nodes, queryable with the existing selector system (e.g., `table > table_row:nth(3) > table_cell:nth(2)`).

4. **Window management:** ❌ **No.** Screenshots, window focus, and window manipulation stay in a sibling crate. xa11y is focused on accessibility trees.

5. **Caching strategy:** ❌ **No.** Always re-traverse. Simplicity and correctness over performance. The snapshot model is inherently cache-free. Event subscriptions provide the reactive alternative to polling with cached trees.
