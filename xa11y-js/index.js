// @ts-check
/**
 * Public entry point for the xa11y Node.js bindings.
 *
 * This file re-exports the raw napi-rs bindings from `./native.js` with two
 * sugar layers on top:
 *
 *   1. Typed error subclasses — the napi `Error` thrown from Rust carries a
 *      `XA11Y_*` tag in its `message`. We catch, split, and re-throw as a
 *      `XA11yError` subclass so consumers can do `instanceof` checks.
 *
 *   2. Subscription async iteration — `for await (const ev of sub)` works by
 *      wrapping the `recv()` polling loop.
 *
 * The Rust-facing API is considered unstable. Always import from this file.
 */

'use strict';

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

class PlatformError extends XA11yError {
  constructor(message) {
    super(message);
    this.name = 'PlatformError';
  }
}

const CODE_TO_CLASS = {
  XA11Y_PERMISSION_DENIED: PermissionDeniedError,
  XA11Y_SELECTOR_NOT_MATCHED: SelectorNotMatchedError,
  XA11Y_ELEMENT_STALE: SelectorNotMatchedError,
  XA11Y_ACTION_NOT_SUPPORTED: ActionNotSupportedError,
  XA11Y_TEXT_VALUE_NOT_SUPPORTED: ActionNotSupportedError,
  XA11Y_TIMEOUT: TimeoutError,
  XA11Y_INVALID_SELECTOR: InvalidSelectorError,
  XA11Y_INVALID_ACTION_DATA: InvalidSelectorError,
  XA11Y_PLATFORM: PlatformError,
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
//
// napi-rs emits classes whose methods throw raw `Error` objects. For instance
// methods (on `.prototype`) the property descriptors are configurable, so we
// rewrite them in place with a typed-error wrapper. Static methods on the
// class itself are non-configurable, so we model `App` as a subclass that
// overrides its three static factories and proxies `instanceof` back to the
// native class via `Symbol.hasInstance`.

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
patchPrototypeMethods(native.Subscription);
patchPrototypeMethods(native.Event);

/**
 * User-facing `App` class. Extends the native class so properties and
 * instance methods behave identically, but overrides the three async static
 * factories to rethrow typed errors. `Symbol.hasInstance` is overridden so
 * `instance instanceof App` also returns true for napi-constructed instances.
 */
class App extends native.App {
  static [Symbol.hasInstance](instance) {
    return instance instanceof native.App;
  }

  static async byName(name) {
    try {
      return await native.App.byName(name);
    } catch (err) {
      throw toTypedError(err);
    }
  }

  static async byPid(pid) {
    try {
      return await native.App.byPid(pid);
    } catch (err) {
      throw toTypedError(err);
    }
  }

  static async list() {
    try {
      return await native.App.list();
    } catch (err) {
      throw toTypedError(err);
    }
  }
}

// ── Subscription sugar ──────────────────────────────────────────────────────
//
// Add a Symbol.asyncIterator so users can `for await (const ev of sub)`.
// The iterator loops with short recv timeouts so the AbortSignal (or an
// external close()) can break it.

if (native.Subscription && native.Subscription.prototype) {
  native.Subscription.prototype[Symbol.asyncIterator] = function events() {
    const sub = this;
    return {
      next: async () => {
        if (!sub.active) return { value: undefined, done: true };
        while (sub.active) {
          try {
            const ev = await sub.recv(0.1);
            return { value: ev, done: false };
          } catch (err) {
            if (err instanceof TimeoutError) continue;
            if (err instanceof PlatformError && /closed/i.test(err.message)) {
              return { value: undefined, done: true };
            }
            throw err;
          }
        }
        return { value: undefined, done: true };
      },
      return: async () => {
        sub.close();
        return { value: undefined, done: true };
      },
    };
  };
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

// ── Event type constants (namespace object) ─────────────────────────────────

const EventType = Object.freeze({
  FocusChanged: 'focusChanged',
  ValueChanged: 'valueChanged',
  NameChanged: 'nameChanged',
  StateChanged: 'stateChanged',
  StructureChanged: 'structureChanged',
  WindowOpened: 'windowOpened',
  WindowClosed: 'windowClosed',
  WindowActivated: 'windowActivated',
  WindowDeactivated: 'windowDeactivated',
  SelectionChanged: 'selectionChanged',
  MenuOpened: 'menuOpened',
  MenuClosed: 'menuClosed',
  Alert: 'alert',
  TextChanged: 'textChanged',
});

// ── Re-exports ──────────────────────────────────────────────────────────────

module.exports = {
  App,
  Element: native.Element,
  Event: native.Event,
  EventType,
  Locator: native.Locator,
  Subscription: native.Subscription,
  locator,

  // Error classes
  XA11yError,
  PermissionDeniedError,
  SelectorNotMatchedError,
  ActionNotSupportedError,
  TimeoutError,
  InvalidSelectorError,
  PlatformError,

  // @internal — used by unit tests
  _makeTestLocator: native._makeTestLocator,
};
