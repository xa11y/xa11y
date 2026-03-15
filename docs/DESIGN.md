# xa11y — Cross-Platform Accessibility Client Library

## Overview

**xa11y** is a Rust library for reading and interacting with accessibility trees across desktop (and eventually mobile) platforms. It provides a unified API over platform-specific accessibility APIs, letting consumers query UI structure and perform actions without writing platform-specific code.

This library is designed to replace the cross-platform accessibility layer in [agent-desktop](https://github.com/crowecawcaw/agent-desktop), with first-class support for FFI bindings (Python, JavaScript) and future mobile platforms (iOS, Android).

### Design Principles

1. **Client-side focus** — xa11y *reads* accessibility trees exposed by other applications. It is not a toolkit for *providing* accessibility (that's what accesskit does). However, we borrow accesskit's data model where it makes sense.
2. **Platform concepts mapped, not hidden** — Each platform has unique behaviors. The abstraction normalizes them but exposes raw/platform-specific data when needed.
3. **Snapshot-based** — Trees are captured as point-in-time snapshots. Handles to live elements are held internally for action dispatch but are not exposed to the consumer.
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
| `TextField` | AXTextField, AXTextArea, AXSearchField, AXSecureTextField | UIA_EditControlTypeId | Entry, PasswordText, SpinButton |
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
| `Dialog` | AXDialog | *(via Window with dialog subrole)* | Dialog, FileChooser |
| `Alert` | *(via AXDialog subrole)* | *(via UIA alert pattern)* | Alert, Notification |
| `ProgressBar` | AXProgressIndicator, AXBusyIndicator | UIA_ProgressBarControlTypeId | ProgressBar |
| `TreeItem` | AXDisclosureTriangle | UIA_TreeItemControlTypeId | TreeItem |
| `WebArea` | AXWebArea | UIA_DocumentControlTypeId | DocumentWeb, DocumentFrame |
| `Heading` | AXHeading | *(via landmark/heading pattern)* | Heading |
| `Separator` | AXSplitter | UIA_SeparatorControlTypeId | Separator |
| `SplitGroup` | AXSplitGroup | UIA_SplitButtonControlTypeId | SplitPane |
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
    SetValue,       // Set text content or numeric value
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
- **SetValue on Linux AT-SPI:** The Value interface only supports numeric values. For text input, the Text interface must be used. xa11y will try Value first, then fall back to simulating text input if the element is editable.
- **Toggle on macOS:** There's no dedicated toggle — AXPress on a checkbox toggles it. xa11y maps both `Press` and `Toggle` to `AXPress` for checkboxes.
- **ShowMenu on Windows:** No direct pattern. Can be accomplished via keyboard simulation (Shift+F10) or by expanding a combo box. xa11y will attempt `ExpandCollapse.Expand()` as fallback.
- **Action discovery:** macOS reports actions via `AXUIElementCopyActionNames()`. Windows uses UIA patterns (each pattern implies certain actions). Linux AT-SPI reports actions via the Action interface with indexed names. xa11y normalizes all these into the `Action` enum.

### 3. `Node` — A single element in the tree

```rust
pub struct Node {
    /// Unique ID within a snapshot (sequential, deterministic DFS order)
    pub id: NodeId,

    /// Element role
    pub role: Role,

    /// Human-readable name (title, label)
    pub name: Option<String>,

    /// Current value (text content, slider position, etc.)
    pub value: Option<String>,

    /// Supplementary description (tooltip, help text)
    pub description: Option<String>,

    /// Bounding rectangle in screen pixels
    pub bounds: Option<Rect>,

    /// Bounding box normalized to [0.0, 1.0] relative to screen dimensions
    pub bounds_normalized: Option<NormalizedRect>,

    /// Available actions
    pub actions: Vec<Action>,

    /// Current state flags
    pub states: StateSet,

    /// Child node IDs (direct children only)
    pub children: Vec<NodeId>,

    /// Parent node ID (None for root)
    pub parent: Option<NodeId>,

    /// Depth in the tree (0 = root)
    pub depth: u32,

    /// Application name (useful when querying all apps)
    pub app_name: Option<String>,

    /// Platform-specific raw data (opt-in, for debugging)
    pub raw: Option<RawPlatformData>,
}

pub type NodeId = u32;
```

#### `StateSet` — Boolean state flags

```rust
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

### 4. `Rect` and `NormalizedRect` — Geometry

```rust
/// Screen-pixel bounding rectangle
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Bounding rectangle normalized to [0.0, 1.0] range
/// relative to screen dimensions
pub struct NormalizedRect {
    pub x1: f64,  // left
    pub y1: f64,  // top
    pub x2: f64,  // right
    pub y2: f64,  // bottom
}
```

Geometry uses accesskit-style `Rect` for pixel coordinates. The `NormalizedRect` is kept for agent-desktop compatibility (useful for vision models that work in normalized coordinates).

**Edge cases:**
- **Multi-monitor:** Coordinates are in the global screen space. On macOS, the origin (0,0) is top-left of the primary display. On Windows, coordinates can be negative for monitors left of or above the primary. On Linux/X11, coordinates are in the X screen space.
- **High-DPI / Retina:** macOS reports bounds in points (not pixels). Windows reports in physical pixels. Linux depends on the toolkit. xa11y reports whatever the platform API returns and documents the coordinate space.
- **Coordinate origin:** macOS accessibility uses top-left origin (unlike AppKit which uses bottom-left). Windows and Linux use top-left. xa11y uses top-left consistently.

### 5. `Tree` — A snapshot of the accessibility tree

```rust
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID (0 for multi-app queries)
    pub pid: u32,

    /// Screen dimensions at capture time
    pub screen_size: (u32, u32),

    /// All nodes in DFS order
    pub nodes: Vec<Node>,

    /// Index from NodeId -> position in nodes vec (for O(1) lookup)
    node_index: HashMap<NodeId, usize>,

    /// Query options used to produce this snapshot
    /// (stored for deterministic re-traversal during action dispatch)
    pub query: QueryOptions,
}
```

The tree is a **flattened snapshot** — nodes reference each other by `NodeId` rather than holding direct pointers. This is critical for:
- Serialization (JSON, msgpack) for FFI
- Deterministic re-traversal for action dispatch (same DFS order → same IDs)
- Thread safety without lifetimes

#### Tree Methods

```rust
impl Tree {
    /// Get a node by ID
    pub fn get(&self, id: NodeId) -> Option<&Node>;

    /// Get the root node
    pub fn root(&self) -> &Node;

    /// Iterate all nodes
    pub fn iter(&self) -> impl Iterator<Item = &Node>;

    /// Query nodes matching a CSS-like selector
    pub fn query(&self, selector: &str) -> Result<Vec<&Node>>;

    /// Get children of a node
    pub fn children(&self, id: NodeId) -> Vec<&Node>;

    /// Get the subtree rooted at a node
    pub fn subtree(&self, id: NodeId) -> Vec<&Node>;

    /// Find nodes by role
    pub fn find_by_role(&self, role: Role) -> Vec<&Node>;

    /// Find nodes by name (substring, case-insensitive)
    pub fn find_by_name(&self, pattern: &str) -> Vec<&Node>;
}
```

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

### 7. `Provider` — Platform backend trait

```rust
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree>;

    /// Snapshot all running applications (shallow)
    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree>;

    /// Perform an action on an element from the last snapshot
    fn perform_action(&self, node_id: NodeId, action: Action, data: Option<ActionData>) -> Result<()>;

    /// Check if accessibility permissions are granted
    fn check_permissions(&self) -> Result<PermissionStatus>;

    /// List running applications with their PIDs
    fn list_apps(&self) -> Result<Vec<AppInfo>>;
}

pub enum AppTarget {
    ByName(String),
    ByPid(u32),
    /// Target a specific window by platform-specific handle
    ByWindow(WindowHandle),
}

pub struct QueryOptions {
    pub max_depth: u32,
    pub max_elements: u32,
    pub visible_only: bool,
    pub roles: Option<Vec<Role>>,   // filter to specific roles
    pub include_raw: bool,           // include platform-specific data
}

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

pub struct AppInfo {
    pub name: String,
    pub pid: u32,
    pub bundle_id: Option<String>,  // macOS
}
```

### 8. `RawPlatformData` — Escape hatch

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
3. **Builds** a `Tree` snapshot with sequential IDs
4. **Caches** live element handles internally (keyed by NodeId) for action dispatch
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

1. Consumer calls `provider.perform_action(node_id, Action::Press, None)`
2. Backend checks its internal element cache for `node_id`
3. If stale (or first call since snapshot), backend re-traverses using stored `QueryOptions` to rebuild the cache
4. Backend maps the xa11y `Action` to platform-specific calls
5. Returns `Ok(())` or an error

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
- **Element ID stability:** IDs are assigned in DFS order during traversal. If the UI changes between snapshot and action dispatch, IDs may no longer match. The re-traversal mechanism mitigates this, but there's an inherent race condition.
- **Role granularity mismatch:** macOS has fewer roles (AXGroup covers many things), Windows has more specific control types, and Linux AT-SPI has the most roles. Normalization loses some information — `include_raw: true` preserves the original.
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

# Interact
provider.press(submit[0].id)
provider.set_value(text_field.id, "hello@example.com")
```

### JavaScript/TypeScript (napi-rs)

```typescript
import { createProvider } from 'xa11y';

const provider = createProvider();
const tree = await provider.getAppTree({ name: 'Chrome' });

const buttons = tree.query('button');
await provider.press(buttons[0].id);
```

### Design Constraints for FFI

- All public types are `Send + Sync`
- No lifetimes in public API — everything is owned
- `Tree` and `Node` are serializable to JSON
- `NodeId` is a simple `u32` (trivially representable in any language)
- Error handling uses `Result<T, Error>` in Rust, exceptions in Python/JS
- Async: Linux backend is internally async but the public API is synchronous. Python/JS bindings can offer async wrappers.

---

## Relationship to AccessKit

AccessKit is a library for **providing** accessibility (making your app accessible to screen readers). xa11y is a library for **consuming** accessibility (reading other apps' accessibility trees and interacting with them).

We borrow from accesskit where concepts align:
- **Role enum:** Our role list is a subset of accesskit's ~150 roles, keeping only roles that commonly appear in desktop applications. We use the same names where possible.
- **Action enum:** Similar to accesskit's Action enum but scoped to client-observable actions (e.g., we don't need `SetTextSelection` or `SetSequentialFocusNavigationStartingPoint`).
- **Geometry types:** We use a similar `Rect` type. We don't need `Affine` transforms since we're reading screen coordinates, not widget-local coordinates.
- **Node ID:** AccessKit uses `u64` NodeIds (unique within a tree). We use `u32` sequential IDs (unique within a snapshot) for simplicity and FFI friendliness. AccessKit's IDs are assigned by the application; ours are assigned during traversal.

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

## Open Questions

1. **Event/change notification:** Should xa11y support subscribing to accessibility events (focus changed, value changed, tree structure changed)? agent-desktop doesn't need this (snapshot-based), but it would be useful for real-time monitoring tools. Could be added as an optional `EventListener` trait.

2. **Text interface:** Rich text operations (cursor position, selection, text ranges) are complex and vary across platforms. Initial version exposes `value` as a plain string. A dedicated `TextProvider` trait for fine-grained text operations could come later.

3. **Table interface:** Table-specific queries (get cell at row/col, get headers) could have a dedicated API. Currently tables are just trees of TableRow/TableCell nodes.

4. **Window management:** agent-desktop has screenshot and window focus capabilities. Should xa11y include these, or keep them separate? Recommendation: keep xa11y focused on accessibility trees; window management can be a sibling crate.

5. **Caching strategy:** Should the library cache trees across calls, or always re-traverse? Current design always re-traverses for simplicity and correctness. Caching could improve performance but introduces staleness risks.
