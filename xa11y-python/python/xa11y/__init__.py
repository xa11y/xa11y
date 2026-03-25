"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> tree = xa11y.app("Safari")
    >>> for button in tree.query("button"):
    ...     print(button.name)
    >>> tree.press("button[name='OK']")

With explicit provider:
    >>> provider = xa11y.connect()
    >>> tree = provider.app("Safari")
    >>> loc = tree.locator("button[name='Submit']")
    >>> loc.press()
"""

from xa11y._native import (
    ActionNotSupportedError,
    AppInfo,
    AppNotFoundError,
    InvalidSelectorError,
    Locator,
    Node,
    NormalizedRect,
    PermissionDeniedError,
    PlatformError,
    # Classes
    Provider,
    Rect,
    SelectorNotMatchedError,
    TimeoutError,
    Tree,
    # Exceptions
    XA11yError,
    app,
    check_permissions,
    # Convenience functions
    connect,
    list_apps,
)

__all__ = [
    "ActionNotSupportedError",
    "AppInfo",
    "AppNotFoundError",
    "InvalidSelectorError",
    "Locator",
    "Node",
    "NormalizedRect",
    "PermissionDeniedError",
    "PlatformError",
    "Provider",
    "Rect",
    "SelectorNotMatchedError",
    "TimeoutError",
    "Tree",
    "XA11yError",
    "app",
    "check_permissions",
    "connect",
    "list_apps",
]
