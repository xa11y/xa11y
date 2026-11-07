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
  ShellSurface,
  Subscription,
  XA11yError,
  SelectorNotMatchedError,
  TimeoutError,
  InvalidActionDataError,
  InvalidTextRangeError,
  Text,
  inputSim,
  screenshot,
  type AppLookupOptions,
  type CellInfo,
  type CheckedState,
  type CountValue,
  type HeaderKindName,
  type TextChange,
  type TextChangeKindName,
  type TextRange,
  type TextRuns,
  type TextUnitName,
  type EventTypeName,
  type LegendEntry,
  type Omission,
  type OmissionReasonName,
  type Rect,
  type ScreenshotOptions,
  type ShellSurfaceKindName,
  type ShellSurfaceLookupOptions,
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

  // App foreground getters are sync booleans; `focused` is the deprecated alias.
  const _isForeground: boolean = app.isForeground;
  const _appFocused: boolean = app.focused;

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
  const _active: boolean = el.active;

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

  // Text edits carry an optional delta; caret moves are their own kind.
  sub.on('textSelectionChanged', (_ev: Event) => {});
  const change: TextChange | null = ev.textChange;
  if (change) {
    const _kind: TextChangeKindName = change.kind;
    const _range: TextRange | null = change.range;
    const _text: string | null = change.text;
    // @ts-expect-error -- the kind is a closed union
    const _bad: TextChangeKindName = 'replaced';
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

  // ── ShellSurface ───────────────────────────────────────────────────
  const surfaces: ShellSurface[] = await ShellSurface.list();
  const taskbar: ShellSurface = await ShellSurface.byKind('taskbar', { timeout: 0 });
  const shellOpts: ShellSurfaceLookupOptions = { timeout: 3000 };
  const _flyout: ShellSurface = await ShellSurface.byKind('flyout', shellOpts);

  // Narrowed by patch-native-dts: kinds are a literal union both ways.
  const surfaceKind: ShellSurfaceKindName = taskbar.kind;
  if (surfaceKind === 'taskbar' || surfaceKind === 'status_items') {
    // OK -- literal narrowing works
  }
  // @ts-expect-error -- a kind that is not one of the eight spellings
  await ShellSurface.byKind('task_bar');

  const _surfaceName: string = taskbar.name;
  const _surfacePid: number | null = taskbar.pid;
  const _shellLoc: Locator = taskbar.locator("button[name='Show Hidden Icons']");
  const _shellChildren: Element[] = await taskbar.children();
  const _shellRoot: Element = taskbar.asElement();
  const _shellDump: string = await taskbar.dump(2);
  void (await taskbar.tree(1));

  // ── screenshot ─────────────────────────────────────────────────────
  const shot: Screenshot = await screenshot();
  const _w: number = shot.width;
  const _h: number = shot.height;
  const _s: number = shot.scale;
  const _px: Buffer = shot.pixels;
  const _png: Buffer = shot.toPng();
  shot.savePng('/tmp/out.png');
  const _mapped: boolean = shot.mappingAvailable;
  const _imagePoint: number[] = shot.desktopToImage(10, 20);
  const _desktopPoint: number[] = shot.imageToDesktop(10, 20);
  const _imageRects: Rect[] = shot.desktopRectToImage({ x: 0, y: 0, width: 10, height: 10 });
  const _cropped: Screenshot = shot.crop({ x: 0, y: 0, width: 10, height: 10 });
  const _resized: Screenshot = shot.resize(5, 5);
  const shotOpts: ScreenshotOptions = {
    region: { x: 0, y: 0, width: 10, height: 10 },
  };
  const _shot2: Screenshot = await screenshot(shotOpts);
  const _shot3: Screenshot = await screenshot({ element: el });

  // Annotated capture: `annotate` takes locators or selector strings, and the
  // legend comes back on the same Screenshot type.
  const _shot4: Screenshot = await screenshot({ annotate: ['button', loc] });
  const _shot5: Screenshot = await screenshot({ element: el, annotate: [loc] });
  const legend: LegendEntry[] = shot.legend;
  const omitted: Omission[] = shot.omitted;
  const _truncated: number = shot.truncated;
  const _tag: string = legend[0].tag;
  const _group: number = legend[0].group;
  const _entrySelector: string = legend[0].selector;
  const _entryName: string | undefined = legend[0].name;
  const _entryBounds: Rect = legend[0].bounds;
  const _entryColor: [number, number, number] = legend[0].color;
  const _reason: OmissionReasonName = omitted[0].reason;

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
  void _shot4;
  void _shot5;
  void _truncated;
  void _tag;
  void _group;
  void _entrySelector;
  void _entryName;
  void _entryBounds;
  void _entryColor;
  void _reason;
  void surfaces;
  void _flyout;
  void _surfaceName;
  void _surfacePid;
  void _shellLoc;
  void _shellChildren;
  void _shellRoot;
  void _shellDump;
}

async function tableAndTextChecks(el: Element, loc: Locator) {
  // Metadata getters are nullable plain objects with narrowed unions.
  const rows: CountValue | null | undefined = el.table?.rowCount;
  const cell: CellInfo | null = el.cell;
  const header: HeaderKindName | null | undefined = cell?.header;
  const size: number | 'unknown' | null | undefined = el.collection?.size;
  const _rowIndex: number | null | undefined = el.tableRow?.index;
  // @ts-expect-error -- a header kind is 'column' or 'row'
  const _badHeader: HeaderKindName = 'col';

  // Table navigation.
  const _at: Element | null = await el.cellAt(0, 1);
  const _heads: Element[] = await el.columnHeaders();
  const _selected: Element[] = await el.selectedItems();

  // Text: `Element.text()` is async, `Locator.text()` is not.
  const text: Text = await el.text();
  const live: Text = loc.text();
  const range: TextRange | null = await text.find('needle', { occurrence: 1, within: null });
  const unit: TextUnitName = 'word';
  const _word: TextRange = await text.rangeAt(0, unit);
  // @ts-expect-error -- not a text unit
  void text.rangeAt(0, 'sentence');
  const runs: TextRuns = await live.runs({ start: 0, end: 10 }, 50);
  const _script = runs.runs[0]?.attributes.script satisfies
    | 'baseline'
    | 'superscript'
    | 'subscript'
    | null
    | undefined;
  await el.selectText(0, 5);
  await loc.selectText({ start: 0, end: 5 });

  try {
    await text.read({ start: 0, end: 99 });
  } catch (err) {
    if (err instanceof InvalidTextRangeError) {
      const _len: number | null = err.length;
      const _parent: InvalidActionDataError = err;
    }
  }
  void rows;
  void header;
  void size;
  void range;
}

void checks;
void tableAndTextChecks;
