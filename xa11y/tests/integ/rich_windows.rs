//! Real WPF Grid/Table/Selection and text patterns, launched by the Windows harness.
use std::time::Duration;
use xa11y::text_api::{TextRange, TextUnit};
use xa11y::*;

fn fixture() -> App {
    let pid = std::env::var("XA11Y_WINDOWS_RICH_PID")
        .expect("Windows harness must set the rich fixture PID")
        .parse()
        .expect("fixture PID must be numeric");
    App::by_pid(pid, Duration::from_secs(5)).expect("Windows rich fixture must be running")
}

fn r(start: u32, end: u32) -> TextRange {
    TextRange { start, end }
}

#[test]
#[ignore]
fn windows_rich_tables_and_headers() {
    let app = fixture();
    let grid = crate::integ::one(&app, r#"table[name="Rich grid"]"#);
    let info = grid.table.expect("grid metadata");
    assert_eq!(info.row_count.and_then(|n| n.known()), Some(2));
    assert_eq!(info.column_count.and_then(|n| n.known()), Some(2));
    let cell = grid.cell_at(0, 0).expect("cell_at").expect("first cell");
    let info = cell.cell.expect("cell metadata");
    assert_eq!(
        (info.row, info.column, info.row_span, info.column_span),
        (Some(0), Some(0), Some(1), Some(1))
    );
    assert!(grid.cell_at(2, 0).expect("outside table").is_none());
    assert!(grid.cell_at(u32::MAX, 0).expect("large index").is_none());
    let columns = grid.column_headers().expect("column headers");
    assert_eq!(columns.len(), 2);
    assert!(columns.iter().all(|e| e
        .cell
        .is_some_and(|c| c.header() == Some(xa11y::table::HeaderKind::Column))));
    assert_eq!(
        app.locator(r#"table[name="Rich grid"] table_cell[header="column"]"#)
            .elements()
            .expect("header selector")
            .len(),
        2
    );
    let headers = cell.column_header_cells().expect("cell headers");
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].name.as_deref(), Some("Name"));
    grid.row_headers().expect("row headers");
    cell.row_header_cells().expect("cell row headers");
    let matched = app
        .locator(r#"table[name="Rich grid"] table_cell[row="0"][column="0"]"#)
        .elements()
        .expect("numeric selector");
    assert_eq!(matched.len(), 1);
}

#[test]
#[ignore]
fn windows_rich_selected_items() {
    let app = fixture();
    let list = crate::integ::one(&app, r#"list[name="Rich list"]"#);
    let selected = list.selected_items().expect("selected items");
    let names: Vec<_> = selected.iter().map(|e| e.name.as_deref()).collect();
    assert_eq!(names, [Some("Apple"), Some("Cherry")]);
}

#[test]
#[ignore]
fn windows_rich_returned_cells_keep_pid_and_minimized_visibility() {
    let app = fixture();
    let window = crate::integ::one(&app, r#"[name="xa11y Windows Rich Fixture"]"#);
    let grid = crate::integ::one(&app, r#"table[name="Rich grid"]"#);
    window.minimize().expect("minimize fixture");
    let result = grid.cell_at(0, 0);
    window.restore().expect("restore fixture");
    let cell = result
        .expect("cell in minimized window")
        .expect("cell exists");
    assert_eq!(cell.pid, grid.pid);
    assert!(
        !cell.states.visible,
        "a returned cell inherits its minimized host"
    );
}

#[test]
#[ignore]
fn windows_rich_collection_indices_and_depth() {
    let app = crate::integ::app_root();
    for (index, name) in ["Apple", "Banana", "Cherry"].iter().enumerate() {
        let item = crate::integ::one(&app, &format!(r#"list_item[name="{name}"]"#));
        let info = item.collection.expect("collection metadata");
        assert_eq!(info.index, Some(index as u32));
        assert_eq!(info.size.and_then(|n| n.known()), Some(3));
    }
    for (name, depth) in [("Root Item", 0), ("Child Item", 1)] {
        let item = crate::integ::one(&app, &format!(r#"[name="{name}"]"#));
        assert_eq!(item.collection.expect("tree metadata").depth, Some(depth));
    }
}

#[test]
#[ignore]
fn windows_rich_utf16_ranges_and_selection() {
    let app = fixture();
    let edit = crate::integ::one(&app, r#"text_field[name="Edit text"]"#);
    let text = edit.text();
    let all = text.read_all().expect("read all");
    assert!(all.starts_with("Hello 😀 world"), "{all:?}");
    assert_eq!(
        text.length().expect("length"),
        all.encode_utf16().count() as u32
    );
    assert_eq!(text.read(r(6, 8)).expect("emoji"), "😀");
    assert_eq!(
        text.range_at(6, TextUnit::Character).expect("character"),
        r(6, 8)
    );
    assert_eq!(text.read(r(9, 14)).expect("after emoji"), "world");
    assert_eq!(
        text.find("world").expect("find after emoji"),
        Some(r(9, 14))
    );
    assert_eq!(text.read(r(14, 16)).expect("line separator"), "\r\n");
    let length = text.length().expect("length");
    for range in [r(7, 8), r(0, length + 1), r(8, 6), r(u32::MAX, u32::MAX)] {
        assert!(
            matches!(text.read(range), Err(Error::InvalidTextRange { .. })),
            "read {range:?}"
        );
        assert!(
            matches!(
                edit.select_text(range.start, range.end),
                Err(Error::InvalidTextRange { .. })
            ),
            "select {range:?}"
        );
    }
    edit.select_text(6, 8).expect("select emoji");
    assert_eq!(text.selections().expect("selection"), [r(6, 8)]);
    edit.select_text(9, 9).expect("collapse");
    assert!(text.selections().expect("collapsed selection").is_empty());
    edit.focus().expect("focus edit");
    assert_eq!(text.caret().expect("caret"), Some(9));
    let word = text.range_at(10, TextUnit::Word).expect("word range");
    assert!(text.read(word).expect("word text").starts_with("world"));
    let lines = text.lines(r(0, length), 1).expect("line page");
    assert_eq!(lines.lines.len(), 1);
    assert!(lines.truncated);
    assert!(lines.next_start.is_some_and(|n| n > 0 && n < length));
    let rest = text
        .lines(r(lines.next_start.expect("next line"), length), 10)
        .expect("rest of lines");
    assert!(!rest.truncated);
    let bounds = text.bounds(r(0, 5)).expect("bounds");
    assert_eq!(bounds.len(), 1);
    assert!(bounds[0].width > 0 && bounds[0].height > 0);
    let both_lines = text.bounds(r(0, length)).expect("bounds across lines");
    assert_eq!(both_lines.len(), 2);
    assert!(both_lines[1].y > both_lines[0].y);
    let paragraph = text.range_at(0, TextUnit::Paragraph).expect("paragraph");
    assert!(paragraph.start == 0 && paragraph.end >= 16);
}

#[test]
#[ignore]
fn windows_rich_formatting() {
    let app = fixture();
    let view = crate::integ::one(&app, r#"[name="Rich text"]"#);
    let text = view.text();
    let all = text.read_all().expect("rich text");
    assert!(all.starts_with("Hello bold italic red"), "{all:?}");
    let length = text.length().expect("length");
    let runs = text.runs(r(0, length), 30).expect("formatting runs");
    assert!(!runs.truncated);
    let at = |offset| {
        &runs
            .runs
            .iter()
            .find(|run| run.range.start <= offset && offset < run.range.end)
            .expect("run at offset")
            .attributes
    };
    assert_eq!(at(6).font_weight, Some(700));
    assert_eq!(at(21).font_weight, Some(700), "one-character bold run");
    assert_eq!(at(22).font_weight, Some(400), "plain run after bold");
    assert_eq!(at(11).italic, Some(true));
    assert_eq!(at(18).underline, Some(true));
    assert_eq!(
        at(18).foreground.map(|c| c.to_hex()).as_deref(),
        Some("#ff0000")
    );
    assert!(at(0).font_size.is_some_and(|s| s > 0.0));
    let page = text.runs(r(0, length), 1).expect("run page");
    assert!(page.truncated);
    assert!(page.next_start.is_some_and(|n| n > 0 && n < length));
}

#[test]
#[ignore]
fn windows_rich_paragraph_breaks_and_document_end() {
    let app = fixture();
    let view = crate::integ::one(&app, r#"[name="Rich text"]"#);
    let text = view.text();
    let all = text.read_all().expect("document text");
    assert!(all.contains("\r\nSecond paragraph"), "{all:?}");
    assert!(all.ends_with("\r\n"), "{all:?}");
    let length = text.length().expect("length");
    let runs = text
        .runs(r(0, length), 100)
        .expect("runs over paragraph breaks");
    assert!(!runs.truncated);
    assert_eq!(runs.runs.last().expect("final run").range.end, length);
    view.select_text(0, length)
        .expect("select complete document");
    assert_eq!(
        text.selections().expect("complete selection"),
        [r(0, length)]
    );
    view.select_text(length, length)
        .expect("caret at document end");
    assert!(text.selections().expect("end selection").is_empty());
    for unit in [TextUnit::Word, TextUnit::Line, TextUnit::Paragraph] {
        let last = text.range_at(length, unit).expect("unit at document end");
        assert!(
            last.end <= length && last.end > last.start,
            "{unit:?}: {last:?}"
        );
    }
    let lines = text.lines(r(0, length), 100).expect("all lines");
    assert!(!lines.truncated);
    assert_eq!(lines.lines.last().expect("last line").end, length);
    assert!(
        text.bounds(r(0, length))
            .expect("bounds through final CRLF")
            .len()
            >= 2
    );
    let cr = all.find("\r\n").expect("paragraph separator");
    let between = all[..cr].encode_utf16().count() as u32 + 1;
    assert!(matches!(
        view.select_text(between, between),
        Err(Error::Unsupported { .. })
    ));
    assert_eq!(
        text.read(r(between, between + 1))
            .expect("read inside CRLF"),
        "\n"
    );
}

#[test]
#[ignore]
fn windows_rich_large_document_offsets_and_search() {
    let app = fixture();
    let edit = crate::integ::one(&app, r#"text_field[name="Long text"]"#);
    let text = edit.text();
    let length = text.length().expect("large text length");
    assert_eq!(length, 6_014);
    assert_eq!(
        text.read(r(6_000, length)).expect("last chunk"),
        "needle 😀 tail"
    );
    assert_eq!(
        text.find("needle 😀").expect("search near end"),
        Some(r(6_000, 6_009))
    );
    assert!(text
        .find("absent")
        .expect("search whole document")
        .is_none());
    assert_eq!(
        text.find_with("line", xa11y::text_api::FindOptions::new().occurrence(999))
            .expect("last repeated match"),
        Some(r(5_994, 5_998))
    );
    edit.select_text(6_000, 6_009)
        .expect("selection after 1k CRLFs");
    assert_eq!(
        text.selections().expect("large document selection"),
        [r(6_000, 6_009)]
    );
    let word = text
        .range_at(6_001, TextUnit::Word)
        .expect("word after 1k CRLFs");
    assert!(text
        .read(word)
        .expect("large document word")
        .starts_with("needle"));
}

#[test]
#[ignore]
fn windows_rich_text_selection_events() {
    let app = fixture();
    let edit = crate::integ::one(&app, r#"text_field[name="Edit text"]"#);
    let sub = app.subscribe().expect("subscribe");
    edit.select_text(1, 3).expect("select text");
    sub.wait_for(
        |e| e.kind == EventKind::TextSelectionChanged,
        Duration::from_secs(5),
    )
    .expect("text selection event");
}

#[test]
#[ignore]
fn windows_rich_button_refuses_text() {
    let app = fixture();
    let button = crate::integ::one(&app, r#"button[name="Plain button"]"#);
    assert!(matches!(
        button.text().length(),
        Err(Error::ActionNotSupported { .. })
    ));
    assert!(button.table.is_none());
    assert!(button.cell.is_none());
    assert!(button.collection.is_none());
}
