"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> tree = xa11y.app("Safari")
    >>> for button in tree.query("button"):
    ...     print(button.name)
    >>> tree.press("button[name='OK']")

Reuse a locator for lazy resolution:
    >>> loc = xa11y.locator("Safari", selector="button[name='Submit']")
    >>> loc.press()
"""

from xa11y._native import (
    ActionNotSupportedError,
    AppInfo,
    AppNotFoundError,
    InvalidSelectorError,
    Locator,
    Node,
    PermissionDeniedError,
    PlatformError,
    Rect,
    SelectorNotMatchedError,
    TimeoutError,
    Tree,
    # Exceptions
    XA11yError,
    all_apps,
    app,
    check_permissions,
    list_apps,
    # Functions
    locator,
)

__all__ = [
    "ActionNotSupportedError",
    "AppInfo",
    "AppNotFoundError",
    "InvalidSelectorError",
    "Locator",
    "Node",
    "PermissionDeniedError",
    "PlatformError",
    "Rect",
    "SelectorNotMatchedError",
    "TimeoutError",
    "Tree",
    "XA11yError",
    "all_apps",
    "app",
    "check_permissions",
    "list_apps",
    "locator",
]
