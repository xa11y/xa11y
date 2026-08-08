# xa11y

[![Crates.io](https://img.shields.io/crates/v/xa11y)](https://crates.io/crates/xa11y)
[![PyPI](https://img.shields.io/pypi/v/xa11y)](https://pypi.org/project/xa11y/)
[![npm](https://img.shields.io/npm/v/@crowecawcaw/xa11y)](https://www.npmjs.com/package/@crowecawcaw/xa11y)
[![CI](https://github.com/xa11y/xa11y/actions/workflows/ci.yml/badge.svg)](https://github.com/xa11y/xa11y/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-xa11y.dev-blueviolet)](https://xa11y.dev)

A Playwright-style library for driving native desktop apps on macOS, Windows, and Linux. Built for end-to-end tests, computer-use agents, and assistive tools.

**Use cases:** end-to-end desktop testing, computer-use agents, MCP tools, assistive technology.

**[Documentation](https://xa11y.dev)** | **[Rust API](https://docs.rs/xa11y)** | **[Python API](https://xa11y.dev/api/python/)** | **[JavaScript API](https://xa11y.dev/api/javascript/)**

## Quick Example

<!-- rust-only-hidden
```rust
use xa11y::*;
use std::time::Duration;

fn main() -> Result<()> {
    // `by_name` polls for up to the given timeout. Useful when the app
    // may not yet be registered with the a11y API. Pass `Duration::ZERO`
    // for a single attempt with no waiting.
    let safari = App::by_name("Safari", Duration::from_secs(5))?;

    // Find elements with CSS-like selectors
    let buttons = safari.locator("button[name='Submit']").elements()?;
    println!("Found {} buttons", buttons.len());

    // Interact with elements via locator (re-resolves every call)
    safari.locator("button[name='Submit']").press()?;

    Ok(())
}
```
-->

<!-- python-only -->
```python
import xa11y

safari = xa11y.App.by_name("Safari")

# Find elements with CSS-like selectors via locator
for button in safari.locator("button").elements():
    print(button.name)

# Interact with elements via locator (re-resolves every call)
safari.locator("button[name='Submit']").press()

safari.locator("text_field[name^='Search']").set_value("hello world")
```
<!-- /python-only -->

## Installation

<!-- rust-only -->
**Rust**

```bash
cargo add xa11y
```
<!-- /rust-only -->

<!-- python-only -->
**Python**

```bash
pip install xa11y
```

Requires Python 3.9+. Pre-built wheels available for Linux, macOS, and Windows.

For pytest suites, [`pytest-xa11y`](https://xa11y.dev/reference/pytest/) adds
fixtures that launch the app under test, capability markers that skip what the
machine cannot do, and the accessibility tree on every failure:

```bash
pip install pytest-xa11y
```
<!-- /python-only -->

<!-- js-only -->
**JavaScript**

```bash
npm install @crowecawcaw/xa11y
```

Requires Node.js 18+. Pre-built native binaries available for Linux, macOS, and Windows.
<!-- /js-only -->

> On **macOS**, grant your terminal **two** permissions in **System Settings > Privacy & Security**:
> 1. **Accessibility**, required for all accessibility API access.
> 2. **Screen & System Audio Recording** (macOS 26+), required to read window content. Without it, only menu bars are visible.
>
> Restart your terminal after changing permissions.
>
> On **Linux**, AT-SPI2 must be running (the default on GNOME and most desktop environments). Nothing else to grant.
>
> **Windows** works as installed.

## Selector Syntax

Query accessibility trees with CSS-like selectors:

| Pattern | Meaning |
| --- | --- |
| `button` | Elements with role Button |
| `button[name='OK']` | Button named exactly "OK" |
| `textfield[name^='Search']` | Text field whose name starts with "Search" |
| `textfield[name*='email']` | Text field whose name contains "email" |
| `group > button` | Buttons that are direct children of a group |
| `window button` | Buttons anywhere inside a window |
| `button:nth(2)` | The 2nd button match |

## Supported Actions

| Action | Description |
| --- | --- |
| `press` | Click / activate |
| `focus` / `blur` | Move or remove keyboard focus |
| `toggle` | Toggle a checkbox or switch |
| `expand` / `collapse` | Expand or collapse a disclosure |
| `select` | Select an item |
| `set_value` | Set a text field's value |
| `type_text` | Type text into an element |
| `increment` / `decrement` | Adjust a slider or stepper |
| `show_menu` | Open a context menu |

## Platform Support

| Platform | Backend |
| --- | --- |
| macOS | AXUIElement |
| Linux | AT-SPI2 (D-Bus) |
| Windows | UI Automation |

## Contributing

```bash
git clone https://github.com/xa11y/xa11y && cd xa11y
cargo build --workspace
cargo xtask check   # fmt, lint, test, python bindings
```

See the [development docs](https://xa11y.dev/explanation/design/) for architecture and setup.

## License

MIT. All dependencies are permissively licensed (MIT, Apache-2.0, BSD, or similar), enforced via `cargo-deny`.
