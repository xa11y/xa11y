"""Selector paths must survive a comma.

A selector group is the form the xa11y docs teach for "either of these", and
this package extends model-supplied selectors into ref paths. xa11y splits on
top-level commas *first*, so plain concatenation rebinds the suffix to the last
clause — and a mis-bound path can still resolve to exactly one element, which is
all ``resolve_ref`` checks before using it. That is the failure the ref design
exists to prevent: acting on the wrong control and reporting success.
"""

from strands_xa11y import _selectors


class TestSplitClauses:
    def test_a_plain_selector_is_one_clause(self):
        assert _selectors.split_clauses("button") == ["button"]

    def test_clauses_are_split_and_trimmed(self):
        assert _selectors.split_clauses("a, b ,c") == ["a", "b", "c"]

    def test_a_comma_inside_an_attribute_value_is_not_a_boundary(self):
        selector = "button[name='All Clear, Please']"
        assert _selectors.split_clauses(selector) == [selector]

    def test_a_comma_inside_double_quotes_is_not_a_boundary(self):
        selector = 'button[name="a,b"], link'
        assert _selectors.split_clauses(selector) == ['button[name="a,b"]', "link"]

    def test_an_escaped_quote_does_not_end_the_string(self):
        selector = r'button[name="a\",b"], link'
        assert _selectors.split_clauses(selector) == [r'button[name="a\",b"]', "link"]

    def test_is_group_only_for_more_than_one_clause(self):
        assert not _selectors.is_group("button")
        assert not _selectors.is_group("button[name='a,b']")
        assert _selectors.is_group("a, b")


class TestChain:
    def test_a_single_clause_is_plain_concatenation(self):
        assert _selectors.chain("group", " > ", "button") == "group > button"

    def test_a_group_distributes_over_every_clause(self):
        # The bug: "a, b > button" parses as ["a", "b > button"], so the first
        # clause loses its scope entirely and matches every `a` on the tree.
        assert _selectors.chain("a, b", " > ", "button") == "a > button, b > button"

    def test_distribution_preserves_quoted_commas(self):
        assert _selectors.chain('x[name="a,b"], y', " > ", "z") == 'x[name="a,b"] > z, y > z'


class TestNth:
    def test_a_single_clause_is_narrowed_by_position(self):
        assert _selectors.nth("button", 2) == "button:nth(2)"

    def test_a_group_has_no_expressible_nth(self):
        # ":nth" binds within a clause, so "a:nth(2), b:nth(2)" selects two
        # elements rather than the second of the union. There is no string that
        # means what the caller wants, so the ref falls back to a stable id or
        # a live handle instead of carrying a path that lies.
        assert _selectors.nth("a, b", 2) is None

    def test_a_quoted_comma_still_gets_its_nth(self):
        selector = "button[name='All Clear, Please']"
        assert _selectors.nth(selector, 3) == f"{selector}:nth(3)"
