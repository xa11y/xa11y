"""Release-only Windows Qt traversal experiment."""
import json
import os
import statistics
import subprocess
import sys
import time
from collections import defaultdict

import xa11y


def snapshot(node):
    fields = "role name description value numeric_value min_value max_value stable_id pid actions raw enabled visible hidden_reason focused active checked selected expanded editable focusable modal required busy password minimized maximized fullscreen"
    data = {f: getattr(node, f) for f in fields.split()}
    rect = node.bounds
    data["bounds"] = [rect.x, rect.y, rect.width, rect.height] if rect else None
    return data


def run(app, fixture, selectors, rounds=8):
    modes = ["ordinary", "packet", "pool2", "pool4", "project", "pool8", "pool1"]
    results = defaultdict(list)
    errors = {}
    print("FRAMEWORK", fixture, [(w.name, w.raw.get("uia_framework_id")) for w in app.children()], flush=True)
    os.environ["XA11Y_QT_TRIAL"] = "ordinary"
    all_nodes = app.locator("*").elements()
    for role in ["*"] + sorted({el.role for el in all_nodes}):
        expected_count = app.locator(role).count()
        os.environ["XA11Y_QT_TRIAL"] = "packet"
        count = app.locator(role).count()
        os.environ["XA11Y_QT_TRIAL"] = "ordinary"
        if count != expected_count:
            print("COUNT_MISMATCH", fixture, role, expected_count, count, flush=True)
    for sel in selectors:
        loc = app.locator(sel)
        os.environ["XA11Y_QT_TRIAL"] = "ordinary"
        original = loc.elements()
        expected = [snapshot(el) for el in original]
        for i in range(rounds + 1):
            order = modes[i % len(modes):] + modes[:i % len(modes)]
            for mode in order:
                os.environ["XA11Y_QT_TRIAL"] = mode
                try:
                    t = time.perf_counter_ns()
                    actual = loc.elements()
                    ms = (time.perf_counter_ns() - t) / 1e6
                    payload = [snapshot(el) for el in actual]
                    if expected != payload:
                        if len(expected) != len(payload):
                            raise AssertionError(f"{len(expected)} vs {len(payload)}")
                        for a,b in zip(expected,payload):
                            if a != b:
                                raise AssertionError(str({k:(a[k],b[k]) for k in a if a[k]!=b[k]})[:900])
                    if i in (0,rounds) and not all(a.same_as(b) for a,b in zip(original,actual)):
                        raise AssertionError("element identity/order mismatch")
                    if i:
                        results[sel,mode].append(ms)
                except Exception as e:
                    errors.setdefault((sel,mode),str(e))
        for mode in modes:
            samples = results[sel,mode]
            print("READ_TRIAL", json.dumps(dict(fixture=fixture,selector=sel,mode=mode,hits=len(expected),
                    samples=samples,median=statistics.median(samples) if samples else None,
                    error=errors.get((sel,mode)))), flush=True)
    os.environ["XA11Y_QT_TRIAL"] = "ordinary"


def wide():
    from PySide6.QtWidgets import QApplication, QWidget,QVBoxLayout,QPushButton,QGroupBox
    qt=QApplication([])
    main=QWidget()
    main.setWindowTitle("Qt wide query trial")
    layout=QVBoxLayout(main)
    for row in range(12):
        box=QGroupBox(str(row))
        line=QVBoxLayout(box)
        for col in range(10):
            line.addWidget(QPushButton("Candidate "+str(row*10+col)))
        layout.addWidget(box)
    main.show()
    print(os.getpid(), flush=True)
    qt.exec()


if __name__ == "__main__":
    if sys.argv[1] == "wide":
        wide()
    else:
        app = xa11y.App.by_pid(int(sys.argv[1]))
        run(app,"stock",["button",'button[name="OK"]','button[name="missing benchmark button"]','text_field, button','button[enabled="true"]'])
        app.locator('button[name="Open Dialog"]').press()
        closer = app.locator('button[name="Close Dialog"]')
        closer.wait_visible(timeout=5)
        run(app,"dialog",["button"],2)
        closer.press()
        closer.wait_hidden(timeout=5)
        proc = subprocess.Popen([sys.executable,"-u",__file__,"wide"], stdout=subprocess.PIPE, text=True, env={**os.environ, "QT_ACCESSIBILITY": "1"})
        try:
            wide_app=xa11y.App.by_pid(int(proc.stdout.readline()), timeout=15)
            wide_app.locator('button[name="Candidate 0"]').wait_attached(timeout=10)
            run(wide_app,"wide",["button",'button[name="Candidate 119"]'],5)
        finally:
            subprocess.run(["taskkill", "/pid", str(proc.pid), "/t", "/f"], capture_output=True)
            proc.wait(timeout=10)
