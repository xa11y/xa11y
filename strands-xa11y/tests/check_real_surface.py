#!/usr/bin/env python3
"""Check this package's assumptions against the real xa11y module.

The test suite swaps in `fake_xa11y` (see conftest.py), which is what lets it
run on a headless CI runner with no display and no accessibility bus. The cost
is that nothing in `tests/` would notice xa11y renaming an exception class,
dropping a diagnosis attribute, or moving a method — the fake would keep
answering happily and the whole suite would stay green while every real call
failed.

Living in the xa11y repository is what makes that checkable: this script runs
against the freshly built bindings in the `python` CI job, so a change to
xa11y's Python surface and the strands tool that reads it break in the same
pull request rather than after a release.

Deliberately not a `test_*.py`: pytest would apply the conftest that installs
the fake, and the check would then verify the fake against itself. Run it
directly:

    python strands-xa11y/tests/check_real_surface.py
"""

from __future__ import annotations

import ast
import importlib
import importlib.util
import inspect
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

# The classes the fake stands in for. A fake class whose name is not here is
# local scaffolding, not a stand-in, and is not checked.
STAND_INS = ("App", "Element", "Locator", "Rect", "Screenshot", "InputSim")

# Methods the fake defines for its own use rather than as a claim about the
# real module. Each needs a reason, and an entry naming a method the fake no
# longer has is reported rather than left to read as a live exemption.
FAKE_ONLY = {
    "Element.descendants": (
        "A helper for the fake's own selector evaluator, which walks the tree in Python. "
        "The real Locator does descendant matching in Rust, so xa11y.Element has no equivalent."
    ),
}

# Attributes `_errors.describe` reads off a failed call to build the message
# the model sees. They are read with a `getattr(..., None)` default, which is
# right at runtime — a plain exception carries none of them — and is exactly
# why their loss would otherwise be silent: the diagnosis would just stop
# appearing in tool results.
DIAGNOSIS_ATTRS = ("condition", "selector", "last_observed", "elapsed", "candidates", "scope")

# The two xa11y errors documented to carry a full diagnosis.
DIAGNOSIS_ERRORS = ("SelectorNotMatchedError", "TimeoutError")


def load_errors_module():
    """Load `_errors.py` on its own, without importing the package.

    `strands_xa11y/__init__.py` pulls in pydantic and the Strands SDK. Neither
    has anything to do with what this script checks, and requiring them would
    mean the `python` CI job — which exists to build the bindings — had to
    install the agent stack to run it. `_errors.py` imports nothing but the
    standard library, so it loads cleanly on its own.
    """
    path = HERE.parent / "src" / "strands_xa11y" / "_errors.py"
    spec = importlib.util.spec_from_file_location("_strands_xa11y_errors", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"Could not load {path}.")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def declared_error_attributes(stub: Path) -> dict[str, set[str]]:
    """Read each exception class's declared attributes out of xa11y's stub.

    The diagnosis fields are set on the exception *instance* when it is
    raised (`value.setattr("selector", ...)` in xa11y-python/src/lib.rs), not
    on the class, so there is nothing to introspect without a real failure —
    and a headless runner cannot produce one: with no accessibility bus every
    call raises PlatformError instead.

    `_native.pyi` ships in the wheel, carries `py.typed`, and declares these
    attributes as the documented contract. Reading it is the check that works
    with no display, no bus, and no windows.
    """
    if not stub.is_file():
        raise SystemExit(f"xa11y's type stub is missing: {stub}")
    tree = ast.parse(stub.read_text(encoding="utf-8"))
    return {
        node.name: {
            statement.target.id
            for statement in node.body
            if isinstance(statement, ast.AnnAssign) and isinstance(statement.target, ast.Name)
        }
        for node in tree.body
        if isinstance(node, ast.ClassDef)
    }


def public_members(cls: type) -> set[str]:
    """Named methods and properties a fake class defines, dunders excluded.

    `__getattr__` catch-alls (fake Locator, fake InputSim) define no named
    members, so they contribute nothing here — which is correct: there is no
    claim about the real surface to check.
    """
    return {
        name
        for name, value in vars(cls).items()
        if not name.startswith("__") and (inspect.isfunction(value) or isinstance(value, (property, staticmethod)))
    }


def check_declared_bound(failures: list[str]) -> None:
    """The declared `xa11y` bound must admit the xa11y that is installed.

    The publish workflow checks only the *shape* of this line, so a bound that
    excludes the current release passes every gate while `pip install
    strands-xa11y` refuses to co-install the xa11y it is built against. That is
    invisible until a user tries it, and it happens on exactly the release that
    matters: the one whose surface this script just verified.
    """
    try:
        from importlib.metadata import version

        import tomllib
        from packaging.requirements import Requirement
    except ImportError as exc:  # pragma: no cover - present in every supported env
        # Visible rather than silent: a check that quietly becomes a no-op is
        # indistinguishable from one that passed.
        print(f"note: skipping the xa11y bound check ({exc}).", file=sys.stderr)
        return

    pyproject = Path(__file__).resolve().parent.parent / "pyproject.toml"
    # Parsed, not line-scanned: the keywords list contains a bare "xa11y" that
    # a substring scan happily mistakes for the dependency, yielding an empty
    # specifier that admits everything — a check that always passes.
    manifest = tomllib.loads(pyproject.read_text())
    declared = next(
        (dep for dep in manifest["project"]["dependencies"] if Requirement(dep).name == "xa11y"),
        None,
    )
    if declared is None:
        failures.append("pyproject.toml declares no `xa11y` dependency to check.")
        return

    installed = version("xa11y")
    if not Requirement(declared).specifier.contains(installed, prereleases=True):
        failures.append(
            f"pyproject declares {declared!r} but the xa11y built from this tree is "
            f"{installed}. The bound would refuse to co-install the release whose "
            f"surface this script just verified — widen it in the same change."
        )


def main() -> int:
    try:
        xa11y = importlib.import_module("xa11y")
    except ImportError as exc:
        print(f"error: the real xa11y module is not importable ({exc}).")
        print("Build the bindings first: cargo xtask test-python")
        return 1

    # The repository root holds a directory called `xa11y/` (the Rust crate),
    # so an invocation that puts the root on sys.path — `python -c`, `python
    # -m` — imports that as an implicit namespace package instead of the
    # bindings. Everything below would then report the whole surface missing.
    if getattr(xa11y, "__file__", None) is None:
        print(f"error: `import xa11y` resolved to a namespace package at {list(xa11y.__path__)}.")
        print("That is the repository's `xa11y/` crate directory shadowing the installed")
        print("bindings. Run this script by path from the repository root, not with -c/-m.")
        return 1

    import fake_xa11y

    _GUIDANCE = load_errors_module()._GUIDANCE

    failures: list[str] = []

    check_declared_bound(failures)

    # 1. Every exception name this package writes guidance for must exist, and
    #    must still be an xa11y error — a name that survived as something else
    #    would silently stop matching in `describe`'s MRO walk.
    for name in sorted(_GUIDANCE):
        real = getattr(xa11y, name, None)
        if real is None:
            failures.append(f"_errors._GUIDANCE has an entry for {name!r}, which xa11y no longer exports.")
        elif not (isinstance(real, type) and issubclass(real, xa11y.XA11yError)):
            failures.append(f"xa11y.{name} is no longer an XA11yError subclass.")

    # 2. The diagnosis attributes `describe` renders.
    declared = declared_error_attributes(Path(xa11y.__file__).parent / "_native.pyi")
    for error_name in DIAGNOSIS_ERRORS:
        if getattr(xa11y, error_name, None) is None:
            failures.append(f"xa11y no longer exports {error_name}, which is expected to carry a diagnosis.")
            continue
        if error_name not in declared:
            failures.append(f"xa11y's type stub no longer declares a class {error_name}.")
            continue
        for attr in sorted(set(DIAGNOSIS_ATTRS) - declared[error_name]):
            failures.append(f"xa11y.{error_name} no longer carries the {attr!r} diagnosis attribute.")

    # 3. Every method the fake claims the real module has.
    for class_name in STAND_INS:
        fake = getattr(fake_xa11y, class_name, None)
        real = getattr(xa11y, class_name, None)
        if fake is None:
            failures.append(f"fake_xa11y no longer defines {class_name}; drop it from STAND_INS.")
            continue
        if real is None:
            failures.append(f"fake_xa11y fakes {class_name}, which xa11y no longer exports.")
            continue
        for member in sorted(public_members(fake)):
            if f"{class_name}.{member}" in FAKE_ONLY:
                continue
            if not hasattr(real, member):
                failures.append(f"fake_xa11y.{class_name}.{member} has no counterpart on xa11y.{class_name}.")

    # 4. A FAKE_ONLY entry that no longer names anything excuses nothing while
    #    still reading as a live design decision, so it fails rather than
    #    accumulating — the same way the bindings parity allowlist treats a
    #    stale per-member entry.
    for entry in sorted(FAKE_ONLY):
        class_name, _, member = entry.partition(".")
        fake = getattr(fake_xa11y, class_name, None)
        if fake is None or member not in public_members(fake):
            failures.append(f"FAKE_ONLY names {entry}, which fake_xa11y no longer defines. Drop the entry.")

    if failures:
        print(f"xa11y surface drift ({len(failures)} problem(s)):\n")
        for failure in failures:
            print(f"  - {failure}")
        print("\nEither the package needs updating for the new xa11y surface, or the fake does.")
        return 1

    print(f"xa11y surface OK: {len(_GUIDANCE)} error names, {len(STAND_INS)} faked classes.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
