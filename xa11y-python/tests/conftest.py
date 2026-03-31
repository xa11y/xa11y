import pytest
from xa11y._native import _make_test_locator


@pytest.fixture
def test_app():
    """A Locator backed by a mock provider targeting the test app element."""
    return _make_test_locator()
