"""Smoke test that type stubs are loadable and basic annotations work."""

import xa11y


def test_stub_types_are_accessible():
    """Verify the key types are importable and recognized as types."""
    # These would fail at import time if stubs were malformed
    assert xa11y.Provider is not None
    assert xa11y.Tree is not None
    assert xa11y.Node is not None
    assert xa11y.Locator is not None
    assert xa11y.Rect is not None
    assert xa11y.NormalizedRect is not None
    assert xa11y.AppInfo is not None


def test_py_typed_marker_exists():
    """Verify py.typed exists so type checkers discover our package."""
    import importlib.resources as resources

    files = resources.files("xa11y")
    py_typed = files / "py.typed"
    assert py_typed.is_file()


def test_stub_file_exists():
    """Verify the .pyi stub exists alongside the native module."""
    import importlib.resources as resources

    files = resources.files("xa11y")
    stub = files / "_native.pyi"
    assert stub.is_file()
