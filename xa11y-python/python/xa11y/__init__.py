"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> app = xa11y.locator('application[name="Safari"]')
    >>> app.child("button[name='Submit']").press()
    >>> for element in app.descendant("button").elements():
    ...     print(element.name)
"""

from xa11y._native import (
    AccessibilityNotEnabledError,
    ActionNotSupportedError,
    App,
    Element,
    Event,
    EventType,
    InputSim,
    InvalidActionDataError,
    InvalidSelectorError,
    Locator,
    PermissionDeniedError,
    PlatformError,
    Rect,
    Screenshot,
    SelectorNotMatchedError,
    Subscription,
    TimeoutError,
    XA11yError,
    input_sim,
    locator,
    screenshot,
)

__all__ = [
    "AccessibilityNotEnabledError",
    "ActionNotSupportedError",
    "App",
    "Element",
    "Event",
    "EventType",
    "InputSim",
    "InvalidActionDataError",
    "InvalidSelectorError",
    "Locator",
    "PermissionDeniedError",
    "PlatformError",
    "Rect",
    "Screenshot",
    "SelectorNotMatchedError",
    "Subscription",
    "TimeoutError",
    "XA11yError",
    "input_sim",
    "locator",
    "screenshot",
]
