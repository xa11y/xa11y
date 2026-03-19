# Integration Test Coverage

Coverage analysis of `xa11y/tests/integ_test.rs` against the public API surface.
All integration tests run against a real GTK3 app via AT-SPI2 on Linux.

Last updated: 2026-03-18

## Provider Trait Methods

| Method | Status | Tests |
|--------|--------|-------|
| `get_app_tree` | Covered | All tree/selector/query tests |
| `get_all_apps` | Covered | `get_all_apps_returns_nonempty_tree` |
| `perform_action` | Covered | Multiple action tests (Press, Focus, Toggle, SetValue, Increment, Decrement, Expand, Collapse) |
| `check_permissions` | Covered | `check_permissions_granted` |
| `list_apps` | Covered | `list_apps_includes_test_app`, `list_apps_has_valid_pids` |

## AppTarget Variants

| Variant | Status | Tests |
|---------|--------|-------|
| `ByName` | Covered | All `get_test_app_tree` calls |
| `ByPid` | Covered | `app_target_by_pid` |
| `ByWindow` | Covered (error path) | `app_target_by_window_returns_platform_error` |

## Tree Methods

| Method | Status | Tests |
|--------|--------|-------|
| `root()` | Covered | `tree_has_root_application_node` |
| `get(id)` | Covered | `tree_get_returns_node_by_id`, `tree_get_invalid_id_returns_none` |
| `iter()` | Covered | `tree_iter_visits_all_nodes` |
| `children(id)` | Covered | `tree_children_returns_direct_children` |
| `subtree(id)` | Covered | `tree_subtree_includes_node_and_descendants` |
| `find_by_role` | Covered | Multiple tree_has_* tests |
| `find_by_name` | Covered | `tree_has_submit_button`, `tree_has_cancel_button_disabled`, `tree_has_labels` |
| `query` | Covered | 11 selector tests covering all features |
| `dump()` | Covered | `tree_dump_is_readable` |
| `len()` | Covered | `query_with_max_depth`, `query_with_max_elements` |
| `is_empty()` | Covered | `tree_is_empty_false_for_real_tree` |
| `rebuild_index()` | Covered | `real_tree_json_roundtrip` |

## Node Fields

| Field | Status | Tests |
|-------|--------|-------|
| `role` | Covered | Multiple role assertions |
| `name` | Covered | Name-based queries and assertions |
| `value` | Covered | Slider, text entry, spin button value checks |
| `description` | Covered | `node_description_on_image` |
| `bounds` | Covered | `node_bounds_present` |
| `bounds_normalized` | Covered | `node_bounds_normalized_present` |
| `actions` | Covered | `node_actions_list` |
| `states` | Covered | See StateSet below |
| `children` | Covered | `node_children_field` |
| `parent` | Covered | `node_parent_field` |
| `depth` | Covered | `node_depth_field` |
| `app_name` | Covered | `node_app_name_populated` |
| `raw` | Covered | `query_with_include_raw` |

## StateSet Fields

| Field | Status | Tests |
|-------|--------|-------|
| `enabled` | Covered | `tree_has_cancel_button_disabled`, `action_toggle_enables_cancel_button` |
| `visible` | Covered | `state_visible_on_shown_widget`, `query_with_visible_only` |
| `focused` | Covered | `state_focused_after_focus_action` |
| `checked` | Covered | `tree_has_checkbox`, `action_toggle_checkbox`, `action_toggle_on_checkbox` |
| `selected` | Covered | `state_selected_on_radio_button` (via checked on RadioButton) |
| `expanded` | Covered | `state_expanded_on_expander` |
| `editable` | Covered | `state_editable_on_text_entry` |
| `required` | Not coverable | No GTK3 widget sets this without custom ATK code |
| `busy` | Not coverable | No GTK3 widget sets this without custom ATK code |

- `Toggled::Off` and `Toggled::On` are covered. `Toggled::Mixed` is not easily testable with standard GTK3 CheckButton.

## QueryOptions Fields

| Field | Status | Tests |
|-------|--------|-------|
| `max_depth` | Covered | `query_with_max_depth` |
| `max_elements` | Covered | `query_with_max_elements` |
| `include_raw` | Covered | `query_with_include_raw` |
| `visible_only` | Covered | `query_with_visible_only` |
| `roles` | Covered | `query_with_roles_filter` |

## Action Variants

| Action | Status | Tests |
|--------|--------|-------|
| `Press` | Covered | `action_press_button`, `action_toggle_checkbox` |
| `Focus` | Covered | `action_focus_text_entry`, `state_focused_after_focus_action` |
| `SetValue` | Covered | `action_set_value_numeric_on_slider`, `action_set_value_text_on_entry` |
| `Toggle` | Covered | `action_toggle_on_checkbox` |
| `Expand` | Covered | `action_expand_collapse_on_expander` |
| `Collapse` | Covered | `action_expand_collapse_on_expander` |
| `Select` | Not coverable | Requires programmatic selection API not exposed by GTK3 ListBox via AT-SPI |
| `ShowMenu` | Not coverable | AT-SPI context menu action not reliably testable |
| `ScrollIntoView` | Not coverable | Requires ScrollTo support which varies by widget |
| `Increment` | Covered | `action_increment_decrement_on_spin_button` |
| `Decrement` | Covered | `action_increment_decrement_on_spin_button` |

### ActionData Variants

| Variant | Status | Tests |
|---------|--------|-------|
| `Value` | Covered | `action_set_value_text_on_entry` |
| `NumericValue` | Covered | `action_set_value_numeric_on_slider` |
| `ScrollAmount` | Not coverable | ScrollIntoView not testable |
| `Point` | Not coverable | No action consumes Point data |

## Selector Features

| Feature | Status | Tests |
|---------|--------|-------|
| Role match (`button`) | Covered | `selector_query_buttons` |
| `name=` (exact) | Covered | `selector_query_button_by_name` |
| `name*=` (contains) | Covered | `selector_query_name_contains` |
| `name^=` (starts-with) | Covered | `selector_name_starts_with` |
| `name$=` (ends-with) | Covered | `selector_name_ends_with` |
| `:nth(n)` | Covered | `selector_query_nth_button` |
| Descendant combinator (` `) | Covered | `selector_descendant_combinator` |
| Child combinator (`>`) | Covered | `selector_child_combinator` |
| `value*=` | Covered | `selector_value_attribute` |
| `role=` (attribute) | Covered | `selector_role_attribute` |
| Complex chains | Covered | `selector_complex_chain` |
| `description=` / `description*=` | Not covered | No widget with both description and matching selector use case |

## Role Variants Seen in Tree

Tested: `Application`, `Window`, `Button`, `CheckBox`, `RadioButton`, `TextArea`, `TextField`, `Slider`, `ProgressBar`, `StaticText`, `Group`, `MenuBar`, `MenuItem`, `Toolbar`, `Tab`, `TabGroup`, `ComboBox`, `Separator`, `Image`, `Table`, `TableCell`, `List`, `ListItem`.

**Not tested** (roles requiring specific widget types not in test app): `Unknown`, `Dialog`, `Alert`, `Menu` (submenu when opened), `Heading`, `TreeItem`, `WebArea`, `Link`, `ScrollBar`, `SplitGroup`, `TableRow`.

## EventProvider Trait

**Not implemented** for `LinuxProvider`. No integration tests possible until the trait is implemented.

## Error Variants

| Variant | Status | Tests |
|---------|--------|-------|
| `AppNotFound` | Covered | `error_app_not_found` |
| `NodeNotFound` | Covered | `error_node_not_found` |
| `InvalidSelector` | Covered | `error_invalid_selector` |
| `Platform` | Covered | `app_target_by_window_returns_platform_error`, `error_action_without_raw_data` |
| `TextValueNotSupported` | Covered | `action_set_value_text_on_entry` (handles this error variant) |
| `PermissionDenied` | Not coverable | Would need to revoke AT-SPI permissions during test |
| `ElementStale` | Not coverable | Would need element to disappear between snapshot and action |
| `Timeout` | Not coverable | EventProvider not implemented |

## Summary

- **~65 tests** covering the full Linux AT-SPI2 provider API surface
- All Provider trait methods covered
- All AppTarget variants covered (including error path for ByWindow)
- All Tree methods covered
- All Node fields covered
- All QueryOptions fields covered
- 8/11 Action variants covered (remaining 3 not reliably testable via AT-SPI)
- All selector features covered except description attribute
- 5 Error variants covered (remaining 3 not coverable without special infrastructure)
- EventProvider not implemented for LinuxProvider — cannot test
- `required` and `busy` StateSet fields not testable without custom ATK widget implementation
