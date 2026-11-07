"""Read one owned browser through the opt-in source on every consumer surface."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    pid = int(sys.argv[1])
    root = Path(__file__).resolve().parents[2]
    cli = root / "target/debug/xa11y.exe"
    os.environ["XA11Y_ACCESSIBILITY_SOURCE"] = "ia2"
    import xa11y

    selector = 'text_field[name="IA2 editor"]'
    app = xa11y.App.by_pid(pid, timeout=5)
    edit = app.locator(selector).element()
    assert edit.raw["accessibility_backend"] == "ia2"
    assert edit.text().read(xa11y.TextRange(6, 8)) == "\U0001f600"
    table = app.locator('table[name="IA2 fixture table"]').element()
    assert table.cell_at(1, 1).cell.column_span == 2
    result = subprocess.run(
        [str(cli), "text", "read", selector, "--pid", str(pid), "--range", "6,8"],
        capture_output=True, text=True, encoding="utf-8", timeout=15, check=True,
    )
    assert json.loads(result.stdout)["text"] == "\U0001f600"
    messages = [
        {"jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": "read", "method": "tools/call", "params": {"name": "text", "arguments": {"pid": pid, "selector": selector, "op": "read", "start": 6, "end": 8}}},
    ]
    result = subprocess.run(
        [str(cli), "mcp"], input="".join(json.dumps(m) + "\n" for m in messages),
        capture_output=True, text=True, encoding="utf-8", timeout=15, check=True,
    )
    response = next(json.loads(line) for line in result.stdout.splitlines() if json.loads(line).get("id") == "read")
    assert not response["result"].get("isError", False), response
    assert response["result"]["structuredContent"]["result"]["text"] == "\U0001f600"
    node = r"""
const assert = require('node:assert/strict');
const xa11y = require(process.argv[1]);
(async () => {
  const app = await xa11y.App.byPid(Number(process.argv[2]), { timeout: 5000 });
  const edit = await app.locator('text_field[name="IA2 editor"]').element();
  assert.equal(edit.raw.accessibility_backend, 'ia2');
  assert.equal(await (await edit.text()).read({ start: 6, end: 8 }), '\u{1f600}');
  const table = await app.locator('table[name="IA2 fixture table"]').element();
  assert.equal((await table.cellAt(1, 1)).cell.columnSpan, 2);
})().catch(e => { console.error(e); process.exitCode = 1; });
"""
    subprocess.run(["node", "-e", node, str(root / "xa11y-js"), str(pid)], capture_output=True, text=True, encoding="utf-8", timeout=15, check=True)
    bad = dict(os.environ, XA11Y_ACCESSIBILITY_SOURCE="unknown")
    result = subprocess.run([str(cli), "apps"], env=bad, capture_output=True, text=True, encoding="utf-8", timeout=15)
    assert result.returncode != 0 and "XA11Y_ACCESSIBILITY_SOURCE" in result.stderr
    print(json.dumps({"python": True, "javascript": True, "cli": True, "mcp": True}))


if __name__ == "__main__":
    main()
