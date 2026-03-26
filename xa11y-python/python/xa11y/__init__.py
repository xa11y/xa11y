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

Subscribe to accessibility events:
    >>> with xa11y.subscribe("Safari", kinds=["focus_changed"]) as sub:
    ...     event = sub.try_recv()

Wait for an element state:
    >>> node = xa11y.wait_for("Safari", selector="button[name='OK']", state="attached")
"""

from xa11y._native import (
    ActionNotSupportedError,
    AppInfo,
    AppNotFoundError,
    Event,
    InvalidSelectorError,
    Locator,
    Node,
    PermissionDeniedError,
    PlatformError,
    Rect,
    SelectorNotMatchedError,
    Subscription,
    TextChangeData,
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
    subscribe,
    wait_for,
    wait_for_event,
)

__all__ = [
    "ActionNotSupportedError",
    "AppInfo",
    "AppNotFoundError",
    "Event",
    "InvalidSelectorError",
    "Locator",
    "Node",
    "PermissionDeniedError",
    "PlatformError",
    "Rect",
    "SelectorNotMatchedError",
    "Subscription",
    "TextChangeData",
    "TimeoutError",
    "Tree",
    "XA11yError",
    "all_apps",
    "app",
    "check_permissions",
    "list_apps",
    "locator",
    "subscribe",
    "wait_for",
    "wait_for_event",
]
