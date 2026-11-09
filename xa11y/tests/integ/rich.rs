//! Tables, collections and text by range against real AT-SPI objects.
//!
//! Most of this file drives `test-apps/rich/rich.py`, a GTK 3 window that
//! `scripts/run_integ_tests.sh` launches next to the AccessKit test app.
//! AccessKit's AT-SPI adapter implements no `Table` / `TableCell` interface
//! and reports no text attribute runs, so the table and rich-text paths of
//! the Linux backend have nothing to read there. Collection positions come
//! from AccessKit instead, because GTK 3 sets no `posinset` / `setsize`
//! object attributes on any widget: see `integ::collections`, which also
//! runs in the Wayland job that skips this module.
//!
//! What neither fixture can show, and so nothing here claims:
//!
//! * a merged cell. GTK 3's `GtkTreeView` has no spans, so every span below
//!   is the 1 the platform reports.
//! * a `level` (`CollectionInfo::depth`). GTK 3 tree cells carry no
//!   `level` attribute and `accesskit_atspi_common` does not export one.
//! * row headers. A `GtkTreeView` has column headers only.
//!
//! Merged cells, depth, declared totals and right-to-left text are covered
//! against Chromium instead, by `tests/suites/python/test_web_rich.py` on the
//! Electron test app.
//!
//! Every expected offset is in UTF-16 code units. GTK counts code points, so
//! the fixture text puts a non-BMP character on each of its first two lines:
//! any offset after one is off by one per character if the conversion is
//! missing.

use std::time::Duration;

use xa11y::text_api::{FindOptions, TextRange, TextUnit};
use xa11y::*;

/// The fixture's AT-SPI application name (`APP_NAME` in `rich.py`).
const FIXTURE: &str = "xa11y-rich-fixture";

/// The text view's content, as `rich.py` joins `TEXT_PARAGRAPHS`.
const TEXT: &str = "Hello wörld 😀 bold italic\nSecond line under red 𝄞 end\n\nFinal paragraph";

fn r(start: u32, end: u32) -> TextRange {
    TextRange { start, end }
}

fn fixture() -> App {
    App::find(Duration::from_secs(5), |d| {
        d.name.as_deref() == Some(FIXTURE)
    })
    .unwrap_or_else(|e| {
        panic!(
            "the rich fixture ({FIXTURE}) is not running: scripts/run_integ_tests.sh \
                 launches test-apps/rich/rich.py. {e}"
        )
    })
}

fn one(app: &App, selector: &str) -> Element {
    let found = app
        .locator(selector)
        .elements()
        .unwrap_or_else(|e| panic!("selector {selector:?} failed: {e}"));
    assert_eq!(
        found.len(),
        1,
        "selector {selector:?} matched {} elements, expected 1: {found:?}",
        found.len()
    );
    found.into_iter().next().expect("length checked above")
}

fn names(elements: &[Element]) -> Vec<String> {
    elements
        .iter()
        .map(|e| e.name.clone().unwrap_or_default())
        .collect()
}

fn fruit_table(app: &App) -> Element {
    one(app, r#"table[name="Fruit table"]"#)
}

fn rich_text(app: &App) -> Element {
    one(app, r#"text_area[name="Rich text"]"#)
}

// ── Tables ──────────────────────────────────────────────────────────

#[test]
#[ignore]
fn table_reports_its_dimensions() {
    let app = fixture();
    let table = fruit_table(&app);
    let info = table.table.expect("a GtkTreeView reports Table");
    assert_eq!(info.row_count, Some(Count::Known(3)), "{info:?}");
    assert_eq!(info.column_count, Some(Count::Known(3)), "{info:?}");
    assert!(
        table.cell.is_none(),
        "a table is not a cell: {:?}",
        table.cell
    );
}

#[test]
#[ignore]
fn cells_carry_zero_based_positions_and_unit_spans() {
    let app = fixture();
    let banana = one(
        &app,
        r#"table[name="Fruit table"] table_cell[row="1"][column="0"]"#,
    );
    assert_eq!(banana.name.as_deref(), Some("Banana"));
    let cell = banana.cell.expect("a tree-view cell reports TableCell");
    assert_eq!((cell.row, cell.column), (Some(1), Some(0)));
    // GTK 3 has no merged cells: every span is 1. See the module docs.
    assert_eq!((cell.row_span, cell.column_span), (Some(1), Some(1)));
    assert_eq!(cell.header, None);

    let qty = one(
        &app,
        r#"table[name="Fruit table"] table_cell[row="2"][column="2"]"#,
    );
    assert_eq!(qty.name.as_deref(), Some("40"));

    // `row_span` is numeric too: all nine body cells have a span of 1, and
    // the three headers have none.
    let spanned = app
        .locator(r#"table[name="Fruit table"] table_cell[row_span="1"]"#)
        .elements()
        .expect("row_span selector");
    assert_eq!(spanned.len(), 9, "{:?}", names(&spanned));
}

#[test]
#[ignore]
fn column_headers_are_header_cells_without_a_position() {
    let app = fixture();
    let table = fruit_table(&app);
    let headers = table.column_headers().expect("column_headers");
    assert_eq!(names(&headers), ["Fruit", "Colour", "Qty"]);
    for h in &headers {
        let cell = h
            .cell
            .unwrap_or_else(|| panic!("header {:?} has no cell info", h.name));
        assert_eq!(cell.header, Some(HeaderKind::Column), "{:?}", h.name);
        // GTK 3 column headers have the header role but no TableCell.
        assert_eq!((cell.row, cell.column), (None, None), "{:?}", h.name);
    }
    assert!(
        table.row_headers().expect("row_headers").is_empty(),
        "a GtkTreeView has no row headers"
    );
}

#[test]
#[ignore]
fn a_cell_names_its_column_header() {
    let app = fixture();
    let yellow = one(
        &app,
        r#"table[name="Fruit table"] table_cell[name="Yellow"]"#,
    );
    let headers = yellow.column_header_cells().expect("column_header_cells");
    assert_eq!(names(&headers), ["Colour"]);
    assert!(yellow
        .row_header_cells()
        .expect("row_header_cells")
        .is_empty());
}

#[test]
#[ignore]
fn cell_at_reads_the_grid_and_answers_none_out_of_range() {
    let app = fixture();
    let table = fruit_table(&app);
    let cell = table
        .cell_at(2, 1)
        .expect("cell_at")
        .expect("(2, 1) is in range");
    assert_eq!(cell.name.as_deref(), Some("Dark red"));
    assert_eq!(cell.cell.and_then(|c| c.row), Some(2));
    assert_eq!(cell.cell.and_then(|c| c.column), Some(1));
    assert!(table.cell_at(3, 0).expect("cell_at").is_none());
    assert!(table.cell_at(0, 3).expect("cell_at").is_none());
}

#[test]
#[ignore]
fn selected_items_reports_the_selected_row() {
    let app = fixture();
    let table = fruit_table(&app);
    // rich.py selects row 1 (Banana) at startup and nothing changes it.
    let selected = table.selected_items().expect("selected_items");
    assert!(!selected.is_empty(), "row 1 is selected in the fixture");
    for item in &selected {
        assert_eq!(
            item.cell.and_then(|c| c.row),
            Some(1),
            "only row 1 is selected: {:?}",
            names(&selected)
        );
    }
    assert!(
        names(&selected).iter().any(|n| n == "Banana"),
        "{:?}",
        names(&selected)
    );
}

#[test]
#[ignore]
fn tree_table_rows_count_expanded_nodes() {
    let app = fixture();
    let tree = one(&app, r#"table[name="Aisle tree"]"#);
    let info = tree.table.expect("a tree view reports Table");
    assert_eq!(info.row_count, Some(Count::Known(4)), "{info:?}");
    let leek = one(&app, r#"table[name="Aisle tree"] table_cell[row="2"]"#);
    assert_eq!(leek.name.as_deref(), Some("Leek"));
    // GTK 3 reports no level or set position on tree cells (module docs);
    // they are table cells, not tree items, so nothing reads them.
    assert!(leek.collection.is_none(), "{:?}", leek.collection);
}

#[test]
#[ignore]
fn table_calls_on_a_non_table_are_refused() {
    let app = fixture();
    let button = one(&app, r#"button[name="Plain button"]"#);
    for (what, result) in [
        ("cell_at", button.cell_at(0, 0).map(|_| ())),
        ("column_headers", button.column_headers().map(|_| ())),
        (
            "column_header_cells",
            button.column_header_cells().map(|_| ()),
        ),
        ("selected_items", button.selected_items().map(|_| ())),
    ] {
        assert!(
            matches!(result, Err(Error::ActionNotSupported { .. })),
            "{what} on a button: {result:?}"
        );
    }
}

// ── Text ────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn text_length_and_reads_are_utf16() {
    let app = fixture();
    let text = rich_text(&app).text().expect("a text view has Text");
    // 70 code points, two of them outside the BMP.
    assert_eq!(text.length().expect("length"), 72);
    assert_eq!(text.read_all().expect("read_all"), TEXT);
    assert_eq!(text.read(r(12, 14)).expect("read"), "😀");
    assert_eq!(text.read(r(49, 55)).expect("read"), "𝄞 end");
    assert_eq!(text.read(r(57, 72)).expect("read"), "Final paragraph");
}

#[test]
#[ignore]
fn text_reads_reject_bad_ranges() {
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    for range in [r(13, 14), r(0, 73)] {
        let e = text.read(range).expect_err("invalid range");
        assert!(
            matches!(e, Error::InvalidTextRange { .. }),
            "read({range}): {e:?}"
        );
    }
}

#[test]
#[ignore]
fn locator_text_resolves_the_element() {
    let app = fixture();
    let text = app.locator(r#"text_area[name="Rich text"]"#).text();
    assert_eq!(text.length().expect("length"), 72);
    assert_eq!(text.find("Final").expect("find"), Some(r(57, 62)));
}

#[test]
#[ignore]
fn find_counts_utf16_and_honours_occurrence_and_within() {
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    assert_eq!(text.find("😀").expect("find"), Some(r(12, 14)));
    assert_eq!(text.find("𝄞").expect("find"), Some(r(49, 51)));
    assert_eq!(text.find("bold").expect("find"), Some(r(15, 19)));
    assert_eq!(text.find("absent").expect("find"), None);
    // `occurrence` is 0-based: the "d" of "wörld" is at 10, of "bold" at 18.
    assert_eq!(
        text.find_with("d", FindOptions::new().occurrence(1))
            .expect("find_with"),
        Some(r(18, 19))
    );
    assert_eq!(
        text.find_with("line", FindOptions::new().within(r(27, 56)))
            .expect("find_with"),
        Some(r(34, 38))
    );
    assert_eq!(
        text.find_with("Hello", FindOptions::new().within(r(27, 56)))
            .expect("find_with"),
        None
    );
}

#[test]
#[ignore]
fn range_at_maps_words_lines_and_characters() {
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    // Inside the surrogate pair: the whole emoji.
    assert_eq!(
        text.range_at(13, TextUnit::Character).expect("char"),
        r(12, 14)
    );
    // ATK words run to the next word start, so they keep their trailing
    // space, and Pango folds the emoji into the word before it.
    assert_eq!(text.range_at(7, TextUnit::Word).expect("word"), r(6, 15));
    assert_eq!(text.range_at(15, TextUnit::Word).expect("word"), r(15, 20));
    // Lines include their newline.
    assert_eq!(text.range_at(0, TextUnit::Line).expect("line"), r(0, 27));
    assert_eq!(text.range_at(30, TextUnit::Line).expect("line"), r(27, 56));
    // Inside a surrogate pair, every unit answers for the pair's start, as
    // on Qt (whose offsets are UTF-16 and need no conversion).
    assert_eq!(text.range_at(13, TextUnit::Line).expect("line"), r(0, 27));
    assert_eq!(text.range_at(13, TextUnit::Word).expect("word"), r(6, 15));
    // At the end: the line that ends there.
    assert_eq!(text.range_at(72, TextUnit::Line).expect("line"), r(57, 72));
    assert!(matches!(
        text.range_at(73, TextUnit::Word),
        Err(Error::InvalidTextRange { .. })
    ));
}

#[test]
#[ignore]
fn paragraphs_are_unsupported_by_gtk3_text_views() {
    // GtkTextView's ATK adapter answers GetStringAtOffset(PARAGRAPH) with
    // (-1, -1): it has no paragraph boundaries to report.
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    let e = text
        .range_at(0, TextUnit::Paragraph)
        .expect_err("GTK 3 reports no paragraphs");
    assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
}

#[test]
#[ignore]
fn lines_page_through_the_text() {
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    let all = text.lines(r(0, 72), 10).expect("lines");
    assert_eq!(all.lines, [r(0, 27), r(27, 56), r(56, 57), r(57, 72)]);
    assert!(!all.truncated);
    assert_eq!(all.next_start, None);

    let page = text.lines(r(0, 72), 2).expect("lines");
    assert_eq!(page.lines, [r(0, 27), r(27, 56)]);
    assert!(page.truncated);
    assert_eq!(page.next_start, Some(56));

    // Each line is clipped to the range asked for.
    let clipped = text.lines(r(20, 30), 10).expect("lines");
    assert_eq!(clipped.lines, [r(20, 27), r(27, 30)]);
}

#[test]
#[ignore]
fn bounds_are_one_rect_per_line_inside_the_view() {
    let app = fixture();
    let view = rich_text(&app);
    let outer = view.bounds.expect("the text view has bounds");
    let text = view.text().expect("text");

    let word = text.bounds(r(0, 5)).expect("bounds");
    assert_eq!(word.len(), 1, "{word:?}");
    let hello = word[0];
    assert!(hello.width > 0 && hello.height > 0, "{hello:?}");
    let inside = |b: &Rect| {
        b.x >= outer.x
            && b.y >= outer.y
            && b.x + b.width as i32 <= outer.x + outer.width as i32
            && b.y + b.height as i32 <= outer.y + outer.height as i32
    };
    assert!(inside(&hello), "{hello:?} outside {outer:?}");

    let lines = text.bounds(r(0, 56)).expect("bounds");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].y > lines[0].y, "{lines:?}");
    assert!(lines.iter().all(inside), "{lines:?} outside {outer:?}");
}

#[test]
#[ignore]
fn runs_report_the_tagged_formatting() {
    let app = fixture();
    let text = rich_text(&app).text().expect("text");
    let runs = text.runs(r(0, 72), 50).expect("runs");
    assert!(!runs.truncated);
    let ranges: Vec<TextRange> = runs.runs.iter().map(|run| run.range).collect();
    assert_eq!(
        ranges,
        [
            r(0, 15),
            r(15, 19),
            r(19, 20),
            r(20, 26),
            r(26, 39),
            r(39, 48),
            r(48, 72)
        ]
    );
    let at = |i: usize| &runs.runs[i].attributes;

    let plain = at(0);
    assert_eq!(plain.font_weight, Some(400));
    assert_eq!(plain.italic, Some(false));
    assert_eq!(plain.underline, Some(false));
    assert_eq!(plain.strikethrough, Some(false));
    assert!(plain.font_size.is_some_and(|s| s > 0.0), "{plain:?}");
    assert!(
        plain.font_family.as_deref().is_some_and(|f| !f.is_empty()),
        "{plain:?}"
    );
    assert_eq!(
        plain.foreground.map(|c| c.to_hex()).as_deref(),
        Some("#000000")
    );

    assert_eq!(at(1).font_weight, Some(700), "bold");
    assert_eq!(at(3).italic, Some(true), "italic");
    assert_eq!(at(5).underline, Some(true), "under red");
    assert_eq!(
        at(5).foreground.map(|c| c.to_hex()).as_deref(),
        Some("#ff0000")
    );

    // Paged: two runs, then resume where it stopped.
    let page = text.runs(r(0, 72), 2).expect("runs");
    assert!(page.truncated);
    assert_eq!(page.next_start, Some(19));
    // Clipped to the range asked for.
    let clipped = text.runs(r(16, 22), 10).expect("runs");
    let ranges: Vec<TextRange> = clipped.runs.iter().map(|run| run.range).collect();
    assert_eq!(ranges, [r(16, 19), r(19, 20), r(20, 22)]);
}

#[test]
#[ignore]
fn select_text_takes_utf16_offsets() {
    let app = fixture();
    let view = rich_text(&app);
    let text = view.text().expect("text");

    view.select_text(12, 14).expect("select the emoji");
    assert_eq!(text.selections().expect("selections"), [r(12, 14)]);
    let caret = text
        .caret()
        .expect("caret")
        .expect("a text view has a caret");
    assert!(
        caret == 12 || caret == 14,
        "caret {caret} not at a selection end"
    );

    view.select_text(49, 55)
        .expect("select past the first emoji");
    let selections = text.selections().expect("selections");
    assert_eq!(selections, [r(49, 55)]);
    assert_eq!(text.read(selections[0]).expect("read"), "𝄞 end");

    // An empty selection is a caret, after both non-BMP characters.
    view.select_text(57, 57).expect("place the caret");
    assert!(text.selections().expect("selections").is_empty());
    assert_eq!(text.caret().expect("caret"), Some(57));

    for (start, end) in [(13, 14), (0, 73)] {
        let e = view.select_text(start, end).expect_err("invalid range");
        assert!(
            matches!(e, Error::InvalidTextRange { .. }),
            "select_text({start}, {end}): {e:?}"
        );
    }
}

/// The fixture's second text view, which this test is the only one to edit.
fn edit_text(app: &App) -> Element {
    one(app, r#"text_area[name="Edit text"]"#)
}

/// Wait for an event on the "Edit text" view matching `want`, naming what
/// arrived instead when none does.
fn wait_on_edit_view(sub: &Subscription, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let seen = std::sync::Mutex::new(Vec::new());
    sub.wait_for(
        |e| {
            let on_view = e
                .target
                .as_ref()
                .is_some_and(|t| t.name.as_deref() == Some("Edit text"));
            seen.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("{:?} {:?}", e.kind, e.text_change));
            on_view && want(e)
        },
        Duration::from_secs(5),
    )
    .unwrap_or_else(|err| {
        panic!(
            "no {what} event on the edit view: {err}; received: {:?}",
            seen.lock().unwrap_or_else(|p| p.into_inner())
        )
    })
}

#[test]
#[ignore]
fn text_edits_and_caret_moves_arrive_as_text_events() {
    let app = fixture();
    let view = edit_text(&app);
    // Reset first, so a rerun against the same fixture starts from "ab".
    view.set_value("ab").expect("reset the edit view");

    let sub = app.subscribe().expect("subscribe to the fixture");
    std::thread::sleep(Duration::from_millis(300));
    while sub.try_recv().is_some() {}

    // A caret move is a text selection event, not an item selection one.
    // Replacing the contents left the caret at the end, so move it to the
    // start: a no-op move raises nothing.
    view.select_text(0, 0)
        .expect("place the caret at the start");
    let caret = wait_on_edit_view(&sub, "caret", |e| e.kind == EventKind::TextSelectionChanged);
    assert!(caret.text_change.is_none(), "{caret:?}");
    assert_eq!(
        view.text().expect("text").caret().expect("caret"),
        Some(0),
        "the event carries no offset; the caret is read from the element"
    );

    // An insertion carries its text. GTK counts code points, so the range is
    // left out rather than converted against text that may have moved on.
    view.type_text("😀x").expect("insert at the caret");
    let inserted = wait_on_edit_view(&sub, "insertion", |e| {
        e.kind == EventKind::TextChanged && e.text_change.is_some()
    });
    let change = inserted.text_change.expect("checked by the predicate");
    assert_eq!(change.kind, TextChangeKind::Inserted, "{change:?}");
    assert_eq!(change.text.as_deref(), Some("😀x"), "{change:?}");
    assert_eq!(change.range, None, "GTK reports code points: {change:?}");

    // A selection change is the same kind as a caret move.
    view.select_text(0, 2).expect("select the first two units");
    wait_on_edit_view(&sub, "selection", |e| {
        e.kind == EventKind::TextSelectionChanged
    });

    // Replacing the contents deletes what was there.
    view.set_value("").expect("clear the edit view");
    let deleted = wait_on_edit_view(&sub, "deletion", |e| {
        e.text_change
            .as_ref()
            .is_some_and(|c| c.kind == TextChangeKind::Deleted)
    });
    let change = deleted.text_change.expect("checked by the predicate");
    assert_eq!(change.text.as_deref(), Some("😀xab"), "{change:?}");
    assert_eq!(change.range, None, "{change:?}");
}

#[test]
#[ignore]
fn text_on_a_button_is_refused() {
    let app = fixture();
    let button = one(&app, r#"button[name="Plain button"]"#);
    match button.text() {
        Err(Error::ActionNotSupported { action, role }) => {
            assert_eq!(action, "text");
            assert_eq!(role, Role::Button);
        }
        other => panic!("text() on a button: {other:?}"),
    }
}

// ── Capture ─────────────────────────────────────────────────────────

#[test]
#[ignore]
fn capture_formatting_detail_reads_the_same_runs_as_text_runs() {
    let app = fixture();
    let view = rich_text(&app);
    let direct = view.text().expect("text").runs(r(0, 72), 50).expect("runs");

    let cap = view
        .capture(&CaptureOptions::new().max_depth(0).formatting(true))
        .expect("capture");
    assert_eq!(cap.backend, Some("atspi"));
    assert!(!cap.truncated.any(), "{:?}", cap.truncated);
    let text = cap.nodes[0].text.as_ref().expect("the view has text");
    assert_eq!(text.content, TEXT);
    let runs = text.runs.as_ref().expect("formatting detail reads runs");
    assert!(!text.runs_truncated);
    let got: Vec<TextRange> = runs.iter().map(|run| run.range).collect();
    let want: Vec<TextRange> = direct.runs.iter().map(|run| run.range).collect();
    assert_eq!(got, want);
    assert_eq!(runs[1].attributes.font_weight, Some(700), "bold");
    assert_eq!(cap.stats.formatting_reads, runs.len());
    assert_eq!(cap.stats.text_reads, 1);

    let cut = view
        .capture(
            &CaptureOptions::new()
                .max_depth(0)
                .formatting(true)
                .max_runs(2),
        )
        .expect("capture");
    assert!(cut.truncated.by_formatting);
    let text = cut.nodes[0].text.as_ref().expect("text");
    assert!(text.runs_truncated);
    assert_eq!(text.runs.as_ref().map(Vec::len), Some(2));
}
