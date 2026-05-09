"""AT-SPI selector tests for state attributes handled by the Linux fast path."""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


pytestmark = pytest.mark.skipif(
    sys.platform != "linux",
    reason="AT-SPI state selector tests run only on Linux",
)


def test_atspi_selector_enabled_true_matches_enabled_button(app, app_config):
    """[enabled="true"] is matched from AT-SPI state flags."""
    ok_name = app_config["ok_button_name"]

    matches = app.locator(f'button[name="{ok_name}"][enabled="true"]').elements()

    assert matches, "expected OK button to match enabled='true'"
    assert all(element.enabled is True for element in matches)


def test_atspi_selector_enabled_false_matches_disabled_button(app, app_config):
    """[enabled="false"] is matched from AT-SPI state flags."""
    cancel_name = app_config["cancel_button_name"]

    matches = app.locator(f'button[name="{cancel_name}"][enabled="false"]').elements()

    assert matches, "expected Cancel button to match enabled='false'"
    assert all(element.enabled is False for element in matches)


def test_atspi_selector_checked_on_matches_checked_widget(app, app_config):
    """[checked="on"] is matched from AT-SPI state flags."""
    checked_name = app_config.get("checkbox_checked_name")
    if checked_name:
        selector = f'check_box[name="{checked_name}"][checked="on"]'
    elif app_config.get("has_radio") and app_config.get("radio_a_name"):
        role = app_config["radio_role"]
        radio_name = app_config["radio_a_name"]
        selector = f'{role}[name="{radio_name}"][checked="on"]'
    else:
        pytest.skip("app has no initially checked widget")

    matches = app.locator(selector).elements()

    assert matches, f"expected {selector} to match"
    assert all(element.checked == "on" for element in matches)


def test_atspi_selector_focused_true_matches_after_focus(app, app_config):
    """[focused="true"] is matched from AT-SPI state flags."""
    ok_name = app_config["ok_button_name"]
    ok = app.locator(f'button[name="{ok_name}"]')

    try:
        ok.focus()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("focus() is not exposed by this AT-SPI bridge for buttons")
    time.sleep(0.3)

    matches = app.locator(f'button[name="{ok_name}"][focused="true"]').elements()

    assert matches, "expected focused OK button to match focused='true'"
    assert all(element.focused is True for element in matches)
