import pytest
from xa11y._native import _make_test_tree


@pytest.fixture
def tree():
    """A test tree with 13 nodes backed by a mock provider."""
    return _make_test_tree()
