"""Unit tests for the coverage-index checker.

These are plain unit tests — they launch nothing and need no bindings. They
exist for the same reason `tests/harness/test_launch.py` does: `matrix_check.py`
is the thing that decides whether the coverage index is believed, so its blind
spots are invisible by construction.

Issue #348 is the worked example. `matrix.yaml` said the Tauri app ran on
`[linux, macos, windows]`; the `integ` matrix in `ci.yml` had no
`windows-latest × tauri` cell and never had. Tauri is the only app carrying the
`input_sim` suite, so the Windows `SendInput` backend was driven by no test of
any kind — while four documentation sources, `platforms:` among them, said
otherwise. The checker didn't notice because it validated claims against *test
files existing*, never against the platforms those tests run on.

Run with:  cargo xtask test-harness
           (or: python -m pytest tests/test_matrix_check.py)
"""

from __future__ import annotations

import textwrap

import pytest

from tests import matrix_check


# ── Helpers ──────────────────────────────────────────────────────────────────


def _workflow(tmp_path, include: str):
    """Write a minimal ci.yml carrying the given `integ` matrix include block."""
    path = tmp_path / "ci.yml"
    path.write_text(
        "name: CI\n"
        "on: [push]\n"
        "jobs:\n"
        "  integ:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n" + textwrap.indent(textwrap.dedent(include), " " * 10)
    )
    return path


@pytest.fixture
def workflow(tmp_path, monkeypatch):
    """Point matrix_check at a synthetic workflow written by the test."""

    def _install(include: str):
        monkeypatch.setattr(matrix_check, "WORKFLOW_PATH", _workflow(tmp_path, include))

    return _install


def _matrix(apps: dict) -> dict:
    return {"apps": apps}


# ── Reading the workflow ─────────────────────────────────────────────────────


def test_integ_cells_maps_runner_labels_to_platform_names(workflow):
    workflow(
        """\
        - { os: ubuntu-latest,  app: tauri }
        - { os: macos-latest,   app: tauri }
        - { os: windows-latest, app: qt }
        """
    )
    assert matrix_check.integ_cells() == {
        "tauri": {"linux", "macos"},
        "qt": {"windows"},
    }


def test_integ_cells_rejects_an_unmapped_runner_label(workflow):
    """A runner rename must fail loudly, not drop the cell from the check."""
    workflow("- { os: ubuntu-24.04, app: tauri }\n")
    with pytest.raises(RuntimeError, match="ubuntu-24.04"):
        matrix_check.integ_cells()


def test_integ_cells_rejects_a_workflow_it_cannot_read(tmp_path, monkeypatch):
    """No `integ` job means the platform check would silently pass everything."""
    path = tmp_path / "ci.yml"
    path.write_text("name: CI\njobs:\n  lint:\n    runs-on: ubuntu-latest\n")
    monkeypatch.setattr(matrix_check, "WORKFLOW_PATH", path)
    with pytest.raises(RuntimeError, match="jobs.integ.strategy.matrix.include"):
        matrix_check.integ_cells()


def test_integ_cells_rejects_a_cell_missing_os_or_app(workflow):
    workflow("- { os: ubuntu-latest }\n")
    with pytest.raises(RuntimeError, match="missing os/app"):
        matrix_check.integ_cells()


# ── The #348 regression ──────────────────────────────────────────────────────


def test_claimed_platform_without_a_cell_is_reported(workflow):
    """The exact shape of #348: `platforms:` names a cell that never existed."""
    workflow(
        """\
        - { os: ubuntu-latest, app: tauri }
        - { os: macos-latest,  app: tauri }
        """
    )
    problems = matrix_check.verify_platforms(
        _matrix({"tauri": {"platforms": ["linux", "macos", "windows"]}})
    )
    assert len(problems) == 1
    assert "apps.tauri.platforms claims 'windows'" in problems[0]


def test_a_matching_declaration_is_clean(workflow):
    workflow(
        """\
        - { os: ubuntu-latest,  app: tauri }
        - { os: macos-latest,   app: tauri }
        - { os: windows-latest, app: tauri }
        """
    )
    assert (
        matrix_check.verify_platforms(
            _matrix({"tauri": {"platforms": ["linux", "macos", "windows"]}})
        )
        == []
    )


def test_cell_for_an_undeclared_platform_is_reported(workflow):
    """Drift in the other direction: CI runs it, the index doesn't say so."""
    workflow(
        """\
        - { os: ubuntu-latest,  app: gtk }
        - { os: windows-latest, app: gtk }
        """
    )
    problems = matrix_check.verify_platforms(_matrix({"gtk": {"platforms": ["linux"]}}))
    assert len(problems) == 1
    assert "runs gtk on windows" in problems[0]


def test_app_with_cells_but_no_matrix_entry_is_reported(workflow):
    workflow("- { os: ubuntu-latest, app: newapp }\n")
    problems = matrix_check.verify_platforms(_matrix({}))
    assert len(problems) == 1
    assert "'newapp'" in problems[0]


# ── The opt-out, and its staleness ───────────────────────────────────────────


def test_covered_outside_integ_excuses_a_missing_cell(workflow):
    workflow("- { os: ubuntu-latest, app: accesskit }\n")
    assert (
        matrix_check.verify_platforms(
            _matrix(
                {
                    "accesskit": {
                        "platforms": ["linux", "windows"],
                        "covered_outside_integ": {"windows": "Rust integ job"},
                    }
                }
            )
        )
        == []
    )


def test_covered_outside_integ_is_stale_when_the_platform_is_dropped(workflow):
    workflow("- { os: ubuntu-latest, app: accesskit }\n")
    problems = matrix_check.verify_platforms(
        _matrix(
            {
                "accesskit": {
                    "platforms": ["linux"],
                    "covered_outside_integ": {"windows": "Rust integ job"},
                }
            }
        )
    )
    assert len(problems) == 1
    assert "no longer claims" in problems[0]


def test_covered_outside_integ_is_stale_once_a_cell_exists(workflow):
    """An excuse that outlived its reason reads as a live design decision."""
    workflow(
        """\
        - { os: ubuntu-latest,  app: accesskit }
        - { os: windows-latest, app: accesskit }
        """
    )
    problems = matrix_check.verify_platforms(
        _matrix(
            {
                "accesskit": {
                    "platforms": ["linux", "windows"],
                    "covered_outside_integ": {"windows": "Rust integ job"},
                }
            }
        )
    )
    assert len(problems) == 1
    assert "now has an integ cell" in problems[0]


def test_covered_outside_integ_requires_a_reason(workflow):
    workflow("- { os: ubuntu-latest, app: accesskit }\n")
    problems = matrix_check.verify_platforms(
        _matrix(
            {
                "accesskit": {
                    "platforms": ["linux", "windows"],
                    "covered_outside_integ": {"windows": "  "},
                }
            }
        )
    )
    assert len(problems) == 1
    assert "has no reason" in problems[0]


# ── Against the real repository ──────────────────────────────────────────────


def test_repository_platforms_match_the_ci_matrix():
    assert matrix_check.verify_platforms(matrix_check.load_matrix()) == []


def test_tauri_runs_on_every_platform_it_carries_input_sim_for():
    """#348: no `windows-latest × tauri` cell meant no Windows input coverage."""
    assert matrix_check.integ_cells()["tauri"] == {"linux", "macos", "windows"}
