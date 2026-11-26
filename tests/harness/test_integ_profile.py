"""Exercise the real Windows harness's Cargo argv without launching a desktop."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

SHELLS = [shell for name in ("pwsh", "powershell.exe") if (shell := shutil.which(name))]
HARNESS = Path(__file__).resolve().parents[2] / "scripts/run_integ_tests_windows.ps1"


@pytest.mark.parametrize("shell", SHELLS or [None])
@pytest.mark.parametrize("profile", [None, "debug", "release", "invalid"])
def test_windows_integ_cargo_profile_preserves_argv(shell, profile, tmp_path):
    if shell is None:
        pytest.skip("PowerShell is unavailable on this host")
    log = tmp_path / "cargo.json"
    driver = tmp_path / "driver.ps1"
    driver.write_text(
        """
function cargo {
    [System.IO.File]::WriteAllText(
        $env:XA11Y_TEST_CARGO_LOG,
        (ConvertTo-Json -InputObject @($args) -Compress))
    # Stop at the build boundary, before any fixture can launch.
    $global:LASTEXITCODE = 91
}
& $env:XA11Y_TEST_HARNESS
exit $LASTEXITCODE
""",
        encoding="utf-8",
    )
    env = dict(os.environ)
    env.pop("XA11Y_TEST_PROFILE", None)
    if profile is not None:
        env["XA11Y_TEST_PROFILE"] = profile
    env["XA11Y_TEST_CARGO_LOG"] = str(log)
    env["XA11Y_TEST_HARNESS"] = str(HARNESS)
    result = subprocess.run(
        [shell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(driver)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    if profile == "invalid":
        assert result.returncode != 91
        assert not log.exists(), (
            "invalid profiles must fail before Cargo or desktop setup"
        )
        assert "must be debug or release" in result.stderr
    else:
        assert result.returncode == 91, (result.stdout, result.stderr)
        expected = ["build"]
        if profile == "release":
            expected.append("--release")
        expected += ["--workspace", "--exclude", "xtask"]
        assert json.loads(log.read_text(encoding="utf-8")) == expected
