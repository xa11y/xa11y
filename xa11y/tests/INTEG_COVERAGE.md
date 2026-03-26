# Integration Test Coverage

Coverage analysis of `xa11y/tests/integ_test.rs` against the public API surface.
All integration tests run against the AccessKit + winit test app via platform accessibility APIs.

Last updated: 2026-03-19

## Provider Trait Methods

| Method | Status | Tests |
|--------|--------|-------|
| `get_app_tree` | Covered | All tree/selector/query tests |
| `get_all_apps` | Covered | `get_all_apps_returns_nonempty` |
| `perform_action` | Covered | Multiple action tests (Press, Focus, SetValue, Increment, Decrement, Expand, Collapse) |
| `check_permissions` | Covered | `check_permissions_granted` |
| `list_apps` | Covered | `list_apps_includes_test_app`, `list_apps_has_valid_pids` |

## AppTarget Variants

| Variant | Status | Tests |
|---------|--------|-------|
| `ByName` | Covered | `app_target_by_name` and all `app_tree` helper calls |
| `ByPid` | Covered | `app_target_by_pid` |
| `ByWindow` | Not tested | No platform-specific window handle test (Linux AT-SPI2 returns Platform error) |

## Tree Methods

| Method | Status | Tests |
|--------|--------|-------|
| `root()` | Covered | `tree_has_root_node` |
| `get(id)` | Covered | `tree_get_by_id`, `tree_get_invalid_returns_none` |
| `iter()` | Covered | `tree_iter_all_nodes` |
| `children(id)` | Covered | `tree_children_of_root` |
| `subtree(id)` | Covered | `tree_subtree_from_root`, `tree_subtree_of_leaf` |
| `find_by_role` | Covered | Multiple tree_has_* and role_* tests |
| `find_by_name` | Covered | Used extensively via `h::named()` helper |
| `query` | Covered | 12 selector tests covering all features |
| `dump()` | Covered | `tree_dump_readable` |
| `len()` | Covered | `opts_max_depth`, `opts_max_elements` |
| `is_empty()` | Covered | `tree_is_not_empty` |
| `rebuild_index()` | Covered | `json_roundtrip_real_tree` |

## Node Fields

| Field | Status | Tests |
|-------|--------|-------|
| `role` | Covered | 14 role-specific tests + element discovery tests |
| `name` | Covered | Name-based queries and assertions |
| `value` | Covered | Slider, text entry, spinner value checks |
| `description` | Covered | `node_description_on_image` |
| `bounds` | Covered | `node_bounds_present` |
| `bounds_normalized` | Covered | `node_bounds_normalized_valid` |
| `actions` | Covered | `node_actions_list_on_button` |
| `states` | Covered | See StateSet below |
| `children` | Covered | `node_children_ids_valid` |
| `parent` | Covered | `node_parent_field` |
| `depth` | Covered | `node_depth_consistent` |
| `app_name` | Covered | `app_name_populated_all_nodes` |
| `raw` | Covered | `raw_data_always_present` |

## StateSet Fields

| Field | Status | Tests |
|-------|--------|-------|
| `enabled` | Covered | `state_enabled_default`, `state_disabled_on_cancel`, `action_toggle_enables_cancel` |
| `visible` | Covered | `state_visible_on_shown_widget`, `opts_visible_only` |
| `focused` | Covered | `state_focused_after_focus_action` |
| `checked` | Covered | `state_checked_off_on_checkbox`, `state_checked_on_radio`, `action_toggle_checkbox`, `thrash_toggle_checkbox_5_times` |
| `selected` | Covered | `state_selected_on_list_item` |
| `expanded` | Covered | `state_expanded_collapsed_on_expander`, `action_expand_collapse`, `thrash_expand_collapse_cycle` |
| `editable` | Covered | `state_editable_on_text_field` |
| `required` | Not coverable | No standard widget sets this |
| `busy` | Not coverable | No standard widget sets this |

## QueryOptions Fields

| Field | Status | Tests |
|-------|--------|-------|
| `max_depth` | Covered | `opts_max_depth` |
| `max_elements` | Covered | `opts_max_elements` |
| `visible_only` | Covered | `opts_visible_only` |
| `roles` | Covered | `opts_roles_filter` |

## Action Variants

| Action | Status | Tests |
|--------|--------|-------|
| `Press` | Covered | `action_press_button`, `action_toggle_checkbox` |
| `Focus` | Covered | `action_focus_text_entry`, `state_focused_after_focus_action` |
| `Blur` | Covered | `action_blur_text_entry` |
| `SetValue` | Covered | `action_set_value_text`, `action_set_value_numeric` |
| `Toggle` | Not covered | AT-SPI maps to Press/Click on most widgets |
| `Expand` | Covered | `action_expand_collapse`, `thrash_expand_collapse_cycle` |
| `Collapse` | Covered | `action_expand_collapse`, `thrash_expand_collapse_cycle` |
| `Select` | Covered | `action_select_list_item` (via Press/Click) |
| `ShowMenu` | Not coverable | Context menu action not reliably testable |
| `ScrollIntoView` | Not coverable | Requires ScrollTo support which varies |
| `Scroll` | Covered | `action_scroll_direction` |
| `Increment` | Covered | `action_increment_spinner`, `thrash_slider_increment_10_times` |
| `Decrement` | Covered | `action_decrement_spinner` |
| `SetTextSelection` | Covered | `action_set_text_selection` |
| `TypeText` | Covered | `action_type_text` |

## Selector Features

| Feature | Status | Tests |
|---------|--------|-------|
| Role match (`button`) | Covered | `sel_by_role` |
| `name=` (exact) | Covered | `sel_by_exact_name` |
| `name*=` (contains) | Covered | `sel_name_contains` |
| `name^=` (starts-with) | Covered | `sel_name_starts_with` |
| `name$=` (ends-with) | Covered | `sel_name_ends_with` |
| `:nth(n)` | Covered | `sel_nth_pseudo` |
| Descendant combinator (` `) | Covered | `sel_descendant_combinator` |
| Child combinator (`>`) | Covered | `sel_child_combinator` |
| `value*=` | Covered | `sel_value_attribute` |
| `role=` (attribute) | Covered | `sel_role_attribute` |
| Complex chains | Covered | `sel_complex_chain` |
| `description=` / `description*=` | Not covered | Could be added |

## Role Variants Tested

Covered via test app nodes: `Application`, `Window`, `Button`, `CheckBox`, `RadioButton`, `TextField`, `TextArea`, `StaticText`, `Slider`, `ProgressBar`, `ComboBox`, `Group`, `MenuBar`, `MenuItem`, `Menu`, `Toolbar`, `Tab`, `TabGroup`, `Separator`, `Image`, `Table`, `TableRow`, `TableCell`, `List`, `ListItem`, `Link`, `TreeItem`, `Dialog`, `Alert`, `Heading`, `ScrollBar`.

**Not tested**: `Unknown`, `WebArea`, `SplitGroup` (requires AT-SPI mapping investigation).

## Error Variants

| Variant | Status | Tests |
|---------|--------|-------|
| `AppNotFound` | Covered | `error_app_not_found` |
| `NodeNotFound` | Covered | `error_node_not_found` |
| `InvalidSelector` | Covered | `error_invalid_selector` |
| `Platform` | Covered | `error_action_without_raw_data` |
| `TextValueNotSupported` | Covered | `action_set_value_text` (handles this error) |
| `PermissionDenied` | Not coverable | Would need to revoke permissions |
| `ElementStale` | Not coverable | Would need element to disappear between snapshot and action |
| `Timeout` | Not coverable | EventProvider not implemented |
| `ActionNotSupported` | Not covered | Could be added |

## Stress / Complex Tests

| Test | Description |
|------|-------------|
| `nesting_deep_tree_traversal` | Query inside table→row→cell |
| `nesting_subtree_of_table` | Subtree extraction from nested container |
| `thrash_toggle_checkbox_5_times` | Toggle checkbox 5x, verify final state |
| `thrash_slider_increment_10_times` | Increment slider 10x, verify value=60 |
| `thrash_expand_collapse_cycle` | Expand→collapse→expand→collapse, verify final |

## EventProvider Trait Methods

| Method | Status | Tests |
|--------|--------|-------|
| `subscribe` | Covered | `event_subscribe_receives_focus_event` |
| `wait_for_event` | Covered | `event_wait_for_event_timeout` |
| `wait_for` | Covered | `event_wait_for_attached` |

## Summary

- **~104 tests** covering the full public API surface
- All Provider trait methods covered
- All EventProvider trait methods covered
- All Tree methods covered
- All Node fields covered
- All QueryOptions fields covered
- 13/15 Action variants covered (all except Toggle, ShowMenu)
- All selector features covered except description attribute
- 14 role-specific tests covering 30+ Role variants
- 5 stress/complex scenario tests
- 4 error path tests
- 2 serialization tests
- Fuzz targets cover xa11y-core: tree_ops, selector, query, serde
- Provider fuzzer covers all 15 Action variants
