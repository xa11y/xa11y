---
title: Python Overview
description: Using xa11y from Python.
---

xa11y provides Python bindings via PyO3. You get the same cross-platform accessibility API in Python.

## Installation

```bash
pip install xa11y
```

## Quick example

```python
import xa11y

tree = xa11y.app("Safari")
buttons = tree.query("button")
print(f"Found {len(buttons)} buttons")
```

See the [Usage](/xa11y/python/usage/) guide for more examples.
