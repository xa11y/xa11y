// Table / collection metadata, table navigation, and text-by-range, against
// the core mock's table fixture (`xa11y_core::mock::build_table_provider`):
//
//   table "Prices" (3 x 3) with a column-spanning "Price" header, row
//   headers "Widget" / "Gadget", a selected "2.00" cell, and a "3.00" cell
//   with no spans reported; list "Results" whose items report size
//   'unknown'; a tree of "Root" > "Child"; and text_area "Notes" holding
//   "Hello 😀 world\nsecond line" (26 UTF-16 units: the emoji is two).

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  _makeTestTableLocator,
  ActionNotSupportedError,
  InvalidActionDataError,
  InvalidTextRangeError,
  Text,
  XA11yError,
  setDefaultTimeout,
} = require('../../index.js');

const NOTES = 'Hello 😀 world\nsecond line';

async function one(selector) {
  return await _makeTestTableLocator().descendant(selector).element();
}

const names = (els) => els.map((e) => e.name);

// ── Metadata getters ───────────────────────────────────────────────────────

test('table getter reports row and column counts', async () => {
  const table = await one('table');
  assert.deepEqual(table.table, { rowCount: 3, columnCount: 3 });
  assert.equal(table.cell, null);
  assert.equal(table.tableRow, null);
  assert.equal(table.collection, null);
});

test('cell getter reports position, spans and header kind', async () => {
  assert.deepEqual((await one('table_cell[name="Price"]')).cell, {
    row: 0,
    column: 1,
    rowSpan: 1,
    columnSpan: 2,
    header: 'column',
  });
  assert.equal((await one('table_cell[name="Widget"]')).cell.header, 'row');
  // A data cell has no header kind, and an unreported span stays null
  // rather than being defaulted to 1.
  assert.deepEqual((await one('table_cell[name="3.00"]')).cell, {
    row: 2,
    column: 1,
    rowSpan: null,
    columnSpan: null,
    header: null,
  });
});

test('tableRow getter reports the 0-based row index', async () => {
  const rows = await _makeTestTableLocator().descendant('table_row').elements();
  assert.deepEqual(
    rows.map((r) => r.tableRow),
    [{ index: 0 }, { index: 1 }, { index: 2 }],
  );
});

test("collection getter reports index, size ('unknown' when virtualized) and depth", async () => {
  assert.deepEqual((await one('list_item[name="First"]')).collection, {
    index: 0,
    size: 'unknown',
    depth: null,
  });
  assert.deepEqual((await one('tree_item[name="Child"]')).collection, {
    index: 0,
    size: 1,
    depth: 1,
  });
});

// ── Table navigation ───────────────────────────────────────────────────────

test('cellAt resolves a slot, a merged slot, and null out of range', async () => {
  const table = await one('table');
  assert.equal((await table.cellAt(1, 2)).name, '2.00');
  // (0, 2) is covered by the column-spanning "Price" header.
  assert.equal((await table.cellAt(0, 2)).name, 'Price');
  assert.equal(await table.cellAt(9, 9), null);
});

test('cellAt rejects a negative or fractional index before any call', async () => {
  const table = await one('table');
  assert.throws(() => table.cellAt(-1, 0), InvalidActionDataError);
  assert.throws(() => table.cellAt(0, 1.5), InvalidActionDataError);
});

test('columnHeaders / rowHeaders list a table’s header cells', async () => {
  const table = await one('table');
  assert.deepEqual(names(await table.columnHeaders()), ['Product', 'Price']);
  assert.deepEqual(names(await table.rowHeaders()), ['Widget', 'Gadget']);
});

test('columnHeaderCells / rowHeaderCells list the headers of one cell', async () => {
  const cell = await one('table_cell[name="3.00"]');
  assert.deepEqual(names(await cell.columnHeaderCells()), ['Price']);
  assert.deepEqual(names(await cell.rowHeaderCells()), ['Gadget']);
});

test('table navigation on a non-table rejects with ActionNotSupportedError', async () => {
  const list = await one('list[name="Results"]');
  await assert.rejects(list.cellAt(0, 0), ActionNotSupportedError);
  await assert.rejects(list.columnHeaders(), ActionNotSupportedError);
});

test('selectedItems lists the selected descendants', async () => {
  const table = await one('table');
  assert.deepEqual(names(await table.selectedItems()), ['2.00']);
});

// ── Text ───────────────────────────────────────────────────────────────────

test('Element.text() returns a Text; a node without text rejects', async () => {
  const notes = await one('text_area');
  const text = await notes.text();
  assert.ok(text instanceof Text);
  assert.equal(await text.length(), 26);
  await assert.rejects((await one('table')).text(), ActionNotSupportedError);
});

test('read / readAll return text by UTF-16 range', async () => {
  const text = await (await one('text_area')).text();
  assert.equal(await text.readAll(), NOTES);
  assert.equal(await text.read({ start: 6, end: 8 }), '😀');
  // JS string indices are UTF-16 too, so a range slices the value directly.
  assert.equal(await text.read({ start: 9, end: 14 }), NOTES.slice(9, 14));
});

test('InvalidTextRangeError carries start / end / length', async () => {
  const text = await (await one('text_area')).text();
  await assert.rejects(text.read({ start: 0, end: 99 }), (err) => {
    assert.ok(err instanceof InvalidTextRangeError);
    // A subclass of InvalidActionDataError, as in Python.
    assert.ok(err instanceof InvalidActionDataError);
    assert.ok(err instanceof XA11yError);
    assert.equal(err.name, 'InvalidTextRangeError');
    assert.equal(err.start, 0);
    assert.equal(err.end, 99);
    assert.equal(err.length, 26);
    assert.equal(err.delivery, 'not_delivered');
    assert.match(err.message, /0\.\.99/);
    return true;
  });
  // Splitting the emoji's surrogate pair is rejected, not mangled.
  await assert.rejects(text.read({ start: 7, end: 8 }), InvalidTextRangeError);
  // A reversed range is a range error with no known length.
  await assert.rejects(text.read({ start: 5, end: 2 }), (err) => {
    assert.ok(err instanceof InvalidTextRangeError);
    assert.equal(err.length, null);
    return true;
  });
});

test('range arguments are validated before any call', async () => {
  const text = await (await one('text_area')).text();
  assert.throws(() => text.read({ start: -1, end: 2 }), InvalidActionDataError);
  assert.throws(() => text.read({ start: 0.5, end: 2 }), InvalidActionDataError);
  assert.throws(() => text.read({ start: 0 }), /end/);
});

test('find honours occurrence and within, and returns null on a miss', async () => {
  const text = await (await one('text_area')).text();
  assert.deepEqual(await text.find('o'), { start: 4, end: 5 });
  assert.deepEqual(await text.find('o', { occurrence: 1 }), { start: 10, end: 11 });
  assert.deepEqual(await text.find('o', { within: { start: 5, end: 26 } }), {
    start: 10,
    end: 11,
  });
  assert.deepEqual(await text.find('line', { occurrence: 0, within: null }), {
    start: 22,
    end: 26,
  });
  assert.equal(await text.find('zzz'), null);
  await assert.rejects(text.find(''), InvalidActionDataError);
  assert.throws(() => text.find('o', { occurrence: -1 }), InvalidActionDataError);
});

test('rangeAt returns the unit at an offset', async () => {
  const text = await (await one('text_area')).text();
  assert.deepEqual(await text.rangeAt(9, 'word'), { start: 9, end: 15 });
  // The second half of the surrogate pair belongs to the whole character.
  assert.deepEqual(await text.rangeAt(7, 'character'), { start: 6, end: 8 });
  assert.deepEqual(await text.rangeAt(20, 'line'), { start: 15, end: 26 });
  assert.deepEqual(await text.rangeAt(0, 'paragraph'), { start: 0, end: 15 });
  assert.throws(() => text.rangeAt(0, 'sentence'), InvalidActionDataError);
  await assert.rejects(text.rangeAt(99, 'word'), InvalidTextRangeError);
});

test('lines pages rendered lines (default limit 200)', async () => {
  const text = await (await one('text_area')).text();
  const all = { start: 0, end: 26 };
  assert.deepEqual(await text.lines(all), {
    lines: [
      { start: 0, end: 15 },
      { start: 15, end: 26 },
    ],
    truncated: false,
    nextStart: null,
  });
  assert.deepEqual(await text.lines(all, 1), {
    lines: [{ start: 0, end: 15 }],
    truncated: true,
    nextStart: 15,
  });
  assert.throws(() => text.lines(all, -1), InvalidActionDataError);
  // A page that holds nothing would never advance a paging loop.
  assert.throws(() => text.lines(all, 0), InvalidActionDataError);
  assert.throws(() => text.runs(all, 0), InvalidActionDataError);
});

test('bounds returns one rect per line fragment', async () => {
  const notes = await one('text_area');
  const text = await notes.text();
  const { x, y } = notes.bounds;
  // The mock lays text out at 8 px per unit and 16 px per line.
  assert.deepEqual(await text.bounds({ start: 0, end: 5 }), [{ x, y, width: 40, height: 16 }]);
  assert.deepEqual(await text.bounds({ start: 12, end: 18 }), [
    { x: x + 96, y, width: 24, height: 16 },
    { x, y: y + 16, width: 24, height: 16 },
  ]);
});

test('runs pages formatting runs with nullable attributes', async () => {
  const text = await (await one('text_area')).text();
  const page = await text.runs({ start: 0, end: 9 });
  assert.equal(page.truncated, false);
  assert.equal(page.nextStart, null);
  assert.deepEqual(
    page.runs.map((r) => [r.range, r.attributes.fontWeight]),
    [
      [{ start: 0, end: 6 }, 700],
      [{ start: 6, end: 9 }, 400],
    ],
  );
  // Every attribute key is present; unreported ones are null, not absent.
  assert.deepEqual(page.runs[0].attributes, {
    fontFamily: 'MockSans',
    fontSize: 12,
    fontWeight: 700,
    italic: false,
    underline: null,
    strikethrough: null,
    foreground: null,
    background: null,
    script: null,
    spellingError: null,
  });
  const first = await text.runs({ start: 0, end: 26 }, 1);
  assert.deepEqual(first.runs.length, 1);
  assert.equal(first.truncated, true);
  assert.equal(first.nextStart, 6);
});

test('selectText takes (start, end) or a range; caret and selections read it back', async () => {
  const locator = _makeTestTableLocator().descendant('text_area');
  const notes = await locator.element();
  const text = locator.text();
  assert.ok(text instanceof Text, 'Locator.text() is synchronous');
  assert.equal(await text.caret(), null);
  assert.deepEqual(await text.selections(), []);

  await notes.selectText(0, 5);
  assert.equal(await text.caret(), 5);
  assert.deepEqual(await text.selections(), [{ start: 0, end: 5 }]);

  await locator.selectText({ start: 6, end: 8 });
  assert.deepEqual(await text.selections(), [{ start: 6, end: 8 }]);
  assert.equal(await text.read((await text.selections())[0]), '😀');

  // A caret alone is not a selection.
  await notes.selectText({ start: 3, end: 3 });
  assert.equal(await text.caret(), 3);
  assert.deepEqual(await text.selections(), []);
});

test('selectText rejects malformed arguments and out-of-range offsets', async () => {
  const locator = _makeTestTableLocator().descendant('text_area');
  const notes = await locator.element();
  for (const target of [notes, locator]) {
    assert.throws(() => target.selectText(1), InvalidActionDataError);
    assert.throws(() => target.selectText({ start: 0, end: 1 }, 2), InvalidActionDataError);
    assert.throws(() => target.selectText(-1, 2), InvalidActionDataError);
    await assert.rejects(target.selectText(5, 2), InvalidActionDataError);
    await assert.rejects(target.selectText(0, 99), (err) => {
      assert.ok(err instanceof InvalidTextRangeError);
      assert.equal(err.length, 26);
      return true;
    });
  }
});

test('Locator.text() re-resolves on every read', async () => {
  setDefaultTimeout(0);
  const text = _makeTestTableLocator().descendant('text_area[name="Missing"]').text();
  // Building the Text resolves nothing; the read is what fails.
  await assert.rejects(text.length(), XA11yError);
  assert.equal(await _makeTestTableLocator().descendant('text_area').text().length(), 26);
});


test('embedded object mapping distinguishes unsupported and invalid offsets', async () => {
  const text = await _makeTestTableLocator().descendant('text_area').text();
  await assert.rejects(text.objectAt(0), /text_object_at/);
  await assert.rejects(text.objectAt(await text.length()), InvalidTextRangeError);
});
