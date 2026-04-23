/**
 * xa11y -- Cross-platform accessibility client library for Node.js.
 *
 * @example
 * ```ts
 * import { App } from '@crowecawcaw/xa11y';
 *
 * const app = await App.byName('Safari');
 * await app.locator('button[name="OK"]').press();
 *
 * // Subscribe to events (Playwright-style)
 * const sub = await app.subscribe();
 * sub.on('focusChanged', (ev) => console.log(ev.target?.name));
 * sub.close();
 * ```
 *
 * @packageDocumentation
 */

import { EventEmitter } from 'node:events';

// Re-export Rust-generated classes (except App and _NativeSubscription,
// which are shadowed by the JS wrapper classes below).
export { Element, Event, Locator, locator, _makeTestLocator } from './native.js';

// Forward the narrowed types from native.d.ts.
export type { CheckedState, EventTypeName, Rect } from './native.js';
import type { Element, Event, EventTypeName, Locator, Rect } from './native.js';

// ── Options ───────────────────────────────────────────────────────────────

export interface SubscribeOptions {
  /** Abort signal; closes the subscription when aborted. */
  signal?: AbortSignal;
}

export interface AppLookupOptions {
  /**
   * Poll the accessibility API until the app appears, up to this many
   * milliseconds. Useful when the app may not yet be registered (e.g.
   * just-launched). Only "not found" errors trigger a retry; permission
   * errors and the like fail fast. Defaults to no polling (single attempt).
   */
  timeout?: number;
}

export interface WaitForEventOptions {
  /** Only resolve for events matching this predicate. */
  predicate?: (event: Event) => boolean;
  /** Timeout in milliseconds. Default: 5000. Rejects with `TimeoutError`. */
  timeout?: number;
  /** Abort signal for cancellation. Rejects with `AbortError`. */
  signal?: AbortSignal;
}

export interface WaitUntilOptions {
  /** Timeout in milliseconds. Default: 5000. Rejects with `TimeoutError`. */
  timeout?: number;
  /** Abort signal for cancellation. Rejects with `AbortError`. */
  signal?: AbortSignal;
}

// Add `waitUntil` to the napi-generated `Locator` class via interface merging.
// The method is attached to `native.Locator.prototype` in index.js.
declare module './native.js' {
  interface Locator {
    /**
     * Poll until `predicate` returns true, or the timeout elapses.
     *
     * The predicate is passed the first matching element, or `undefined` if
     * none match — this lets callers wait for either appearance or
     * detachment.
     */
    waitUntil(
      predicate: (element: Element | undefined) => boolean | Promise<boolean>,
      opts?: WaitUntilOptions,
    ): Promise<void>;
  }
}

// ── Subscription (EventEmitter) ───────────────────────────────────────────

/**
 * EventEmitter-based subscription for accessibility events.
 *
 * Events are emitted under their specific type name (`'focusChanged'`,
 * `'valueChanged'`, ...) and the catch-all `'event'` name.
 */
export class Subscription extends EventEmitter {
  /** Whether the subscription has been closed. */
  readonly closed: boolean;

  /** Stop event delivery and release the underlying platform subscription. */
  close(): void;

  // Typed listener overloads.
  on(type: EventTypeName, listener: (event: Event) => void): this;
  on(type: 'event', listener: (event: Event) => void): this;
  once(type: EventTypeName, listener: (event: Event) => void): this;
  once(type: 'event', listener: (event: Event) => void): this;
  off(type: EventTypeName | 'event', listener: (event: Event) => void): this;

  /**
   * Wait for a single event matching `type` and optional predicate.
   *
   * @example
   * ```ts
   * const ev = await sub.waitForEvent('focusChanged', {
   *   predicate: (e) => e.target?.role === 'button',
   *   timeout: 3000,
   * });
   * ```
   */
  waitForEvent(
    type: EventTypeName | 'event',
    opts?: WaitForEventOptions,
  ): Promise<Event>;

  /**
   * Wait for a single event matching `predicate`, regardless of type.
   * Convenience wrapper over `waitForEvent('event', { predicate, ...opts })`.
   */
  waitFor(
    predicate: (event: Event) => boolean,
    opts?: Omit<WaitForEventOptions, 'predicate'>,
  ): Promise<Event>;
}

// ── App (full declaration with subscribe + waitForEvent) ──────────────────
//
// The native App class is generated from Rust (native.d.ts). The JS wrapper
// in index.js extends it and overrides `subscribe()` to return the
// EventEmitter-based Subscription. We declare the full public type here
// rather than re-exporting the native one, so the return type of
// `subscribe()` and the extra `waitForEvent()` method are correctly typed.

export declare class App {
  /** Find an application by exact name. */
  static byName(name: string, options?: AppLookupOptions): Promise<App>;
  /** Find an application by process ID. */
  static byPid(pid: number, options?: AppLookupOptions): Promise<App>;
  /** List all running applications with an accessibility tree. */
  static list(): Promise<App[]>;

  get name(): string;
  get pid(): number | null;

  /** Create a `Locator` scoped to this application's accessibility tree. */
  locator(selector: string): Locator;
  /** Get direct children (typically windows) of this application. */
  children(): Promise<Element[]>;

  /**
   * Subscribe to accessibility events from this application.
   *
   * Returns an `EventEmitter`-based `Subscription`. Use `.on()` /
   * `.once()` to attach handlers and `.close()` to stop delivery.
   *
   * @example
   * ```ts
   * const sub = await app.subscribe({ signal: ctrl.signal });
   * sub.on('focusChanged', (ev) => console.log(ev.target?.name));
   * ```
   */
  subscribe(opts?: SubscribeOptions): Promise<Subscription>;

  /**
   * Wait for a single accessibility event from this application.
   *
   * Creates a temporary subscription, waits for a matching event, then
   * closes it. For multiple waits, use `.subscribe()` directly.
   *
   * @example
   * ```ts
   * const [opened] = await Promise.all([
   *   app.waitForEvent('windowOpened'),
   *   app.locator('button[name="Settings"]').press(),
   * ]);
   * ```
   */
  waitForEvent(
    type: EventTypeName | 'event',
    opts?: WaitForEventOptions,
  ): Promise<Event>;
}

// ── JS-only: typed error hierarchy ─────────────────────────────────────────

/** Base class for all xa11y errors. */
export class XA11yError extends Error {}

/** Accessibility permissions have not been granted. */
export class PermissionDeniedError extends XA11yError {}

/**
 * The target app advertises an accessibility tree but it is empty.
 *
 * Raised on Linux when a Chromium/Electron app is launched without
 * `--force-renderer-accessibility` (or the `ACCESSIBILITY_ENABLED=1`
 * environment variable), so the renderer accessibility bridge never
 * populates the window's subtree.
 */
export class AccessibilityNotEnabledError extends XA11yError {}

/** No element matched the selector (also used for stale elements). */
export class SelectorNotMatchedError extends XA11yError {}

/** The requested action is not supported on the target element. */
export class ActionNotSupportedError extends XA11yError {}

/** An operation exceeded its timeout. */
export class TimeoutError extends XA11yError {}

/** The selector string has invalid syntax. */
export class InvalidSelectorError extends XA11yError {}

/** The data passed to an action method was rejected (e.g. out-of-range slider value). */
export class InvalidActionDataError extends XA11yError {}

/** An OS-level accessibility error occurred. */
export class PlatformError extends XA11yError {}
