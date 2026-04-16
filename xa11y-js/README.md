# @xa11y/xa11y

[![npm](https://img.shields.io/npm/v/@xa11y/xa11y)](https://www.npmjs.com/package/@xa11y/xa11y)
[![CI](https://github.com/xa11y/xa11y/actions/workflows/ci.yml/badge.svg)](https://github.com/xa11y/xa11y/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/xa11y/xa11y/blob/main/LICENSE)
[![Docs](https://img.shields.io/badge/docs-xa11y.dev-blueviolet)](https://xa11y.dev)

Cross-platform accessibility library for Node.js. One API for macOS, Windows,
and Linux — built on top of the Rust [`xa11y`](https://github.com/xa11y/xa11y)
crate, exposed via [napi-rs](https://napi.rs).

**Use cases:** UI testing, AI agent tooling, desktop automation, accessibility
auditing.

**[Documentation](https://xa11y.dev)** ·
**[JavaScript API](https://xa11y.dev/api/javascript/)** ·
**[Rust API](https://docs.rs/xa11y)** ·
**[Python API](https://xa11y.dev/api/python/)**

## Quick example

```js
import { App } from '@xa11y/xa11y';

const safari = await App.byName('Safari');

// Find elements with CSS-like selectors via locator
for (const button of await safari.locator('button').elements()) {
  console.log(button.name);
}

// Interact with elements (re-resolves on every call)
await safari.locator('button[name="Submit"]').press();
await safari.locator('text_field[name^="Search"]').setValue('hello world');
```

## Installation

```bash
npm install @xa11y/xa11y
```

Requires Node.js 18 or newer. Pre-built native binaries are published for
Linux (x64/arm64), macOS (x64/arm64), and Windows (x64/arm64).

> **macOS:** Grant your terminal (or the process running Node) two permissions
> in **System Settings → Privacy & Security**: **Accessibility** *and* **Screen
> Recording**. The first lets xa11y read the AX tree; the second is needed for
> some apps that expose their tree only when the screen-recording prompt has
> been answered.

## Async by default

Every operation that touches the accessibility tree is asynchronous and runs
on a worker thread, so the Node event loop is never blocked. Property getters
on `Element` snapshots are synchronous because the data is captured up front.

```js
const app = await App.byName('com.example.MyApp');
const submit = await app.locator('button[name="Submit"]').element();

console.log(submit.role);   // sync — already captured
console.log(submit.enabled); // sync
await submit.children();    // async — re-queries the provider
```

## Locators auto-wait

Action methods on `Locator` poll the tree until the element is **visible** and
**enabled**, up to a 5-second budget, before performing the action. This makes
tests resilient to UI populating asynchronously.

```js
// Waits until the dialog appears AND its OK button is enabled, then clicks it.
await app.locator('dialog[name="Save Changes?"] button[name="OK"]').press();
```

## Subscribing to events

Subscriptions are async iterables, so you can use `for await` to consume
events. Calling `close()` (or breaking out of the iterator) tears down the
underlying receiver.

```js
const sub = await app.subscribe();
for await (const event of sub) {
  if (event.eventType === 'focusChanged') {
    console.log('focus moved to', event.target?.name);
    if (event.target?.role === 'window') break;
  }
}
```

## Errors

All operations throw subclasses of `XA11yError`:

| Class | When |
| --- | --- |
| `PermissionDeniedError` | Accessibility permissions not granted (macOS) |
| `SelectorNotMatchedError` | `element()` / actions found nothing |
| `ActionNotSupportedError` | The element doesn't support the action |
| `TimeoutError` | A `wait*` or auto-wait budget expired |
| `InvalidSelectorError` | Bad selector or action data |
| `PlatformError` | Other OS-level failure |

Catch a specific subclass and let the rest propagate:

```js
import { App, SelectorNotMatchedError } from '@xa11y/xa11y';

try {
  await app.locator('button[name="Submit"]').press();
} catch (err) {
  if (err instanceof SelectorNotMatchedError) {
    /* expected miss */
  } else {
    throw err;
  }
}
```

## License

MIT — see [LICENSE](https://github.com/xa11y/xa11y/blob/main/LICENSE).
