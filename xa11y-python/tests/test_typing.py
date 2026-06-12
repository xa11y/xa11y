"""Smoke test that type stubs are loadable and basic annotations work."""

import ast
import importlib.resources as resources

import xa11y
from xa11y import _native


def _load_stub_tree() -> ast.Module:
    stub = resources.files("xa11y") / "_native.pyi"
    return ast.parse(stub.read_text(encoding="utf-8"))


def _stub_class_members(tree: ast.Module) -> dict[str, set[str]]:
    """Map each stub class name to the names declared in its body.

    Collects methods (``def``), and class-body assignments (constants on
    ``EventType``, documented exception attributes). Instance attributes
    declared via ``AnnAssign`` don't exist on the runtime *class*, so the
    stub→runtime direction below only checks methods.
    """
    members: dict[str, set[str]] = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        names: set[str] = set()
        for item in node.body:
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                names.add(item.name)
            elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                names.add(item.target.id)
            elif isinstance(item, ast.Assign):
                for target in item.targets:
                    if isinstance(target, ast.Name):
                        names.add(target.id)
        members[node.name] = names
    return members


def _stub_class_methods(tree: ast.Module) -> dict[str, set[str]]:
    """Map each stub class name to just its method names."""
    methods: dict[str, set[str]] = {}
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            methods[node.name] = {
                item.name
                for item in node.body
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
            }
    return methods


def _public(names) -> set[str]:
    return {n for n in names if not n.startswith("_")}


def test_stub_covers_every_native_class_member():
    """Every public member of a native class must appear in the stub.

    The Python API reference on xa11y.dev is generated from _native.pyi, so
    a binding method missing from the stub is invisible to type checkers,
    IDEs, *and* the docs site — exactly how App.find shipped in 0.8.2
    without ever appearing in the documentation.
    """
    stub_members = _stub_class_members(_load_stub_tree())
    missing: list[str] = []
    for cls_name, declared in stub_members.items():
        runtime_cls = getattr(_native, cls_name, None)
        if runtime_cls is None:
            continue
        for member in _public(vars(runtime_cls)):
            if member not in declared:
                missing.append(f"{cls_name}.{member}")
    assert not missing, (
        "native members missing from _native.pyi (they will not appear in "
        f"the generated API docs): {sorted(missing)}"
    )


def test_stub_methods_all_exist_at_runtime():
    """Every method the stub declares must exist on the native class —
    catches stubs going stale after a binding rename/removal."""
    stub_methods = _stub_class_methods(_load_stub_tree())
    stale: list[str] = []
    for cls_name, declared in stub_methods.items():
        if cls_name.startswith("_"):
            # Typing-only fictions (e.g. _TestActionProbe) describe objects
            # that are reachable but not module attributes.
            continue
        runtime_cls = getattr(_native, cls_name, None)
        if runtime_cls is None:
            stale.append(cls_name)
            continue
        for method in _public(declared):
            if not hasattr(runtime_cls, method):
                stale.append(f"{cls_name}.{method}")
    assert not stale, f"stub declares members the native module lacks: {sorted(stale)}"


def test_stub_covers_module_level_names():
    """Public module-level classes/functions must match between the native
    module and the stub, in both directions."""
    tree = _load_stub_tree()
    stub_names = {
        node.name
        for node in tree.body
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
    }
    runtime_names = _public(vars(_native))
    missing_from_stub = sorted(runtime_names - stub_names)
    stale_in_stub = sorted(_public(stub_names) - runtime_names)
    assert not missing_from_stub, f"module names missing from _native.pyi: {missing_from_stub}"
    assert not stale_in_stub, f"stub declares module names the native module lacks: {stale_in_stub}"


def test_stub_types_are_accessible():
    """Verify the key types are importable and recognized as types."""
    # These would fail at import time if stubs were malformed
    assert xa11y.Element is not None
    assert xa11y.Locator is not None
    assert xa11y.Rect is not None


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
