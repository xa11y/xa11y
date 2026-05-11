import pytest
from xa11y._native import _make_test_app, _make_test_locator


@pytest.fixture
def test_app():
    """A Locator backed by a mock provider targeting the test app element."""
    return _make_test_locator()


@pytest.fixture
def mock_app():
    """An App backed by a mock provider (resolves TestApp from the shared mock tree)."""
    return _make_test_app()
