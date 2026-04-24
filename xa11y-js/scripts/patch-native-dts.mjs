// Post-process xa11y-js/native.d.ts (emitted by `napi build`) to narrow a
// few widened types so consumers get literal-union types instead of plain
// strings. Runs automatically from `package.json` scripts after every
// napi build.
//
// Responsibilities:
//   1. Prepend shared type aliases (`CheckedState`, `EventTypeName`) that
//      the Rust side can't express.
//   2. Narrow `Element.checked: string | null` -> `CheckedState | null`.
//   3. Narrow `Event.type: string` -> `EventTypeName`.
//   4. Append a `;` to the napi-emitted `NativeSubscription` type alias
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
  | 'alert'
  | 'textChanged';

`;

/**
 * List of narrowings. Each entry is a required substitution -- if `from`
 * doesn't appear, the script exits non-zero so the drift is caught in CI.
 */
const REPLACEMENTS = [
  {
    name: 'Element.checked -> CheckedState | null',
    from: '  get checked(): string | null',
    to: '  get checked(): CheckedState | null',
  },
  {
    name: 'Event.type -> EventTypeName',
    from: '  get type(): string\n',
    to: '  get type(): EventTypeName\n',
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
  for (const { name, from, to } of REPLACEMENTS) {
    if (!patched.includes(from)) {
      problems.push(`  - ${name}: pattern not found: ${JSON.stringify(from)}`);
      continue;
    }
    patched = patched.replace(from, to);
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
