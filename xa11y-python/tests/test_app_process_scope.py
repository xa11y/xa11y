"""App operations include every native registration for one process."""

from xa11y._native import _make_test_app


def test_process_scope_is_shared_by_all_app_inspection_methods():
    app = _make_test_app(split=True)
    expected = ["Main Window", "Second Window"]
    assert [window.name for window in app.windows()] == expected
    assert [child.name for child in app.children()] == expected
    assert [element.name for element in app.locator("window").elements()] == expected
    assert [child["name"] for child in app.tree(max_depth=1)["children"]] == expected
    assert 'window "Second Window"' in app.dump(max_depth=1)
    assert app.locator("window:nth(2)").element().name == "Second Window"
    assert app.locator("window:nth(1)").count() == 1
    assert app.tree(max_depth=0)["children"] == []
    assert len(app.dump(max_depth=0).splitlines()) == 1
    # Element remains the explicit escape hatch for a single native root.
    assert len(app.as_element().children()) == 1
