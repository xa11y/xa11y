"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> root = xa11y.app("Safari")
    >>> for button in root.query("button"):
    ...     print(button.name)

Reuse a locator for lazy resolution:
    >>> loc = root.locator("button[name='Submit']")
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
    "XA11yError",
    "all_apps",
    "app",
    "check_permissions",
    "list_apps",
    "locator",
]
