// Type-level sanity check: compile with `tsc --noEmit` to make sure the
// public API narrows as expected. This file is never executed — it only
// exists to keep the type surface honest.

import {
  App,
  Element,
  Event,
  EventType,
  Locator,
  Subscription,
  XA11yError,
  SelectorNotMatchedError,
  type CheckedState,
  type EventTypeName,
  type Rect,
} from '../../index.js';

async function checks() {
  // Factory methods return Promise<App>.
  const app: App = await App.byName('Test');
  const _app2: App = await App.byPid(1234);
  const apps: App[] = await App.list();

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
    // OK — exhaustive
  }

  // Subscription / Event
  const sub: Subscription = await app.subscribe();
  const ev: Event = await sub.recv();
  const kind: EventTypeName = ev.eventType;
  if (
    kind === 'focusChanged' ||
    kind === 'valueChanged' ||
    kind === 'textChanged'
  ) {
    // OK — literal narrowing works
  }

  // Async iteration over a subscription
  for await (const e of sub) {
    const _k: EventTypeName = e.eventType;
  }
  sub.close();

  // EventType constants object
  const _focus: EventTypeName = EventType.FocusChanged;

  // Error hierarchy — instanceof narrows to subclass
  try {
    await loc.element();
  } catch (e) {
    if (e instanceof SelectorNotMatchedError) {
      const _msg: string = e.message;
    }
    if (e instanceof XA11yError) {
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
