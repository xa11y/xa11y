"""Document semantics (#476) against the mock document fixture."""

import pytest
import xa11y
from xa11y import _native


def one(selector):
    return _native._make_test_document_locator(selector).element()


def test_landmarks_and_headings():
    main = one('landmark[landmark_type="main"]')
    assert main.role == "landmark"
    assert main.landmark_type == "main"
    assert main.web_role == "main"
    assert one('heading[heading_level="2"]').name == "Section"
    assert one('heading[heading_level="1"]').heading_level == 1
    assert one('[name="Docs"]').landmark_type is None


def test_link_and_live_region():
    link = one('link[url*="example.com"]')
    assert link.link_url == "https://example.com/docs"
    status = one('[live="polite"]')
    live = status.live_region
    assert isinstance(live, xa11y.LiveRegionInfo)
    assert (live.politeness, live.atomic, live.relevant) == ("polite", True, "additions text")
    assert "polite" in repr(live)
    assert one('[name="Docs"]').live_region is None


def test_navigation_is_refused_with_its_replacement():
    with pytest.raises(xa11y.InvalidSelectorError, match="landmark_type"):
        _native._make_test_document_locator("navigation").element()


def test_relations_by_kind_in_request_order():
    field = one('text_field[name="Email"]')
    rel = field.relations(["described_by", "labelled_by", "controls"])
    assert list(rel) == ["described_by", "labelled_by", "controls"]
    assert [e.name for e in rel["labelled_by"]] == ["Email address"]
    assert [e.name for e in rel["described_by"]] == ["We never share it"]
    assert rel["controls"] == []
    label = rel["labelled_by"][0]
    assert [e.name for e in label.relations(["label_for"])["label_for"]] == ["Email"]


def test_relations_errors():
    field = one('text_field[name="Email"]')
    with pytest.raises(xa11y.ActionNotSupportedError, match="linked"):
        field.relations(["linked"])
    with pytest.raises(xa11y.InvalidActionDataError, match="labelled_by"):
        field.relations(["labeled_by"])
