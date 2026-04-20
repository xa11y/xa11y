# Native Accessibility Events: Design Document

## Goal

Replace the current polling-based event delivery with native push notifications on all three platforms, and establish a common schema that is accurate, honest about platform differences, and avoids papering over gaps.

The existing implementation spawns a background thread that walks the full accessibility tree every 100ms, comparing snapshots to detect focus and structure changes. This works but is slow to react, expensive on CPU, and discards most of the information the platform actually delivers. All three platforms have native push event APIs that deliver richer data at lower cost.

## Platform Capability Survey

### macOS — AXObserver (Push)

AXObserver registers callbacks against a CFRunLoop. Notifications are CFString constants; the callback receives the element that changed as a live `AXUIElementRef` (valid only during the callback).

Registration can be at **app level** (pass `AXUIElementCreateApplication(pid)`) or **element level** (pass a specific element). App-level registration covers most notifications. A small number of notifications are only delivered to element-level observers (see below).

Full notification set (~40 constants):

| Notification | Scope | Notes |
|---|---|---|
| `AXFocusedUIElementChanged` | App | Element that gained focus |
| `AXFocusedWindowChanged` | App | Window focus changed |
| `AXMainWindowChanged` | App | Main window changed |
| `AXApplicationActivated` | App | App came to foreground |
| `AXApplicationDeactivated` | App | App lost foreground |
| `AXApplicationHidden` | App | App hidden |
| `AXApplicationShown` | App | App shown |
| `AXWindowCreated` | App | New window |
| `AXWindowMoved` | App | Window repositioned |
| `AXWindowResized` | App | Window resized |
| `AXWindowMiniaturized` | App | Window minimized |
| `AXWindowDeminiaturized` | App | Window un-minimized |
| `AXValueChanged` | App | AXValue changed (controls, text fields) |
| `AXTitleChanged` | App | AXTitle changed |
| `AXUIElementDestroyed` | App | Element removed |
| `AXElementBusyChanged` | App/Elem | Busy/loading state changed |
| `AXSelectedTextChanged` | App | Text selection changed |
| `AXMenuOpened` | App | Menu appeared |
| `AXMenuClosed` | App | Menu closed |
| `AXMenuItemSelected` | **Element only** | Menu item activated |
| `AXRowCountChanged` | **Element only** | Table row count changed |
| `AXRowExpanded` | **Element only** | Tree row expanded |
| `AXRowCollapsed` | **Element only** | Tree row collapsed |
| `AXSelectedRowsChanged` | **Element only** | Table row selection changed |
| `AXSelectedColumnsChanged` | **Element only** | Table column selection changed |
| `AXSelectedCellsChanged` | **Element only** | Table cell selection changed |
| `AXSelectedChildrenChanged` | **Element only** | Container selection changed |
| `AXCreated` | **Element only** | Element created |
| `AXResized` | **Element only** | Element resized |
| `AXMoved` | **Element only** | Element moved |
| `AXLayoutChanged` | **Element only** | Layout invalidated |
| `AXAnnouncementRequested` | App | Announcement with text + priority |
| `AXDrawerCreated` | App | Legacy drawer (deprecated) |
| `AXSheetCreated` | App | Sheet created |
| `AXHelpTagCreated` | App | Help tag shown |
| `AXUnitsChanged` | App | Measurement units changed |

**What the callback delivers:** the changed element (live reference, callback-scoped only) and the notification string. No old/new value, no delta. To get state after the change, query the element's attributes during the callback.

**Key limitation:** `AXValueChanged` fires for text fields but carries no position or delta — only the element. Diffing old vs new text to determine what changed is possible but fragile. macOS has no `AXTextChanged` notification with position data.

**Element-level registration trade-off:** App-level observers cover the majority of useful notifications. Element-level is required for fine-grained selection, row expand/collapse, and element creation. Supporting element-level registration requires tracking which elements to watch, which adds significant complexity. This document scopes to app-level only.

---

### Linux — AT-SPI2 D-Bus Signals (Push)

AT-SPI2 emits standard D-Bus signals. Subscriptions are global on the accessibility bus; filtering by sender bus name restricts to a specific application. zbus 5 supports signal subscriptions in both async and blocking modes.

Signal categories and the signals within each:

**Object signals** (`org.a11y.atspi.Object`):

| Signal | Parameters | Notes |
|---|---|---|
| `StateChanged` | state_name (string), new_value (bool) | One signal per state bit; e.g., state_name="focused", new_value=true |
| `ChildrenChanged` | change_type ("add"/"remove"), index (i32) | Tree structure change |
| `PropertyChange` | property_name (string) | Name, Description, Parent changed |
| `BoundsChanged` | — | Position or size changed |
| `VisibleDataChanged` | — | Catch-all for visible data |
| `SelectionChanged` | — | Selection changed in container |
| `ActiveDescendantChanged` | — | Composite widget active child changed |
| `Announcement` | text (string) | Accessibility announcement |

**Text signals** (`org.a11y.atspi.Text` via Object event bus):

| Signal | Parameters | Notes |
|---|---|---|
| `TextChanged` | change_type ("insert"/"delete"), position (i32), length (i32) | Position and length are precise |
| `TextSelectionChanged` | — | Text selection changed |
| `CaretMoved` | position (i32) | Cursor moved; not universally emitted |
| `AttributesChanged` | — | Formatting changed |

**Value signals:**

| Signal | Notes |
|---|---|
| `Value:ValueChanged` | Numeric value changed (slider, progress, spin) |

**Window signals** (`org.a11y.atspi.Window`):

| Signal | Notes |
|---|---|
| `Create` | Window created |
| `Destroy` | Window destroyed |
| `Activate` | Window activated |
| `Deactivate` | Window deactivated |
| `Minimize` | Window minimized |
| `Maximize` | Window maximized |
| `Restore` | Window restored |
| `Move` | Window moved |
| `Resize` | Window resized |
| `Raise` / `Lower` | Z-order changed |

**Focus signal** (`org.a11y.atspi.Focus`):

| Signal | Notes |
|---|---|
| `Focus` | Keyboard focus moved to element (redundant with Object:StateChanged(focused)) |

**Source element in signals:** a D-Bus `(bus_name, object_path)` pair — a live reference, not a snapshot. Attributes must be queried immediately via subsequent D-Bus method calls.

**Toolkit reliability issues:**
- WebKit2GTK and Electron often omit `Text:TextChanged`; polling may still be needed for those.
- GTK4 occasionally misses `Object:StateChanged(focused)` — cross-check with `Focus:Focus`.
- `Object:ChildrenChanged` can be skipped during high-volume bulk updates.

---

### Windows — UI Automation COM Event Handlers (Push)

UIA provides COM event handler interfaces registered against the `IUIAutomation` root object. All handlers are invoked on an MTA background thread.

**Registration methods on `IUIAutomation`:**

| Method | Scope | Notes |
|---|---|---|
| `AddFocusChangedEventHandler` | System-wide (no scope parameter) | Always process-wide focus |
| `AddAutomationEventHandler(eventId, element, scope, ...)` | TreeScope flag | Most events |
| `AddPropertyChangedEventHandler(element, scope, ...)` | TreeScope flag | Watch specific property IDs |
| `AddStructureChangedEventHandler(element, scope, ...)` | TreeScope flag | Tree changes |
| `AddNotificationEventHandler(element, scope, ...)` | TreeScope flag | Windows 10+ announcements |

**TreeScope flags:** `Element`, `Children`, `Descendants`, `Subtree` (Element + Descendants), `Parent`, `Ancestors` — can be OR'd.

**Event IDs (selected):**

| Event ID | Handler data | Notes |
|---|---|---|
| `UIA_AutomationFocusChangedEventId` | sender element | System-wide |
| `UIA_AutomationPropertyChangedEventId` | sender, propertyId, newValue (VARIANT) | Per-property |
| `UIA_StructureChangedEventId` | sender, StructureChangeType, runtimeId | Add/Remove/Invalidate/Reorder |
| `UIA_Window_WindowOpenedEventId` | sender | Window created |
| `UIA_Window_WindowClosedEventId` | sender | Window closed |
| `UIA_MenuOpenedEventId` | sender | Menu appeared |
| `UIA_MenuClosedEventId` | sender | Menu closed |
| `UIA_Invoke_InvokedEventId` | sender | Button/link activated |
| `UIA_Text_TextChangedEventId` | sender | Text content changed |
| `UIA_Text_TextSelectionChangedEventId` | sender | Selection changed |
| `UIA_SelectionItem_ElementSelectedEventId` | sender | Item selected |
| `UIA_SelectionItem_ElementAddedToSelectionEventId` | sender | Item added to selection |
| `UIA_SelectionItem_ElementRemovedFromSelectionEventId` | sender | Item removed from selection |
| `UIA_NotificationEventId` | sender, kind, processingMode, displayString | Windows 10+ live regions |
| `UIA_LiveRegionChangedEventId` | sender | Live region updated |
| `UIA_SystemAlertEventId` | sender | System alert |

**Watchable property IDs (for PropertyChanged):** `Name`, `IsEnabled`, `HasKeyboardFocus`, `ToggleState`, `Value_Value`, `RangeValue_Value`, `ExpandCollapseState`, `SelectionItem_IsSelected`, `BoundingRectangle`, `IsOffscreen`.

**`StructureChangeType` enum:** `ChildAdded`, `ChildRemoved`, `ChildrenInvalidated`, `ChildrenBulkAdded`, `ChildrenBulkRemoved`, `ChildrenReordered`.

**Element references:** Live COM pointers. Can be used immediately in the handler. A `CacheRequest` can pre-fetch properties to avoid additional COM round-trips. Elements become invalid once the application destroys them.

**Thread model:** MTA required (`CoInitializeEx(NULL, COINIT_MULTITHREADED)`). Handlers are called on a background MTA thread managed by the UIA runtime. xa11y already uses MTA.

**Reliability caveat:** Many older or non-native apps (Java Swing, VB6, Adobe products, web browsers) have incomplete UIA provider implementations and may not fire all events.

---

## Cross-Platform Comparison

| Concept | macOS | Linux | Windows |
|---|---|---|---|
| **Delivery** | Push (CFRunLoop) | Push (D-Bus signals) | Push (COM MTA callbacks) |
| **Default scope** | App (via app element) | Global bus (filter by sender) | App subtree with TreeScope |
| **Focus changed** | `AXFocusedUIElementChanged` | `Object:StateChanged(focused)` + `Focus:Focus` | `AddFocusChangedEventHandler` |
| **Value changed** | `AXValueChanged` | `Value:ValueChanged` + `Object:StateChanged` | `PropertyChanged(Value/RangeValue/Toggle)` |
| **Name changed** | `AXTitleChanged` | `Object:PropertyChange(Name)` | `PropertyChanged(Name)` |
| **State changed** | Separate per-state notifications¹ | `Object:StateChanged(state, value)` | `PropertyChanged(IsEnabled/ToggleState/etc.)` |
| **Structure changed** | `AXUIElementDestroyed` (app) / `AXCreated` (elem only) | `Object:ChildrenChanged` | `StructureChangedEventHandler` |
| **Window opened** | `AXWindowCreated` | `Window:Create` | `UIA_Window_WindowOpenedEventId` |
| **Window closed** | `AXUIElementDestroyed` on window | `Window:Destroy` | `UIA_Window_WindowClosedEventId` |
| **Window activated** | `AXFocusedWindowChanged` | `Window:Activate` | Inferred (no dedicated event)² |
| **Window deactivated** | `AXFocusedWindowChanged` | `Window:Deactivate` | Inferred² |
| **Menu opened/closed** | `AXMenuOpened` / `AXMenuClosed` | None reliably | `UIA_MenuOpenedEventId` / `UIA_MenuClosedEventId` |
| **Text changed** | `AXValueChanged` (no position) | `Text:TextChanged` (position + type) | `UIA_Text_TextChangedEventId` |
| **Selection changed** | `AXSelectedTextChanged` (app) / row/cell (elem only) | `Object:SelectionChanged` | `SelectionItem_*` events |
| **Announcement** | `AXAnnouncementRequested` | `Object:Announcement` | `UIA_NotificationEventId` + `UIA_LiveRegionChangedEventId` |

¹ macOS has no single "state changed" notification. Separate notifications exist for some state transitions (`AXValueChanged` covers checkbox toggle, `AXElementBusyChanged` for busy), but `AXEnabledChanged` does not exist as a public notification. To detect enabled/disabled changes on macOS, you must either poll or observe `AXValueChanged` on the element and re-query `AXEnabled`. This is a genuine gap.

² Windows has no `WindowActivated` UIA event ID. Window focus can be inferred from `AddFocusChangedEventHandler` when the focused element is a window-level element, but this is indirect.

---

## Common Use Cases

Before defining the schema, consider what consumers actually do with events. This drives priorities and informs which payloads are worth carrying.

```python
# 1. Wait for async content to appear (most common pattern)
with app.subscribe() as sub:
    app.locator('button[name="Search"]').first().press()
    sub.wait_for(lambda e: e.kind == "StructureChanged", timeout=5.0)
    results = app.locator('list_item').all()

# 2. Focus verification after keyboard navigation
with app.subscribe() as sub:
    keyboard.press("Tab")
    event = sub.wait_for(lambda e: e.kind == "FocusChanged", timeout=2.0)
    assert event.target.name == "Password"

# 3. Wait for new window, then interact with it
with app.subscribe() as sub:
    app.locator('button[name="Open Settings"]').first().press()
    event = sub.wait_for(lambda e: e.kind == "WindowOpened", timeout=5.0)
    # event.target is a snapshot of the new window element

# 4. Value confirmation after action
with app.subscribe() as sub:
    app.locator('slider[name="Volume"]').first().set_value(75)
    event = sub.wait_for(
        lambda e: e.kind == "ValueChanged" and e.target and e.target.name == "Volume",
        timeout=2.0,
    )
    assert event.target.numeric_value == pytest.approx(75.0)

# 5. Wait for loading spinner to clear
with app.subscribe() as sub:
    app.locator('button[name="Generate"]').first().press()
    sub.wait_for(
        lambda e: e.kind == "StateChanged" and e.flag == "Busy" and not e.value,
        timeout=30.0,
    )

# 6. Wait for button to become enabled
with app.subscribe() as sub:
    app.locator('text_field[name="Name"]').first().type_text("Alice")
    sub.wait_for(
        lambda e: e.kind == "StateChanged" and e.flag == "Enabled" and e.value
                  and e.target and e.target.name == "Submit",
        timeout=3.0,
    )

# 7. Catch validation announcement after bad input
with app.subscribe() as sub:
    app.locator('button[name="Submit"]').first().press()
    sub.wait_for(lambda e: e.kind == "Announcement", timeout=3.0)
    error = app.locator('[role="alert"]').first().name  # re-query for text

# 8. AI agent: collect all changes after an action
with app.subscribe() as sub:
    app.locator('button[name="Send"]').first().press()
    changes = []
    deadline = time.time() + 2.0
    while time.time() < deadline:
        event = sub.recv(timeout=max(0, deadline - time.time()))
        if event:
            changes.append(f"{event.kind}: {event.target.name if event.target else '?'}")
```

**What this reveals about priorities:**

- `target` snapshot is the highest-value field across every use case. Reliable target population matters more than any payload field.
- `StructureChanged` + `FocusChanged` + `ValueChanged` cover the majority of test wait patterns.
- `StateChanged { flag, value }` fields are actively used in predicates (cases 5 and 6) — worth keeping as required variant data.
- `TextChanged` payload (position, change type) is rarely filtered on in practice; consumers re-query the element's text value instead.
- `Announcement` text is nice but consumers fall back to re-querying `[role="alert"]` when it's absent.

---

## Proposed Event Schema

### Design principles

1. **Only model what at least two platforms deliver natively.** A single-platform event belongs in a future platform-specific extension.
2. **Variant payload only when the data is always present when the event fires.** If a field would be `None` or `Unknown` on one platform, drop it from the variant — a bare event kind that consumers react to and then re-query is more honest than a struct with guaranteed-empty fields.
3. **Target element is always a snapshot.** Live references are valid only in the callback; xa11y converts them to `ElementData` at receipt time. This is the highest-value field.
4. **Acknowledge gaps explicitly.** If a platform cannot emit a given event kind at all, say so in the doc comment.

### EventKind

```rust
/// The kind of accessibility event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// Keyboard focus moved to a new element.
    /// Target: the element that gained focus.
    FocusChanged,

    /// An element's value changed (slider position, text field contents,
    /// checkbox state, spin button, progress, etc.).
    /// Target: the element whose value changed.
    ValueChanged,

    /// An element's name or label changed.
    /// Target: the element whose name changed.
    NameChanged,

    /// A boolean state flag changed on an element.
    /// Target: the element whose state changed.
    ///
    /// `flag` and `value` are always populated — this variant is only emitted
    /// when both are known. Coverage varies by platform:
    /// - Linux: all state bits via Object:StateChanged.
    /// - Windows: IsEnabled, ToggleState, ExpandCollapseState,
    ///   SelectionItem_IsSelected via PropertyChanged events.
    /// - macOS: Checked (via AXValueChanged on checkbox/radio) and Busy
    ///   (via AXElementBusyChanged). Enabled is NOT deliverable via any
    ///   public app-level macOS notification and will never fire on macOS.
    StateChanged {
        flag: StateFlag,
        value: bool,
    },

    /// Children were added to or removed from an element, or the tree
    /// structure was otherwise invalidated.
    /// Target: the parent element whose children changed.
    StructureChanged,

    /// A new window was created.
    /// Target: the window element.
    WindowOpened,

    /// A window was closed or destroyed.
    /// Target: snapshot taken at destruction time; some attributes may be absent.
    WindowClosed,

    /// A window became the active/focused window.
    /// Target: the window element.
    ///
    /// - macOS: AXFocusedWindowChanged.
    /// - Linux: Window:Activate.
    /// - Windows: no first-class UIA event; inferred from focus changes.
    WindowActivated,

    /// A window lost active status.
    /// Target: the window element.
    WindowDeactivated,

    /// The selection changed in a list, table, or other container.
    /// Target: the container element (not the selected items).
    SelectionChanged,

    /// A menu became visible.
    /// Target: the menu element.
    ///
    /// - macOS: AXMenuOpened.
    /// - Windows: UIA_MenuOpenedEventId.
    /// - Linux: not reliably emitted; this event will not fire on Linux.
    MenuOpened,

    /// A menu was dismissed.
    /// Target: the menu element.
    MenuClosed,

    /// Text content changed in an editable element.
    /// Target: the text element (re-query its value for current contents).
    ///
    /// No payload: macOS AXValueChanged carries no delta, so change_type and
    /// position cannot be populated cross-platform. Consumers that need the
    /// new text value should re-query the target element after receipt.
    TextChanged,

    /// An accessibility announcement was posted (live region update, alert,
    /// or explicit announcement request).
    /// Target: the element that made the announcement, if available.
    ///
    /// No text payload: Windows UIA_LiveRegionChangedEventId carries no text,
    /// so the announcement text cannot be populated cross-platform. Consumers
    /// should re-query a nearby alert or live region element for the content.
    ///
    /// - macOS: AXAnnouncementRequested.
    /// - Linux: Object:Announcement.
    /// - Windows: UIA_NotificationEventId and UIA_LiveRegionChangedEventId.
    Announcement,
}
```

### Supporting types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateFlag {
    Enabled,
    Visible,
    Focused,
    Checked,
    Selected,
    Expanded,
    Editable,
    Focusable,
    Modal,
    Required,
    Busy,
}
```

### Event struct

```rust
#[derive(Debug, Clone)]
pub struct Event {
    /// What happened and any type-specific data.
    pub kind: EventKind,
    /// Snapshot of the element that triggered the event.
    /// None for events where the element is not available or already destroyed.
    pub target: Option<ElementData>,
    /// Name of the application that produced this event.
    pub app_name: String,
    /// Process ID of the application that produced this event.
    pub app_pid: u32,
    /// Monotonic timestamp at event receipt.
    pub timestamp: std::time::Instant,
}
```

The flat `event_type` + optional `state_flag`/`state_value`/`text_change` fields from the current design are replaced by `EventKind` variants. `StateChanged` is the only variant with payload (`flag` and `value`) because those fields are always determined when the event fires on any supporting platform. All other variants are bare — react to the kind, re-query `target` for current state.

---

## Subscription Model

No change to the external API. `subscribe(element: &ElementData)` returns a `Subscription`. The element is used only to identify the target application (via PID + app name). Subscriptions are app-scoped on all platforms; element-scoped subscriptions are not in scope for this design.

```rust
pub fn subscribe(&self, element: &ElementData) -> Result<Subscription>;
```

`Subscription` exposes:
- `recv(timeout) -> Result<Event>` — block until next event or timeout
- `try_recv() -> Option<Event>` — non-blocking poll
- `wait_for(predicate, timeout) -> Result<Event>` — block until a matching event
- `close()` — idempotent shutdown (also called on drop)

---

## Platform Implementation Plan

### macOS

Replace the not-yet-used polling stub with a real `AXObserver` backed by CFRunLoop (the infrastructure is already there).

**Register at app level:**
- `AXFocusedUIElementChanged` → `FocusChanged`
- `AXValueChanged` → `ValueChanged` (also covers checkbox toggle; emit `StateChanged { Checked }` when the element role is checkbox/radio)
- `AXTitleChanged` → `NameChanged`
- `AXElementBusyChanged` → `StateChanged { Busy, _ }`
- `AXWindowCreated` → `WindowOpened`
- `AXUIElementDestroyed` → `WindowClosed` (when role is window) or `StructureChanged` (otherwise)
- `AXFocusedWindowChanged` → `WindowActivated` / `WindowDeactivated` (emit both: deactivate previous, activate new)
- `AXWindowMiniaturized` → `WindowDeactivated`
- `AXWindowDeminiaturized` → `WindowActivated`
- `AXSelectedTextChanged` → `SelectionChanged`
- `AXMenuOpened` → `MenuOpened`
- `AXMenuClosed` → `MenuClosed`
- `AXAnnouncementRequested` → `Announcement`

**Do not emit `StateChanged { Enabled }`** on macOS — there is no app-level notification for enable/disable state transitions. Document this gap.

**Snapshot strategy:** Query the element's attributes inside the callback before returning. The `AXUIElementRef` is only valid during callback execution.

### Linux

Replace the polling loop with zbus signal subscriptions.

Subscribe to signals on the AT-SPI2 bus, filtered to the target app's D-Bus sender name (obtained via `org.a11y.atspi.Registry.GetRegisteredEvents` or by matching sender to the app's `org.a11y.atspi.Application` object).

**Signal mappings:**
- `Object:StateChanged(focused, true)` → `FocusChanged`
- `Value:ValueChanged` → `ValueChanged`
- `Object:StateChanged(checked, _)` → `StateChanged { Checked, _ }`
- `Object:StateChanged(enabled, _)` → `StateChanged { Enabled, _ }`
- `Object:StateChanged(expanded, _)` → `StateChanged { Expanded, _ }`
- `Object:StateChanged(busy, _)` → `StateChanged { Busy, _ }`
- `Object:StateChanged(visible, _)` → `StateChanged { Visible, _ }`
- `Object:PropertyChange(Name)` → `NameChanged`
- `Object:ChildrenChanged` → `StructureChanged`
- `Window:Create` → `WindowOpened`
- `Window:Destroy` → `WindowClosed`
- `Window:Activate` → `WindowActivated`
- `Window:Deactivate` → `WindowDeactivated`
- `Object:SelectionChanged` → `SelectionChanged`
- `Text:TextChanged` → `TextChanged`
- `Object:Announcement` → `Announcement`

**zbus integration:** The blocking API can use `zbus::blocking::MessageIterator` or a background async runtime to receive signals and forward them to a sync `mpsc` channel. A dedicated async task subscribing to filtered match rules is the cleanest approach. Use D-Bus match rules to pre-filter by sender and interface to minimise traffic.

**Fallback:** For WebKit2GTK and Electron, `Text:TextChanged` is often absent. If `ValueChanged` fires on a text element, emit `TextChanged` as a fallback.

### Windows

Replace the polling loop with native UIA event handlers registered via `IUIAutomation`.

All handlers use `TreeScope_Subtree` on the application's root element (obtained via `FindFirst` by PID).

**Registrations:**
- `AddFocusChangedEventHandler` → `FocusChanged`
- `AddStructureChangedEventHandler` → `StructureChanged`
- `AddAutomationEventHandler(UIA_Window_WindowOpenedEventId)` → `WindowOpened`
- `AddAutomationEventHandler(UIA_Window_WindowClosedEventId)` → `WindowClosed`
- `AddAutomationEventHandler(UIA_MenuOpenedEventId)` → `MenuOpened`
- `AddAutomationEventHandler(UIA_MenuClosedEventId)` → `MenuClosed`
- `AddAutomationEventHandler(UIA_Text_TextChangedEventId)` → `TextChanged`
- `AddAutomationEventHandler(UIA_SelectionItem_ElementSelectedEventId)` → `SelectionChanged`
- `AddAutomationEventHandler(UIA_NotificationEventId)` → `Announcement`
- `AddAutomationEventHandler(UIA_LiveRegionChangedEventId)` → `Announcement`
- `AddPropertyChangedEventHandler([Name, IsEnabled, ToggleState, RangeValue_Value, Value_Value, ExpandCollapseState])`:
  - `Name` → `NameChanged`
  - `IsEnabled` → `StateChanged { Enabled, newValue }`
  - `ToggleState` → `StateChanged { Checked, newValue == ToggleState_On }` + `ValueChanged`
  - `RangeValue_Value` / `Value_Value` → `ValueChanged`
  - `ExpandCollapseState` → `StateChanged { Expanded, newValue == Expanded }`

**Cache request:** Pre-fetch `Name`, `ControlType`, `BoundingRectangle`, `IsEnabled`, `HasKeyboardFocus` in the cache request passed to all registration calls to avoid extra COM round-trips in handlers.

**Thread safety:** Handlers are invoked on a UIA MTA background thread. Build the `ElementData` snapshot inside the handler before the live reference crosses a thread boundary.

---

## What Is Not Included

The following concepts were considered and excluded:

**CaretMoved** (AT-SPI2 `Text:CaretMoved`): Linux-only, not emitted by all toolkits, and cursor position can be queried from the element after a `FocusChanged` or `TextChanged` event. Not worth a cross-platform event type.

**Window moved/resized**: AT-SPI2 `Window:Move/Resize` and macOS `AXWindowMoved/Resized` exist, but there is no clean UIA equivalent. Consumer polling via `BoundingRectangle` is sufficient for the use cases (drag/resize detection).

**Drag and drop**: UIA has `Drag_*` and `DropTarget_*` events; the other platforms do not. Excluded.

**Document load complete**: AT-SPI2 `Document:LoadComplete` is web/document-specific and has no equivalent on macOS or Windows accessibility APIs at the right level. Excluded.

**TextSelectionChanged as distinct from SelectionChanged**: AT-SPI2 and UIA have a separate text selection signal. macOS uses `AXSelectedTextChanged`. These are unified under `SelectionChanged` since the consumer re-queries selection state on receipt anyway.

**InvokeEvent (button activated)**: UIA `Invoke_InvokedEventId` has no direct counterpart on macOS or Linux at the accessibility event level. Excluded. Consumers wanting to observe button presses should watch `StateChanged` or `ValueChanged` on the downstream effect.

**Element-scoped subscriptions**: All three platforms support listening to a narrower subtree or individual element. This requires tracking which elements to observe and handling their lifecycle. Valuable, but a future addition — app-scoped subscriptions cover the current use cases (testing, automation).

---

## Implementation Status

### macOS — shipping

Implemented in `xa11y-macos/src/ax.rs`:

- `AXObserverCreate` is called for the target app's PID; the returned run loop source is attached to a dedicated thread's `CFRunLoop`, so delivery is push-based and does not block the calling thread.
- App-level registrations cover `AXFocusedUIElementChanged`, `AXValueChanged`, `AXTitleChanged`, `AXElementBusyChanged`, `AXWindowCreated`, `AXUIElementDestroyed`, `AXFocusedWindowChanged`, `AXWindowMiniaturized`, `AXWindowDeminiaturized`, `AXSelectedTextChanged`, `AXSelectedRowsChanged`, `AXSelectedCellsChanged`, `AXSelectedChildrenChanged`, `AXMenuOpened`, `AXMenuClosed`, and `AXAnnouncementRequested`.
- The callback builds a full `ElementData` snapshot via a standalone `build_snapshot_data` (shared with `Provider::build_element_data`) before the live `AXUIElementRef` goes out of scope, so `event.target` is a durable snapshot rather than a zombie reference.
- `AXValueChanged` on a checkbox/radio emits `StateChanged { Checked, … }` alongside `ValueChanged`. The `Checked` value is sourced from the snapshot's resolved `states.checked`, which handles both `CFBoolean` and `CFNumber` representations (AccessKit's macOS bridge exposes checkbox values as `CFBoolean` — the earlier `ax_number_f64` path returned `false` unconditionally and was corrected).
- `AXValueChanged` on a text role additionally emits `TextChanged`. `AXUIElementDestroyed` emits `WindowClosed` when the destroyed element's role is `AXWindow` and `StructureChanged` otherwise.

### Windows — shipping

Implemented in `xa11y-windows/src/uia.rs`:

- Four COM event handlers are registered via the `IUIAutomation` root object: `AddFocusChangedEventHandler` (system-wide focus), `AddAutomationEventHandler` (window open/close, menu open/close, text changed, selection-item changes, live-region and notification events), `AddPropertyChangedEventHandler` (name, IsEnabled, ToggleState, Value, RangeValue, ExpandCollapseState), and `AddStructureChangedEventHandler`. All scoped handlers target `TreeScope_Subtree` on the app root resolved via `find_app_by_pid`.
- Handlers are COM-implementable via the `#[implement(...)]` macro from `windows-core`. Each wraps an `Arc<EventContext>` that holds the `mpsc::Sender<Event>` (protected by a `Mutex` because `std::sync::mpsc::Sender` is `!Sync`), the application name, PID, and a shared cache request.
- Handlers filter by PID inside the callback (the focus-changed registration has no scope parameter, and scoped handlers occasionally deliver events from neighbouring processes), then build a full `ElementData` snapshot via `build_snapshot_data` — the same free function the tree-read path uses — and queue an `Event` on the channel. `event.target` is therefore a durable snapshot, not a handle to a COM pointer that may become invalid as UIA cleans up.
- `PropertyChanged(ToggleState)` emits both `StateChanged { Checked }` and `ValueChanged` so consumers can filter on either; `PropertyChanged(ExpandCollapseState)` emits `StateChanged { Expanded, new == Expanded }`; `PropertyChanged(IsEnabled)` emits `StateChanged { Enabled, new }`. VARIANT payloads are decoded via the `TryFrom<&VARIANT>` impls provided by `windows::Win32::System::Variant`.
- The `Subscription`'s `CancelHandle` holds the four handler COM pointers + automation/root via a `ComSend<T>` wrapper (an explicit `unsafe impl Send for ComSend<T>` whose safety rests on the same MTA guarantee that backs the existing `unsafe impl Send for WindowsProvider`, with a private inner field so Rust 2021 disjoint captures don't peel the wrapper back). On drop, the closure calls each `RemoveXxxEventHandler` synchronously — when those return, UIA guarantees no further handler invocations, so a subsequent `subscribe()` call starts with a clean slate.

### Linux — shipping

Implemented in `xa11y-linux/src/events.rs`:

- Each `subscribe()` call opens a dedicated `zbus::blocking::Connection` to the AT-SPI accessibility bus (falling back to the session bus when the a11y bus launcher is unavailable) and registers four AT-SPI2 signal match rules via `org.freedesktop.DBus.AddMatch`: one per event interface (`Event.Object`, `Event.Window`, `Event.Focus`, `Event.Document`). Every rule is scoped by the target app's D-Bus unique name — resolved via `LinuxProvider::find_app_by_pid` — so signals from other applications never enter the subscription's channel.
- A dedicated OS thread drives `zbus::blocking::MessageIterator::from(conn.clone())` and fan-outs received signals to an `mpsc::Sender<Event>`. Because match rules are installed on the same Connection the iterator reads, the D-Bus daemon only forwards relevant traffic to us.
- Each incoming signal is dispatched through `signal_to_kinds(interface, member, detail, detail1, target_role)`, a pure mapping table covered by unit tests. The table intentionally emits multiple `EventKind`s from a single signal when the cross-platform model requires it: `Object:StateChanged(focused,true)` raises both `FocusChanged` and `StateChanged{Focused,true}` (AccessKit emits only the StateChanged variant, not `Focus:Focus`), and `Object:PropertyChange(accessible-value)` on a text role raises both `ValueChanged` and `TextChanged` (matching the macOS fan-out). AccessKit's AT-SPI bridge publishes slider/range value changes as `Object:PropertyChange(accessible-value)` rather than on the (older) `Object:ValueChanged` signal; both are wired through the table for compatibility with non-AccessKit toolkits.
- The event's `target` is built via a free `build_event_snapshot(conn, aref, pid)` helper that fetches role (name + numeric fallback), states (parsed from the AT-SPI2 bitfield), name, value (Text interface, then Value), and numeric bounds via direct D-Bus method calls. The snapshot's `handle` is always `0` — event targets are read-only, not live handles into the provider's cache. Consumers that need to drive actions must re-locate through the normal locator path.
- Cancellation is cooperative: on drop the cancel closure flips an `AtomicBool`, removes every match rule via `DBusProxy::remove_match_rule`, and triggers a D-Bus `Peer.Ping` round-trip whose reply travels through the iterator thread, forcing `.next()` to return so the stop flag can be observed. The thread is then joined — a subsequent `subscribe()` starts with a clean slate.
- `MenuOpened`/`MenuClosed` never fire on Linux (AT-SPI2 has no menu signals — documented in the design section above). `StateChanged{Enabled}` is sourced from either `Object:StateChanged(enabled, _)` or `Object:StateChanged(sensitive, _)` — AT-SPI2 still exposes both, and xa11y collapses them.

### End-to-end test coverage (macOS + Windows + Linux)

Integration tests in `xa11y/tests/integ_test.rs` are gated on `#[cfg(target_os = "macos")]`, `#[cfg(target_os = "windows")]`, and `#[cfg(target_os = "linux")]` and drive the AccessKit + winit test app. Each test subscribes, performs a deterministic action that is known to change AccessKit's tree, and hard-asserts that the expected `EventKind` arrives within 3 s. **No test catches `Error::Timeout` to pass silently** — a prior iteration of the suite did, and it hid real regressions.

On Windows the tests are **not** exercised by GitHub Actions (the hosted Windows runners lack the interactive desktop UIA needs); they run locally via `scripts/run_integ_tests_windows.ps1`. `xa11y-windows`'s unit tests — which exercise the subscribe/cancel round-trip, the VARIANT decoders, and the event-ID allowlists — are run by the Windows CI job. The Linux tests run in the Ubuntu CI job (`scripts/run_integ_tests.sh` sets up Xvfb + dbus-run-session + at-spi2-registryd); `xa11y-linux::events::tests` unit-tests the `signal_to_kinds` mapping table in isolation.

| EventKind                         | macOS | Windows | Linux | Trigger                                                       |
|-----------------------------------|-------|---------|-------|---------------------------------------------------------------|
| `FocusChanged`                    | Yes   | Yes     | Yes   | `focus()` on Cancel button                                    |
| `ValueChanged`                    | Yes   | Yes     | Yes   | `set_numeric_value()` on Slider (also asserted alongside `ToggleState` on Windows) |
| `NameChanged`                     | Yes   | Yes     | Yes   | Flip checkbox + press Submit to force a status-label update   |
| `StateChanged { Checked }`        | Yes   | Yes     | Yes   | Toggle checkbox                                               |
| `TextChanged`                     | Yes   | Yes¹    | **No**³ | `set_value()` on Name text field                              |
| `Announcement`                    | Yes   | **No**  | **No** | Press "Announce" button (updates a `Live::Polite` label value) |
| `StructureChanged`                | **No** | **No** | **No** | See below                                                     |
| `SelectionChanged`                | **No** | **No** | **No** | See below                                                     |
| `WindowOpened` / `WindowClosed`   | **No** | **No** | **No** | Test app is single-window                                     |
| `WindowActivated` / `WindowDeactivated` | **No** | **No** | **No** | Requires an OS-level key-window change                   |
| `MenuOpened` / `MenuClosed`       | **No** | **No** | **No**² | AccessKit bridges do not synthesize menu events               |
| `StateChanged` (non-`Checked` flags) | **No** | **No** | **No** | AccessKit does not post Busy/Enabled/Expanded state deltas in the test app |

¹ On Windows the text-field test accepts either `TextChanged` or `ValueChanged` because AccessKit's text provider may not expose the UIA TextPattern on every widget, causing the UIA runtime to emit `UIA_Value_PropertyChanged` instead of `UIA_Text_TextChangedEventId`. Either signal is a real text-mutation notification; treating them interchangeably keeps the test from breaking on unrelated AccessKit refactors.

² `MenuOpened` / `MenuClosed` will never fire on Linux. AT-SPI2 has no menu-open/close signal — the design doc calls this out as a genuine gap. Non-Linux bridges synthesize it; Linux consumers must poll or observe structural changes.

³ `TextChanged` on Linux has no end-to-end test yet. AccessKit's AT-SPI bridge only emits `Object:TextChanged` when the AccessKit tree's text content changes (`TextInserted` / `TextRemoved` diffs), but the xa11y `set_value` path on Linux goes through the AT-SPI `EditableText` interface — which AccessKit's bridge does not implement — so the tree never observes a mutation and no signal fires. The provider's mapping is covered by unit tests in `xa11y-linux::events::tests` (both `Object:TextChanged` and `PropertyChange(accessible-value)` on a text role promote to `TextChanged`). Real end-to-end coverage will require a GTK/Qt test harness on Linux.

**Limitations uncovered by the implementation effort:**

1. **`AXUIElementDestroyed` does not propagate to app-level observers through AccessKit.** Registration at the `AXApplication` element succeeds (`AXError == kAXErrorSuccess`), and most other notifications reach the callback as expected — but `AXUIElementDestroyed` fired by `accesskit_macos::EventGenerator::node_removed` on a subtree removal never reaches the observer. Other tests in the same session prove the observer is otherwise healthy. Covering `StructureChanged` therefore requires either per-element observer registration (tracking element lifetime) or a non-AccessKit test harness. Kept the design-level app-scope commitment; documented the gap here.
2. **AccessKit's `AXSelectedTextChangedNotification` requires `supports_text_ranges`,** which in turn requires `Role::TextRun` children on the text field. The current test app's `TextInput` node has no runs, so `set_text_selection` lands at the AX layer but AccessKit never synthesizes the notification. `AXSelectedRowsChangedNotification` / `AXSelectedChildrenChangedNotification` are documented as element-scope only and likewise don't surface at the app observer for descendants. Either a text-run-aware test app or element-scoped subscriptions unblock `SelectionChanged`.
3. **AccessKit does not post `AXElementBusyChanged`, `AXMenuOpened`, `AXMenuClosed`, or `AXWindowCreated`/`Miniaturized`/`Deminiaturized`.** These are driven by native AppKit objects (NSMenu, NSWindow), not by AccessKit's synthesized tree. A Cocoa/AppKit test app under `test-apps/cocoa/` is the right place to cover them.

### Follow-ups

- Either element-scoped subscriptions or a Cocoa test harness to cover the EventKinds that AccessKit's macOS bridge does not raise.
- A dedicated Win32/WPF test app (or widget additions to the AccessKit test app) so the Windows integration suite can cover `Announcement`, `MenuOpened`/`MenuClosed`, `WindowOpened`/`WindowClosed`, and `StateChanged { Enabled | Expanded }`.
- A GTK / Qt harness on Linux so we can reliably drive `Object:Announcement`, `Window:Create`/`Destroy`, and state deltas that AccessKit's AT-SPI bridge doesn't synthesize.
- Test-app enhancements: `Role::TextRun` children on the text field to enable text-selection events; a secondary-window workflow; `Node::set_busy()` drive for `StateChanged { Busy }`.
