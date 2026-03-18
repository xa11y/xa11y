# Integration Test Coverage

Coverage analysis of `xa11y/tests/integ_test.rs` against the public API surface.
All integration tests run against a real GTK3 app via AT-SPI2 on Linux.

Last updated: 2026-03-18

## Provider Trait Methods

| Method | Status | Tests |
|--------|--------|-------|
| `get_app_tree` | Covered | All tree/selector/query tests |
| `get_all_apps` | **Not covered** | — |
| `perform_action` | Partial | `action_press_button`, `action_toggle_checkbox`, `action_toggle_enables_cancel_button` |
| `check_permissions` | Covered | `check_permissions_granted` |
| `list_apps` | Covered | `list_apps_includes_test_app` |

## AppTarget Variants

| Variant | Status |
|---------|--------|
| `ByName` | Covered |
| `ByPid` | **Not covered** |
| `ByWindow` | **Not covered** |

## Tree Methods

| Method | Status | Tests |
|--------|--------|-------|
| `root()` | Covered | `tree_has_root_application_node` |
| `get(id)` | **Not covered** | — |
| `iter()` | **Not covered** | — |
| `children(id)` | **Not covered** | — |
| `subtree(id)` | **Not covered** | — |
| `find_by_role` | Covered | Multiple tree_has_* tests |
| `find_by_name` | Covered | `tree_has_submit_button`, `tree_has_cancel_button_disabled`, `tree_has_labels` |
| `query` | Partial | 4 selector tests; missing complex chains |
| `dump()` | Covered | `tree_dump_is_readable` |
| `len()` | Covered | `query_with_max_depth`, `query_with_max_elements` |
| `is_empty()` | **Not covered** | — |
| `rebuild_index()` | Covered | `real_tree_json_roundtrip` |

## Node Fields

| Field | Status | Notes |
|-------|--------|-------|
| `role` | Covered | Multiple role assertions |
| `name` | Covered | Name-based queries and assertions |
| `value` | Partial | Slider value checked; text entry value checked |
| `description` | **Not covered** | — |
| `bounds` | **Not covered** | — |
| `bounds_normalized` | **Not covered** | — |
| `actions` | **Not covered** | Actions list never inspected directly |
| `states` | Partial | See StateSet below |
| `children` | **Not covered** | Children list never inspected directly |
| `parent` | **Not covered** | — |
| `depth` | **Not covered** | — |
| `app_name` | Partial | Checked in JSON roundtrip |
| `raw` | Partial | `query_with_include_raw` checks Linux variant |

## StateSet Fields

| Field | Status | Tests |
|-------|--------|-------|
| `enabled` | Covered | `tree_has_cancel_button_disabled`, `action_toggle_enables_cancel_button` |
| `visible` | **Not covered** | — |
| `focused` | **Not covered** | — |
| `checked` | Covered | `tree_has_checkbox`, `action_toggle_checkbox` |
| `selected` | **Not covered** | — |
| `expanded` | **Not covered** | — |
| `editable` | **Not covered** | — |
| `required` | **Not covered** | — |
| `busy` | **Not covered** | — |

- `Toggled::Off` and `Toggled::On` are covered; `Toggled::Mixed` is **not covered**.

## QueryOptions Fields

| Field | Status | Tests |
|-------|--------|-------|
| `max_depth` | Covered | `query_with_max_depth` |
| `max_elements` | Covered | `query_with_max_elements` |
| `include_raw` | Covered | `query_with_include_raw` |
| `visible_only` | **Not covered** | — |
| `roles` | **Not covered** | — |

## Action Variants

| Action | Status | Notes |
|--------|--------|-------|
| `Press` | Covered | Button press and checkbox toggle |
| `Focus` | **Not covered** | — |
| `SetValue` | **Not covered** | — |
| `Toggle` | **Not covered** | (Press used on checkbox instead) |
| `Expand` | **Not covered** | — |
| `Collapse` | **Not covered** | — |
| `Select` | **Not covered** | — |
| `ShowMenu` | **Not covered** | — |
| `ScrollIntoView` | **Not covered** | — |
| `Increment` | **Not covered** | — |
| `Decrement` | **Not covered** | — |

### ActionData Variants

All **not covered**: `Value`, `NumericValue`, `ScrollAmount`, `Point`.

## Selector Features

| Feature | Status | Tests |
|---------|--------|-------|
| Role match (`button`) | Covered | `selector_query_buttons` |
| `name=` (exact) | Covered | `selector_query_button_by_name` |
| `name*=` (contains) | Covered | `selector_query_name_contains` |
| `:nth(n)` | Covered | `selector_query_nth_button` |
| Descendant combinator (` `) | **Not covered** | — |
| Child combinator (`>`) | **Not covered** | — |
| `value=` / `value*=` | **Not covered** | — |
| `description=` / `description*=` | **Not covered** | — |
| `role=` (attribute) | **Not covered** | — |
| `name^=` (starts-with) | **Not covered** | — |
| `name$=` (ends-with) | **Not covered** | — |

## Role Variants Seen in Tree

Tested: `Application`, `Window`, `Button`, `CheckBox`, `RadioButton`, `TextArea`, `Slider`, `ProgressBar`, `StaticText`, `Group`.

**Not tested** (25 roles): `Unknown`, `Dialog`, `Alert`, `ComboBox`, `List`, `ListItem`, `Menu`, `MenuItem`, `MenuBar`, `Tab`, `TabGroup`, `Table`, `TableRow`, `TableCell`, `Toolbar`, `ScrollBar`, `TextField`, `Image`, `Link`, `Heading`, `TreeItem`, `WebArea`, `Separator`, `SplitGroup`.

## EventProvider Trait

**Completely not covered.** No integration tests for `subscribe`, `wait_for_event`, or `wait_for`.

## Error Variants

| Variant | Status |
|---------|--------|
| `PermissionDenied` | **Not covered** |
| `AppNotFound` | **Not covered** (only hit in retry loop, not asserted) |
| `NodeNotFound` | **Not covered** |
| `ElementStale` | **Not covered** |
| `ActionNotSupported` | **Not covered** |
| `TextValueNotSupported` | **Not covered** |
| `Timeout` | **Not covered** |
| `InvalidSelector` | **Not covered** |
| `Platform` | **Not covered** |

## Summary

- **25 tests** covering happy-path tree reading, basic selectors, simple actions
- Major gaps: EventProvider, most Action variants, most StateSet fields, complex selectors, error paths, alternative AppTarget variants
- The test app has buttons, checkbox, radio buttons, text entry, slider, progress bar, and labels — but no expandable widgets, menus, tables, lists, or tree views
