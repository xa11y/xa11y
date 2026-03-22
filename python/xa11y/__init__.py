"""xa11y — Cross-platform accessibility automation library.

Example usage::

    import xa11y

    provider = xa11y.create_provider()
    provider.check_permissions()

    # Snapshot-based API
    tree = provider.get_tree("Safari")
    buttons = tree.query("button")
    print(tree.dump())

    # Playwright-style Locator API
    save = provider.locator("MyApp", 'button[name="Save"]')
    save.press()
    save.wait_visible(timeout_secs=5.0)
"""

__version__ = "0.0.1"

from xa11y._native import (
    Action,
    AppInfo,
    Locator,
    Node,
    NormalizedRect,
    Provider,
    QueryOptions,
    Rect,
    Role,
    ScrollDirection,
    StateSet,
    Toggled,
    Tree,
    create_provider,
)

__all__ = [
    "Action",
    "AppInfo",
    "Locator",
    "Node",
    "NormalizedRect",
    "Provider",
    "QueryOptions",
    "Rect",
    "Role",
    "ScrollDirection",
    "StateSet",
    "Toggled",
    "Tree",
    "create_provider",
]
