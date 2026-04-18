// Type-level sanity check: compile with `tsc --noEmit` to make sure the
// public API narrows as expected. This file is never executed -- it only
// exists to keep the type surface honest.

import {
  App,
  Element,
  Event,
  Locator,
  Subscription,
  XA11yError,
  SelectorNotMatchedError,
  TimeoutError,
  type AppLookupOptions,
  type CheckedState,
  type EventTypeName,
  type Rect,
  type SubscribeOptions,
  type WaitForEventOptions,
} from '../../index.js';

async function checks() {
  // Factory methods return Promise<App>.
  const app: App = await App.byName('Test');
  const _app2: App = await App.byPid(1234);
  const apps: App[] = await App.list();

  // Optional lookup options.
  const lookupOpts: AppLookupOptions = { timeout: 30_000 };
  const _app3: App = await App.byName('Test', lookupOpts);
  const _app4: App = await App.byPid(1234, { timeout: 5_000 });

  // Locator instance methods return promises or locators.
  const loc: Locator = app.locator('button[name="OK"]');
  const _count: number = await loc.count();
  const _exists: boolean = await loc.exists();
  const el: Element = await loc.element();
  const _all: Element[] = await loc.elements();
  await loc.press();
  await loc.setValue('hello');
  await loc.scrollDown();
  await loc.scrollDown(2.5);

  // Element getters are sync and narrowed.
  const _role: string = el.role;
  const _name: string | null = el.name;
  const _bounds: Rect | null = el.bounds;

  // Narrowed by patch-native-dts:
  const checked: CheckedState | null = el.checked;
  if (checked === 'on' || checked === 'off' || checked === 'mixed' || checked === null) {
    // OK -- exhaustive
  }

  // ── Subscription (EventEmitter) ─────────────────────────────────────

  // Subscribe with options
  const ctrl = new AbortController();
  const subOpts: SubscribeOptions = { signal: ctrl.signal };
  const sub: Subscription = await app.subscribe(subOpts);

  // Typed on/once/off
  sub.on('focusChanged', (ev: Event) => {
    const _type: EventTypeName = ev.type;
    const _target: Element | null = ev.target;
  });
  sub.once('windowOpened', (_ev: Event) => {});
  sub.on('event', (_ev: Event) => {});
  sub.off('focusChanged', () => {});

  // waitForEvent with options
  const waitOpts: WaitForEventOptions = {
    predicate: (e) => e.target?.role === 'button',
    timeout: 3000,
    signal: ctrl.signal,
  };
  const ev: Event = await sub.waitForEvent('focusChanged', waitOpts);
  const kind: EventTypeName = ev.type;
  if (
    kind === 'focusChanged' ||
    kind === 'valueChanged' ||
    kind === 'textChanged'
  ) {
    // OK -- literal narrowing works
  }

  // closed getter
  const _closed: boolean = sub.closed;

  // close
  sub.close();

  // app.waitForEvent (convenience)
  const ev2: Event = await app.waitForEvent('windowOpened', { timeout: 2000 });
  void ev2;

  // Error hierarchy -- instanceof narrows to subclass
  try {
    await loc.element();
  } catch (e) {
    if (e instanceof SelectorNotMatchedError) {
      const _msg: string = e.message;
    }
    if (e instanceof XA11yError) {
      const _msg: string = e.message;
    }
    if (e instanceof TimeoutError) {
      const _msg: string = e.message;
    }
  }

  // Unused to silence noUnusedLocals
  void _app2;
  void apps;
  void _count;
  void _exists;
  void _all;
  void _role;
  void _name;
  void _bounds;
}

void checks;
