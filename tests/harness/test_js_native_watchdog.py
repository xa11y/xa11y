"""The test watchdog must terminate a process with a stuck native worker."""

import subprocess
from pathlib import Path

import pytest

WATCHDOG = Path(__file__).with_name("js_native_watchdog.cjs")


def test_stuck_native_test_fails_promptly_with_its_name():
    result = subprocess.run(
        [
            "node",
            "-e",
            """
        const { withNativeWatchdog } = require(process.argv[1]);
        // An unfinished native task also holds the event loop open.
        setInterval(() => {}, 1000);
        withNativeWatchdog('UWP minimized controls', () => new Promise(() => {}), 30);
        """,
            str(WATCHDOG),
        ],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    assert result.returncode == 1
    assert "UWP minimized controls" in result.stderr
    assert "30ms" in result.stderr


@pytest.mark.parametrize("fails", [False, True])
def test_completed_test_clears_watchdog_and_preserves_its_result(fails):
    script = """
    const assert = require('node:assert/strict');
    const { withNativeWatchdog } = require(process.argv[1]);
    (async () => {
      const fails = process.argv[2] === 'true';
      const failure = new Error('original failure');
      try {
        const value = await withNativeWatchdog('completed', async () => {
          if (fails) throw failure;
          return 42;
        }, 30);
        assert.equal(value, 42);
        assert.equal(fails, false);
      } catch (error) {
        assert.equal(error, failure);
      }
      await new Promise(resolve => setTimeout(resolve, 80));
    })().catch(error => { console.error(error); process.exitCode = 2; });
    """
    result = subprocess.run(
        ["node", "-e", script, str(WATCHDOG), str(fails).lower()],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "stalled" not in result.stderr
