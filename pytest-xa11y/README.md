# pytest-xa11y

Desktop UI testing for pytest, built on [xa11y](https://github.com/xa11y/xa11y).

xa11y drives real applications through the platform accessibility APIs —
AT-SPI2 on Linux, AXUIElement on macOS, UI Automation on Windows. This plugin
supplies the part around the test: getting the app running and attached,
deciding what this machine can actually exercise, and putting the tree in
front of you when something fails.

```python
# conftest.py
import pytest
from pytest_xa11y import AppLauncher

@pytest.fixture(scope="session")
def xa11y_launcher():
    return AppLauncher(
        command=["./target/debug/my-app"],
        ready='button[name="Sign in"]',
    )
```

```python
# test_login.py
def test_sign_in(xa11y_app):
    xa11y_app.locator('text_field[name="Email"]').set_value("a@b.c")
    xa11y_app.locator('button[name="Sign in"]').press()
    xa11y_app.locator('static_text[name^="Welcome"]').wait_visible()
```

```
pip install pytest-xa11y
```

`xa11y_app` is an ordinary `xa11y.App`. The plugin wraps none of the library's
own API — locators, elements, actions and errors are xa11y's, documented in
xa11y's own reference. There is no second API surface here to learn or to
drift out of step.

## What the plugin does

**Launch and attach.** `AppLauncher` describes how the app starts; the plugin
spawns it, polls until it registers with the platform accessibility API, waits
for the readiness selector, and terminates it at the end of the session. A
process that exits during startup is reported with its exit code and the tail
of its output, rather than as a selector that never matched.

**Liveness between tests.** If the app dies mid-run, the next test says so —
with the process's output — instead of the remaining eighty tests each failing
on an unrelated lookup.

**Reset, not relaunch.** Launching a desktop app costs seconds, so `xa11y_app`
is session-scoped. `AppLauncher(reset=...)` runs before each test to return
the app to a known state. Use `xa11y_fresh_app` for the cases a reset cannot
reach.

**Capability skips.** `@pytest.mark.xa11y_requires("screenshot")` skips when
this session has no capture path — a headless X server, a missing macOS
Screen Recording grant — instead of failing for a reason the developer cannot
act on.

**Failure diagnostics.** A failing test gets the accessibility tree, the
platform's focus state, the app's output, and any recorded events attached to
its report. xa11y's structured error diagnosis (what it waited for, what it
last observed, near-miss candidates) is rendered as its own labelled section.
All of it bounded, on the failure path only, capped per run.

## Fixtures

| Fixture | Scope | What it gives you |
| --- | --- | --- |
| `xa11y_launcher` | session | **You define this.** The launch recipe. |
| `xa11y_app` | session | The app under test. |
| `xa11y_fresh_app` | function | A newly launched app, torn down after the test. |
| `xa11y_app_factory` | session | Launch additional apps (dialogs that register separately, multi-process suites). |
| `xa11y_events` | function | An `EventRecorder` for a block of test code. |
| `xa11y_capabilities` | session | What this session can exercise, and why not. |
| `xa11y_artifacts` | session | The artifacts directory, or `None`. |

## AppLauncher

```python
AppLauncher(
    command=[BINARY, "--headless"],   # argv, as a list
    env={"QT_ACCESSIBILITY": "1"},    # merged over os.environ
    cwd=PROJECT_ROOT,
    app_names=["my-app"],             # match the a11y name too, not just the PID
    app_name_prefix="Submit to ",     # ...or narrow to one app within our PID
    spawns_and_exits=False,           # the command hands off and exits
    ready='button[name="OK"]',        # gate startup on content
    startup_timeout=60,               # overrides --xa11y-startup-timeout
    frontmost=True,                   # claim the macOS front slot
    reset=lambda app: app.locator('button[name="Home"]').press(),
    label="widgets",                  # used in diagnostics and artifact names
)
```

`attach_pid=N` attaches to an already-running process instead of launching
one; the plugin never terminates a process it did not start.

`app_names` matters when the process that registers with the accessibility
API is not the one you launched — Electron helper processes, launcher shims
that spawn a child and exit, anything that re-execs. It matches PID **or**
name, widening the search.

`spawns_and_exits` is separate on purpose. Needing a name to *find* the app
says nothing about which process to *watch*, and conflating the two costs the
liveness reporting above: with death detection off, a startup crash is
reported as "never registered" after the full timeout, and a mid-run exit is
not noticed at all. Set it only for a command that genuinely hands off and
goes — a Windows launcher shim — and leave it alone for an app that stays
running under its own PID, `app_names` or not.

`app_name_prefix` is the opposite pairing: PID **and** name prefix, narrowing
it. Reach for it when one process registers several accessibility apps and
only one of them is what you want. A Qt dialog hosted inside a DCC
application (Maya, Nuke, Cinema 4D) appears as its own accessibility app
sharing the host's PID on Windows UIA, and matching on PID alone attaches to
the host. The two fields are mutually exclusive — one widens what the other
narrows.

## Markers

```python
@pytest.mark.xa11y_requires("screenshot")   # or "input_sim"
@pytest.mark.xa11y_frontmost                # claim the macOS front slot first
```

Capability names are plain strings. `pytest_xa11y.Capability` is a `str` enum
of the same values, for completion and type checking; `Capability.SCREENSHOT`
and `"screenshot"` are interchangeable everywhere.

`xa11y_requires("screenshot")` probes once per session with a full-display
capture — the weakest thing the marker can be taken to mean, since a
full-display capture can succeed where a region capture is rejected. Only the
errors that mean "this session has no capture path" produce a skip. A capture
that fails for any other reason propagates and fails the test, because a
broken capture pipeline must not be able to turn a suite green.

`xa11y_requires("input_sim")` cannot probe on macOS or Windows, and the plugin
does not pretend otherwise: `CGEventPost` returns void, so without the
Accessibility and Input Monitoring grants the events are silently discarded
and every layer reports success. Declare it with `--xa11y-skip=input_sim` (or
`XA11Y_SKIP_INPUT_SIM=1`) on machines that lack the grant. On Linux the probe
is real — both backends validate eagerly.

Tests carrying either marker skip when pytest-xdist is running more than one
worker. Input synthesis and the frontmost slot are process-global, and
parallel workers take them from each other.

**The `xa11y_` marker prefix is reserved**, and every way of writing one of
these markers wrongly fails collection rather than warning: a typo in the
marker name, a capability that does not exist, `xa11y_requires()` with no
arguments, or arguments passed to `xa11y_frontmost`, which takes none. pytest only warns about an unknown marker, which means the test
still runs — unguarded, while reading as guarded. A test claiming a guard it
does not have is worse than one with no guard at all, so this is an error and
every offending marker in the run is reported at once.

## Events

```python
def test_focus_moves(xa11y_app, xa11y_events):
    with xa11y_events(xa11y_app) as events:
        xa11y_app.locator('button[name="OK"]').focus()
        events.expect("focus_changed", name="OK", timeout=2.0)
```

`expect()` blocks in xa11y's own wait (so it releases the GIL) and, on
failure, reports the events that did arrive instead. Whatever a recorder saw
is attached to the failure report.

It takes several event types when the platforms disagree about which one an
interaction emits, which for accessibility bridges is common — toggling a
checkbox is `state_changed` on one and `value_changed` on another:

```python
events.expect(("state_changed", "value_changed"), timeout=5.0)
```

There is no `expect()` that means "the next event, whatever it is". A filter
left off by accident would pass against anything that arrived; use
`Subscription.recv` when that is genuinely what you want.

## Options

| Option | Default | Effect |
| --- | --- | --- |
| `--xa11y-timeout=SECONDS` | library default (5s) | Calls `xa11y.set_default_timeout()`. Outranks `XA11Y_DEFAULT_TIMEOUT`; a per-call `timeout=` still wins over both. |
| `--xa11y-startup-timeout=SECONDS` | 30 | Waiting for the app to appear and become ready. |
| `--xa11y-artifacts=DIR` | off | Save a screenshot of each failing test. |
| `--xa11y-skip=CAPABILITY` | none | Declare a capability unavailable. Repeatable. |
| `--xa11y-dump-depth=N` | 12 | Depth of the tree dump on failure. |
| `--xa11y-max-diagnostics=N` | 10 | Cap diagnostics per run, so a wholesale failure does not bury its own report. |

## Suite-specific diagnostics

For state the plugin cannot know about:

```python
import pytest_xa11y

pytest_xa11y.register_diagnostic(
    "event log",
    lambda app: app.locator('text_area[name="Event log"]').element().value or "",
)
```

Collectors run on failures only. One that raises is reported in the block
rather than dropped, so a stale collector cannot quietly stop contributing.

## Versioning

pytest-xa11y versions independently of xa11y and depends on it with a lower
bound, not a pin. The launch path makes a single long `App.find` call, which
needs the GIL fix from xa11y/xa11y#359 — the declared floor understates that
until a release carrying it exists. See [RELEASING.md](RELEASING.md).

## License

MIT
