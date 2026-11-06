"""TEMPORARY CI diagnostic for PR #619 findings; deleted before merge."""

import sys
import time

import pytest

from test_web_accessibility import electron_only, electron_without_web_accessibility  # noqa: F401


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS only")
def test_diag_first_access(electron_only, electron_without_web_accessibility):  # noqa: F811
    app = electron_without_web_accessibility
    log = []
    t0 = time.monotonic()
    for _ in range(40):
        try:
            n_web = len(app.locator("web_area").elements())
            n_kids = len(app.locator("web_area *").elements())
            n_btn = len(app.locator('button[name="OK"]').elements())
            log.append(f"t={time.monotonic()-t0:.2f}s web_area={n_web} descendants={n_kids} ok_buttons={n_btn}")
        except Exception as e:  # noqa: BLE001
            log.append(f"t={time.monotonic()-t0:.2f}s error {type(e).__name__}: {str(e)[:150]}")
        time.sleep(0.1)
    cells = app.locator('table[name="Users Table"] table_cell').elements()
    dump = [f"cells={len(cells)}"]
    for c in cells[:2]:
        dump.append(f"cell raw={c.raw} name={c.name!r} value={c.value!r}")
    for st in app.locator('table[name="Users Table"] static_text').elements()[:3]:
        dump.append(f"static_text raw={st.raw} name={st.name!r} value={st.value!r}")
    pytest.fail("DIAG\n" + "\n".join(log) + "\n" + "\n".join(dump))
