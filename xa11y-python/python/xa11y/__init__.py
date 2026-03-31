"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> app = xa11y.locator('application[name="Safari"]')
    >>> app.child("button[name='Submit']").press()
    >>> for element in app.descendant("button").elements():
    ...     print(element.name)
"""

from xa11y._native import (
    ActionNotSupportedError,
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
    XA11yError,
    check_permissions,
    locator,
)

__all__ = [
    "ActionNotSupportedError",
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
    "check_permissions",
    "locator",
]
