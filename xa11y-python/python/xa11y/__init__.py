"""xa11y — Cross-Platform Accessibility Client Library for Python.

Quick start:
    >>> import xa11y
    >>> tree = xa11y.app("Safari")
    >>> for button in tree.query("button"):
    ...     print(button.name)
    >>> tree.press("button[name='OK']")

With explicit provider:
    >>> with xa11y.connect() as provider:
    ...     tree = provider.app("Safari")
    ...     loc = tree.locator("button[name='Submit']")
    ...     loc.press()

Testing with pytest::

    import xa11y
    import pytest

    @pytest.fixture
    def app():
        return xa11y.app("MyApp")

    def test_submit_form(app):
        app.set_value("text_input[name='Name']", "Alice")
        app.press("check_box[name='I agree']")
        app.press("button[name='Submit']")

        app = xa11y.app("MyApp")
        assert app.find_by_name("Submitted")

    def test_slider(app):
        slider = app.locator("slider[name='Volume']")
        assert slider.get().numeric_value == 50.0
        slider.increment()
        app = xa11y.app("MyApp")
        assert app.locator("slider[name='Volume']").get().numeric_value == 51.0
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
