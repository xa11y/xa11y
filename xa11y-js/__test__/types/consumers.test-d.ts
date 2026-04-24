// Type-level sanity check: compile with `tsc --noEmit` to make sure the
// public API narrows as expected. This file is never executed -- it only
// exists to keep the type surface honest.

import {
  App,
  Element,
  Event,
  InputSim,
  Locator,
  Screenshot,
  Screenshotter,
  Subscription,
  XA11yError,
  SelectorNotMatchedError,
  TimeoutError,
  inputSim,
  screenshotter,
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

  // ── InputSim ───────────────────────────────────────────────────────
  const sim: InputSim = inputSim();
  await sim.click([10, 20]);
  await sim.click(el); // element target
  await sim.doubleClick(el);
  await sim.drag([0, 0], [100, 100]);
  await sim.scroll([10, 10], 0, -3);
  await sim.press('Enter');
  await sim.chord('a', ['Shift']);
  await sim.typeText('hello');

  // ── Screenshotter ──────────────────────────────────────────────────
  const shooter: Screenshotter = screenshotter();
  const shot: Screenshot = await shooter.capture();
  const _w: number = shot.width;
  const _h: number = shot.height;
  const _s: number = shot.scale;
  const _px: Buffer = shot.pixels;
  const _png: Buffer = shot.toPng();
  shot.savePng('/tmp/out.png');
  const _shot2: Screenshot = await shooter.captureRegion({
    x: 0,
    y: 0,
    width: 10,
    height: 10,
  });
  const _shot3: Screenshot = await shooter.captureElement(el);

  // Unused to silence noUnusedLocals
  void _app2;
  void apps;
  void _count;
  void _exists;
  void _all;
  void _role;
  void _name;
  void _bounds;
  void _w;
  void _h;
  void _s;
  void _px;
  void _png;
  void _shot2;
  void _shot3;
}

void checks;
