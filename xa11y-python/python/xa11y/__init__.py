"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> app = xa11y.app("Safari")
    >>> app.locator("button[name='Submit']").press()
    >>> for node in app.locator("button").nodes():
    ...     print(node.name)
"""

from xa11y._native import (
    ActionNotSupportedError,
    # Types
    App,
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
    # Functions
    app,
    apps,
    check_permissions,
)

__all__ = [
    "ActionNotSupportedError",
    "App",
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
    "app",
    "apps",
    "check_permissions",
]
