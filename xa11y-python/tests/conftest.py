import pytest
from xa11y._native import _make_test_app


@pytest.fixture
def test_app():
    """An App backed by a mock provider with 13-node test tree."""
    return _make_test_app()


@pytest.fixture
def tree(test_app):
    """The root Node of a test tree with 13 nodes."""
    return test_app.nodes()
