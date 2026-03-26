---
title: Python Usage
description: Common patterns for using xa11y in Python.
---

## Getting an app tree

```python
import xa11y

tree = xa11y.app("Safari")
for node in tree:
    print(f"{node.role} — {node.name or ''}")
```

## Listing applications

```python
apps = xa11y.list_apps()
for app in apps:
    print(app.name)
```

## All apps at once

```python
tree = xa11y.all_apps()
```

## Selectors

```python
buttons = tree.query("button[name='Submit']")
```

## Performing actions

```python
tree.press("button[name='Submit']")
tree.set_value("text_field", "hello")
```

## Locators

Locators resolve lazily — they re-query the tree on each operation:

```python
loc = tree.locator("button[name='Submit']")
loc.press()

# Or create one directly:
loc = xa11y.locator("Safari", selector="button[name='Submit']")
loc.wait_visible(timeout=5.0)
loc.press()
```

## Further reading

- [API Reference](/xa11y/python/api/) — full Python API docs
