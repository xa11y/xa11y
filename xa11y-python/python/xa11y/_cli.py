"""xa11y CLI — accessibility tree explorer.

Installed as the ``xa11y`` console script via ``pip install xa11y``.
"""

from __future__ import annotations

import sys


def main() -> None:
    args = sys.argv[1:]
    cmd = args[0] if args else None
    try:
        if cmd == "apps":
            _cmd_apps()
        elif cmd == "tree":
            _cmd_tree(args[1:])
        elif cmd == "find":
            _cmd_find(args[1:])
        elif cmd == "action":
            _cmd_action(args[1:])
        elif cmd == "events":
            _cmd_events(args[1:])
        else:
            _print_usage()
    except KeyboardInterrupt:
        pass
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)


def _print_usage() -> None:
    print(
        """\
xa11y — accessibility tree explorer

Usage:
  xa11y apps                                List running applications
  xa11y tree   [--app NAME | --pid PID]     Print the accessibility tree
  xa11y find   SELECTOR [--app NAME | --pid PID]
                                            Find elements matching a selector
  xa11y action ACTION SELECTOR [--app NAME | --pid PID] [--value V]
                                            Perform an action on an element
  xa11y events [--app NAME | --pid PID]     Stream accessibility events

Actions: press, focus, blur, toggle, expand, collapse, select, show-menu,
  scroll-into-view, scroll-down, scroll-right, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  select-text (requires --value START,END)""",
        file=sys.stderr,
    )


# ── Argument helpers ─────────────────────────────────────────────────────────


def _parse_opts(args: list[str]) -> tuple[dict[str, str | None], list[str]]:
    opts: dict[str, str | None] = {"app": None, "pid": None, "value": None}
    positional: list[str] = []
    i = 0
    while i < len(args):
        if args[i] == "--app" and i + 1 < len(args):
            i += 1
            opts["app"] = args[i]
        elif args[i] == "--pid" and i + 1 < len(args):
            i += 1
            opts["pid"] = args[i]
        elif args[i] == "--value" and i + 1 < len(args):
            i += 1
            opts["value"] = args[i]
        else:
            positional.append(args[i])
        i += 1
    return opts, positional


def _resolve_app_root(opts: dict[str, str | None]):
    """Return the root Element for the target app."""
    import xa11y

    if opts["app"]:
        loc = xa11y.locator(f'application[name="{opts["app"]}"]')
        return loc.element()
    if opts["pid"]:
        # Find app by PID: list all apps and filter
        apps = xa11y.locator("application").elements()
        for app in apps:
            if app.pid is not None and str(app.pid) == opts["pid"]:
                return app
        raise RuntimeError(f"no application with pid={opts['pid']}")
    raise RuntimeError("specify --app NAME or --pid PID")


# ── Output helpers ───────────────────────────────────────────────────────────


def _format_element(el) -> str:
    parts = [el.role]

    if el.name is not None:
        parts.append(f'"{el.name}"')
    if el.value is not None:
        parts.append(f'value="{el.value}"')
    if el.numeric_value is not None:
        nv = f"numeric_value={el.numeric_value}"
        if el.min_value is not None:
            nv += f" min={el.min_value}"
        if el.max_value is not None:
            nv += f" max={el.max_value}"
        parts.append(nv)
    if el.description is not None:
        parts.append(f'description="{el.description}"')

    # States
    states = []
    states.append("enabled" if el.enabled else "disabled")
    states.append("visible" if el.visible else "hidden")
    if el.focused:
        states.append("focused")
    if el.focusable:
        states.append("focusable")
    if el.editable:
        states.append("editable")
    if el.selected:
        states.append("selected")
    if el.modal:
        states.append("modal")
    if el.required:
        states.append("required")
    if el.busy:
        states.append("busy")
    if el.checked is not None:
        states.append(f"checked={el.checked}")
    if el.expanded is not None:
        states.append("expanded" if el.expanded else "collapsed")
    if states:
        parts.append(f"[{' '.join(states)}]")

    if el.bounds is not None:
        b = el.bounds
        parts.append(f"bounds=({b.x},{b.y},{b.width},{b.height})")
    if el.stable_id is not None:
        parts.append(f'id="{el.stable_id}"')
    if el.actions:
        parts.append(f"actions=[{','.join(el.actions)}]")

    return " ".join(parts)


def _print_tree(el, prefix: str = "", is_last: bool = True, is_root: bool = True) -> None:
    if is_root:
        connector = ""
    elif is_last:
        connector = "└── "
    else:
        connector = "├── "
    print(f"{prefix}{connector}{_format_element(el)}")

    try:
        children = el.children()
    except Exception as exc:
        child_prefix = prefix if is_root else (prefix + ("    " if is_last else "│   "))
        print(f"{child_prefix}└── <error: {exc}>")
        return

    child_prefix = prefix if is_root else (prefix + ("    " if is_last else "│   "))
    for i, child in enumerate(children):
        _print_tree(child, child_prefix, is_last=(i == len(children) - 1), is_root=False)


# ── Commands ─────────────────────────────────────────────────────────────────


def _cmd_apps() -> None:
    import xa11y

    apps = xa11y.locator("application").elements()
    if not apps:
        print("No applications found.")
        return
    for app in apps:
        pid = app.pid if app.pid is not None else "-"
        print(f"{pid}\t{app.name}")


def _cmd_tree(args: list[str]) -> None:
    opts, _pos = _parse_opts(args)
    root = _resolve_app_root(opts)
    _print_tree(root)


def _cmd_find(args: list[str]) -> None:
    opts, positional = _parse_opts(args)
    if not positional:
        raise RuntimeError("usage: xa11y find SELECTOR [--app NAME | --pid PID]")
    selector = positional[0]
    root = _resolve_app_root(opts)
    elements = root.locator(selector).elements()
    for el in elements:
        print(_format_element(el))
    suffix = "" if len(elements) == 1 else "es"
    print(f"({len(elements)} match{suffix})")


def _cmd_action(args: list[str]) -> None:
    opts, positional = _parse_opts(args)
    if len(positional) < 2:
        raise RuntimeError(
            "usage: xa11y action ACTION SELECTOR [--app NAME | --pid PID] [--value V]"
        )
    action_name = positional[0]
    selector = positional[1]
    value = opts["value"]

    root = _resolve_app_root(opts)
    loc = root.locator(selector)

    simple_actions = {
        "press": loc.press,
        "focus": loc.focus,
        "blur": loc.blur,
        "toggle": loc.toggle,
        "expand": loc.expand,
        "collapse": loc.collapse,
        "select": loc.select,
        "show-menu": loc.show_menu,
        "scroll-into-view": loc.scroll_into_view,
        "increment": loc.increment,
        "decrement": loc.decrement,
    }

    if action_name in simple_actions:
        simple_actions[action_name]()
    elif action_name == "scroll-down":
        amount = float(value) if value else 1.0
        loc.scroll_down(amount)
    elif action_name == "scroll-right":
        amount = float(value) if value else 1.0
        loc.scroll_right(amount)
    elif action_name == "set-value":
        if value is None:
            raise RuntimeError("set-value requires --value")
        loc.set_value(value)
    elif action_name == "type-text":
        if value is None:
            raise RuntimeError("type-text requires --value")
        loc.type_text(value)
    elif action_name == "select-text":
        if value is None:
            raise RuntimeError("select-text requires --value START,END")
        parts = value.split(",")
        if len(parts) != 2:
            raise RuntimeError("select-text --value must be START,END (e.g. 0,5)")
        loc.select_text(int(parts[0].strip()), int(parts[1].strip()))
    else:
        raise RuntimeError(f"unknown action: {action_name}")
    print("ok")


def _cmd_events(args: list[str]) -> None:
    opts, _pos = _parse_opts(args)
    root = _resolve_app_root(opts)
    sub = root.subscribe()
    name = root.name or "app"
    print(f'Listening for events on "{name}" (ctrl-c to stop)...', file=sys.stderr)
    for event in sub:
        if event is None:
            continue
        target_str = "-"
        if event.target is not None:
            t = event.target
            name_part = f' "{t.name}"' if t.name else ""
            target_str = f"{t.role}{name_part}"
        print(f"[{event.event_type}] {target_str}")


if __name__ == "__main__":
    main()
