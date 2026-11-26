"""Compare baseline and production wheels on the same live Qt windows."""
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

import xa11y
from tests.harness import launch


def snapshot(el):
    data = {k:getattr(el,k) for k in "role name stable_id value description numeric_value min_value max_value actions raw enabled visible hidden_reason focused active checked selected expanded editable focusable modal required busy password minimized maximized fullscreen".split()}
    b=el.bounds
    data["bounds"] = (b.x,b.y,b.width,b.height) if b else None
    return data


def client(pid, fixture, label):
    selectors = ["button", 'button[enabled="true"]', 'button, text_field',
                 'button[name="OK"]' if fixture=="stock" else 'button[name="Candidate 119"]',
                 'button[name="missing benchmark button"]']
    for selector in selectors:
        app=xa11y.App.by_pid(pid)
        loc=app.locator(selector)
        times=[]
        first=None
        for _ in range(7):
            start=time.perf_counter_ns()
            hits=loc.elements()
            times.append((time.perf_counter_ns()-start)/1e6)
            if first is None:
                first=hits
                reference=[snapshot(e) for e in hits]
            else:
                assert reference==[snapshot(e) for e in hits]
                assert len(hits)==len(first) and all(x.same_as(y) for x,y in zip(first,hits))
        digest=hashlib.sha256(json.dumps(reference,sort_keys=True).encode()).hexdigest()
        print(json.dumps(dict(fixture=fixture,selector=selector,variant=label,hits=len(first),hash=digest,cold=times[0],samples=times[1:])),flush=True)


def compare(args):
    from collections import defaultdict
    scripts=Path(__file__).resolve()
    out=args.output.resolve()
    out.mkdir(parents=True,exist_ok=True)
    rows=[]
    def fixture(pid,name):
        for label in ["base","head","head","base"]:
            python=args.baseline_python if label=="base" else args.head_python
            text=subprocess.check_output([python,"-m","tests.harness.qt_production_probe","client",str(pid),name,label],text=True)
            rows.extend(json.loads(l) for l in text.splitlines())
            (out/"qt-production.json").write_text(json.dumps(rows,indent=2))
    proc,_,pid=launch._launch_app("qt")
    try:
        fixture(pid,"stock")
    finally:
        launch._kill_app(proc)
    proc=subprocess.Popen([args.head_python,"-m","tests.harness.qt_production_probe","wide"],stdout=subprocess.PIPE,text=True,env={**os.environ,"QT_ACCESSIBILITY":"1"})
    try:
        pid=int(proc.stdout.readline())
        xa11y.App.by_pid(pid,timeout=15).locator('button[name="Candidate 0"]').wait_attached(timeout=10)
        fixture(pid,"wide")
    finally:
        subprocess.run(["taskkill","/pid",str(proc.pid),"/t","/f"],capture_output=True)
        proc.wait(timeout=10)
    by=defaultdict(list)
    for r in rows: by[r["fixture"],r["selector"]].append(r)
    for k,g in by.items():
        assert len({r["hash"] for r in g})==1, k
    print("All advertised result data matched across both wheels.")
    return 0


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
    if sys.argv[1]=="wide":
        wide()
    else:
        client(int(sys.argv[2]),sys.argv[3],sys.argv[4])
