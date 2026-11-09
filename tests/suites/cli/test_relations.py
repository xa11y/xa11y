"""CLI integration tests for ``xa11y relations``.

Each kind prints a header line, ``<kind> (<count>):``, followed by its
targets indented, one per line as ``find`` prints them.
"""

from __future__ import annotations

import re

import xa11y

HEADER = re.compile(r"^([a-z_]+) \((\d+)\):$")


def _supported_kinds() -> list[str]:
    caps = xa11y.capabilities()
    return [
        name.removeprefix("relation:")
        for name in caps.names()
        if name.startswith("relation:") and caps.support(name) != "unsupported"
    ]


def test_relations_lists_every_supported_kind_once(run_cli, app_pid):
    rc, stdout, stderr = run_cli("relations", 'button[name="OK"]', "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    headers = []
    for line in stdout.splitlines():
        match = HEADER.match(line)
        if match:
            headers.append(match.group(1))
        else:
            assert line.startswith("  "), f"a target line is indented: {line!r}"
    assert headers == _supported_kinds(), stdout


def test_relations_reads_only_the_kinds_named(run_cli, app_pid):
    kind = _supported_kinds()[0]
    rc, stdout, stderr = run_cli(
        "relations", 'button[name="OK"]', kind, "--pid", str(app_pid)
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert HEADER.match(stdout.splitlines()[0]).group(1) == kind, stdout


def test_an_unknown_relation_kind_is_a_usage_error(run_cli, app_pid):
    rc, _stdout, stderr = run_cli(
        "relations", 'button[name="OK"]', "labeled_by", "--pid", str(app_pid)
    )
    assert rc == 2, f"expected exit 2, got {rc}\nstderr: {stderr}"
    assert "labelled_by" in stderr, "the error names the valid kinds"
