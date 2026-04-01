# xa11y

[![Crates.io](https://img.shields.io/crates/v/xa11y)](https://crates.io/crates/xa11y)
[![PyPI](https://img.shields.io/pypi/v/xa11y)](https://pypi.org/project/xa11y/)
[![CI](https://github.com/xa11y/xa11y/actions/workflows/ci.yml/badge.svg)](https://github.com/xa11y/xa11y/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-xa11y.dev-blueviolet)](https://xa11y.dev)

Cross-platform accessibility library for reading and interacting with accessibility trees. One API for macOS, Windows, and Linux.

**Use cases:** UI testing, AI agent tooling, assistive technology, desktop automation.

**[Documentation](https://xa11y.dev)** | **[Rust API](https://docs.rs/xa11y)** | **[Python API](https://xa11y.dev/api/python/)**

## Quick Example

<!-- rust-only -->
```rust
use xa11y::*;

fn main() -> Result<()> {
    let safari = App::by_name("Safari")?;

    // Find elements with CSS-like selectors
    let buttons = safari.locator("button[name='Submit']").elements()?;
    println!("Found {} buttons", buttons.len());

    // Interact with elements via locator (re-resolves every call)
    safari.locator("button[name='Submit']").press()?;

    Ok(())
}
```
<!-- /rust-only -->

<!-- python-only -->
```python
import xa11y

# Find elements with CSS-like selectors via locator
app = xa11y.locator('application[name="Safari"]')
for button in app.descendant("button").elements():
    print(button.name)

# Interact with elements via locator (re-resolves every call)
app.descendant("button[name='Submit']").press()

app.descendant("text_field[name^='Search']").set_value("hello world")
```
<!-- /python-only -->

## Installation

<!-- rust-only -->
```toml
[dependencies]
xa11y = "0.4"
```
<!-- /rust-only -->

<!-- python-only -->
```bash
pip install xa11y
```

Requires Python 3.9+. Pre-built wheels available for Linux, macOS, and Windows.
<!-- /python-only -->

> **macOS:** Grant your terminal **two** permissions in **System Settings > Privacy & Security**:
> 1. **Accessibility** — required for all accessibility API access.
> 2. **Screen & System Audio Recording** (macOS 26+) — required to read window content. Without this, only menu bars are visible.
>
> Restart your terminal after changing permissions.
>
> **Linux:** AT-SPI2 must be running (default on GNOME/most DEs). No special permissions needed.
>
> **Windows:** No special permissions needed.

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
| `scroll` | Scroll in a direction |
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

See the [development docs](https://xa11y.dev/guides/overview/) for architecture and setup.

## License

MIT. All dependencies are permissively licensed (MIT, Apache-2.0, BSD, or similar), enforced via `cargo-deny`.
