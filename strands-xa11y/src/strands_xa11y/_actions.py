"""Executing one action.

The layering the whole package exists for: everything that can go through a semantic
accessibility action does, and synthesised input is reserved for the gestures that
have no accessibility equivalent — global shortcuts, drags, scroll wheels.
"""

from __future__ import annotations

import os
import platform
import subprocess
from typing import Any, Callable, Dict, List

from . import _selectors, models
from ._errors import ToolError, describe, error_result, success_result, xa11y
from ._refs import REFS
from ._session import (
    Resolved,
    app_key,
    focus_app,
    normalize_keys,
    pointer_argument,
    require_consent,
    resolve_app,
    resolve_element,
    resolve_pointer,
)
from ._snapshot import (
    _Budget,
    collect_basic,
    collect_rich,
    describe_element,
    properties,
    prune,
    render,
)

# Anything larger would blow past model context once base64-encoded.
MAX_IMAGE_BYTES = 5 * 1024 * 1024


# ── Tier 1: perceive ─────────────────────────────────────────────────────────


def _list_apps(action: models.ListAppsAction) -> Dict[str, Any]:
    module = xa11y()
    apps = list(module.App.list())
    if not apps:
        return success_result(
            "No applications are reachable through the accessibility bridge. On Linux, check that "
            "AT-SPI2 is running; on macOS, check the Accessibility permission grant."
        )
    lines = []
    for app in apps:
        marker = " [foreground]" if getattr(app, "is_foreground", False) else ""
        pid = getattr(app, "pid", None)
        lines.append(f"- {app.name}" + (f" (pid {pid})" if pid else "") + marker)
    return success_result(f"{len(apps)} application(s):\n" + "\n".join(lines))


def _snapshot(action: models.SnapshotAction) -> Dict[str, Any]:
    if action.selector is not None or action.ref is not None:
        resolved = resolve_element(models.ElementTarget(app=action.app, selector=action.selector, ref=action.ref))
        root_element = resolved.as_element()
        app = resolved.app
        # Paths are only reusable when they are anchored at the application root; a
        # scope we cannot express as a path yields refs backed by stable_id instead.
        base_path = action.selector if action.selector is not None else REFS.get(str(action.ref)).path
        header = f"{app.name} — scope: {resolved.label}"
    else:
        app = resolve_app(action.app)
        root_element = app.as_element()
        base_path = ""
        header = f"{app.name}" + (f" (pid {app.pid})" if app.pid else "")

    budget = _Budget(action.max_nodes)
    if action.detail == "rich":
        root = collect_rich(root_element, action.max_depth, budget, base_path, action.include_bounds)
    else:
        root = collect_basic(root_element.tree(action.max_depth), action.max_depth, budget, base_path)

    if root is None:
        return success_result(f"{header}\n(no accessible content)")

    prune(root)
    lines: List[str] = []
    render(root, app_key(app), lines, 0, action.interactive_only)

    body = "\n".join(lines) if lines else "(every node was filtered out; retry with interactive_only=false)"
    notes = []
    if budget.truncated:
        notes.append(
            f"TRUNCATED at the {action.max_nodes}-node budget — this tree is incomplete. "
            f"Raise max_nodes, lower max_depth, or scope the snapshot with a selector."
        )
    if action.interactive_only:
        notes.append("Filtered to interactive and text nodes; pass interactive_only=false for everything.")
    return success_result("\n".join([header, body] + notes))


def _find(action: models.FindAction) -> Dict[str, Any]:
    app = resolve_app(action.app)
    matches = list(app.locator(action.selector).elements())
    if not matches:
        return success_result(f"No element matches {action.selector!r} in {app.name}.")

    shown = matches[: action.limit]
    key = app_key(app)
    lines = []
    for position, element in enumerate(shown, start=1):
        path = _selectors.nth(action.selector, position) if len(matches) > 1 else action.selector
        ref = REFS.issue(
            key,
            element.role,
            name=element.name,
            value=element.value,
            stable_id=element.stable_id,
            path=path,
            element=element,
        )
        lines.append(f"{ref.ref} {describe_element(element)}")

    header = f"{len(matches)} match(es) for {action.selector!r} in {app.name}"
    if len(shown) < len(matches):
        header += f" (showing the first {len(shown)}; raise 'limit' for more)"
    return success_result(header + ":\n" + "\n".join(lines))


def _read(action: models.ReadAction) -> Dict[str, Any]:
    resolved = resolve_element(action.target)
    values = properties(resolved.as_element())
    lines = [f"  {name}: {value!r}" for name, value in values.items() if value not in (None, "", [], False)]
    return success_result(f"{resolved.label}:\n" + "\n".join(lines))


_WAIT_METHODS = {
    "visible": "wait_visible",
    "hidden": "wait_hidden",
    "attached": "wait_attached",
    "detached": "wait_detached",
    "enabled": "wait_enabled",
    "disabled": "wait_disabled",
    "focused": "wait_focused",
    "unfocused": "wait_unfocused",
}


def _wait(action: models.WaitAction) -> Dict[str, Any]:
    resolved = resolve_element(action.target)
    if resolved.locator is None:
        raise ToolError(
            f"{resolved.label} resolved to a captured element handle, which cannot be polled. "
            f"Wait on a selector instead."
        )
    getattr(resolved.locator, _WAIT_METHODS[action.condition])(timeout=action.timeout)
    return success_result(f"{resolved.label} is {action.condition}.")


# ── Tier 2: act ──────────────────────────────────────────────────────────────


def _click(action: models.ClickAction) -> Dict[str, Any]:
    resolved = resolve_pointer(action.target)
    semantic = action.target.point is None and action.button == "left" and action.count == 1 and not action.modifiers
    if semantic:
        resolved.actor.press()
        return success_result(f"Pressed {resolved.label}.")

    module = xa11y()
    if action.button == "right" and action.target.point is None and action.count == 1:
        try:
            resolved.actor.show_menu()
            return success_result(f"Opened the context menu for {resolved.label}.")
        except module.ActionNotSupportedError:
            pass  # No accessibility action for it; synthesise the click below.

    module.input_sim().click(
        pointer_argument(resolved, action.target),
        button=action.button,
        count=action.count,
        held=normalize_keys(action.modifiers) or None,
    )
    return success_result(
        f"Synthesised a {action.button} click x{action.count} on {resolved.label}"
        + (f" holding {'+'.join(normalize_keys(action.modifiers))}" if action.modifiers else "")
        + "."
    )


def _type(action: models.TypeAction) -> Dict[str, Any]:
    module = xa11y()
    if action.target is None:
        module.input_sim().type_text(action.text)
        target_label = "the focused element"
    else:
        resolved = resolve_element(action.target)
        target_label = resolved.label
        if action.replace:
            resolved.actor.set_value(action.text)
        else:
            try:
                resolved.actor.focus()
            except Exception:  # noqa: BLE001 - some controls take text without accepting focus
                pass
            resolved.actor.type_text(action.text)

    if action.press_enter:
        module.input_sim().press("Enter")
    verb = "Replaced the value of" if action.replace else "Typed into"
    return success_result(f"{verb} {target_label}." + (" Pressed Enter." if action.press_enter else ""))


def _focus(action: models.FocusAction) -> Dict[str, Any]:
    resolved = resolve_element(action.target)
    resolved.actor.focus()
    return success_result(f"Focused {resolved.label}.")


def _act(action: models.ActAction) -> Dict[str, Any]:
    resolved = resolve_element(action.target)
    actor = resolved.actor
    verb = action.verb

    if verb in ("check", "uncheck"):
        return _reconcile_checked(resolved, wanted="on" if verb == "check" else "off")
    if verb == "set_number":
        actor.set_numeric_value(action.number)
        return success_result(f"Set {resolved.label} to {action.number}.")
    if verb == "select_text":
        actor.select_text(action.start, action.end)
        return success_result(f"Selected text [{action.start}, {action.end}) in {resolved.label}.")
    if verb == "raw":
        actor.perform_action(action.action_name)
        return success_result(f"Performed {action.action_name!r} on {resolved.label}.")
    if verb in ("increment", "decrement"):
        for _ in range(action.repeat):
            getattr(actor, verb)()
        return success_result(f"Applied {verb} x{action.repeat} to {resolved.label}.")

    getattr(actor, verb)()
    note = ""
    if verb == "scroll_into_view" and platform.system().lower() == "darwin":
        note = " Note: macOS has no accessibility equivalent, so this was a no-op — use 'scroll' instead."
    return success_result(f"Applied {verb} to {resolved.label}.{note}")


def _reconcile_checked(resolved: Resolved, wanted: str) -> Dict[str, Any]:
    """Toggle only when the element is not already in the wanted state."""
    current = resolved.as_element().checked
    if current is None:
        raise ToolError(f"{resolved.label} has no checked state — it is not a checkbox, radio button, or switch.")
    if current == wanted:
        return success_result(f"{resolved.label} was already {wanted}; nothing to do.")
    resolved.actor.toggle()
    # Re-read rather than assert. `toggle` flips; it does not set. From a
    # tri-state ("mixed") control it may land on either value depending on the
    # platform and the widget, so reporting `wanted` as fact states a result
    # that was never observed.
    actual = resolved.as_element().checked
    if actual == wanted:
        return success_result(f"Toggled {resolved.label} from {current} to {wanted}.")
    return success_result(
        f"Toggled {resolved.label} from {current}; it is now {actual}, not {wanted}. "
        f"Toggling flips the state rather than setting it, so a tri-state control may "
        f"need another toggle — read it again before deciding."
    )


# ── Tier 3: synthesised input and pixels ─────────────────────────────────────


def _key(action: models.KeyAction) -> Dict[str, Any]:
    # Keystrokes go wherever focus happens to be, so a failed raise is not cosmetic — it
    # sends the sequence to the wrong application. Report it instead of swallowing it.
    focus_note = ""
    if action.app:
        app = resolve_app(action.app)
        if not focus_app(app):
            focus_note = (
                f" WARNING: {app.name} could not be brought to the foreground, so these keys may "
                f"have gone to another application. Check with 'snapshot' before continuing."
            )
    keys = normalize_keys(action.keys)
    held = normalize_keys(action.hold)
    sim = xa11y().input_sim()
    for _ in range(action.repeat):
        for key in keys:
            if held:
                sim.chord(key, held)
            else:
                sim.press(key)
    combo = "+".join(held + [" ".join(keys)]) if held else " ".join(keys)
    return success_result(f"Sent {combo}" + (f" x{action.repeat}" if action.repeat > 1 else "") + "." + focus_note)


def _mouse(action: models.MouseAction) -> Dict[str, Any]:
    sim = xa11y().input_sim()
    if action.op == "move":
        target = action.target
        assert target is not None  # guaranteed by the model validator
        resolved = resolve_pointer(target)
        sim.move_to(pointer_argument(resolved, target))
        return success_result(f"Moved the pointer to {resolved.label}.")
    getattr(sim, f"mouse_{action.op}")(action.button)
    return success_result(f"Mouse {action.button} button {action.op} at the current pointer position.")


def _drag(action: models.DragAction) -> Dict[str, Any]:
    start, end = resolve_pointer(action.start), resolve_pointer(action.end)
    xa11y().input_sim().drag(
        pointer_argument(start, action.start),
        pointer_argument(end, action.end),
        button=action.button,
        held=normalize_keys(action.modifiers) or None,
        duration=action.duration,
    )
    return success_result(f"Dragged from {start.label} to {end.label}.")


def _scroll(action: models.ScrollAction) -> Dict[str, Any]:
    resolved = resolve_pointer(action.target)
    xa11y().input_sim().scroll(pointer_argument(resolved, action.target), action.dx, action.dy)
    return success_result(f"Scrolled dx={action.dx} dy={action.dy} over {resolved.label}.")


def _screenshot(action: models.ScreenshotAction) -> Dict[str, Any]:
    module = xa11y()
    if action.target is not None:
        resolved = resolve_element(action.target)
        shot = module.screenshot(element=resolved.as_element())
        where = resolved.label
    elif action.region is not None:
        shot = module.screenshot(region=tuple(action.region))
        where = f"region {tuple(action.region)}"
    else:
        shot = module.screenshot()
        where = "the primary display"

    png = shot.to_png()
    notes = [f"Captured {where}: {shot.width}x{shot.height} physical px at scale {shot.scale}."]
    if action.save_path:
        shot.save_png(action.save_path)
        notes.append(f"Saved to {action.save_path}.")

    if not action.send_image:
        notes.append("Image withheld from the transcript; pass send_image=true to include it.")
        return success_result(" ".join(notes))
    if len(png) > MAX_IMAGE_BYTES:
        notes.append(
            f"Image withheld: {len(png) / 1024 / 1024:.1f}MB exceeds the {MAX_IMAGE_BYTES // 1024 // 1024}MB limit. "
            f"Capture a single element or a region instead."
        )
        return success_result(" ".join(notes))
    return success_result(" ".join(notes), extra=[{"image": {"format": "png", "source": {"bytes": png}}}])


# ── Lifecycle ────────────────────────────────────────────────────────────────


def _open_app(action: models.OpenAppAction) -> Dict[str, Any]:
    system = platform.system().lower()
    if system == "darwin":
        command = ["open", "-a", action.name]
    elif system == "windows":
        command = ["cmd", "/c", "start", "", action.name]
    else:
        command = [action.name]

    try:
        # Both pipes are discarded rather than captured: nothing here ever reads them, and an
        # unread PIPE deadlocks the child as soon as it fills its buffer.
        process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (OSError, ValueError) as exc:
        raise ToolError(f"Could not launch {action.name!r}: {exc}") from exc

    module = xa11y()
    try:
        # Wait for the accessibility bridge to register it, rather than sleeping and hoping.
        app = module.App.by_name(action.name, timeout=action.timeout)
    except module.XA11yError:
        # 'open' and 'start' exit immediately, so their pid is not the app's; only worth
        # trying where the launched process is the app itself.
        if system not in ("darwin", "windows"):
            app = module.App.by_pid(process.pid, timeout=action.timeout)
        else:
            raise
    return success_result(
        f"Launched {action.name}; {app.name} is reachable" + (f" (pid {app.pid})" if app.pid else "") + "."
    )


def _close_app(action: models.CloseAppAction) -> Dict[str, Any]:
    try:
        import psutil
    except ImportError as exc:
        raise ToolError(
            "Closing applications needs psutil. Install it with: pip install 'strands-xa11y[process]'"
        ) from exc

    # The sweep matches on a substring, so it would happily terminate the process hosting this
    # agent — 'python', 'node' and 'code' all match something. Exclude ourselves explicitly.
    own_pid = os.getpid()
    closed = []
    for process in psutil.process_iter(["pid", "name"]):
        name = process.info.get("name") or ""
        if process.info["pid"] == own_pid or action.name.lower() not in name.lower():
            continue
        try:
            process.terminate()
            closed.append(f"{name} (pid {process.info['pid']})")
        except psutil.Error as exc:  # noqa: PERF203 - per-process failures should not abort the sweep
            closed.append(f"{name}: failed to terminate ({exc})")
    if not closed:
        return success_result(f"No running process matches {action.name!r}.")
    return success_result(f"Terminated {len(closed)} process(es): " + ", ".join(closed))


HANDLERS: Dict[str, Callable[[Any], Dict[str, Any]]] = {
    "list_apps": _list_apps,
    "snapshot": _snapshot,
    "find": _find,
    "read": _read,
    "wait": _wait,
    "click": _click,
    "type": _type,
    "focus": _focus,
    "act": _act,
    "key": _key,
    "mouse": _mouse,
    "drag": _drag,
    "scroll": _scroll,
    "screenshot": _screenshot,
    "open_app": _open_app,
    "close_app": _close_app,
}


def summarize(action: Any) -> str:
    """A one-line description of an action, for the consent prompt."""
    fields = action.model_dump(exclude_none=True, exclude_defaults=True)
    fields.pop("type", None)
    rendered = ", ".join(f"{name}={value!r}" for name, value in fields.items())
    return f"{action.type}({rendered})" if rendered else action.type


def needs_consent(action: Any) -> bool:
    """Whether an action changes the machine's state, or ships pixels somewhere.

    A screenshot is read-only right up until it leaves the process: ``send_image`` puts
    whatever is on the user's screen into the transcript, and ``save_path`` writes it to
    disk. Both are decisions to ask about; capturing and describing one is not.
    """
    if action.type in models.MUTATING_ACTIONS:
        return True
    return bool(action.type == "screenshot" and (action.send_image or action.save_path))


def run(action: Any) -> Dict[str, Any]:
    """Execute one action, converting every failure into a tool result."""
    try:
        if needs_consent(action):
            require_consent(summarize(action))
        return HANDLERS[action.type](action)
    except ToolError as exc:
        return error_result(str(exc))
    except Exception as exc:  # noqa: BLE001 - the agent gets the diagnosis, not a traceback
        return error_result(describe(exc))
