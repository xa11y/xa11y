// @ts-check
/**
 * Public entry point for the xa11y Node.js bindings.
 *
 * This file re-exports the raw napi-rs bindings from `./native.js` with three
 * sugar layers on top:
 *
 *   1. Typed error subclasses -- the napi `Error` thrown from Rust carries a
 *      `XA11Y_*` tag in its `message`. We catch, split, and re-throw as a
 *      `XA11yError` subclass so consumers can do `instanceof` checks.
 *
 *   2. Subscription as EventEmitter -- `Subscription` extends `EventEmitter`.
 *      The native `_NativeSubscription` is internal; the public class emits
 *      typed events (`'focusChanged'`, `'valueChanged'`, ...) and supports
 *      `close()` / `signal` for cancellation.
 *
 *   3. `waitForEvent()` on App and Subscription -- a Playwright-style one-shot
 *      wait with optional predicate and timeout.
 *
 * The Rust-facing API is considered unstable. Always import from this file.
 */

'use strict';

const { EventEmitter } = require('node:events');
const native = require('./native.js');

// ── Error types ────────────────────────────────────────────────────────────

/** Base class for all xa11y-thrown errors. */
class XA11yError extends Error {
  constructor(message) {
    super(message);
    this.name = 'XA11yError';
  }
}

class PermissionDeniedError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'PermissionDeniedError';
  }
}

class AccessibilityNotEnabledError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'AccessibilityNotEnabledError';
  }
}

class SelectorNotMatchedError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'SelectorNotMatchedError';
  }
}

class ActionNotSupportedError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'ActionNotSupportedError';
  }
}

class TimeoutError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'TimeoutError';
  }
}

class InvalidSelectorError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'InvalidSelectorError';
  }
}

class InvalidActionDataError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'InvalidActionDataError';
  }
}

class PlatformError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'PlatformError';
  }
}

const CODE_TO_CLASS = {
  XA11Y_PERMISSION_DENIED: PermissionDeniedError,
  XA11Y_ACCESSIBILITY_NOT_ENABLED: AccessibilityNotEnabledError,
  XA11Y_SELECTOR_NOT_MATCHED: SelectorNotMatchedError,
  XA11Y_ELEMENT_STALE: SelectorNotMatchedError,
  XA11Y_ACTION_NOT_SUPPORTED: ActionNotSupportedError,
  XA11Y_TEXT_VALUE_NOT_SUPPORTED: ActionNotSupportedError,
  XA11Y_TIMEOUT: TimeoutError,
  XA11Y_INVALID_SELECTOR: InvalidSelectorError,
  XA11Y_INVALID_ACTION_DATA: InvalidActionDataError,
  XA11Y_PLATFORM: PlatformError,
  XA11Y_NO_ELEMENT_BOUNDS: InvalidActionDataError,
  XA11Y_UNSUPPORTED: ActionNotSupportedError,
};

/** Convert any thrown value into a typed xa11y error if it carries our tag. */
function toTypedError(err) {
  if (!(err instanceof Error) || typeof err.message !== 'string') return err;
  const colon = err.message.indexOf(':');
  if (colon < 0) return err;
  const tag = err.message.slice(0, colon);
  const Cls = CODE_TO_CLASS[tag];
  if (!Cls) return err;
  const detail = err.message.slice(colon + 2);
  const typed = new Cls(detail);
  typed.stack = err.stack;
  return typed;
}

/** Wrap a function so any thrown (or rejected) napi error becomes typed. */
function wrap(fn) {
  return function wrapped(...args) {
    try {
      const result = fn.apply(this, args);
      if (result && typeof result.then === 'function') {
        return result.then(
          (v) => v,
          (err) => {
            throw toTypedError(err);
          },
        );
      }
      return result;
    } catch (err) {
      throw toTypedError(err);
    }
  };
}

// ── Patch native classes ────────────────────────────────────────────────────

function patchPrototypeMethods(cls) {
  if (!cls || !cls.prototype) return;
  for (const key of Object.getOwnPropertyNames(cls.prototype)) {
    if (key === 'constructor') continue;
    const desc = Object.getOwnPropertyDescriptor(cls.prototype, key);
    if (!desc || !desc.configurable || typeof desc.value !== 'function') continue;
    Object.defineProperty(cls.prototype, key, { ...desc, value: wrap(desc.value) });
  }
}

patchPrototypeMethods(native.App);
patchPrototypeMethods(native.Element);
patchPrototypeMethods(native.Locator);
patchPrototypeMethods(native.Event);
patchPrototypeMethods(native.InputSim);
patchPrototypeMethods(native.Screenshot);

// ── Locator.waitUntil (JS-side polling loop) ───────────────────────────────
//
// Implemented on top of the existing `elements()` call rather than surfaced
// through napi, mirroring the Rust `Locator::wait_until` internal loop.
// The 50ms poll interval matches the Python equivalent in
// `xa11y-python/src/lib.rs::Locator::wait_until`.
Object.defineProperty(native.Locator.prototype, 'waitUntil', {
  configurable: true,
  writable: true,
  value: async function waitUntil(predicate, opts = {}) {
    const { timeout = 5000, signal } = opts;
    const deadline = Date.now() + timeout;

    if (signal && signal.aborted) {
      throw new DOMException('The operation was aborted', 'AbortError');
    }

    while (true) {
      // Predicate sees `undefined` when no element matches (mirror of
      // Python's `None`) so callers can wait for detachment, too.
      const elements = await this.elements();
      const el = elements[0];
      if (await predicate(el)) return;

      if (signal && signal.aborted) {
        throw new DOMException('The operation was aborted', 'AbortError');
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new TimeoutError(
          `Timeout after ${timeout}ms waiting for predicate on '${this.selector}'`,
        );
      }

      await new Promise((resolve, reject) => {
        const delay = Math.min(50, remaining);
        const timer = setTimeout(() => {
          if (signal) signal.removeEventListener('abort', onAbort);
          resolve();
        }, delay);
        const onAbort = () => {
          clearTimeout(timer);
          reject(new DOMException('The operation was aborted', 'AbortError'));
        };
        if (signal) signal.addEventListener('abort', onAbort, { once: true });
      });
    }
  },
});

// ── Subscription (EventEmitter wrapper) ────────────────────────────────────

/**
 * EventEmitter-based subscription for accessibility events.
 *
 * Events are emitted both under their specific type name (`'focusChanged'`,
 * `'valueChanged'`, ...) and the catch-all `'event'` name. Use standard
 * `.on()` / `.once()` / `.off()` to attach handlers.
 *
 * @example
 * ```js
 * const sub = await app.subscribe();
 * sub.on('focusChanged', (ev) => console.log(ev.target?.name));
 * sub.on('event', (ev) => metrics.record(ev.type));
 * // ...
 * sub.close();
 * ```
 */
class Subscription extends EventEmitter {
  /** @param {object} nativeSub - A _NativeSubscription instance from Rust */
  constructor(nativeSub) {
    super();
    this._native = nativeSub;
    this._closed = false;

    // Register the wake-up callback. Each time Rust pushes events, it calls
    // this function on the JS main thread. We drain and emit.
    this._native.start(() => {
      if (this._closed) return;
      const events = this._native.drain();
      for (const ev of events) {
        this.emit(ev.type, ev);
        this.emit('event', ev);
      }
    });
  }

  /** Whether the subscription has been closed. */
  get closed() {
    return this._closed;
  }

  /** Stop event delivery and release the underlying platform subscription. */
  close() {
    if (this._closed) return;
    this._closed = true;
    this._native.close();
    this.removeAllListeners();
  }

  /**
   * Wait for a single event matching `type` and optional `predicate`.
   *
   * @param {string} type - Event type name (e.g. `'focusChanged'`) or `'event'`
   * @param {object} [opts]
   * @param {(ev: object) => boolean} [opts.predicate] - Filter function
   * @param {number} [opts.timeout=5000] - Timeout in milliseconds
   * @param {AbortSignal} [opts.signal] - Abort signal for cancellation
   * @returns {Promise<object>} The matching event
   */
  waitForEvent(type, opts = {}) {
    const { predicate, timeout = 5000, signal } = opts;
    return new Promise((resolve, reject) => {
      if (this._closed) {
        reject(new PlatformError('Subscription is closed'));
        return;
      }

      let timer = null;

      const cleanup = () => {
        if (timer) clearTimeout(timer);
        this.off(type, onEvent);
        if (signal) signal.removeEventListener('abort', onAbort);
      };

      const onEvent = (ev) => {
        if (predicate && !predicate(ev)) return;
        cleanup();
        resolve(ev);
      };

      const onAbort = () => {
        cleanup();
        reject(new DOMException('The operation was aborted', 'AbortError'));
      };

      if (signal) {
        if (signal.aborted) {
          onAbort();
          return;
        }
        signal.addEventListener('abort', onAbort, { once: true });
      }

      if (timeout > 0) {
        timer = setTimeout(() => {
          cleanup();
          reject(new TimeoutError(`Timeout after ${timeout}ms waiting for '${type}'`));
        }, timeout);
      }

      this.on(type, onEvent);
    });
  }

  /**
   * Wait for a single event matching `predicate`, regardless of type.
   *
   * Convenience wrapper over `waitForEvent('event', { predicate, ...opts })`.
   *
   * @param {(ev: object) => boolean} predicate
   * @param {object} [opts]
   * @param {number} [opts.timeout=5000]
   * @param {AbortSignal} [opts.signal]
   * @returns {Promise<object>}
   */
  waitFor(predicate, opts = {}) {
    return this.waitForEvent('event', { ...opts, predicate });
  }
}

// ── App wrapper ────────────────────────────────────────────────────────────

/**
 * User-facing `App` class. Extends the native class so properties and
 * instance methods behave identically, but overrides the three async static
 * factories and `subscribe()` to produce the EventEmitter-based Subscription.
 */
class App extends native.App {
  static [Symbol.hasInstance](instance) {
    return instance instanceof native.App;
  }

  static async byName(name, options) {
    try {
      const a = await native.App.byName(name, options);
      Object.setPrototypeOf(a, App.prototype);
      return a;
    } catch (err) {
      throw toTypedError(err);
    }
  }

  static async byPid(pid, options) {
    try {
      const a = await native.App.byPid(pid, options);
      Object.setPrototypeOf(a, App.prototype);
      return a;
    } catch (err) {
      throw toTypedError(err);
    }
  }

  static async list() {
    try {
      const apps = await native.App.list();
      for (const a of apps) Object.setPrototypeOf(a, App.prototype);
      return apps;
    } catch (err) {
      throw toTypedError(err);
    }
  }

  /**
   * Subscribe to accessibility events from this application.
   *
   * @param {object} [opts]
   * @param {AbortSignal} [opts.signal] - Abort signal; closes the sub on abort
   * @returns {Promise<Subscription>}
   */
  async subscribe(opts = {}) {
    let nativeSub;
    try {
      nativeSub = await native.App.prototype.subscribe.call(this);
    } catch (err) {
      throw toTypedError(err);
    }
    const sub = new Subscription(nativeSub);

    if (opts.signal) {
      if (opts.signal.aborted) {
        sub.close();
        return sub;
      }
      const onAbort = () => sub.close();
      opts.signal.addEventListener('abort', onAbort, { once: true });
      // Clean up the abort listener when the sub is closed independently.
      const origClose = sub.close.bind(sub);
      sub.close = function () {
        opts.signal.removeEventListener('abort', onAbort);
        origClose();
      };
    }

    return sub;
  }

  /**
   * Wait for a single accessibility event from this application.
   *
   * Creates a temporary subscription, waits for a matching event, then
   * closes it. For multiple waits, use `.subscribe()` and call
   * `.waitForEvent()` on the subscription directly.
   *
   * @param {string} type - Event type name (e.g. `'focusChanged'`) or `'event'`
   * @param {object} [opts]
   * @param {(ev: object) => boolean} [opts.predicate]
   * @param {number} [opts.timeout=5000]
   * @param {AbortSignal} [opts.signal]
   * @returns {Promise<object>}
   */
  async waitForEvent(type, opts = {}) {
    const sub = await this.subscribe({ signal: opts.signal });
    try {
      return await sub.waitForEvent(type, opts);
    } finally {
      sub.close();
    }
  }
}

// ── Top-level locator ────────────────────────────────────────────────────────

/**
 * Create a top-level Locator searching from the system root.
 *
 * @param {string} selector
 * @returns {native.Locator}
 */
function locator(selector) {
  return wrap(native.locator)(selector);
}

/**
 * Construct an `InputSim` backed by the platform's native input path.
 * Errors are wrapped in typed `XA11yError` subclasses like every other
 * entry point.
 */
function inputSim() {
  return wrap(native.inputSim)();
}

/**
 * Capture pixels from the screen.
 *
 * With no arguments, captures the full primary display. Pass `element` to
 * capture the pixels under an element's current bounds, or `region` as
 * `{x, y, width, height}` to capture an explicit rectangle in logical
 * screen coordinates. Passing both throws `InvalidActionDataError`.
 *
 * @param {object} [options]
 * @param {import('./native.js').Element} [options.element]
 * @param {{x: number, y: number, width: number, height: number}} [options.region]
 * @returns {Promise<import('./native.js').Screenshot>}
 */
function screenshot(options) {
  if (options && options.element && options.region) {
    throw new InvalidActionDataError(
      'screenshot: pass either `element` or `region`, not both',
    );
  }
  if (options && options.element) {
    return wrap(native._screenshotElement)(options.element);
  }
  if (options && options.region) {
    return wrap(native._screenshotRegion)(options.region);
  }
  return wrap(native._screenshot)();
}

// ── Re-exports ──────────────────────────────────────────────────────────────

module.exports = {
  App,
  Element: native.Element,
  Event: native.Event,
  InputSim: native.InputSim,
  Locator: native.Locator,
  Screenshot: native.Screenshot,
  Subscription,
  inputSim,
  locator,
  screenshot,

  // Error classes
  XA11yError,
  PermissionDeniedError,
  AccessibilityNotEnabledError,
  SelectorNotMatchedError,
  ActionNotSupportedError,
  TimeoutError,
  InvalidSelectorError,
  InvalidActionDataError,
  PlatformError,

  // @internal -- used by unit tests
  _makeTestLocator: native._makeTestLocator,
  _makeTestActionProbe: native._makeTestActionProbe,
  _makeDisconnectedSubscription: native._makeDisconnectedSubscription,
  _Subscription: Subscription,
};
