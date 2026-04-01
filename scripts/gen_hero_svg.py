#!/usr/bin/env python3
"""Generate the animated hero SVG for the xa11y homepage/README.

Outputs a self-contained SVG using SMIL animations (no JS/CSS keyframes)
so it works when embedded on GitHub via <img>.

Supports light/dark mode via @media (prefers-color-scheme). Static elements
use CSS classes; animated elements (SMIL bakes colors into attributes) are
duplicated per theme and toggled with display:none.

Usage:
    python scripts/gen_hero_svg.py > docs/site/public/hero.svg
"""

from __future__ import annotations

import random
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Layout constants (shared across themes)
# ---------------------------------------------------------------------------

WIDTH = 600
HEIGHT = 520
FONT = "ui-monospace, 'Cascadia Code', 'Fira Code', monospace"
FONT_SIZE = 13
CHAR_W = FONT_SIZE * 0.602
NODE_H = 28
NODE_RX = 6
NODE_PAD_X = 14
INDENT = 28
TREE_X = 40
TREE_Y = 110
LINE_H = 38
CMD_X = 40
CMD_Y = 70
CMD_TEXT_X = CMD_X + 36  # after ">>> "
CMD_TEXT_Y = CMD_Y
TYPING_SPEED = 0.025
TYPING_JITTER = 0.04  # max random deviation per char
JITTER_SEED = 42

# ---------------------------------------------------------------------------
# Theme palettes
# ---------------------------------------------------------------------------

@dataclass
class Theme:
    name: str           # "dark" or "light"
    bg: str
    tree_line: str
    tree_role: str      # dim role text
    tree_name: str      # brighter name text
    node_bg: str
    node_border: str
    prompt: str
    cursor: str
    code_default: str   # fallback code text color
    accent: str
    green: str
    red: str
    badge_bg_accent: str
    badge_bg_green: str
    badge_bg_red: str


DARK = Theme(
    name="dark",
    bg="#0f1117",
    tree_line="#3a3d4a",
    tree_role="#6b7080",
    tree_name="#94a3b8",
    node_bg="#1a1d27",
    node_border="#3a3d4a",
    prompt="#6366f1",
    cursor="#e2e8f0",
    code_default="#e2e8f0",
    accent="#22d3ee",
    green="#4ade80",
    red="#f87171",
    badge_bg_accent="#0a2030",
    badge_bg_green="#0a2e1a",
    badge_bg_red="#2e0a0a",
)

LIGHT = Theme(
    name="light",
    bg="#ffffff",
    tree_line="#d1d9e0",
    tree_role="#656d76",
    tree_name="#1f2328",
    node_bg="#f6f8fa",
    node_border="#d1d9e0",
    prompt="#6366f1",
    cursor="#1f2328",
    code_default="#1f2328",
    accent="#0891b2",
    green="#16a34a",
    red="#dc2626",
    badge_bg_accent="#cffafe",
    badge_bg_green="#dcfce7",
    badge_bg_red="#fee2e2",
)

THEMES = [DARK, LIGHT]


# ---------------------------------------------------------------------------
# Tree definition
# ---------------------------------------------------------------------------

@dataclass
class TNode:
    role: str
    name: str
    id: str
    children: list[TNode] = field(default_factory=list)


TREE = TNode("window", "Slack", "n-window", children=[
    TNode("list", "Channels", "n-channels", children=[
        TNode("button", "general", "n-general"),
        TNode("button", "random", "n-random"),
        TNode("button", "engineering", "n-eng"),
    ]),
    TNode("group", "Messages", "n-messages", children=[
        TNode("static_text", "Hey team, PR is ready", "n-pr"),
        TNode("static_text", "LGTM, merging now", "n-lgtm"),
        TNode("text_field", "Message", "n-msgfield"),
        TNode("button", "Send", "n-send"),
    ]),
])


# ---------------------------------------------------------------------------
# Animation sequence
# ---------------------------------------------------------------------------

@dataclass
class Step:
    kind: str  # "select", "action", "assert_pass", "assert_fail", "type_text"
    code: str
    target: str | None = None
    label: str | None = None
    value_text: str | None = None


STEPS = [
    Step("select",
         "slack = xa11y.locator('application[name=\"Slack\"]')",
         target="n-window"),
    Step("action",
         "slack.descendant('button[name=\"general\"]').press()",
         target="n-general", label="press"),

    Step("select",
         "msg = slack.descendant('text_field[name=\"Message\"]')",
         target="n-msgfield"),
    Step("type_text",
         'msg.type_text("Looks good, shipping it!")',
         target="n-msgfield",
         value_text="Looks good, shipping it!"),

    Step("select",
         "send = slack.descendant('button[name=\"Send\"]')",
         target="n-send"),
    Step("assert_pass", "assert send.exists()", target="n-send",
         label="exists() \u2713"),

    Step("select",
         "pr = slack.descendant('static_text[name*=\"PR\"]')",
         target="n-pr"),
    Step("assert_fail", 'assert pr.element().value == "no"', target="n-pr",
         label='value \u2717'),
]

PAUSE_AFTER_STEP = 0.8
PAUSE_AFTER_MATCH = 0.4
ACTION_DUR = 0.6
HOLD_END = 3.0
FADE_RESET = 1.0


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def flatten_tree(node: TNode, depth: int = 0) -> list[tuple[TNode, int]]:
    result = [(node, depth)]
    for child in node.children:
        result.extend(flatten_tree(child, depth + 1))
    return result


def text_w(text: str) -> float:
    return len(text) * CHAR_W


def node_label(n: TNode) -> str:
    return f'{n.role} "{n.name}"'


def step_color(step: Step, theme: Theme) -> str:
    if step.kind == "select":
        return theme.accent
    if step.kind == "assert_pass":
        return theme.green
    if step.kind == "assert_fail":
        return theme.red
    return theme.code_default


def hl_color(step: Step, theme: Theme) -> str:
    if step.kind == "assert_pass":
        return theme.green
    if step.kind == "assert_fail":
        return theme.red
    return theme.accent


def badge_colors(step: Step, theme: Theme) -> tuple[str, str]:
    """Returns (text_color, bg_color) for a badge."""
    if step.kind == "assert_pass":
        return theme.green, theme.badge_bg_green
    if step.kind == "assert_fail":
        return theme.red, theme.badge_bg_red
    return theme.accent, theme.badge_bg_accent


def mb(offset: float) -> str:
    """begin= value relative to master clock."""
    return f"m.begin+{offset:.3f}s"


def jittered_char_times(n: int, base_speed: float, jitter: float,
                        rng: random.Random) -> list[float]:
    """Return cumulative per-character durations with jitter.

    Returns a list of n floats: the cumulative time after each character.
    """
    cumulative = []
    t = 0.0
    for _ in range(n):
        dt = base_speed + rng.uniform(-jitter, jitter)
        dt = max(dt, 0.01)  # floor so nothing goes negative
        t += dt
        cumulative.append(t)
    return cumulative


# ---------------------------------------------------------------------------
# SVG builder
# ---------------------------------------------------------------------------

class SvgBuilder:
    def __init__(self):
        self.svg = ET.Element("svg", {
            "xmlns": "http://www.w3.org/2000/svg",
            "viewBox": f"0 0 {WIDTH} {HEIGHT}",
            "width": str(WIDTH),
            "height": str(HEIGHT),
        })
        self.defs = ET.SubElement(self.svg, "defs")
        self.flat = flatten_tree(TREE)
        self.node_pos: dict[str, tuple[float, float, float]] = {}
        self.total_dur = 0.0
        self._compute_layout()

    def _compute_layout(self):
        for i, (node, depth) in enumerate(self.flat):
            x = TREE_X + depth * INDENT
            y = TREE_Y + i * LINE_H
            w = text_w(node_label(node)) + NODE_PAD_X * 2
            self.node_pos[node.id] = (x, y, w)

    def build(self) -> str:
        timeline = self._compute_timeline()
        self._add_style()
        self._add_master_clock()

        for theme in THEMES:
            cls = theme.name
            g = ET.SubElement(self.svg, "g", {"class": cls})
            self._add_bg(g, theme)
            self._add_tree(g, theme)
            self._add_command_area(g, theme, timeline, cls)
            self._add_highlights(g, theme, timeline, cls)
            self._add_badges(g, theme, timeline)
            self._add_fade_overlay(g, theme)

        ET.indent(self.svg, space="  ")
        return '<?xml version="1.0" encoding="UTF-8"?>\n' + ET.tostring(
            self.svg, encoding="unicode"
        )

    # -- CSS ---------------------------------------------------------------

    def _add_style(self):
        style = ET.SubElement(self.defs, "style")
        style.text = (
            "\n"
            "  .light { display: none; }\n"
            "  @media (prefers-color-scheme: light) {\n"
            "    .dark { display: none; }\n"
            "    .light { display: inline; }\n"
            "  }\n"
        )

    # -- timeline ----------------------------------------------------------

    def _compute_timeline(self) -> list[dict]:
        timeline = []
        t = 1.5
        rng = random.Random(JITTER_SEED)

        for step in STEPS:
            char_times = jittered_char_times(
                len(step.code), TYPING_SPEED, TYPING_JITTER, rng
            )
            typing_dur = char_times[-1] if char_times else 0.0
            entry = {
                "step": step,
                "type_start": t,
                "type_dur": typing_dur,
                "char_times": char_times,  # cumulative per-char offsets
            }
            t += typing_dur + 0.2

            if step.target:
                entry["hl_start"] = t
                t += PAUSE_AFTER_MATCH

            if step.kind in ("action", "type_text"):
                entry["act_start"] = t
                t += ACTION_DUR + 0.2

            if step.kind.startswith("assert"):
                entry["badge_start"] = t
                t += ACTION_DUR + 0.2

            t += PAUSE_AFTER_STEP
            timeline.append(entry)

        self.total_dur = t + HOLD_END + FADE_RESET
        return timeline

    # -- master clock ------------------------------------------------------

    def _add_master_clock(self):
        clock = ET.SubElement(self.svg, "rect", {
            "width": "0", "height": "0", "opacity": "0",
        })
        ET.SubElement(clock, "animate", {
            "id": "m",
            "attributeName": "visibility",
            "from": "hidden", "to": "hidden",
            "begin": "0s;m.end",
            "dur": f"{self.total_dur:.2f}s",
        })

    # -- background --------------------------------------------------------

    def _add_bg(self, parent: ET.Element, theme: Theme):
        ET.SubElement(parent, "rect", {
            "width": str(WIDTH), "height": str(HEIGHT),
            "fill": theme.bg,
        })

    # -- tree --------------------------------------------------------------

    def _add_tree(self, parent: ET.Element, theme: Theme):
        g = ET.SubElement(parent, "g")

        for i, (node, depth) in enumerate(self.flat):
            x, y, w = self.node_pos[node.id]

            # connector lines
            if depth > 0:
                for j in range(i - 1, -1, -1):
                    if self.flat[j][1] == depth - 1:
                        pnode = self.flat[j][0]
                        px, py, _ = self.node_pos[pnode.id]
                        ET.SubElement(g, "path", {
                            "d": (f"M {px + 12} {py + NODE_H + 2} "
                                  f"L {px + 12} {y + NODE_H // 2} "
                                  f"L {x - 4} {y + NODE_H // 2}"),
                            "stroke": theme.tree_line,
                            "stroke-width": "1.5",
                            "fill": "none",
                            "stroke-linecap": "round",
                        })
                        break

            ng = ET.SubElement(g, "g")

            ET.SubElement(ng, "rect", {
                "x": str(x), "y": str(y),
                "width": str(w), "height": str(NODE_H),
                "rx": str(NODE_RX),
                "fill": theme.node_bg,
                "stroke": theme.node_border,
                "stroke-width": "1",
            })

            role_str = f"{node.role} "
            t1 = ET.SubElement(ng, "text", {
                "x": str(x + NODE_PAD_X),
                "y": str(y + NODE_H // 2 + FONT_SIZE * 0.35),
                "font-family": FONT,
                "font-size": str(FONT_SIZE),
                "fill": theme.tree_role,
            })
            t1.text = role_str

            t2 = ET.SubElement(ng, "text", {
                "x": str(x + NODE_PAD_X + text_w(role_str)),
                "y": str(y + NODE_H // 2 + FONT_SIZE * 0.35),
                "font-family": FONT,
                "font-size": str(FONT_SIZE),
                "fill": theme.tree_name,
            })
            t2.text = f'"{node.name}"'

    # -- command area ------------------------------------------------------

    def _add_command_area(self, parent: ET.Element, theme: Theme,
                          timeline: list[dict], cls: str):
        g = ET.SubElement(parent, "g")

        # Bare prompt — no terminal window chrome
        prompt = ET.SubElement(g, "text", {
            "x": str(CMD_X), "y": str(CMD_TEXT_Y),
            "font-family": FONT, "font-size": str(FONT_SIZE),
            "fill": theme.prompt,
        })
        prompt.text = ">>>"

        # Typing text — one <text> per step with jittered clip-path reveal
        for si, entry in enumerate(timeline):
            step = entry["step"]
            t_start = entry["type_start"]
            t_dur = entry["type_dur"]
            char_times = entry["char_times"]
            code = step.code
            color = step_color(step, theme)
            full_w = text_w(code) + 12
            n = len(code)

            # Build keyTimes (0..1 normalized) and values (widths) with jitter
            key_times = ["0"]
            values = ["0"]
            for ci, ct in enumerate(char_times):
                frac = ct / t_dur if t_dur > 0 else 1.0
                frac = min(frac, 1.0)
                w = text_w(code[:ci + 1]) + CHAR_W
                key_times.append(f"{frac:.4f}")
                values.append(f"{w:.1f}")

            clip_id = f"clip-cmd-{cls}-{si}"
            clip = ET.SubElement(self.defs, "clipPath", {"id": clip_id})
            clip_rect = ET.SubElement(clip, "rect", {
                "x": str(CMD_TEXT_X),
                "y": str(CMD_TEXT_Y - FONT_SIZE),
                "width": "0",
                "height": str(FONT_SIZE + 8),
            })
            ET.SubElement(clip_rect, "animate", {
                "attributeName": "width",
                "values": ";".join(values),
                "keyTimes": ";".join(key_times),
                "begin": mb(t_start), "dur": f"{t_dur:.3f}s",
                "fill": "freeze",
                "calcMode": "linear",
            })
            ET.SubElement(clip_rect, "set", {
                "attributeName": "width",
                "to": "0",
                "begin": mb(0),
            })

            txt = ET.SubElement(g, "text", {
                "x": str(CMD_TEXT_X), "y": str(CMD_TEXT_Y),
                "font-family": FONT, "font-size": str(FONT_SIZE),
                "fill": color,
                "clip-path": f"url(#{clip_id})",
                "visibility": "hidden",
            })
            txt.text = code

            ET.SubElement(txt, "set", {
                "attributeName": "visibility",
                "to": "visible",
                "begin": mb(t_start),
            })
            if si < len(timeline) - 1:
                ET.SubElement(txt, "set", {
                    "attributeName": "visibility",
                    "to": "hidden",
                    "begin": mb(timeline[si + 1]["type_start"]),
                })
            ET.SubElement(txt, "set", {
                "attributeName": "visibility",
                "to": "hidden",
                "begin": mb(0),
            })

        # Blinking cursor with jittered position
        cursor = ET.SubElement(g, "rect", {
            "x": str(CMD_TEXT_X), "y": str(CMD_TEXT_Y - FONT_SIZE),
            "width": "2", "height": str(FONT_SIZE + 4),
            "fill": theme.cursor,
        })
        ET.SubElement(cursor, "animate", {
            "attributeName": "opacity",
            "values": "1;1;0;0",
            "dur": "1s",
            "repeatCount": "indefinite",
        })
        for entry in timeline:
            t_start = entry["type_start"]
            t_dur = entry["type_dur"]
            char_times = entry["char_times"]
            code = entry["step"].code
            n = len(code)

            # Jittered cursor x positions matching the clip reveal
            key_times = ["0"]
            values = [str(CMD_TEXT_X)]
            for ci, ct in enumerate(char_times):
                frac = ct / t_dur if t_dur > 0 else 1.0
                frac = min(frac, 1.0)
                cx = CMD_TEXT_X + text_w(code[:ci + 1]) + CHAR_W
                key_times.append(f"{frac:.4f}")
                values.append(f"{cx:.1f}")

            ET.SubElement(cursor, "animate", {
                "attributeName": "x",
                "values": ";".join(values),
                "keyTimes": ";".join(key_times),
                "begin": mb(t_start), "dur": f"{t_dur:.3f}s",
                "fill": "freeze",
                "calcMode": "linear",
            })
        ET.SubElement(cursor, "set", {
            "attributeName": "x",
            "to": str(CMD_TEXT_X),
            "begin": mb(0),
        })

    # -- node highlights ---------------------------------------------------

    def _add_highlights(self, parent: ET.Element, theme: Theme,
                        timeline: list[dict], cls: str):
        g = ET.SubElement(parent, "g")

        for si, entry in enumerate(timeline):
            step = entry["step"]
            if not step.target or "hl_start" not in entry:
                continue

            x, y, w = self.node_pos[step.target]
            hl_start = entry["hl_start"]
            color = hl_color(step, theme)

            off_time = self.total_dur - FADE_RESET
            for j in range(si + 1, len(timeline)):
                if timeline[j]["step"].kind == "select":
                    off_time = timeline[j]["type_start"]
                    break

            # Glow outline
            glow = ET.SubElement(g, "rect", {
                "x": str(x - 2), "y": str(y - 2),
                "width": str(w + 4), "height": str(NODE_H + 4),
                "rx": str(NODE_RX + 2),
                "fill": "none",
                "stroke": color,
                "stroke-width": "2",
                "opacity": "0",
            })
            ET.SubElement(glow, "animate", {
                "attributeName": "opacity",
                "from": "0", "to": "1",
                "begin": mb(hl_start), "dur": "0.3s",
                "fill": "freeze",
            })
            ET.SubElement(glow, "animate", {
                "attributeName": "opacity",
                "from": "1", "to": "0",
                "begin": mb(off_time), "dur": "0.3s",
                "fill": "freeze",
            })
            ET.SubElement(glow, "set", {
                "attributeName": "opacity",
                "to": "0",
                "begin": mb(0),
            })

            # Throb for press
            if step.kind == "action" and "act_start" in entry:
                act_t = entry["act_start"]
                throb = ET.SubElement(g, "rect", {
                    "x": str(x - 4), "y": str(y - 4),
                    "width": str(w + 8), "height": str(NODE_H + 8),
                    "rx": str(NODE_RX + 4),
                    "fill": color, "opacity": "0",
                })
                ET.SubElement(throb, "animate", {
                    "attributeName": "opacity",
                    "values": "0;0.35;0",
                    "begin": mb(act_t), "dur": "0.35s",
                    "fill": "freeze",
                })
                ET.SubElement(glow, "animate", {
                    "attributeName": "stroke-width",
                    "values": "2;5;2",
                    "begin": mb(act_t), "dur": "0.35s",
                    "fill": "freeze",
                })

            # type_text value reveal
            if step.kind == "type_text" and step.value_text and "act_start" in entry:
                act_t = entry["act_start"]
                val = ET.SubElement(g, "text", {
                    "x": str(x + w + 10),
                    "y": str(y + NODE_H // 2 + FONT_SIZE * 0.35),
                    "font-family": FONT,
                    "font-size": str(FONT_SIZE - 1),
                    "fill": color, "opacity": "0",
                })
                val.text = f'= "{step.value_text}"'

                val_full_w = text_w(f'= "{step.value_text}"') + 4
                val_clip_id = f"clip-val-{cls}-{si}"
                val_clip = ET.SubElement(self.defs, "clipPath", {"id": val_clip_id})
                val_clip_rect = ET.SubElement(val_clip, "rect", {
                    "x": str(x + w + 10),
                    "y": str(y - 2),
                    "width": "0",
                    "height": str(NODE_H + 4),
                })
                val_type_dur = len(step.value_text) * TYPING_SPEED * 0.6
                ET.SubElement(val_clip_rect, "animate", {
                    "attributeName": "width",
                    "from": "0", "to": f"{val_full_w:.1f}",
                    "begin": mb(act_t), "dur": f"{val_type_dur:.3f}s",
                    "fill": "freeze",
                })
                ET.SubElement(val_clip_rect, "set", {
                    "attributeName": "width",
                    "to": "0",
                    "begin": mb(0),
                })

                val.set("clip-path", f"url(#{val_clip_id})")
                ET.SubElement(val, "set", {
                    "attributeName": "opacity",
                    "to": "1",
                    "begin": mb(act_t),
                })
                ET.SubElement(val, "animate", {
                    "attributeName": "opacity",
                    "from": "1", "to": "0",
                    "begin": mb(off_time), "dur": "0.3s",
                    "fill": "freeze",
                })
                ET.SubElement(val, "set", {
                    "attributeName": "opacity",
                    "to": "0",
                    "begin": mb(0),
                })

    # -- badges ------------------------------------------------------------

    def _add_badges(self, parent: ET.Element, theme: Theme,
                    timeline: list[dict]):
        g = ET.SubElement(parent, "g")

        for si, entry in enumerate(timeline):
            step = entry["step"]
            if not step.label or not step.target:
                continue
            if step.kind == "type_text":
                continue

            begin_key = "act_start" if step.kind == "action" else "badge_start"
            if begin_key not in entry:
                continue

            badge_t = entry[begin_key]
            x, y, w = self.node_pos[step.target]
            bx = x + w + 10
            by = y

            off_time = self.total_dur - FADE_RESET
            for j in range(si + 1, len(timeline)):
                if timeline[j]["step"].kind == "select":
                    off_time = timeline[j]["type_start"]
                    break

            color, bg_c = badge_colors(step, theme)
            bw = text_w(step.label) + 20

            bg = ET.SubElement(g, "g", {"opacity": "0"})

            ET.SubElement(bg, "rect", {
                "x": str(bx), "y": str(by),
                "width": f"{bw:.0f}", "height": str(NODE_H),
                "rx": "14", "fill": bg_c,
                "stroke": color, "stroke-width": "1",
            })
            bt = ET.SubElement(bg, "text", {
                "x": str(bx + 10),
                "y": str(by + NODE_H // 2 + FONT_SIZE * 0.35),
                "font-family": FONT,
                "font-size": str(FONT_SIZE - 1),
                "fill": color,
            })
            bt.text = step.label

            ET.SubElement(bg, "animate", {
                "attributeName": "opacity",
                "from": "0", "to": "1",
                "begin": mb(badge_t), "dur": "0.2s",
                "fill": "freeze",
            })
            ET.SubElement(bg, "animate", {
                "attributeName": "opacity",
                "from": "1", "to": "0",
                "begin": mb(off_time), "dur": "0.3s",
                "fill": "freeze",
            })
            ET.SubElement(bg, "set", {
                "attributeName": "opacity",
                "to": "0",
                "begin": mb(0),
            })

    # -- fade overlay ------------------------------------------------------

    def _add_fade_overlay(self, parent: ET.Element, theme: Theme):
        reset_t = self.total_dur - FADE_RESET
        overlay = ET.SubElement(parent, "rect", {
            "width": str(WIDTH), "height": str(HEIGHT),
            "fill": theme.bg, "opacity": "0",
            "pointer-events": "none",
        })
        ET.SubElement(overlay, "animate", {
            "attributeName": "opacity",
            "values": "0;1;1;0",
            "keyTimes": "0;0.45;0.55;1",
            "begin": mb(reset_t), "dur": f"{FADE_RESET:.2f}s",
            "fill": "freeze",
        })
        ET.SubElement(overlay, "set", {
            "attributeName": "opacity",
            "to": "0",
            "begin": mb(0),
        })


def main():
    builder = SvgBuilder()
    print(builder.build())


if __name__ == "__main__":
    main()
