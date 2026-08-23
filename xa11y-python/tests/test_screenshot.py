"""The `screenshot()` binding: argument validation and the annotation legend.

None of this reaches a display. `screenshot()` needs a real capture backend,
which no headless CI runner has, so what is testable here is everything that
happens *before* the first OS call — argument parsing, the `Locator | str`
union, the shapes of the types the legend is made of — plus the promise that
an unannotated capture reports an empty legend rather than nothing at all.

The end-to-end path (a real capture, boxes over the test app's buttons) is
`xa11y/tests/integ/screenshot.rs`.
"""

import inspect

import pytest
import xa11y

# ── Argument validation, before any capture ──────────────────────────────────


def test_element_and_region_together_is_rejected(test_app):
    element = test_app.element()
    with pytest.raises(ValueError, match="either `element` or `region`"):
        xa11y.screenshot(element=element, region=(0, 0, 10, 10))


def test_screenshot_takes_only_keyword_arguments():
    signature = inspect.signature(xa11y.screenshot)
    assert [p.kind for p in signature.parameters.values()] == [inspect.Parameter.KEYWORD_ONLY] * 3
    assert list(signature.parameters) == ["element", "region", "annotate"]


@pytest.mark.parametrize("entry", [1, None, 2.5, b"button", ["button"], {"selector": "button"}])
def test_annotate_rejects_anything_that_is_not_a_locator_or_a_string(entry):
    # Parsing runs before the capture, so a bad argument costs no pixels —
    # and is testable with no display, which is the only coverage available.
    with pytest.raises(TypeError, match="Locator or a selector string"):
        xa11y.screenshot(annotate=[entry])


def test_annotate_type_error_names_the_type_it_got():
    with pytest.raises(TypeError) as excinfo:
        xa11y.screenshot(annotate=[42])
    assert "int" in str(excinfo.value)


def test_annotate_rejects_a_bad_entry_after_a_good_one(test_app):
    # The whole list is parsed before anything happens, so a valid first
    # group does not get half-applied.
    with pytest.raises(TypeError, match="Locator or a selector string"):
        xa11y.screenshot(annotate=[test_app, object()])


def test_annotate_must_be_a_sequence(test_app):
    # A bare Locator is a common slip; it must not be silently accepted as a
    # one-element list.
    with pytest.raises(TypeError):
        xa11y.screenshot(annotate=test_app)


# ── The legend types ─────────────────────────────────────────────────────────


def test_legend_and_omission_classes_are_exported():
    assert xa11y.LegendEntry is not None
    assert xa11y.Omission is not None
    for name in ("LegendEntry", "Omission"):
        assert name in xa11y.__all__


@pytest.mark.parametrize(
    ("cls", "attributes"),
    [
        (
            xa11y.LegendEntry,
            ("tag", "group", "index", "selector", "role", "name", "bounds", "color"),
        ),
        (xa11y.Omission, ("selector", "role", "name", "reason")),
    ],
)
def test_legend_types_expose_the_documented_attributes(cls, attributes):
    for attribute in attributes:
        descriptor = getattr(cls, attribute, None)
        assert inspect.isdatadescriptor(descriptor), f"{cls.__name__}.{attribute}"


def test_legend_types_have_no_constructor():
    # A legend entry only ever comes from a capture, so there is nothing for a
    # caller to construct. This is *not* the frozen check: `cls()` raises
    # because the pyclass declares no `#[new]`, and it would raise just the
    # same without `#[pyclass(frozen)]`. See the two tests below for that.
    for cls in (xa11y.LegendEntry, xa11y.Omission):
        with pytest.raises(TypeError):
            cls()


def _annotated_capture():
    """An annotated capture, or a skip when this session cannot take one.

    Neither legend type has a constructor, so a real capture is the only way
    to hold an instance — and a capture needs a display plus whatever the
    platform asks for screen recording, which a headless runner has not got.

    One group per running application rather than a bare ``"button"``: a
    rootless group is refused, because its ``:nth(n)`` counts within one
    application while the legend counts across all of them.
    """
    try:
        apps = xa11y.App.list()
    except xa11y.XA11yError as exc:
        pytest.skip(f"no applications to annotate here: {exc}")
    if not apps:
        pytest.skip("no running application to scope an annotation group to")
    try:
        return xa11y.screenshot(annotate=[app.locator("button") for app in apps])
    except xa11y.XA11yError as exc:
        pytest.skip(f"no annotated capture available here: {exc}")


def test_legend_entries_are_frozen():
    # `#[pyclass(frozen)]`: a legend describes a capture that already
    # happened, so an entry a caller edited would describe nothing. Assignment
    # on an instance is the only thing that proves it.
    shot = _annotated_capture()
    if not shot.legend:
        pytest.skip("nothing on screen matched `button`, so there is no entry to assign to")
    entry = shot.legend[0]
    original = entry.tag
    with pytest.raises(AttributeError):
        entry.tag = "Z9"
    assert entry.tag == original


def test_omissions_are_frozen():
    shot = _annotated_capture()
    if not shot.omitted:
        pytest.skip("every match was drawable here, so there is no omission to assign to")
    omission = shot.omitted[0]
    original = omission.reason
    with pytest.raises(AttributeError):
        omission.reason = "made_up"
    assert omission.reason == original


# ── The unannotated capture still answers ────────────────────────────────────


def test_screenshot_declares_legend_omitted_and_truncated():
    # Both lists are `[]` and `truncated` is 0 on an unannotated capture, so
    # consumers need no version check — asserted on the class because a real
    # capture needs a display.
    for name in ("legend", "omitted", "truncated"):
        assert inspect.isdatadescriptor(getattr(xa11y.Screenshot, name, None)), name
