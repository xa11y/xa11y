// End-to-end xa11y example: drive the AccessKit test app from launch to teardown.
//
// This script is a complete, copy-pasteable starting point for writing your
// first xa11y test in JavaScript. It targets the AccessKit test app shipped
// with this repo (`test-apps/accesskit`) so it runs identically on Linux,
// macOS, and Windows.
//
// What it demonstrates:
//   * Launching a test app and polling the accessibility API until the OS
//     registers it (`App.byPid` with a `timeout`).
//   * Dumping the tree (`App.dump`) to discover the role and name of every
//     element before writing selectors.
//   * The `Locator` pattern with auto-waiting actions (`press`, `setValue`).
//   * Wait helpers: `waitVisible` (seconds) and `waitUntil` (milliseconds).
//   * Reading element fields (`role`, `name`, `actions`, `checked`).
//   * Subscribing to events with `app.subscribe()` and the
//     `Subscription.waitFor` helper.
//   * Tearing the subprocess down cleanly.
//
// Run from the repo root, after building the test app and JS bindings:
//
//   cargo build -p xa11y-test-app
//   (cd xa11y-js && npm install && npm run build:debug)
//   node examples/js/end_to_end.mjs

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..', '..');

const isWindows = process.platform === 'win32';
const BINARY = resolve(
  REPO_ROOT,
  'target',
  'debug',
  isWindows ? 'xa11y-test-app.exe' : 'xa11y-test-app',
);

// Import the locally-built JS bindings without forcing a global install.
const xa11yUrl = pathToFileURL(resolve(REPO_ROOT, 'xa11y-js', 'index.js')).href;
const xa11y = await import(xa11yUrl);
const { App, SelectorNotMatchedError, PlatformError, TimeoutError } = xa11y;

const STARTUP_TIMEOUT_MS = 30_000;

async function waitForRegistration(pid) {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  let lastErr;
  while (Date.now() < deadline) {
    try {
      return await App.byPid(pid, { timeout: 1000 });
    } catch (err) {
      if (err instanceof SelectorNotMatchedError || err instanceof PlatformError) {
        lastErr = err;
        await sleep(200);
        continue;
      }
      throw err;
    }
  }
  throw new Error(`Test app (pid=${pid}) did not register within ${STARTUP_TIMEOUT_MS}ms: ${lastErr}`);
}

async function main() {
  if (!existsSync(BINARY)) {
    console.error(`Build the test app first: cargo build -p xa11y-test-app (looked at ${BINARY})`);
    process.exit(1);
  }

  // 1. Launch the test app. The example owns its subprocess lifecycle so a CI
  //    run never leaks processes between attempts.
  const proc = spawn(BINARY, [], { stdio: 'ignore' });
  try {
    // 2. Poll the accessibility API until the OS registers the new process.
    const app = await waitForRegistration(proc.pid);
    console.log(`App registered: ${app.name} (pid=${app.pid})`);

    // 3. Dump the tree once to discover the role/name of every element. Copy
    //    selectors out of this output instead of guessing.
    console.log('\n--- Tree (depth 4) ---');
    console.log(await app.dump(4));

    // 4. Locators auto-wait and re-resolve on every call, so they stay correct
    //    even if the UI mutates between operations.
    const submit = app.locator('button[name="Submit"]');
    await submit.waitVisible(5); // timeout in seconds for native wait helpers

    // 5. Read element fields via .element().
    const button = await submit.element();
    assert.equal(button.role, 'button');
    assert.equal(button.enabled, true);
    assert.ok(button.actions.includes('press'));

    // 6. Press the primary button.
    await submit.press();

    // 7. Drive a text input. `waitUntil` polls until the predicate is true —
    //    preferable to a fixed sleep. Timeout is milliseconds, mirroring
    //    other Node APIs.
    const nameField = app.locator('text_field[name="Name"]');
    await nameField.setValue('Ada Lovelace');
    try {
      await nameField.waitUntil((el) => el !== undefined && el.value === 'Ada Lovelace', {
        timeout: 2000,
      });
    } catch (err) {
      if (!(err instanceof TimeoutError)) throw err;
      // Some Linux AT-SPI adapters don't echo set_value back through the
      // tree; the call still went through. Print so the example is
      // transparent rather than silently passing.
      console.log('note: text value not echoed back via accessibility (adapter quirk)');
    }

    // 8. Toggle the checkbox via the `press` semantic verb and confirm the
    //    new state with `waitUntil`.
    const checkbox = app.locator('check_box[name="I agree to terms"]');
    const before = (await checkbox.element()).checked;
    await checkbox.press();
    await checkbox.waitUntil((el) => el !== undefined && el.checked !== before, {
      timeout: 2000,
    });
    const after = (await checkbox.element()).checked;
    console.log(`checkbox toggled: ${JSON.stringify(before)} -> ${JSON.stringify(after)}`);

    // 9. Iterate matching elements with `.elements()`.
    const buttons = await app.locator('button').elements();
    console.log(`discovered ${buttons.length} buttons total`);
    assert.ok(buttons.length >= 2);

    // 10. Subscribe to events and wait for one matching condition. The
    //     Subscription is an EventEmitter, and `waitFor` is the convenience
    //     wrapper for one-shot waits.
    const sub = await app.subscribe();
    try {
      const evPromise = sub.waitFor(
        (e) => e.target !== undefined && e.target.name === 'Submit',
        { timeout: 3000 },
      );
      await submit.press();
      const event = await evPromise;
      console.log(`observed event: ${event.eventType} on ${JSON.stringify(event.target.name)}`);
    } finally {
      sub.close();
    }

    console.log('\nOK — example completed successfully.');
  } finally {
    proc.kill('SIGTERM');
    await new Promise((r) => proc.once('exit', r));
  }
}

await main();
