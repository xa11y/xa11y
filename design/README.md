# Introduction

xa11y is a cross-platform library that provides a single API for reading accessibility trees, performing actions on UI elements, and subscribing to accessibility event streams across macOS, Windows, and Linux. Target use cases include desktop UI testing, AI agent tooling, and desktop automation. Each platform exposes accessibility data through a different API — macOS uses AXUIElement, Linux uses AT-SPI2 over D-Bus, and Windows uses UI Automation via COM — but the underlying concepts (trees of elements with roles, names, states, and actions) are shared. xa11y normalizes these into a unified interface so consumers write their logic once and it works across platforms.

## Core challenges

* **Divergent platform APIs.** The three platforms use fundamentally different IPC mechanisms (Mach messages, D-Bus, COM) and expose accessibility data in different shapes. Roles, actions, and attributes don't map 1:1 — some concepts exist on one platform but not others.
* **Lossy abstraction risk.** Normalizing three APIs into one inevitably loses platform-specific detail. The library must balance useful abstraction with escape hatches for platform-specific data to avoid becoming a lowest-common-denominator tool.
* **Live, mutable trees.** Accessibility trees change continuously as apps update their UI. The library must query lazily (no stale snapshots) and handle elements disappearing between calls.

## Major features

1. **Reading accessibility trees** — traverse an app's full element hierarchy with lazy, live queries. Each element exposes role, name, value, description, bounds, states, and available actions.
2. **Performing actions** — press buttons, type text, set values, toggle checkboxes, expand/collapse disclosures, scroll, and more — all through accessibility APIs, never input simulation.
3. **Event streams** — subscribe to accessibility events (focus changes, value changes, structure changes, etc.) per-app via a pull-based stream.

# High-level accessibility background

Desktop operating systems define accessibility APIs that let applications expose a structured, machine-readable representation of their UI. Originally motivated by assistive technology — screen readers, switch controls, voice navigation — these interfaces also serve as a general-purpose mechanism for programmatic interaction with desktop applications.

Accessibility data is **opt-in at the application level**. OS-native UI elements (Cocoa controls, GTK widgets, Win32 controls) typically implement it automatically, and most major UI frameworks (Qt, Electron, Flutter, SwiftUI) provide it out of the box. However, custom-drawn UIs or less common frameworks may expose incomplete or missing trees. The quality and consistency of the data varies across apps and platforms.

Despite the platform differences, the core concepts are consistent:

* **Elements** — the nodes of the accessibility tree, each representing a UI component (a button, a text field, a window, etc.). Elements carry:
  * **Roles** — a semantic type (button, text field, checkbox, menu item, etc.) that describes the element's purpose in the UI.
  * **Attributes** — properties like name, value, description, bounding rectangle, and boolean flags (enabled, focused, visible, checked, expanded, etc.). Some attributes are read-only, others (like value on a text field) can be set.
* **Actions** — operations an element supports: press, toggle, expand, set value, etc. The set of available actions varies by role and platform.
* **Events** — notifications emitted when the UI changes: focus moved, value changed, element created/destroyed, window opened/closed.

## Linux (AT-SPI2)

**IPC:** D-Bus. A dedicated accessibility bus (separate from the session bus) carries all traffic. The registry daemon (`at-spi2-registryd`) tracks registered applications and manages event subscriptions. Each accessible element is a D-Bus object implementing one or more `org.a11y.atspi.*` interfaces.

**How apps participate:** Toolkits implement the AT-SPI2 D-Bus interfaces directly (GTK4, Qt5) or via the ATK bridge (GTK3, Chromium, Firefox, LibreOffice). An app registers with the registry by calling `Embed()` on the registry's root socket, then exposes its widget tree as a hierarchy of D-Bus objects under `/org/a11y/atspi/accessible/`.

**Roles:** Defined as a C enum (`AtspiRole`) with 131 values, transmitted as `u32` on the wire. Examples: `PUSH_BUTTON` (43), `CHECK_BOX` (7), `ENTRY` (79), `TEXT` (61), `FRAME` (23).

**States:** A bitfield packed into two `u32` values (64 bits total). Each state is a bit position from the `AtspiStateType` enum (44 defined). Examples: `ENABLED` (8), `FOCUSED` (12), `VISIBLE` (30), `EDITABLE` (7), `CHECKED` (4).

**Actions:** String-named, exposed via the `org.a11y.atspi.Action` interface. `GetName(index)` returns the name (e.g. `"click"`, `"toggle"`, `"expand"`), `DoAction(index)` performs it. Action names are not standardized across toolkits — GTK uses `"click"`, Qt uses `"Press"`.

**Attributes:** The `org.a11y.atspi.Accessible` interface exposes core properties (`Name`, `Description`, `Parent`, `ChildCount`) as D-Bus properties. Freeform key-value attributes are available via `GetAttributes()`. Numeric values use the `org.a11y.atspi.Value` interface (`f64` only). Text content uses the `org.a11y.atspi.Text` interface; text editing uses `org.a11y.atspi.EditableText`. Unlike macOS and Windows, Linux separates **states** from attributes — boolean flags like enabled, focused, visible, and checked are returned as a dedicated 64-bit bitfield via `GetState()`, not as individual properties.

**Example — Button:**

```
Role:    PUSH_BUTTON (43)
Name:    "Submit"
States:  ENABLED | SENSITIVE | VISIBLE | SHOWING | FOCUSABLE
Actions: ["click"]
Interfaces: Accessible, Action, Component
```

**Example — Text entry:**

```
Role:    ENTRY (79)
Name:    "Username"
States:  ENABLED | SENSITIVE | VISIBLE | SHOWING | FOCUSABLE | EDITABLE | SINGLE_LINE
Actions: ["activate"]
Interfaces: Accessible, Action, Component, Text, EditableText
Text.GetText(0, -1) → "hello world"
```

**Querying the tree:** Each property of each element is a separate D-Bus call. `GetChildren()` returns references (`(bus_name, object_path)` pairs), not data — you must then query each child individually for its role, name, states, bounds, actions, etc. Building one complete element requires 6-10 D-Bus round-trips. For a subtree of N nodes, that's O(N \* P) calls where P is properties per node. There is no API to fetch a subtree with properties in one call. (A bulk `Cache.GetItems` interface exists for initial population, but it returns only a partial property set for the entire app — not a scoped subtree query.)

**References:**

* [Architecture overview](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/architecture.html)
* [D-Bus interface specs](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Accessible.html) (Accessible, Action, Text, Value, etc.)
* [AtspiRole enum (all values)](https://docs.gtk.org/atspi2/enum.Role.html)
* [AtspiStateType enum (all values)](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/enum.StateType.html)
* [Cache interface](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Cache.html)
* [at-spi2-core source](https://github.com/GNOME/at-spi2-core)

## macOS (AXUIElement)

**IPC:** Mach messages. The client-side API (`AXUIElement.h` in HIServices/ApplicationServices) sends Mach messages to the target app process. Each `AXUIElementRef` is an opaque handle encoding a PID and internal element identifier. Every attribute query or action crosses the process boundary via Mach IPC.

**How apps participate:** Apps implement the `NSAccessibility` protocol (modern, method-based) on their views. Standard AppKit and SwiftUI controls provide accessibility automatically. The accessibility server runs inside each app process and responds to incoming Mach queries by calling the appropriate protocol methods.

**Roles:** Plain CFStrings (e.g. `"AXButton"`, `"AXTextField"`, `"AXTextArea"`). `AXRoleConstants.h` defines \~50 conventional roles, but since they're just strings, apps and web content can report arbitrary values not in the SDK (e.g. WebKit adds `"AXWebArea"`, `"AXLink"`, `"AXHeading"`). There is no closed enum — the SDK constants are conventions, not a fixed list. Subroles (also CFStrings, e.g. `"AXDialog"`, `"AXSwitch"`, `"AXTabButton"`) refine the base role and are similarly open-ended.

**States:** Not a bitfield. Individual boolean attributes: `AXEnabled`, `AXFocused`, `AXSelected`, `AXExpanded`, `AXHidden`, etc. Each is queried separately via `AXUIElementCopyAttributeValue`.

**Actions:** Also plain CFStrings (e.g. `"AXPress"`, `"AXIncrement"`, `"AXShowMenu"`). `AXActionConstants.h` defines only \~10, and like roles, apps can define custom action strings. The small set is by design — many operations that are "actions" on other platforms are done by **setting attributes** on macOS instead (e.g. setting `AXFocused` to `true` to focus, setting `AXValue` to change text, setting `AXExpanded` to expand/collapse).

**Attributes:** Key-value pairs where keys are CFStrings and values are CF types (CFString, CFNumber, CFBoolean, CFArray, AXUIElementRef, AXValue). `AXUIElementCopyAttributeNames` lists available attributes; `AXUIElementIsAttributeSettable` checks writability. Parameterized attributes (e.g. `AXStringForRange`) support text queries.

**Example — Button:**

```
Role:       "AXButton"
Title:      "Submit"
Enabled:    true
Focused:    false
Position:   {200, 300}
Size:       {80, 30}
Actions:    ["AXPress"]
```

**Example — Text area:**

```
Role:       "AXTextArea"
Value:      "hello world"
Enabled:    true
Focused:    true
Position:   {50, 100}
Size:       {400, 200}
Actions:    ["AXShowMenu"]
Settable:   AXValue, AXFocused, AXSelectedTextRange
NumberOfCharacters: 11
SelectedTextRange:  {loc: 11, len: 0}
```

**Querying the tree:** `AXChildren` returns an array of opaque `AXUIElementRef` handles — references, not data. You must query each child separately for its properties. However, `AXUIElementCopyMultipleAttributeValues` batches multiple attribute reads for a single element into one Mach IPC round-trip (e.g. fetch role, name, value, states, bounds all at once). Actions require a separate `AXUIElementCopyActionNames` call. This gives \~2-3 IPC calls per node — better than AT-SPI2's 6-10, but still O(N) for a subtree of N nodes. There is no API to fetch a subtree with properties in one call.

**References:**

* [Accessibility API overview](https://developer.apple.com/documentation/accessibility/accessibility-api)
* [AXUIElementCopyMultipleAttributeValues](https://developer.apple.com/documentation/applicationservices/1462051-axuielementcopymultipleattribute)
* [AXUIElement.h functions](https://developer.apple.com/documentation/applicationservices/axuielement_h)
* [AXRoleConstants.h (roles + subroles)](https://developer.apple.com/documentation/applicationservices/axroleconstants_h)
* [AXActionConstants.h (actions)](https://developer.apple.com/documentation/applicationservices/axactionconstants_h)
* [AXAttributeConstants.h (attributes)](https://developer.apple.com/documentation/applicationservices/axattributeconstants_h)
* [The OS X Accessibility Model](https://developer.apple.com/library/archive/documentation/Accessibility/Conceptual/AccessibilityMacOSX/OSXAXmodel.html)

## Windows (UI Automation)

**IPC:** COM. The UI Automation Core (`UIAutomationCore.dll`) brokers cross-process COM calls between client and provider. Clients use `IUIAutomation` / `IUIAutomationElement`; apps implement `IRawElementProviderSimple` (and optionally `IRawElementProviderFragment`). Standard Win32 controls get UIA support automatically through built-in proxies. WPF, WinForms, XAML, and WinUI have built-in AutomationPeer classes.

**How apps participate:** When UIA Core asks for an element's provider, the app's `WM_GETOBJECT` handler returns the provider via `UiaReturnRawElementProvider()`. For standard Win32 controls, built-in proxies translate legacy MSAA/IAccessible information into UIA properties automatically.

**Control types (roles):** Integer constants (e.g. `UIA_ButtonControlTypeId` = 50000, `UIA_EditControlTypeId` = 50004), defined in `UIAutomationClient.h`. A fixed enumeration — \~40 types. Each control type specifies required and recommended patterns and properties.

**States:** Not a bitfield. Individual boolean properties: `IsEnabled`, `HasKeyboardFocus`, `IsKeyboardFocusable`, `IsOffscreen`, `IsPassword`, etc. Each queried via `GetCurrentPropertyValue(propertyId)`.

**Patterns (actions):** The behavioral model uses **control patterns** — COM interfaces that describe what an element can do. Key patterns: `InvokePattern` (press), `TogglePattern` (toggle, exposes On/Off/Indeterminate), `ValuePattern` (get/set string values), `RangeValuePattern` (numeric values with min/max), `ExpandCollapsePattern` (expand/collapse), `SelectionItemPattern` (select), `ScrollPattern` (scroll), `TextPattern` (rich text). Clients check for pattern support via `GetCurrentPattern(patternId)`.

**Properties:** Identified by integer property IDs, returning typed `VARIANT` values. Examples: `UIA_NamePropertyId` (30005, string), `UIA_BoundingRectanglePropertyId` (30001, double array), `UIA_AutomationIdPropertyId` (30011, string).

**Querying the tree:** UIA has a `CacheRequest` mechanism that can fetch an entire subtree with specified properties and patterns in a single cross-process call. The client creates an `IUIAutomationCacheRequest`, calls `AddProperty` for each desired property and `AddPattern` for each desired pattern, sets the `TreeScope` (element, children, or subtree), then calls `FindAllBuildCache`. The UIA Core marshals the request to the provider process, walks the tree there, and returns all matching elements with their cached data in one response. Subsequent reads use local cached data — no further IPC. This is fundamentally different from Linux and macOS, where reading N nodes always requires O(N) cross-process calls. UIA also provides three tree views — Raw (every element), Control (`IsControlElement == true`, the default), and Content (`IsContentElement == true`, most distilled).

**Example — Button:**

```
ControlType:  ButtonControlType (50000)
Name:         "Submit"
AutomationId: "btnSubmit"
IsEnabled:    true
BoundingRect: [200, 300, 80, 30]
Patterns:     InvokePattern (required)
  Invoke() → clicks the button
```

**Example — Text box:**

```
ControlType:  EditControlType (50004)
Name:         "Username"
AutomationId: "txtUsername"
IsEnabled:    true
IsPassword:   false
Patterns:     ValuePattern (required), TextPattern (recommended)
  Value → "hello world"
  SetValue("new text") → replaces content
  IsReadOnly → false
```

**References:**

* [UI Automation overview](https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiauto-win32)
* [Control Types overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-controltypesoverview)
* [Control Patterns overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-controlpatternsoverview)
* [Property identifiers](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-automation-element-propids)
* [Tree overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-treeoverview)
* [Caching properties and patterns](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-cachingforclients)

# Design inspirations

## WAI-ARIA

[WAI-ARIA](https://www.w3.org/TR/wai-aria/) defines a platform-neutral vocabulary for accessibility semantics — roles (button, checkbox, slider), states (checked, expanded, disabled), and properties (name, value, description). Rather than adopting one platform's role taxonomy or inventing a new one, ARIA provides a ready-made, well-documented set of concepts that map naturally to all three desktop platforms.

## AccessKit

[AccessKit](https://accesskit.dev/) solves the inverse problem — providing accessibility *to* platform APIs from the app side, rather than reading *from* them. But the data model is relevant: AccessKit defines a platform-neutral tree of nodes with roles, properties, and actions that maps to macOS, Windows, and Linux APIs. It demonstrates that a single normalized model can faithfully represent all three platforms without excessive loss.

## The web DOM

Accessibility trees are structurally similar to the web DOM — a tree of typed nodes with attributes and parent/child relationships. This suggests a DOM-like data model where elements have roles (analogous to HTML tag names), named attributes, and a traversable hierarchy. CSS selector syntax (`button[name='Submit']`, `group > text_field`) is a proven way to query such trees and a natural fit for accessibility tree traversal.

## Playwright

[Playwright](https://playwright.dev/) is a browser automation library with an API designed around **Locators** — lazy selectors that re-resolve on every operation. This makes scripts resilient to UI changes between steps. The pattern of separating "find" from "act" via a chainable locator is well-suited for live accessibility trees, where elements can appear, disappear, or move between calls. The traditional "get element handle, call methods on it" pattern is prone to stale-reference bugs in this context.

# Tenets

1. **Abstract where platforms agree.** Create unified types (roles, actions, states) where the three platforms share a concept. The abstraction should feel natural, not forced.
2. **Escape hatches over lossy abstractions.** Where platforms diverge, expose raw platform-specific data rather than papering over differences with a lowest-common-denominator model. A consumer can always drop down to the platform layer for full fidelity.
3. **Don't add value where there is none to add.** If the library can't meaningfully improve on using a platform-specific API directly, leave it out. Someone can always use a platform-specific library for specialized features.
4. **No silent fallbacks.** If an operation fails, return the error — don't silently try a different mechanism. Fallbacks hide bugs and make behavior unpredictable for consumers.
5. **Only expose what accessibility APIs support.** If a platform has no accessibility interface for an operation, don't implement it with input simulation — leave it out.

# Design

## Concepts

The library has four user-facing concepts: **App**, **Locator**, **Element**, and **Subscription**. Internally, a **Provider** per platform implements the actual accessibility queries.

### App

An App represents a running application. It is the entry point to the library — you get an App, then use it to query the accessibility tree.

**Discovery.** Apps are found by name or PID:

```python
app = xa11y.App.by_name("Safari")
app = xa11y.App.by_pid(12345)
apps = xa11y.App.list()  # all running apps with accessibility trees
```

An App exposes:
- `name` — the application name
- `pid` — the process ID
- `locator(selector)` — returns a Locator scoped to this app's tree
- `children()` — returns the app's direct child Elements (typically windows)
- `subscribe()` — returns a Subscription for this app's accessibility events

App is not an Element. It does not have a role, attributes, or actions. It is purely a handle to an application's accessibility tree. The app's own accessible node (role=Application) is accessible as the root of the tree but is not the App object itself.

### Locator

A Locator is a lazy reference to one or more elements in the accessibility tree, inspired by Playwright's Locator pattern. It stores a CSS-like selector string and re-resolves it against the live tree on every operation. This makes it immune to stale-reference bugs — the element can move, disappear, or change between calls, and the Locator will always query the current state.

```python
loc = app.locator("button[name='Submit']")

# nothing has been queried yet — the locator is lazy

loc.press()            # resolves selector, finds element, performs action
loc.element()          # resolves selector, returns Element snapshot
loc.elements()         # resolves selector, returns all matching Elements
```

Locators support chaining to narrow scope:

```python
toolbar = app.locator("toolbar")
save_button = toolbar.child("button[name='Save']")   # toolbar > button[name='Save']
any_button = toolbar.descendant("button")             # toolbar button
second = app.locator("list_item").nth(2)              # 1-based index
```

**Action methods** — Locators have convenience methods for every normalized action (press, focus, toggle, set_value, etc.). Action methods **auto-wait**: before performing the action, the Locator polls until the element is attached, visible, and enabled (with a configurable timeout, default 5 seconds). If the element does not become actionable within the timeout, the call returns a `Timeout` error. This eliminates the need for manual `wait_visible()` calls before most actions.

**Wait methods** — For cases where the caller needs explicit control, Locators also expose wait methods (`wait_visible`, `wait_attached`, `wait_enabled`, `wait_checked`, etc.) that poll with a caller-specified timeout and return the resolved Element on success.

### Element

An Element is a snapshot of a node in the accessibility tree at the time it was queried. It carries all the data for that node — role, name, value, states, actions, bounds, and platform-specific raw data.

```python
el = app.locator("text_field[name='Username']").element()

el.role          # "text_field"
el.name          # "Username"
el.value         # "hello world"
el.enabled       # True
el.focused       # True
el.editable      # True
el.actions       # ["press", "focus", "set_value", "type_text", ...]
el.bounds        # Rect(x=50, y=100, width=400, height=30)
```

Elements support navigation — `children()` and `parent()` — which return fresh Elements by querying the live tree. But the Element's own properties are a frozen snapshot.

Elements do not have action methods. To perform actions, use a Locator (which re-resolves before acting). This is a deliberate design choice: since the tree can change between reading an Element's properties and performing an action, the Locator pattern ensures the action targets the current state of the element.

The full set of Element properties is described in the sections below (Roles, States, Attributes, Actions).

### Subscription

A Subscription is a pull-based stream of accessibility events for a single application. Events include focus changes, value changes, structure changes, window lifecycle, and more.

```python
sub = app.subscribe()

# blocking wait with timeout
event = sub.recv(timeout=5.0)
print(event.event_type)   # "focus_changed"
print(event.target.name)  # element that received focus

# non-blocking poll
event = sub.try_recv()    # returns None if no event pending

# wait for specific event
event = sub.wait_for(lambda e: e.event_type == "value_changed", timeout=10.0)
```

### Provider (internal)

A Provider is the platform-specific backend that implements accessibility queries. There is one provider per platform: macOS (AXUIElement), Linux (AT-SPI2 over D-Bus), and Windows (UI Automation via COM). Providers are in separate crates and implement a common trait with methods for `get_children`, `get_parent`, `find_elements`, `perform_action`, and `subscribe`. The library automatically selects the correct provider for the current platform. Consumers do not interact with providers directly in normal use, but can inject a custom provider for testing.

## Reading trees

### Constraints

- **Trees can be large.** macOS and Linux require per-element IPC to fetch data (6–10 D-Bus calls per node on Linux, 2–3 Mach IPC calls on macOS). Fetching a full subtree can be slow. The interface must let callers efficiently select just the elements they need, while still allowing full subtree queries for callers who want simplicity.
- **Roles and attributes partially overlap across platforms.** All three platforms have buttons, checkboxes, text fields, etc., but each also has platform-specific concepts. macOS roles and actions are open-ended strings with no fixed enum — apps and web content can report arbitrary values. The library must normalize the common cases without discarding platform-specific data.

### Roles

Roles are exposed as a normalized enum. Multiple platform concepts can map to a single normalized value (e.g. macOS `AXTextField` and `AXSecureTextField` both map to `text_field`). The enum covers the set of roles that exist on at least two of the three platforms — roughly 40 values. Elements whose platform role has no normalized equivalent get `unknown`.

```python
element.role == "button"
element.role == "text_field"
```

### Attributes

Every element has a set of named attributes. Some are common across all platforms and are exposed as named properties on the Element object. Boolean attributes are called "states" informally, but there is no separate state concept — states are just boolean-valued attributes.

```python
element.name             # "Submit"
element.value            # "hello world"
element.enabled          # True
element.checked          # Toggled.On, Toggled.Off, Toggled.Mixed, or None
element.bounds           # Rect(x=200, y=300, width=80, height=30)
element.numeric_value    # 0.75
```

Beyond the named properties, elements expose their full set of attributes — including platform-specific ones — via an attributes map. Attribute keys use consistent `snake_case` naming regardless of platform origin. Named properties also appear in this map.

```python
element.attributes["color_value"]       # "#FF0000" (platform-specific)
element.attributes["enabled"]           # True
element.attributes["name"]             # "Submit"
```

The full set of named attributes and their platform mappings:

| Attribute | Type | macOS | Linux | Windows | Notes |
|-----------|------|-------|-------|---------|-------|
| `name` | string? | `AXTitle`, fallback `AXDescription` | `Name` property | `Name` property | Human-readable label |
| `value` | string? | `AXValue` | `Text.GetText` or `Value.CurrentValue` | `ValuePattern.Value` | Current value (text content, etc.) |
| `description` | string? | `AXHelp` or `AXDescription` | `Description` property | `HelpText` property | Supplementary description |
| `bounds` | Rect? | `AXPosition` + `AXSize` | `Component.GetExtents` | `BoundingRectangle` | Screen coordinates and size |
| `numeric_value` | float? | `AXValue` (as number) | `Value.CurrentValue` | `RangeValuePattern.Value` | For sliders, progress bars, spinners |
| `min_value` | float? | `AXMinValue` | `Value.MinimumValue` | `RangeValuePattern.Minimum` | Minimum of numeric range |
| `max_value` | float? | `AXMaxValue` | `Value.MaximumValue` | `RangeValuePattern.Maximum` | Maximum of numeric range |
| `stable_id` | string? | `AXIdentifier` | D-Bus object path | `AutomationId` | Stable across queries, **not guaranteed unique** — unique per app on Linux, among siblings only on Windows, no guarantee on macOS (Qt stamps one id per subtree) |
| `enabled` | bool | `AXEnabled` | `ENABLED` state bit | `IsEnabled` | Always reported; `true` unless explicitly disabled |
| `visible` | bool | `!AXHidden` | `VISIBLE` state bit | `!IsOffscreen` | Always reported; `true` unless explicitly hidden |
| `focused` | bool | `AXFocused` | `FOCUSED` state bit | `HasKeyboardFocus` | Whether element has keyboard focus |
| `active` | bool | `AXMain` | `ACTIVE` state bit | Foreground `HWND` | Whether this is the active (foreground) window; meaningful for window-like elements |
| `focusable` | bool | Computed from role/attributes | `FOCUSABLE` state bit | `IsKeyboardFocusable` | Whether element can receive focus |
| `selected` | bool | `AXSelected` | `SELECTED` state bit | `SelectionItemPattern.IsSelected` | Whether element is selected |
| `editable` | bool | Computed from role | `EDITABLE` state bit | `!ValuePattern.IsReadOnly` | Whether element's value can be edited |
| `expanded` | bool? | `AXExpanded` | `EXPANDABLE` state bit | `ExpandCollapsePattern` state | `true`/`false` if expandable, `None` if not |
| `checked` | Toggled? | `AXValue` (0/1/2 on checkable roles) | `CHECKED` + `INDETERMINATE` state bits | `TogglePattern.ToggleState` | Enum: `on`, `off`, `mixed`. `None` if not checkable. |

### Raw platform data

Each element carries a `raw` field — an untyped key-value map (dict in Python, JSON-like map in Rust) — containing the original platform-specific data exactly as the platform reported it. This is the escape hatch for consumers who need full platform fidelity without polluting the normalized interface. The raw field is not a typed struct; it is a flat map so that adding new platform fields never requires a type change.

**macOS** — includes the original AX role, subrole, identifier, and the full set of AX attributes:

```python
el = app.locator("button[name='Submit']").element()
el.role                          # "button"
el.raw["ax_role"]                # "AXButton"
el.raw["ax_subrole"]             # None
el.raw["ax_identifier"]          # "submit-btn"
el.raw["AXTitle"]                # "Submit"
el.raw["AXEnabled"]              # True
el.raw["AXFocused"]              # False
el.raw["AXPosition"]             # {"x": 200, "y": 300}
el.raw["AXSize"]                 # {"width": 80, "height": 30}
```

**Linux** — includes the AT-SPI2 role, D-Bus coordinates, the state bitfield as a list of state names, and the freeform attributes from `GetAttributes()`:

```python
el = app.locator("text_field[name='Username']").element()
el.role                          # "text_field"
el.raw["atspi_role"]             # "entry"
el.raw["bus_name"]               # ":1.42"
el.raw["object_path"]            # "/org/a11y/atspi/accessible/57"
el.raw["states"]                 # ["enabled", "sensitive", "visible", "showing", "focusable", "editable"]
el.raw["toolkit"]                # "gtk"
el.raw["layout"]                 # "single-line"
```

**Windows** — includes the UIA control type, AutomationId, class name, and the list of supported UIA patterns:

```python
el = app.locator("check_box[name='Remember me']").element()
el.role                          # "check_box"
el.checked                       # "on"
el.raw["control_type_id"]        # 50002
el.raw["automation_id"]          # "chkRemember"
el.raw["class_name"]             # "CheckBox"
el.raw["patterns"]               # ["TogglePattern", "InvokePattern"]
el.raw["IsEnabled"]              # True
el.raw["HasKeyboardFocus"]       # False
```

### Selectors

Selectors use CSS-like syntax to query the accessibility tree. They support matching on role, normalized attributes and states, the full attributes map, and original platform role names.

**Syntax:**
- `button` — match by normalized role
- `AXButton` — match by original platform role name (no special syntax; the role segment matches against both normalized and original role names)
- `button[name="Submit"]` — role + attribute filter
- `[name*="Save"]` — attribute contains (case-insensitive)
- `[name^="Sav"]` — attribute starts with
- `[name$="ave"]` — attribute ends with
- `toolbar > button` — direct child combinator
- `window button` — descendant combinator
- `button:nth(2)` — positional filter (1-based)

Selectors can filter on any attribute in the attributes map, not just name/value/description. This allows selectors like `check_box[checked="true"]` or `[color_value="#FF0000"]`.

**Not supported:** comma (or) combinators, `:not()`, `:first`/`:last` pseudo-classes, sibling combinators (`+`, `~`), and universal selector (`*`). The selector language is intentionally minimal — complex queries are better expressed as multiple Locator calls or filtering in application code.

### Selector evaluation

Selectors are lazy. Nothing is queried from the tree until the caller either performs an action (`locator.press()`) or explicitly requests element data (`locator.element()`, `locator.elements()`).

On macOS and Linux, the provider attempts to efficiently query only the data needed to navigate toward matching elements — fetching minimal properties (role, name) to decide which branches of the tree to descend into, and only fetching full element data for nodes that match the selector. On Windows, UIA's `CacheRequest` mechanism can fetch an entire subtree with specified properties in a single cross-process call, so the optimization strategy is different (bulk fetch with server-side filtering).

**Performance note:** On macOS and Linux, every node in the tree costs multiple IPC round-trips (2–3 Mach messages on macOS, 6–10 D-Bus calls on Linux). Prefer narrow selectors that avoid full-tree traversal — `toolbar > button[name='Save']` is much cheaper than `button[name='Save']` in a deep tree, because the combinator constrains which subtrees the engine descends into.

### Actions on elements

Each element reports its list of available actions. These are the normalized action names derived from the platform's supported actions for that element (see the Taking Actions section for how this mapping works). The list tells the caller what operations can be performed on the element.

```python
element.actions   # ["press", "focus", "set_value"]
```

## Taking actions

### Normalized actions

The library defines a fixed set of normalized actions. These are derived from Windows UIA's control patterns, which are the most structured of the three platforms. Each normalized action has a clear semantic meaning that maps to all three platforms.

Actions are performed via Locator methods:

```python
app.locator("button[name='Submit']").press()
app.locator("text_field[name='Username']").set_value("alice")
app.locator("check_box[name='Remember']").toggle()
app.locator("slider").set_numeric_value(0.75)
app.locator("combo_box").expand()
app.locator("text_area").type_text("hello")
app.locator("text_area").select_text(0, 5)
```

Actions are split into two categories:

**First-class methods** — common actions that every provider must implement as individual typed methods. These have proper signatures (no generic data bag) and are the primary way callers interact with elements:

| Method | Signature | Description |
|--------|-----------|-------------|
| `press()` | — | Click, tap, or invoke the element |
| `focus()` | — | Set keyboard focus to the element |
| `blur()` | — | Remove keyboard focus from the element |
| `toggle()` | — | Toggle a checkbox or switch |
| `select()` | — | Select a list item, tab, or menu item |
| `expand()` | — | Expand a collapsible element |
| `collapse()` | — | Collapse an expanded element |
| `show_menu()` | — | Show the element's context menu or dropdown |
| `increment()` | — | Increment a slider or spinner by one step |
| `decrement()` | — | Decrement a slider or spinner by one step |
| `scroll_into_view()` | — | Scroll the element into the visible area |
| `set_value(value)` | `&str` | Set the element's text value |
| `set_numeric_value(value)` | `f64` | Set the element's numeric value |
| `type_text(text)` | `&str` | Insert text at the current cursor position |
| `select_text(start, end)` | `u32, u32` | Select a range of text (0-based positions) |

**Generic `perform_action(name)` escape hatch** — for platform-specific actions not covered by the methods above. Takes a `snake_case` action name string. Well-known action names (`"press"`, `"focus"`, etc.) also work here — providers delegate to the corresponding method. Custom platform actions (e.g. macOS `AXCustomThing` → `"custom_thing"`) are resolved by the provider.

**`element.actions`** is a `Vec<String>` listing the actions the element reports. Well-known actions use their standard names (`"press"`, `"toggle"`, etc.). Platform-specific actions use their `snake_case` converted names. Typed operations like `set_value` and `type_text` are role-based capabilities, not reported actions.

### How actions map to platforms

Each platform expresses actions differently. The library uses table-driven 1:1 mappings to translate between normalized actions and platform actions. The mapping must be **lossless**: converting a platform action to a normalized action and back must yield the same platform action.

#### Windows (UIA control patterns)

Windows is the most straightforward — UIA patterns map directly to normalized actions:

| Action | UIA mechanism |
|--------|---------------|
| `press` | `InvokePattern.Invoke()` |
| `toggle` | `TogglePattern.Toggle()` |
| `select` | `SelectionItemPattern.Select()` |
| `focus` | `Element.SetFocus()` |
| `blur` | `SetFocus()` on the root element |
| `expand` | `ExpandCollapsePattern.Expand()` |
| `collapse` | `ExpandCollapsePattern.Collapse()` |
| `set_value` | `ValuePattern.SetValue()` or `RangeValuePattern.SetValue()` |
| `increment` | `RangeValuePattern.SetValue(current + SmallChange)` |
| `decrement` | `RangeValuePattern.SetValue(current - SmallChange)` |
| `scroll_into_view` | `ScrollItemPattern.ScrollIntoView()` |
| `type_text` | Read cursor position via `TextPattern.GetSelection()`, splice text into the current value at that position, then `ValuePattern.SetValue()` with the modified string. Falls back to appending if `TextPattern` is unavailable. This is a read-modify-write — not atomic with respect to concurrent user input. |
| `set_text_selection` | `TextPattern` range operations |

#### macOS (AX actions and settable attributes)

macOS exposes some operations as AX actions (performed via `AXUIElementPerformAction`) and others as settable attributes (performed via `AXUIElementSetAttributeValue`). Both are freeform strings. The library maps each normalized action to either an AX action name or an attribute-set operation:

| Action | macOS mechanism |
|--------|-----------------|
| `press` | `AXPress` action |
| `toggle` | `AXPress` action (same mechanism as press for checkboxes) |
| `show_menu` | `AXShowMenu` action |
| `increment` | `AXIncrement` action |
| `decrement` | `AXDecrement` action |
| `focus` | Set `AXFocused = true` |
| `blur` | Set `AXFocused = false` |
| `select` | Set `AXSelected = true` |
| `expand` | Set `AXExpanded = true` |
| `collapse` | Set `AXExpanded = false` |
| `set_value` | Set `AXValue` (string or numeric) |
| `type_text` | Set `AXSelectedText` (replaces current selection) |
| `set_text_selection` | Set `AXSelectedTextRange` |
| `scroll_into_view` | Not supported (no AX equivalent) |

For **reading** which actions an element supports: the provider calls `AXUIElementCopyActionNames` to get the element's action list (e.g. `["AXPress", "AXShowMenu", "AXCustomThing"]`). Known AX action names map to their standard `snake_case` name (e.g. `"AXPress"` → `"press"`). All other actions have the `AX` prefix stripped and are converted from `PascalCase` to `snake_case` (e.g. `"AXRaise"` → `"raise"`, `"AXCustomThing"` → `"custom_thing"`). No actions are silently hidden — if the platform reports it, it appears in `element.actions`.

The provider also adds implicit actions based on settable attributes (e.g. if `AXFocused` is present, add `"focus"`; if the role is a text field or slider, add `"set_value"`).

For **performing** an action via `perform_action("custom_thing")`: the provider first checks if it's a well-known name and delegates to the corresponding typed method. For unknown names, it converts `snake_case` back to `AXPascalCase` and looks for that in the element's action list. If found, it invokes it via `AXUIElementPerformAction`. If not found, it tries the literal `snake_case` name. If neither is supported, it returns `ActionNotSupported`. For example, `perform_action("custom_thing")` would first try `"AXCustomThing"`, then `"custom_thing"`, then fail.

#### Linux (AT-SPI2 action names and D-Bus interfaces)

Linux exposes some operations via the `Action` D-Bus interface (string-named, performed by index) and others via specialized D-Bus interfaces (`EditableText`, `Value`, `Component`). Action names are not standardized across toolkits — GTK uses `"click"`, Qt uses `"Press"`.

The library handles this with an alias table: each normalized action maps to a canonical name and a set of known aliases. During element discovery, the provider iterates the element's AT-SPI2 actions, matches each name against the alias table, and stores the index. When performing an action, it calls `DoAction(index)` with the stored index.

| Action | AT-SPI2 mechanism | Known aliases |
|--------|-------------------|---------------|
| `press` | `Action.DoAction` | `"click"`, `"activate"`, `"press"`, `"invoke"` |
| `toggle` | `Action.DoAction` | `"toggle"`, `"check"`, `"uncheck"` |
| `expand` | `Action.DoAction` | `"expand"`, `"open"` |
| `collapse` | `Action.DoAction` | `"collapse"`, `"close"` |
| `select` | `Action.DoAction` | `"select"` |
| `show_menu` | `Action.DoAction` | `"menu"`, `"showmenu"`, `"popup"`, `"show menu"` |
| `increment` | `Action.DoAction` or manual `Value` adjustment | `"increment"` |
| `decrement` | `Action.DoAction` or manual `Value` adjustment | `"decrement"` |
| `focus` | `Component.GrabFocus()` or `Action.DoAction` | `"focus"` |
| `blur` | `Component.GrabFocus()` on parent | — |
| `set_value` | `Value.CurrentValue` (numeric) or `EditableText.SetTextContents` (text) | — |
| `type_text` | `EditableText.InsertText()` at cursor | — |
| `set_text_selection` | `Text.SetSelection()` or `Text.AddSelection()` | — |
| `scroll_into_view` | `Component.ScrollTo()` | — |

### Action fidelity requirement

If an element reports an action name in its `actions` list, calling that action must result in the original platform action being invoked — not a substitute or alias. For example, if a GTK button's AT-SPI2 actions include `"click"` and the library normalizes it to `press`, then calling `locator.press()` must invoke the original `"click"` action (via its stored index), not some other alias like `"activate"`.

### Unsupported actions

If an action is not supported by the element on the current platform, the library returns an error. It does not silently fall back to a different mechanism (e.g. input simulation). An action appears in `element.actions` only if the platform reports support for it.

## Errors

All fallible operations return a typed error. The error types are:

| Error | When |
|-------|------|
| `PermissionDenied` | OS denied accessibility access (e.g. macOS requires screen recording/accessibility permission) |
| `SelectorNotMatched` | A Locator resolved to zero elements when exactly one was expected |
| `ElementStale` | The element no longer exists in the tree (removed between queries) |
| `ActionNotSupported` | The requested action is not available on this element/platform |
| `TextValueNotSupported` | A text operation was attempted on a non-text element |
| `Timeout` | A wait or auto-wait exceeded the configured timeout |
| `InvalidSelector` | The selector string failed to parse |
| `InvalidActionData` | Wrong or missing data for an action (e.g. `set_value` without a value) |
| `Platform` | An underlying platform API call failed (includes platform error code and message) |

Errors are not silently swallowed. Platform-specific error codes are preserved in `Platform` errors for debugging.

## Thread safety

App, Locator, and Element are all `Send + Sync`. They can be shared freely across threads. This is a hard requirement — automation frameworks commonly dispatch actions from multiple threads (e.g. a test runner with parallel workers, or an AI agent loop with concurrent tool calls). Providers must ensure their internal state (IPC connections, caches) is safe for concurrent access.

## Events

(future)

# Testing

notes: fill this in with what we've got.

* unit tests everywhere
* integ tests
* end to end tests - real apps with a variety of frameworks. selecors and actions should work across frameworks and platforms
* fuzzing tests, especially for selector parsing and logic

