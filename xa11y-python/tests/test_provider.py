"""Tests for module-level functions: locator()."""


def test_app_has_name(test_app):
    app = test_app.element()
    assert app.name == "TestApp"


def test_app_has_pid(test_app):
    app = test_app.element()
    assert app.pid == 1234


def test_app_has_children(test_app):
    app = test_app.element()
    assert len(app.children()) > 0
