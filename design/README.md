# Introduction

xa11y is a cross-platform library that provides a single API for reading accessibility trees, performing actions on UI elements, and subscribing to accessibility event streams across macOS, Windows, and Linux. Target use cases include desktop UI testing, AI agent tooling, and desktop automation. Each platform exposes accessibility data through a different API — macOS uses AXUIElement, Linux uses AT-SPI2 over D-Bus, and Windows uses UI Automation via COM — but the underlying concepts (trees of elements with roles, names, states, and actions) are shared. xa11y normalizes these into a unified interface so consumers write their logic once and it works across platforms.

## Core challenges

- **Divergent platform APIs.** The three platforms use fundamentally different IPC mechanisms (Mach messages, D-Bus, COM) and expose accessibility data in different shapes. Roles, actions, and attributes don't map 1:1 — some concepts exist on one platform but not others.
- **Lossy abstraction risk.** Normalizing three APIs into one inevitably loses platform-specific detail. The library must balance useful abstraction with escape hatches for platform-specific data to avoid becoming a lowest-common-denominator tool.
- **Live, mutable trees.** Accessibility trees change continuously as apps update their UI. The library must query lazily (no stale snapshots) and handle elements disappearing between calls.

## Major features

1. **Reading accessibility trees** — traverse an app's full element hierarchy with lazy, live queries. Each element exposes role, name, value, description, bounds, states, and available actions.
2. **Performing actions** — press buttons, type text, set values, toggle checkboxes, expand/collapse disclosures, scroll, and more — all through accessibility APIs, never input simulation.
3. **Event streams** — subscribe to accessibility events (focus changes, value changes, structure changes, etc.) per-app via a pull-based stream.

# High-level accessibility background

Desktop operating systems define accessibility APIs that let applications expose a structured, machine-readable representation of their UI. Originally motivated by assistive technology — screen readers, switch controls, voice navigation — these interfaces also serve as a general-purpose mechanism for programmatic interaction with desktop applications.

Accessibility data is **opt-in at the application level**. OS-native UI elements (Cocoa controls, GTK widgets, Win32 controls) typically implement it automatically, and most major UI frameworks (Qt, Electron, Flutter, SwiftUI) provide it out of the box. However, custom-drawn UIs or less common frameworks may expose incomplete or missing trees. The quality and consistency of the data varies across apps and platforms.

Despite the platform differences, the core concepts are consistent:

- **Elements** — the nodes of the accessibility tree, each representing a UI component (a button, a text field, a window, etc.). Elements carry:
  - **Roles** — a semantic type (button, text field, checkbox, menu item, etc.) that describes the element's purpose in the UI.
  - **Attributes** — properties like name, value, description, and bounding rectangle. Some attributes are read-only, others (like value on a text field) can be set.
  - **States** — a set of flags describing the element's current condition: enabled, focused, visible, checked, expanded, etc.
- **Actions** — operations an element supports: press, toggle, expand, set value, etc. The set of available actions varies by role and platform.
- **Events** — notifications emitted when the UI changes: focus moved, value changed, element created/destroyed, window opened/closed.

## Linux (AT-SPI2)

**IPC:** D-Bus. A dedicated accessibility bus (separate from the session bus) carries all traffic. The registry daemon (`at-spi2-registryd`) tracks registered applications and manages event subscriptions. Each accessible element is a D-Bus object implementing one or more `org.a11y.atspi.*` interfaces.

**How apps participate:** Toolkits implement the AT-SPI2 D-Bus interfaces directly (GTK4, Qt5) or via the ATK bridge (GTK3, Chromium, Firefox, LibreOffice). An app registers with the registry by calling `Embed()` on the registry's root socket, then exposes its widget tree as a hierarchy of D-Bus objects under `/org/a11y/atspi/accessible/`.

**Roles:** Defined as a C enum (`AtspiRole`) with 131 values, transmitted as `u32` on the wire. Examples: `PUSH_BUTTON` (43), `CHECK_BOX` (7), `ENTRY` (79), `TEXT` (61), `FRAME` (23).

**States:** A bitfield packed into two `u32` values (64 bits total). Each state is a bit position from the `AtspiStateType` enum (44 defined). Examples: `ENABLED` (8), `FOCUSED` (12), `VISIBLE` (30), `EDITABLE` (7), `CHECKED` (4).

**Actions:** String-named, exposed via the `org.a11y.atspi.Action` interface. `GetName(index)` returns the name (e.g. `"click"`, `"toggle"`, `"expand"`), `DoAction(index)` performs it. Action names are not standardized across toolkits — GTK uses `"click"`, Qt uses `"Press"`.

**Attributes:** The `org.a11y.atspi.Accessible` interface exposes core properties (`Name`, `Description`, `Parent`, `ChildCount`) as D-Bus properties. Freeform key-value attributes are available via `GetAttributes()`. Numeric values use the `org.a11y.atspi.Value` interface (`f64` only). Text content uses the `org.a11y.atspi.Text` interface; text editing uses `org.a11y.atspi.EditableText`.

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

**Querying the tree:** Each property of each element is a separate D-Bus call. `GetChildren()` returns references (`(bus_name, object_path)` pairs), not data — you must then query each child individually for its role, name, states, bounds, actions, etc. Building one complete element requires 6-10 D-Bus round-trips. For a subtree of N nodes, that's O(N * P) calls where P is properties per node. There is no API to fetch a subtree with properties in one call. (A bulk `Cache.GetItems` interface exists for initial population, but it returns only a partial property set for the entire app — not a scoped subtree query.)

**References:**
- [Architecture overview](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/architecture.html)
- [D-Bus interface specs](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Accessible.html) (Accessible, Action, Text, Value, etc.)
- [AtspiRole enum (all values)](https://docs.gtk.org/atspi2/enum.Role.html)
- [AtspiStateType enum (all values)](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/enum.StateType.html)
- [Cache interface](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Cache.html)
- [at-spi2-core source](https://github.com/GNOME/at-spi2-core)

## macOS (AXUIElement)

**IPC:** Mach messages. The client-side API (`AXUIElement.h` in HIServices/ApplicationServices) sends Mach messages to the target app process. Each `AXUIElementRef` is an opaque handle encoding a PID and internal element identifier. Every attribute query or action crosses the process boundary via Mach IPC.

**How apps participate:** Apps implement the `NSAccessibility` protocol (modern, method-based) on their views. Standard AppKit and SwiftUI controls provide accessibility automatically. The accessibility server runs inside each app process and responds to incoming Mach queries by calling the appropriate protocol methods.

**Roles:** Plain CFStrings (e.g. `"AXButton"`, `"AXTextField"`, `"AXTextArea"`). `AXRoleConstants.h` defines ~50 conventional roles, but since they're just strings, apps and web content can report arbitrary values not in the SDK (e.g. WebKit adds `"AXWebArea"`, `"AXLink"`, `"AXHeading"`). There is no closed enum — the SDK constants are conventions, not a fixed list. Subroles (also CFStrings, e.g. `"AXDialog"`, `"AXSwitch"`, `"AXTabButton"`) refine the base role and are similarly open-ended.

**States:** Not a bitfield. Individual boolean attributes: `AXEnabled`, `AXFocused`, `AXSelected`, `AXExpanded`, `AXHidden`, etc. Each is queried separately via `AXUIElementCopyAttributeValue`.

**Actions:** Also plain CFStrings (e.g. `"AXPress"`, `"AXIncrement"`, `"AXShowMenu"`). `AXActionConstants.h` defines only ~10, and like roles, apps can define custom action strings. The small set is by design — many operations that are "actions" on other platforms are done by **setting attributes** on macOS instead (e.g. setting `AXFocused` to `true` to focus, setting `AXValue` to change text, setting `AXExpanded` to expand/collapse).

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

**Querying the tree:** `AXChildren` returns an array of opaque `AXUIElementRef` handles — references, not data. You must query each child separately for its properties. However, `AXUIElementCopyMultipleAttributeValues` batches multiple attribute reads for a single element into one Mach IPC round-trip (e.g. fetch role, name, value, states, bounds all at once). Actions require a separate `AXUIElementCopyActionNames` call. This gives ~2-3 IPC calls per node — better than AT-SPI2's 6-10, but still O(N) for a subtree of N nodes. There is no API to fetch a subtree with properties in one call.

**References:**
- [Accessibility API overview](https://developer.apple.com/documentation/accessibility/accessibility-api)
- [AXUIElementCopyMultipleAttributeValues](https://developer.apple.com/documentation/applicationservices/1462051-axuielementcopymultipleattribute)
- [AXUIElement.h functions](https://developer.apple.com/documentation/applicationservices/axuielement_h)
- [AXRoleConstants.h (roles + subroles)](https://developer.apple.com/documentation/applicationservices/axroleconstants_h)
- [AXActionConstants.h (actions)](https://developer.apple.com/documentation/applicationservices/axactionconstants_h)
- [AXAttributeConstants.h (attributes)](https://developer.apple.com/documentation/applicationservices/axattributeconstants_h)
- [The OS X Accessibility Model](https://developer.apple.com/library/archive/documentation/Accessibility/Conceptual/AccessibilityMacOSX/OSXAXmodel.html)

## Windows (UI Automation)

**IPC:** COM. The UI Automation Core (`UIAutomationCore.dll`) brokers cross-process COM calls between client and provider. Clients use `IUIAutomation` / `IUIAutomationElement`; apps implement `IRawElementProviderSimple` (and optionally `IRawElementProviderFragment`). Standard Win32 controls get UIA support automatically through built-in proxies. WPF, WinForms, XAML, and WinUI have built-in AutomationPeer classes.

**How apps participate:** When UIA Core asks for an element's provider, the app's `WM_GETOBJECT` handler returns the provider via `UiaReturnRawElementProvider()`. For standard Win32 controls, built-in proxies translate legacy MSAA/IAccessible information into UIA properties automatically.

**Control types (roles):** Integer constants (e.g. `UIA_ButtonControlTypeId` = 50000, `UIA_EditControlTypeId` = 50004), defined in `UIAutomationClient.h`. A fixed enumeration — ~40 types. Each control type specifies required and recommended patterns and properties.

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
- [UI Automation overview](https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiauto-win32)
- [Control Types overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-controltypesoverview)
- [Control Patterns overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-controlpatternsoverview)
- [Property identifiers](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-automation-element-propids)
- [Tree overview](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-treeoverview)
- [Caching properties and patterns](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-cachingforclients)

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
