# strands-xa11y

Drive native desktop applications from a [Strands](https://strandsagents.com) agent — through the operating system's accessibility tree, not screenshots.

Accessibility APIs are the most robust way to control a desktop — they expose the same structured tree of roles, names, and states that assistive technology uses — and this library drives them with Playwright-style selectors, so targets survive a UI that moves, restyles, or re-renders between turns. Built on [xa11y](https://xa11y.dev), which speaks AXUIElement on macOS, UI Automation on Windows, and AT-SPI2 on Linux behind one API.

```python
from strands import Agent
from strands_xa11y import use_desktop

agent = Agent(tools=[use_desktop])
agent("Open TextEdit, type 'hello' into the document, and save it as notes.txt")
```

## Why not screenshots

A screenshot-and-OCR loop has to infer what is on screen and then guess where to click. The accessibility tree already knows.

| | screenshot + OCR | strands-xa11y |
|---|---|---|
| Targeting | coordinates inferred from pixels | selectors and refs resolved against the real UI tree |
| State | invisible — "is this button disabled?" is a guess | `enabled`, `checked`, `focused`, `expanded`, `selected` read directly |
| Cost per turn | a full image | a few hundred lines of text |
| Failure mode | silently clicks the wrong thing | names what it was waiting for, what it saw, and near-miss elements |
| Setup | Tesseract, OpenCV, numpy | one wheel |

Pixels are still there when you need them — canvases, video, rendering bugs — as an explicit fallback rather than the default.

## Install

```bash
pip install strands-xa11y
```

Prebuilt wheels cover macOS, Windows, and Linux; Python 3.10+.

### Permissions

This is where first runs fail, so check here first.

- **macOS** — grant Accessibility to whatever hosts the agent (System Settings › Privacy & Security). The pane is named **Accessibility** through macOS 26 and **Device Control and Data Access** on macOS 27 and newer. Screenshots additionally need Screen & System Audio Recording. Restart the process after granting.
- **Linux** — AT-SPI2 must be running (standard on GNOME). Chromium and Electron apps only publish a tree when launched with `--force-renderer-accessibility`. On Wayland, synthesised input needs `/dev/uinput`, which means membership of the `input` group.
- **Windows** — nothing to grant. If a target app runs elevated, the agent has to as well.

## The loop

**1. Snapshot.** Each line is one node, with a ref.

```python
use_desktop({"action": {"type": "snapshot", "app": "TextEdit"}})
```

```
TextEdit (pid 4242)
e1 application "TextEdit"
  e2 window "Untitled" [active]
    e3 toolbar
      e4 button "Bold"
      e5 button "Italic" [disabled]
    e7 group
      e8 text_field "File name" value="untitled"
      e9 check_box "Wrap lines" [unchecked]
    e10 text_area "document" value="Dear Alice,"
```

**2. Act on refs.**

```python
use_desktop({"action": {"type": "click", "target": {"ref": "e4"}}})
use_desktop({"action": {"type": "type", "target": {"ref": "e8"}, "text": "notes.txt", "replace": True}})
use_desktop({"action": {"type": "act", "target": {"ref": "e9"}, "verb": "check"}})
```

**3. Re-snapshot after the UI changes.** Refs are never reused, so a stale one fails loudly instead of hitting whatever moved into its place.

Refs re-resolve through the platform's `stable_id` where one exists, otherwise through a structural selector path, and only fall back to a captured handle as a last resort — so the first two paths auto-wait and survive a re-render.

You can skip refs and address elements directly with [xa11y selectors](https://xa11y.dev/reference/selectors/):

```python
use_desktop({"action": {"type": "click", "target": {"app": "TextEdit", "selector": "button[name='Save']"}}})
```

## Actions

**Perceive** — `list_apps`, `snapshot`, `find`, `read`, `wait`

**Act, through the accessibility layer** — `click`, `type`, `focus`, and `act` for the rest: `toggle`, `check`, `uncheck`, `select`, `expand`, `collapse`, `increment`, `decrement`, `set_number`, `select_text`, `show_menu`, `scroll_into_view`, `blur`, and `raw` as an escape hatch to any platform action name.

**Fall back to synthesised input and pixels** — `key`, `mouse`, `drag`, `scroll`, `screenshot`

**Lifecycle** — `open_app` (waits for the app to register with the accessibility bridge rather than sleeping), `close_app` (needs `pip install 'strands-xa11y[process]'`)

Every action is a separate variant in a discriminated union, so the model is shown exactly the fields that action takes.

## Read-only agents

`inspect_desktop` is the same tool with the acting half removed — `list_apps`, `snapshot`, `find`, `read`, `wait`, `screenshot`. Give an agent that one alone when it should observe but never touch.

```python
from strands_xa11y import inspect_desktop

agent = Agent(tools=[inspect_desktop])
```

The restriction is in the schema, not a runtime check, so there is no acting action for the model to reach for.

## Consent and privacy

Anything that changes the machine — clicking, typing, launching, terminating — asks for confirmation on the terminal first. Set `BYPASS_TOOL_CONSENT=true` to hand approval to your agent runtime instead, which is what you want when the runtime has its own approval UX or when running unattended. With no terminal to ask on and no bypass set, the action is refused rather than assumed.

Reading the tree never prompts. `screenshot` prompts when the capture leaves the process — `send_image=true` puts whatever is on the user's screen into the transcript, and `save_path` writes it to disk. Screenshots are withheld from the model by default, and images over 5MB are dropped rather than sent.

`close_app` terminates every process whose name contains the string you give it, so it takes the most specific name you have and never signals the process hosting the agent.

## Errors are meant to be read

xa11y reports what a failed call was waiting for, what it last observed, and which elements nearly matched. That gets passed through verbatim:

```
TimeoutError: timed out
  condition: visible
  selector: button[name='Sav']
  last observed: selector never matched
  near misses: button 'Save'; button 'Save As…'
```

Usually enough for the model to fix its own selector without falling back to pixels.

## Known limits

- With xa11y 0.15.4 and earlier, `scroll_into_view` does nothing on macOS, so use `scroll` there. Later releases perform `AXScrollToVisible` when the element advertises it, and raise `ActionNotSupported` otherwise.
- `blur` only works on macOS.
- The `*` selector materialises every element's attributes; it is cheap on Windows, expensive on macOS and Linux.
- `detail="rich"` snapshots read each property individually, which is one D-Bus round trip apiece on Linux. Use `detail="basic"` on large trees, or scope with `selector`.
- Snapshots are bounded by `max_depth` and `max_nodes` and are filtered to interactive nodes by default. Truncation is always reported.

## Development

This package lives in the [xa11y repository](https://github.com/xa11y/xa11y), alongside the library it drives. It has its own version line, its own `strands-xa11y-v*` tag series, and its own publish workflow; see [RELEASING.md](RELEASING.md).

```bash
cd strands-xa11y
pip install hatch
hatch run prepare   # format, lint, typecheck, test
hatch run cov       # the same tests, with a coverage report
```

Tests run against a fake accessibility backend, including a small selector evaluator, so they need no display, no bus, and no windows. The suite covers every statement in `src/`, and CI fails below that, so a new branch arrives with the test that exercises it.

That fake is also the gap `tests/check_real_surface.py` closes. It asserts that every xa11y exception name this package writes guidance for, every diagnosis attribute it renders, and every method the fake claims to stand in for still exists on the real module:

```bash
python tests/check_real_surface.py     # needs the xa11y bindings importable
```

It is deliberately not a `test_*.py`: pytest would apply the conftest that installs the fake, and the check would verify the fake against itself.

CI runs the suite on Python 3.10–3.13 (`strands`), checks formatting, lint, types and packaging (`strands-package`), runs the surface check against the freshly built bindings (`python`), and imports the package on macOS and Windows to prove both tool schemas build everywhere it claims to run. A weekly canary re-runs everything against the latest `strands-agents` and `xa11y`, so SDK drift breaks there before it breaks anyone's install.

## Documentation

[Drive a desktop from a Strands agent](https://xa11y.dev/guides/strands/) is the guide version of this page. The [selector syntax](https://xa11y.dev/reference/selectors/), the [platform permissions](https://xa11y.dev/guides/install/), and the [error diagnosis](https://xa11y.dev/explanation/errors-and-diagnosis/) this tool passes through are all xa11y's, and documented there.

## License

MIT. Built on [xa11y](https://github.com/xa11y/xa11y), also MIT.
