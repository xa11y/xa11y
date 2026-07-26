"""Smoke test that type stubs are loadable and basic annotations work."""

import ast
import importlib.resources as resources
import inspect

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


def _stub_class_constants(tree: ast.Module) -> dict[str, dict[str, object]]:
    """Map each stub class name to its constants: class-body assignments
    that carry a literal value (e.g. ``FOCUS_CHANGED: str = "focus_changed"``).

    Annotation-only declarations (``selector: str | None``) describe
    *instance* attributes set at runtime and are excluded — they have no
    class-level counterpart to compare against.
    """
    constants: dict[str, dict[str, object]] = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        values: dict[str, object] = {}
        for item in node.body:
            if (
                isinstance(item, ast.AnnAssign)
                and isinstance(item.target, ast.Name)
                and isinstance(item.value, ast.Constant)
            ):
                values[item.target.id] = item.value.value
            elif isinstance(item, ast.Assign) and isinstance(item.value, ast.Constant):
                for target in item.targets:
                    if isinstance(target, ast.Name):
                        values[target.id] = item.value.value
        if values:
            constants[node.name] = values
    return constants


def test_stub_constants_match_runtime_values():
    """Every constant the stub declares with a value (e.g. the EventType
    strings) must exist on the native class with that exact value — catches
    both stale constants (EventType.ALERT once outlived its runtime
    counterpart) and value drift."""
    constants = _stub_class_constants(_load_stub_tree())
    problems: list[str] = []
    for cls_name, values in constants.items():
        runtime_cls = getattr(_native, cls_name, None)
        if runtime_cls is None:
            problems.append(f"{cls_name}: class missing from native module")
            continue
        for name, expected in values.items():
            if not hasattr(runtime_cls, name):
                problems.append(f"{cls_name}.{name}: missing from native module")
            elif getattr(runtime_cls, name) != expected:
                problems.append(
                    f"{cls_name}.{name}: stub says {expected!r}, "
                    f"native has {getattr(runtime_cls, name)!r}"
                )
    assert not problems, f"stub constants out of sync with native module: {problems}"


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


# ── Signature parity ─────────────────────────────────────────────────────────
#
# The checks above compare member *names*. Renaming `Screenshot.to_png` to
# `to_pngg` is caught there; changing only its signature was caught by
# nothing — not by `cargo xtask check-bindings-parity` (names only) and not
# by a type checker, which has no source of truth beyond the stub itself.
#
# PyO3 emits a `__text_signature__` for every method, so `inspect.signature`
# reports the real parameter names, keyword-only split, and default values of
# the compiled module. That makes the stub checkable against the thing it
# claims to describe, with no Rust parsing and no extra dependency.
#
# What this does NOT compare is *types*: PyO3 attaches no annotations, so
# `param: str` in the stub has no runtime counterpart. Catching a wrong type
# annotation needs the Rust->PyO3 type-mapping table tracked separately.


def _normalise_default(value: object) -> str:
    """Render a runtime default the way ``ast.unparse`` renders a stub one."""
    if value is Ellipsis:
        return "..."
    return repr(value)


def _stub_shape(fn: ast.FunctionDef) -> tuple[list[str], list[str], dict[str, str]]:
    """Positional names, keyword-only names, and defaults declared in the stub."""
    args = fn.args
    positional = [p.arg for p in args.args if p.arg not in ("self", "cls")]
    keyword_only = sorted(p.arg for p in args.kwonlyargs)

    defaults: dict[str, str] = {}
    # Defaults align to the *tail* of the positional list.
    for param, default in zip(args.args[len(args.args) - len(args.defaults) :], args.defaults):
        defaults[param.arg] = ast.unparse(default)
    for param, default in zip(args.kwonlyargs, args.kw_defaults):
        if default is not None:
            defaults[param.arg] = ast.unparse(default)
    return positional, keyword_only, defaults


def _runtime_shape(fn: object) -> tuple[list[str], list[str], dict[str, str]] | None:
    """The same shape, read off the compiled module. ``None`` when PyO3
    exposes no signature (slot wrappers without a text signature)."""
    try:
        signature = inspect.signature(fn)  # type: ignore[arg-type]
    except (ValueError, TypeError):
        return None

    positional: list[str] = []
    keyword_only: list[str] = []
    defaults: dict[str, str] = {}
    for name, param in signature.parameters.items():
        if name in ("self", "cls"):
            continue
        if param.kind is param.KEYWORD_ONLY:
            keyword_only.append(name)
        elif param.kind is param.VAR_POSITIONAL:
            positional.append(f"*{name}")
        elif param.kind is param.VAR_KEYWORD:
            keyword_only.append(f"**{name}")
        else:
            positional.append(name)
        if param.default is not param.empty:
            defaults[name] = _normalise_default(param.default)
    return positional, sorted(keyword_only), defaults


def _is_property(fn: ast.FunctionDef) -> bool:
    return any(isinstance(d, ast.Name) and d.id == "property" for d in fn.decorator_list)


def _signature_problems(label: str, stub: ast.FunctionDef, runtime: object) -> list[str]:
    shape = _runtime_shape(runtime)
    if shape is None:
        return []
    rt_positional, rt_keyword_only, rt_defaults = shape
    st_positional, st_keyword_only, st_defaults = _stub_shape(stub)

    # Dunders are invoked positionally by the interpreter, and PyO3 owns the
    # rendered signature of slot-backed ones — `Rect.__eq__` reports its
    # parameter as `value` no matter what the Rust source calls it. Compare
    # how many parameters they take and leave the naming alone.
    if stub.name.startswith("__") and stub.name.endswith("__"):
        if len(rt_positional) != len(st_positional):
            return [
                f"{label}: takes {len(rt_positional)} argument(s), "
                f"stub declares {len(st_positional)}"
            ]
        return []

    problems = []
    if rt_positional != st_positional:
        problems.append(
            f"{label}: positional parameters are {rt_positional}, stub declares {st_positional}"
        )
    if rt_keyword_only != st_keyword_only:
        problems.append(
            f"{label}: keyword-only parameters are {rt_keyword_only}, "
            f"stub declares {st_keyword_only}"
        )
    if rt_defaults != st_defaults:
        problems.append(f"{label}: defaults are {rt_defaults}, stub declares {st_defaults}")
    return problems


def test_stub_method_signatures_match_runtime():
    """Every method the stub declares must have the signature the compiled
    module actually exposes — parameter names, keyword-only split, defaults.

    Members missing from one side entirely are the other tests' job; this one
    compares the ones both sides agree exist.
    """
    tree = _load_stub_tree()
    problems: list[str] = []
    compared = 0

    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        runtime_cls = getattr(_native, node.name, None)
        if runtime_cls is None:
            continue
        for item in node.body:
            if not isinstance(item, ast.FunctionDef) or _is_property(item):
                continue
            runtime_fn = getattr(runtime_cls, item.name, None)
            if runtime_fn is None:
                continue
            compared += 1
            problems.extend(_signature_problems(f"{node.name}.{item.name}", item, runtime_fn))

    # A refactor that stopped resolving runtime members would otherwise make
    # this test pass by comparing nothing at all.
    assert compared > 50, f"only compared {compared} methods; the lookup is probably broken"
    assert not problems, "stub signatures disagree with the native module:\n  " + "\n  ".join(
        problems
    )


def test_stub_function_signatures_match_runtime():
    """The same check for module-level functions."""
    tree = _load_stub_tree()
    problems: list[str] = []
    compared = 0

    for node in tree.body:
        if not isinstance(node, ast.FunctionDef):
            continue
        runtime_fn = getattr(_native, node.name, None)
        if runtime_fn is None:
            continue
        compared += 1
        problems.extend(_signature_problems(node.name, node, runtime_fn))

    assert compared >= 5, f"only compared {compared} functions; the lookup is probably broken"
    assert not problems, "stub signatures disagree with the native module:\n  " + "\n  ".join(
        problems
    )
