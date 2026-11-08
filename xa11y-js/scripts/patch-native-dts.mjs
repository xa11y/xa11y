// Post-process xa11y-js/native.d.ts (emitted by `napi build`) to narrow a
// few widened types so consumers get literal-union types instead of plain
// strings. Runs automatically from `package.json` scripts after every
// napi build.
//
// Responsibilities:
//   1. Prepend shared type aliases (`CheckedState`, `HiddenReasonName`,
//      `AutoScrollOutcomeName`, `EventTypeName`, `MouseButtonName`, `AnchorName`,
//      `ShellSurfaceKindName`) that the Rust side can't express.
//   2. Narrow `Element.checked: string | null` -> `CheckedState | null`, and
//      `Element.hiddenReason` -> `HiddenReasonName | null`.
//   3. Narrow `Event.type: string` -> `EventTypeName`.
//   4. Narrow the input layer's button and anchor strings.
//   5. Narrow `ShellSurface.kind` and `ShellSurface.byKind`'s parameter to
//      `ShellSurfaceKindName`.
//   5b. Narrow `Omission.reason` -> `OmissionReasonName` and
//      `LegendEntry.color` -> a three-element tuple.
//   5c. Narrow the table / text plain objects: `Count` (number or
//      'unknown'), `CellInfo.header`, `TextAttributes.script`,
//      `TextChange.kind`, and `Text.rangeAt`'s unit parameter.
//   5d. Narrow the capability table's platform and support strings to
//      `PlatformName` / `SupportName`.
//   5e. Narrow the document-semantics strings: `LandmarkName`,
//      `PolitenessName` and `RelationKindName`.
//   6. Append a `;` to the napi-emitted `NativeSubscription` type alias
//      (napi-rs omits terminators, which confuses the docs generator's
//      multi-line type-alias parser).
//
// Each substitution is guarded: if a pattern stops matching (because napi
// changed its output format), the script fails loudly so the drift is
// caught in CI instead of silently shipping a wider type.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const dtsPath = join(here, '..', 'native.d.ts');

const MARKER = '/* patched by scripts/patch-native-dts.mjs */';

const HEADER = `${MARKER}

/** Checked state of a toggleable element. */
export type CheckedState = 'on' | 'off' | 'mixed';

/** Accessibility event type names, normalised across platforms. */
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
  | 'textChanged'
  | 'textSelectionChanged'
  | 'announcement';

/** Mouse buttons accepted by the input-simulation API. */
export type MouseButtonName = 'left' | 'right' | 'middle';

/** Named anchor points inside an element's bounds. */
export type AnchorName =
  | 'center'
  | 'top_left'
  | 'top_right'
  | 'bottom_left'
  | 'bottom_right';

/** Why an element reads \`visible: false\`. Spelled as in Python. */
export type HiddenReasonName =
  | 'window_minimized'
  | 'scrolled_out'
  | 'not_showing'
  | 'unknown';

/**
 * Whether a failed action may already have taken effect (\`XA11yError.delivery\`).
 * \`not_delivered\` is safe to retry. Spelled as in Python.
 */
export type DeliveryName = 'not_delivered' | 'unknown';

/** What an auto-waiting action's scroll attempt came to. Spelled as in Python. */
export type AutoScrollOutcomeName = 'refused' | 'no_effect' | 'revealed';

/** Why an annotated element is not in the image. Spelled as in Python. */
export type OmissionReasonName = 'no_bounds' | 'zero_area' | 'outside_capture';

/** Kinds of OS shell surface, spelled as in Python, the CLI and MCP. */
export type ShellSurfaceKindName =
  | 'menu_bar'
  | 'status_items'
  | 'taskbar'
  | 'panel'
  | 'dock'
  | 'desktop'
  | 'flyout'
  | 'unknown';

/**
 * A total the platform reported: a number, or \`'unknown'\` when it says it
 * does not know (virtualized content). Spelled as in Python.
 */
export type CountValue = number | 'unknown';

/** Which kind of header a header cell is. Spelled as in Python. */
export type HeaderKindName = 'column' | 'row';

/** A landmark's kind, as its ARIA role (\`Element.landmarkType\`). */
export type LandmarkName =
  | 'banner'
  | 'complementary'
  | 'contentinfo'
  | 'form'
  | 'main'
  | 'navigation'
  | 'region'
  | 'search'
  | 'unknown';

/** A live region's politeness (\`LiveRegionInfo.politeness\`). */
export type PolitenessName = 'off' | 'polite' | 'assertive';

/** A relation kind (\`Element.relations\`). */
export type RelationKindName =
  | 'labelled_by'
  | 'label_for'
  | 'described_by'
  | 'description_for'
  | 'details'
  | 'details_for'
  | 'controls'
  | 'controlled_by'
  | 'flows_to'
  | 'flows_from'
  | 'error_message'
  | 'error_for'
  | 'owns'
  | 'member_of'
  | 'linked';

/** Which read of a captured node failed (\`CaptureError.step\`). */
export type CaptureStepName = 'children' | 'identity' | 'text' | 'relations' | 'cycle';

/** Extra reads a capture can make (\`CaptureOptions.detail\`). */
export type CaptureDetailName = 'text' | 'relations';

/** A platform xa11y has a backend for. */
export type PlatformName = 'linux' | 'macos' | 'windows';

/** Whether a backend can report a feature (see \`capabilities()\`). */
export type SupportName = 'supported' | 'partial' | 'unsupported';

/** Units accepted by \`Text.rangeAt\`. Spelled as in Python. */
export type TextUnitName = 'character' | 'word' | 'line' | 'paragraph';

/** Where a run of text sits relative to the baseline. Spelled as in Python. */
export type TextScriptName = 'baseline' | 'superscript' | 'subscript';

/** Whether a \`TextChange\` inserted or deleted text. Spelled as in Python. */
export type TextChangeKindName = 'inserted' | 'deleted';

`;

/**
 * List of narrowings. Each entry is a required substitution -- if `from`
 * doesn't appear, the script exits non-zero so the drift is caught in CI.
 *
 * `all: true` replaces every occurrence and asserts the expected count, so a
 * new call site that quietly picks up the wide type is caught too.
 */
const REPLACEMENTS = [
  // Before `TextChange.kind`: both are `kind: string` until this one is
  // narrowed, and that entry asserts a single occurrence.
  {
    name: 'CapturedRelation.kind -> RelationKindName',
    from: "  kind: string\n  /**\n   * Each target: a `number` index",
    to: "  kind: RelationKindName\n  /**\n   * Each target: a `number` index",
    all: 1,
  },
  {
    name: 'CaptureError.step -> CaptureStepName',
    from: "  step: string\n  /** The error code the call would have rejected with",
    to: "  step: CaptureStepName\n  /** The error code the call would have rejected with",
    all: 1,
  },
  {
    name: 'CaptureOptions.detail -> CaptureDetailName[]',
    from: "  detail?: Array<string>",
    to: "  detail?: Array<CaptureDetailName>",
    all: 1,
  },
  {
    name: 'Relation.kind -> RelationKindName',
    from: '  kind: string\n  /** The related elements',
    to: '  kind: RelationKindName\n  /** The related elements',
    all: 1,
  },
  {
    name: 'Element.checked -> CheckedState | null',
    from: '  get checked(): string | null',
    to: '  get checked(): CheckedState | null',
  },
  {
    name: 'Element.hiddenReason -> HiddenReasonName | null',
    from: '  get hiddenReason(): string | null',
    to: '  get hiddenReason(): HiddenReasonName | null',
  },
  {
    name: 'Event.type -> EventTypeName',
    from: '  get type(): string\n',
    to: '  get type(): EventTypeName\n',
  },
  {
    // `ClickOptions.button` and `DragOptions.button`.
    name: 'click/drag options button -> MouseButtonName',
    from: '  button?: string\n',
    to: '  button?: MouseButtonName\n',
    all: 2,
  },
  {
    // `InputSim.mouseDown` and `InputSim.mouseUp`.
    name: 'mouseDown/mouseUp button -> MouseButtonName',
    from: '(button?: string | undefined | null): Promise<void>',
    to: '(button?: MouseButtonName | undefined | null): Promise<void>',
    all: 2,
  },
  {
    // The kind a caller writes as a literal, on the way in...
    name: 'ShellSurface.byKind kind -> ShellSurfaceKindName',
    from: '  static byKind(kind: string,',
    to: '  static byKind(kind: ShellSurfaceKindName,',
    all: 1,
  },
  {
    // ...and the same union on the way back out. Test helpers
    // (`_makeTestShellSurfaceByKind`) keep the wide `string`: they are
    // internal, and narrowing them would make the count assertions above
    // move whenever a test helper is added.
    name: 'ShellSurface.kind -> ShellSurfaceKindName',
    from: '  get kind(): string\n',
    to: '  get kind(): ShellSurfaceKindName\n',
    all: 1,
  },
  {
    // The only string-valued member of the legend. A reason missing from the
    // union is a literal a consumer's type checker rejects even though the
    // runtime returns it, which is why it is also under
    // [[types.variant_coverage]] in bindings/parity_allowlist.toml.
    name: 'Omission.reason -> OmissionReasonName',
    from: '  reason: string\n',
    to: '  reason: OmissionReasonName\n',
    all: 1,
  },
  {
    // A colour is exactly three channels; `Array<number>` lets a two-element
    // array type-check.
    name: 'LegendEntry.color -> [number, number, number]',
    from: '  color: Array<number>\n',
    to: '  color: [number, number, number]\n',
    all: 1,
  },
  {
    name: 'ClickOptions.anchor -> AnchorName | [number, number]',
    from: '  anchor?: string | Array<number>\n',
    to: '  anchor?: AnchorName | [number, number]\n',
  },
  {
    // The Rust side spells `Count` as `Either<u32, String>` behind a type
    // alias napi does not emit, so the generated name dangles until it is
    // replaced: `TableInfo.rowCount` / `columnCount` and
    // `CollectionInfo.size`.
    name: 'Count -> CountValue',
    from: ': Count | null\n',
    to: ': CountValue | null\n',
    all: 3,
  },
  {
    name: 'CellInfo.header -> HeaderKindName | null',
    from: '  header: string | null\n',
    to: '  header: HeaderKindName | null\n',
    all: 1,
  },
  {
    name: 'TextAttributes.script -> TextScriptName | null',
    from: '  script: string | null\n',
    to: '  script: TextScriptName | null\n',
    all: 1,
  },
  {
    name: 'TextChange.kind -> TextChangeKindName',
    from: '  kind: string\n',
    to: '  kind: TextChangeKindName\n',
    all: 1,
  },
  {
    name: 'Text.rangeAt unit -> TextUnitName',
    from: '  rangeAt(offset: number, unit: string)',
    to: '  rangeAt(offset: number, unit: TextUnitName)',
    all: 1,
  },
  {
    name: 'Capabilities.platform -> PlatformName',
    from: '  get platform(): string\n',
    to: '  get platform(): PlatformName\n',
    all: 1,
  },
  {
    name: 'CapabilitiesOptions.platform -> PlatformName',
    from: '  platform?: string\n',
    to: '  platform?: PlatformName\n',
    all: 1,
  },
  {
    name: 'Capability.support -> SupportName',
    from: '  support: string\n',
    to: '  support: SupportName\n',
    all: 1,
  },
  {
    name: 'Capabilities.support() -> SupportName',
    from: '  support(name: string): string\n',
    to: '  support(name: string): SupportName\n',
    all: 1,
  },
  {
    name: 'Element.landmarkType -> LandmarkName',
    from: '  get landmarkType(): string | null\n',
    to: '  get landmarkType(): LandmarkName | null\n',
    all: 1,
  },
  {
    name: 'LiveRegionInfo.politeness -> PolitenessName',
    from: '  politeness: string | null\n',
    to: '  politeness: PolitenessName | null\n',
    all: 1,
  },
  {
    name: 'Element.relations kinds -> RelationKindName',
    from: '  relations(kinds?: Array<string> | undefined | null)',
    to: '  relations(kinds?: Array<RelationKindName> | undefined | null)',
    all: 1,
  },
  {
    // napi-rs emits this line without a trailing `;`. The docs generator
    // (docs/generate_js_api.py) uses `;` to terminate multi-line type
    // aliases, so an unterminated alias silently swallows every later
    // class declaration until it hits any line that happens to end in `;`.
    // Appending the terminator here is a one-line fix that keeps the parser
    // honest.
    name: 'NativeSubscription type alias terminator',
    from: 'export type NativeSubscription = _NativeSubscription\n',
    to: 'export type NativeSubscription = _NativeSubscription;\n',
  },
];

function patch() {
  const source = readFileSync(dtsPath, 'utf8');

  // napi build regenerates native.d.ts from scratch every time, so the
  // marker only survives across a no-op rerun. If it's still there, the
  // file is already patched and we short-circuit -- this keeps `npm run
  // build && npm run build` idempotent.
  if (source.includes(MARKER)) {
    console.error(`native.d.ts already patched -- skipping`);
    return;
  }

  const problems = [];
  let patched = source;
  for (const { name, from, to, all } of REPLACEMENTS) {
    const parts = patched.split(from);
    const found = parts.length - 1;
    if (found === 0) {
      problems.push(`  - ${name}: pattern not found: ${JSON.stringify(from)}`);
      continue;
    }
    if (all === undefined) {
      patched = parts.shift() + to + parts.join(from);
      continue;
    }
    if (found !== all) {
      problems.push(
        `  - ${name}: expected ${all} occurrence(s) of ${JSON.stringify(from)}, found ${found}`,
      );
      continue;
    }
    patched = parts.join(to);
  }

  if (problems.length > 0) {
    console.error('patch-native-dts.mjs: one or more patterns failed:');
    console.error(problems.join('\n'));
    console.error('\nThis usually means napi-rs changed its .d.ts output format.');
    console.error('Inspect native.d.ts and update REPLACEMENTS in this script.');
    process.exit(1);
  }

  writeFileSync(dtsPath, HEADER + patched);
  console.error(`patched ${dtsPath} (${REPLACEMENTS.length} substitutions)`);
}

patch();
