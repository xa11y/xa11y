---
title: Python API Reference
description: Full API reference for the xa11y Python package.
---

:::note
This page is a placeholder. Full API documentation will be generated and linked here once the Python package is published.
:::

## Module-level functions

- `xa11y.app(name, *, pid)` — get an app's accessibility tree
- `xa11y.all_apps()` — get all apps' accessibility trees
- `xa11y.list_apps()` — list running applications
- `xa11y.locator(name, *, pid, selector)` — create a lazy locator
- `xa11y.check_permissions()` — check accessibility permissions

## Key classes

- `Tree` — snapshot of an accessibility tree; supports `query()`, actions, `locator()`
- `Node` — a single element with role, name, value, bounds, states
- `Locator` — lazy element handle; re-queries on each operation
- `AppInfo` — application name, pid, bundle_id
