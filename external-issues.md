# External issues

Upstream issues discovered while integrating new test apps or providers
against xa11y. Each entry records the symptom, the layer at fault, the
relevant source pointers, and the workaround (if any) in xa11y or its
test suites.

Issues are listed newest first.

---

## egui — `TextEdit` does not handle `accesskit::Action::SetValue`

**Discovered**: PR #211 (egui test app + cross-OS integ matrix), 2026-05.

**Symptom.** Calling `xa11y.set_value("…")` on an egui `TextEdit` widget
through xa11y's macOS or Windows provider returns success, but the text
in the widget does not change. On Linux the same call surfaces a clean
`Error::TextValueNotSupported`.

The asymmetry is purely architectural — see the analysis below — and not
an xa11y bug on any platform. The real defect is in egui.

**Root cause.** egui's `TextEdit` widget never iterates the AccessKit
action-request queue for `Action::SetValue`. The widget's source files
([`crates/egui/src/widgets/text_edit/builder.rs`](https://github.com/emilk/egui/blob/main/crates/egui/src/widgets/text_edit/builder.rs)
and
[`crates/egui/src/widgets/text_edit/state.rs`](https://github.com/emilk/egui/blob/main/crates/egui/src/widgets/text_edit/state.rs))
declare a role
(`TextInput` / `MultilineTextInput` / `PasswordInput`) but do not
consume `input.accesskit_action_requests(id, Action::SetValue)`. Compare
with `crates/egui/src/widgets/drag_value.rs` and
`crates/egui/src/widgets/slider.rs`, which do drain `Action::SetValue`
requests and apply the value.

**Why the platforms diverge.**

- **macOS** — `accesskit_macos`'s `setAccessibilityValue:` is an
  ObjC `void` method that calls `context.do_action(ActionRequest{…})`
  ([`platforms/macos/src/node.rs`](https://github.com/AccessKit/accesskit/blob/main/platforms/macos/src/node.rs)).
  Neither `do_action` nor `setAccessibilityValue:` can signal failure
  back to the AX client.

- **Windows** — `accesskit_windows`'s `IValueProvider::SetValue` calls
  `context.do_action(|| (Action::SetValue, …))`
  ([`platforms/windows/src/node.rs`](https://github.com/AccessKit/accesskit/blob/main/platforms/windows/src/node.rs)).
  `ActionHandler::do_action` returns `()`
  ([`platforms/windows/src/context.rs`](https://github.com/AccessKit/accesskit/blob/main/platforms/windows/src/context.rs)),
  so UIA's `S_OK` is reported as soon as the request is dispatched —
  not when the app actually applies it.

- **Linux** — `accesskit_unix`'s AT-SPI bridge does not expose the
  `EditableText` interface at all
  ([`platforms/atspi-common/src/node.rs`](https://github.com/AccessKit/accesskit/blob/main/platforms/atspi-common/src/node.rs)),
  only `Value.SetCurrentValue`. xa11y's Linux `set_value` calls
  `EditableText.SetTextContents`, which the bus rejects with
  `UnknownInterface`. xa11y maps that to
  `Error::TextValueNotSupported`. Linux never reaches AccessKit's
  action dispatch.

**Fix locations on xa11y's side.** None of the providers need changes —
they correctly follow the contract of their underlying platform AT.

- `xa11y-macos/src/ax.rs:2039-2057` (set_value)
- `xa11y-windows/src/uia.rs:1224-1234` (set_value)
- `xa11y-linux/src/atspi.rs:1719-1755` (set_value)

**Workaround in xa11y.** `tests/suites/python/test_actions.py` skips
`test_textfield_set_value` and `test_textfield_set_value_via_element`
when `app_name == "egui"`. Both skips reference this file.

**Upstream tracking.** No egui issue found at PR-time; a search on
`emilk/egui` for `SetValue` returned only the `drag_value.rs` and
`slider.rs` consumers, never a TextEdit handler.
**TODO: file egui issue.** Once filed, link it here and remove the
"no egui issue found" note.

---
