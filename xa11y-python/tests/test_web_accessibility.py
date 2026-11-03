"""App.enable_web_accessibility reaches the core and maps its errors.

The mock provider has no web content switch, so it answers with the core's
default refusal. What the switch does on a real Chromium app is covered by
the integration suite.
"""

import pytest
import xa11y


def test_mock_provider_refuses_with_action_not_supported(mock_app):
    with pytest.raises(xa11y.ActionNotSupportedError, match="enable_web_accessibility"):
        mock_app.enable_web_accessibility(timeout=0)


def test_timeout_is_keyword_only(mock_app):
    with pytest.raises(TypeError):
        mock_app.enable_web_accessibility(0)


def test_negative_timeout_is_a_value_error(mock_app):
    with pytest.raises(ValueError, match="timeout"):
        mock_app.enable_web_accessibility(timeout=-1)
