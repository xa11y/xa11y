"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> app = xa11y.app("Safari")
    >>> app.locator("button[name='Submit']").press()
    >>> for element in app.locator("button").elements():
    ...     print(element.name)
"""

from xa11y._native import (
    ActionNotSupportedError,
    # Types
    App,
    AppNotFoundError,
    Element,
    Event,
    EventType,
    InvalidSelectorError,
    Locator,
    PermissionDeniedError,
    PlatformError,
    Rect,
    SelectorNotMatchedError,
    Subscription,
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
    "Element",
    "Event",
    "EventType",
    "InvalidSelectorError",
    "Locator",
    "PermissionDeniedError",
    "PlatformError",
    "Rect",
    "SelectorNotMatchedError",
    "Subscription",
    "TimeoutError",
    "XA11yError",
    "app",
    "apps",
    "check_permissions",
]
