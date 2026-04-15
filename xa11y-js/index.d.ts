/**
 * xa11y — Cross-platform accessibility client library for Node.js.
 *
 * @example
 * ```ts
 * import { App, locator } from '@xa11y/xa11y';
 *
 * const app = await App.byName('Safari');
 * await app.locator('button[name="OK"]').press();
 *
 * for (const btn of await app.locator('button').elements()) {
 *   console.log(btn.name);
 * }
 * ```
 *
 * @packageDocumentation
 */

// ── Errors ──────────────────────────────────────────────────────────────────

/** Base class for all xa11y errors. */
export class XA11yError extends Error {}

/** Accessibility permissions have not been granted. */
export class PermissionDeniedError extends XA11yError {}

/** No element matched the selector (also used for stale elements). */
export class SelectorNotMatchedError extends XA11yError {}

/** The requested action is not supported on the target element. */
export class ActionNotSupportedError extends XA11yError {}

/** An operation exceeded its timeout. */
export class TimeoutError extends XA11yError {}

/** The selector string has invalid syntax or the action data was rejected. */
export class InvalidSelectorError extends XA11yError {}

/** An OS-level accessibility error occurred. */
export class PlatformError extends XA11yError {}

// ── Data types ──────────────────────────────────────────────────────────────

/** A bounding rectangle in screen coordinates (pixels). */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Checked state of a toggleable element. */
export type CheckedState = 'on' | 'off' | 'mixed';

/** Accessibility event type names (camelCase). */
export type EventTypeName =
  | 'focusChanged'
  | 'valueChanged'
  | 'nameChanged'
  | 'stateChanged'
  | 'structureChanged'
  | 'windowOpened'
  | 'windowClosed'
  | 'windowActivated'
  | 'windowDeactivated'
  | 'selectionChanged'
  | 'menuOpened'
  | 'menuClosed'
  | 'alert'
  | 'textChanged';

/**
 * Enum-like object with the valid `EventType` name strings. Using this is
 * equivalent to writing the string literal, but gives autocomplete and avoids
 * typos.
 */
export const EventType: Readonly<Record<
  | 'FocusChanged'
  | 'ValueChanged'
  | 'NameChanged'
  | 'StateChanged'
  | 'StructureChanged'
  | 'WindowOpened'
  | 'WindowClosed'
  | 'WindowActivated'
  | 'WindowDeactivated'
  | 'SelectionChanged'
  | 'MenuOpened'
  | 'MenuClosed'
  | 'Alert'
  | 'TextChanged',
  EventTypeName
>>;

// ── Element ─────────────────────────────────────────────────────────────────

/**
 * A live element snapshot with lazy navigation back into the provider.
 *
 * Property getters are synchronous — the values were captured when the element
 * was resolved. Navigation methods (`children`, `parent`, `subscribe`) re-query
 * the platform on demand and return Promises.
 */
export class Element {
  /** Role, as a snake_case string (e.g. `"button"`, `"check_box"`). */
  readonly role: string;
  readonly name: string | null;
  readonly value: string | null;
  readonly description: string | null;
  readonly numericValue: number | null;
  readonly minValue: number | null;
  readonly maxValue: number | null;
  readonly stableId: string | null;
  readonly pid: number | null;
  readonly actions: string[];
  readonly bounds: Rect | null;
  readonly enabled: boolean;
  readonly visible: boolean;
  readonly focused: boolean;
  readonly checked: CheckedState | null;
  readonly selected: boolean;
  readonly expanded: boolean | null;
  readonly editable: boolean;
  readonly focusable: boolean;
  readonly modal: boolean;
  readonly required: boolean;
  readonly busy: boolean;

  /** Get direct children (each call re-queries the provider). */
  children(): Promise<Element[]>;
  /** Get the parent element, or `null` if this is the root. */
  parent(): Promise<Element | null>;
  /** Subscribe to accessibility events for this element. */
  subscribe(): Promise<Subscription>;
}

// ── Locator ─────────────────────────────────────────────────────────────────

/**
 * A resilient, Playwright-style element reference.
 *
 * Locators never hold a live UI element — they store a selector and re-resolve
 * it on each action, which makes them immune to tree staleness. Action methods
 * auto-wait up to a default 5s for the element to become visible and enabled.
 */
export class Locator {
  /** The CSS-like selector string. */
  readonly selector: string;

  /** Return a new Locator that selects the *n*-th match (1-based). */
  nth(n: number): Locator;
  /** Return a new Locator that selects the first match. */
  first(): Locator;
  /** Return a new Locator scoped to direct children matching `selector`. */
  child(selector: string): Locator;
  /** Return a new Locator scoped to descendants matching `selector`. */
  descendant(selector: string): Locator;

  /** Check whether a matching element exists (does *not* throw). */
  exists(): Promise<boolean>;
  /** Count matching elements. */
  count(): Promise<number>;
  /** Resolve to a single snapshot; throws `SelectorNotMatchedError` on miss. */
  element(): Promise<Element>;
  /** Resolve to all matching snapshots. */
  elements(): Promise<Element[]>;

  /** Click / invoke the matched element. */
  press(): Promise<void>;
  /** Set keyboard focus on the matched element. */
  focus(): Promise<void>;
  /** Remove keyboard focus from the matched element. */
  blur(): Promise<void>;
  /** Toggle the matched element (checkbox, switch). */
  toggle(): Promise<void>;
  /** Expand the matched element. */
  expand(): Promise<void>;
  /** Collapse the matched element. */
  collapse(): Promise<void>;
  /** Select the matched element (list item, tab, etc.). */
  select(): Promise<void>;
  /** Show the context menu for the matched element. */
  showMenu(): Promise<void>;
  /** Scroll the matched element into the visible area. */
  scrollIntoView(): Promise<void>;
  /** Increment the matched element (slider, spinner). */
  increment(): Promise<void>;
  /** Decrement the matched element (slider, spinner). */
  decrement(): Promise<void>;

  /** Set the text value of the matched element. */
  setValue(value: string): Promise<void>;
  /** Set the numeric value of the matched element. */
  setNumericValue(value: number): Promise<void>;
  /** Type text at the current cursor on the matched element. */
  typeText(text: string): Promise<void>;
  /** Select a text range within the matched element (0-based offsets). */
  selectText(start: number, end: number): Promise<void>;

  scrollUp(amount?: number): Promise<void>;
  scrollDown(amount?: number): Promise<void>;
  scrollLeft(amount?: number): Promise<void>;
  scrollRight(amount?: number): Promise<void>;

  /** Perform an action by its snake_case name. */
  performAction(action: string): Promise<void>;

  /** Wait until the element is visible. */
  waitVisible(timeoutSeconds?: number): Promise<Element>;
  /** Wait until the element exists in the tree. */
  waitAttached(timeoutSeconds?: number): Promise<Element>;
  /** Wait until the element is removed from the tree. */
  waitDetached(timeoutSeconds?: number): Promise<void>;
  /** Wait until the element is enabled. */
  waitEnabled(timeoutSeconds?: number): Promise<Element>;
  /** Wait until the element is hidden or removed. */
  waitHidden(timeoutSeconds?: number): Promise<void>;
  /** Wait until the element is disabled. */
  waitDisabled(timeoutSeconds?: number): Promise<Element>;
  /** Wait until the element has keyboard focus. */
  waitFocused(timeoutSeconds?: number): Promise<Element>;
  /** Wait until the element does not have keyboard focus. */
  waitUnfocused(timeoutSeconds?: number): Promise<Element>;
}

// ── App ─────────────────────────────────────────────────────────────────────

/**
 * A running application — the entry point for accessibility queries.
 *
 * `App` is **not** an `Element`. It represents the application as a whole and
 * provides a {@link App.locator | locator()} to search its accessibility tree.
 */
export class App {
  readonly name: string;
  readonly pid: number | null;

  /** Find an application by exact name. */
  static byName(name: string): Promise<App>;
  /** Find an application by process ID. */
  static byPid(pid: number): Promise<App>;
  /** List all running applications. */
  static list(): Promise<App[]>;

  /** Create a `Locator` scoped to this application's accessibility tree. */
  locator(selector: string): Locator;
  /** Get direct children (typically windows) of this application. */
  children(): Promise<Element[]>;
  /** Subscribe to accessibility events from this application. */
  subscribe(): Promise<Subscription>;
}

// ── Event / Subscription ────────────────────────────────────────────────────

/** An accessibility event delivered to subscribers. */
export class Event {
  /** Event kind, as a camelCase string. */
  readonly eventType: EventTypeName;
  readonly appName: string;
  readonly appPid: number;
  /** Snapshot of the element that triggered the event, if available. */
  readonly target: Element | null;
}

/**
 * A live event subscription. Use `recv()` / `tryRecv()` or iterate with
 * `for await (const ev of sub)`. Call `close()` (or drop out of the
 * iterator, which calls it for you) to unsubscribe.
 */
export class Subscription {
  /** Whether the subscription is still open. */
  readonly active: boolean;

  /** Try to receive an event without waiting. Returns `null` if none ready. */
  tryRecv(): Event | null;
  /** Wait for the next event, up to `timeoutSeconds` (default 5). */
  recv(timeoutSeconds?: number): Promise<Event>;
  /** Close the subscription. */
  close(): void;

  [Symbol.asyncIterator](): AsyncIterator<Event>;
}

// ── Module-level function ──────────────────────────────────────────────────

/** Create a top-level Locator searching from the system accessibility root. */
export function locator(selector: string): Locator;
