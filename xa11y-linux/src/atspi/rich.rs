//! Tables, collections and text by range over AT-SPI.
//!
//! # Table metadata
//!
//! Read while building each element, gated on what `GetInterfaces` already
//! reported, so a node without `Table` / `TableCell` costs nothing extra:
//!
//! - `Table.NRows` / `NColumns` → [`TableInfo`] (negative means unknown).
//! - `TableCell.GetRowColumnSpan` → [`CellInfo`] row, column and spans.
//! - The `column header` / `row header` roles → [`CellInfo::header`]. GTK 3
//!   tree-view column headers have the role but no `TableCell` interface, so
//!   they carry a header kind and no position.
//! - Object attributes `posinset` / `setsize` / `level` (1-based, as in ARIA)
//!   → [`CollectionInfo`], read only for list items, tree items, tabs, radio
//!   buttons and rows.
//!
//! AT-SPI has no row-index API for a row object, so [`RowInfo`] is never
//! populated on Linux; a row's cells carry the row.
//!
//! Like every other per-field read in the element snapshot, a read that fails
//! leaves its field `None` rather than failing the whole element.
//!
//! # Text offsets
//!
//! The public unit is UTF-16 code units. The AT-SPI specification counts
//! characters (code points), which GTK, Chromium, Gecko and LibreOffice
//! follow; Qt's adaptor counts UTF-16 code units instead. The unit is chosen
//! from `Application.ToolkitName` ([`offset_unit_for`]). For code-point
//! toolkits every conversion reads the text before the offset, which is the
//! accepted cost of one public unit.
//!
//! # Line ends
//!
//! A rendered line or paragraph includes the line terminator that ends it,
//! as GTK reports it. Qt's `GetStringAtOffset` leaves the terminator out of
//! every line and, asked at the terminator itself, answers the line before
//! it, a range that does not contain the offset asked about. The terminator
//! is appended to such a line ([`include_line_terminator`]), so a line walk
//! advances and every toolkit reports the same lines.

use xa11y_core::table::index_from_one_based;
use xa11y_core::text_api::{
    char_to_utf16_offset, check_in_bounds, slice_utf16, utf16_len, utf16_to_char_offset, Color,
    TextAttributeParts, TextRange, TextRun, TextUnit,
};
use xa11y_core::{
    CellInfo, CellParts, CollectionInfo, CollectionParts, Count, HeaderKind, RowInfo, TableInfo,
    TableParts,
};

use super::*;

/// What [`LinuxProvider::read_table_metadata`] found for one element.
#[derive(Default)]
pub(super) struct TableMeta {
    pub(super) table: Option<TableInfo>,
    pub(super) cell: Option<CellInfo>,
    pub(super) table_row: Option<RowInfo>,
    pub(super) collection: Option<CollectionInfo>,
}

/// The offset unit a toolkit's `org.a11y.atspi.Text` speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetUnit {
    /// The AT-SPI specification's unit: code points.
    CodePoints,
    /// Qt's adaptor: UTF-16 code units (issue #493).
    Utf16,
}

/// The offset unit for a toolkit name. `None` (no `ToolkitName` at all) is
/// the specification's code points.
pub(crate) fn offset_unit_for(toolkit: Option<&str>) -> OffsetUnit {
    match toolkit {
        Some(t) if t.eq_ignore_ascii_case("Qt") => OffsetUnit::Utf16,
        _ => OffsetUnit::CodePoints,
    }
}

/// Read `bus`'s offset unit from `proxy`, which must address the
/// application object's `org.a11y.atspi.Application` interface. Shared by
/// the provider's per-bus cache and the event subscription, which resolves
/// each application's unit once when it subscribes.
pub(crate) fn read_offset_unit(proxy: &Proxy<'_>, bus: &str) -> Result<OffsetUnit> {
    read_offset_unit_unless_gone(proxy, bus)?.ok_or_else(|| Error::Platform {
        code: -1,
        message: format!(
            "reading Application.ToolkitName of {bus} to choose the text offset unit failed: \
             the application has gone"
        ),
    })
}

/// [`read_offset_unit`], answering `None` when the application has left the
/// bus. The event subscription reads every sender's unit; an application
/// that exits between being found and being asked has no events to
/// interpret, and must not fail the whole subscription.
pub(crate) fn read_offset_unit_unless_gone(
    proxy: &Proxy<'_>,
    bus: &str,
) -> Result<Option<OffsetUnit>> {
    let toolkit = match proxy.get_property::<String>("ToolkitName") {
        Ok(name) => Some(name),
        Err(e) if is_property_absent(&e) => None,
        Err(e) if crate::atspi::is_gone(&e) => return Ok(None),
        Err(e) => {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "reading Application.ToolkitName of {bus} to choose the text offset \
                     unit failed: {e}"
                ),
            })
        }
    };
    Ok(Some(offset_unit_for(toolkit.as_deref())))
}

/// The header kind an AT-SPI role names, if any.
pub(super) fn header_kind_for(role_num: u32, role_name: &str) -> Option<HeaderKind> {
    match (role_num, role_name) {
        (10, _) | (_, "column header" | "table column header") => Some(HeaderKind::Column),
        (47, _) | (_, "row header" | "table row header") => Some(HeaderKind::Row),
        _ => None,
    }
}

/// Collection position from object attributes (1-based `posinset` /
/// `level`, `setsize` with `-1` for unknown).
pub(super) fn collection_from_attributes(
    attrs: &HashMap<String, String>,
) -> Option<CollectionInfo> {
    let int = |key: &str| attrs.get(key).and_then(|v| v.trim().parse::<i64>().ok());
    let info: CollectionInfo = CollectionParts {
        index: int("posinset").and_then(index_from_one_based),
        size: int("setsize").map(Count::from_signed),
        depth: int("level").and_then(index_from_one_based),
    }
    .into();
    (!info.is_empty()).then_some(info)
}

/// Roles whose elements can sit in a set.
fn is_collection_role(role: Role) -> bool {
    matches!(
        role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::RadioButton | Role::TableRow
    )
}

/// Parse an AT-SPI colour attribute: `"r,g,b"` with 8-bit channels (GTK 4)
/// or 16-bit channels (ATK, GTK 3). Values above 255 mark the 16-bit form.
pub(super) fn parse_atspi_color(v: &str) -> Option<Color> {
    let parts: Vec<u32> = v
        .split(',')
        .map(|p| p.trim().parse::<u32>().ok())
        .collect::<Option<_>>()?;
    let [r, g, b] = parts[..] else { return None };
    let wide = r > 255 || g > 255 || b > 255;
    let ch = |c: u32| -> Option<u8> {
        if wide {
            u8::try_from(c / 257).ok()
        } else {
            u8::try_from(c).ok()
        }
    };
    Some(Color {
        r: ch(r)?,
        g: ch(g)?,
        b: ch(b)?,
        a: 255,
    })
}

/// Map an AT-SPI attribute run to [`TextAttributeParts`]. Anything absent
/// stays `None`.
pub(super) fn attributes_from_atspi(attrs: &HashMap<String, String>) -> TextAttributeParts {
    let get = |k: &str| attrs.get(k).map(|s| s.trim());
    TextAttributeParts {
        font_family: get("family-name")
            .map(unquote_family)
            .filter(|s| !s.is_empty())
            .map(String::from),
        font_size: get("size").and_then(|s| s.parse::<f64>().ok()),
        font_weight: get("weight").and_then(parse_weight),
        italic: get("style").map(|s| s == "italic" || s == "oblique"),
        underline: get("underline").map(|s| s != "none" && s != "false"),
        strikethrough: get("strikethrough").map(|s| s == "true"),
        foreground: get("fg-color").and_then(parse_atspi_color),
        background: get("bg-color").and_then(parse_atspi_color),
        // ATK reports `rise` in Pango units with no stated baseline, so a
        // script position cannot be read reliably.
        script: None,
        spelling_error: get("invalid").map(|s| s == "spelling"),
    }
}

/// A font weight: numeric as ATK reports it, or the CSS keyword Qt sends
/// (`"bold"`; Qt leaves the attribute out for normal text). The relative
/// keywords `bolder` / `lighter` name no weight on their own, so they are
/// not reported.
fn parse_weight(v: &str) -> Option<u16> {
    match v {
        "normal" => Some(400),
        "bold" => Some(700),
        n => n.parse::<u16>().ok(),
    }
}

/// A family name without the CSS quotes Qt wraps it in (`"Sans Serif"`
/// with the quotes, from Qt's IAccessible2-style `font-family`).
fn unquote_family(v: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|&q| v.strip_prefix(q).and_then(|r| r.strip_suffix(q)))
        .unwrap_or(v)
        .trim()
}

/// Whether a UTF-16 unit ends a line. `\r\n` is handled by the caller.
fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// `line`, a line or paragraph reported by the toolkit, with the terminator
/// that follows it appended when the toolkit left it out (Qt; see the module
/// docs). A line whose last unit is already a terminator, or that is not
/// followed by one (a soft wrap, the end of the text), is returned as is.
pub(super) fn include_line_terminator(units: &[u16], line: TextRange) -> TextRange {
    let end = line.end as usize;
    let Some(&next) = units.get(end) else {
        return line;
    };
    let ends_with_terminator =
        end > line.start as usize && units.get(end - 1).is_some_and(|&u| is_line_terminator(u));
    if ends_with_terminator || !is_line_terminator(next) {
        return line;
    }
    let width = if next == 0x0D && units.get(end + 1) == Some(&0x0A) {
        2
    } else {
        1
    };
    TextRange {
        start: line.start,
        end: line.end + width,
    }
}

/// An `(so)` object reference reply, or `None` for AT-SPI's null object.
fn object_ref(reply: zbus::Message, what: &str) -> Result<Option<AccessibleRef>> {
    let (bus_name, path): (String, zbus::zvariant::OwnedObjectPath) =
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("{what} deserialize failed: {e}"),
        })?;
    Ok(live_ref(bus_name, path.to_string()))
}

fn live_ref(bus_name: String, path: String) -> Option<AccessibleRef> {
    if path == "/org/a11y/atspi/null" || bus_name.is_empty() || path.is_empty() {
        None
    } else {
        Some(AccessibleRef { bus_name, path })
    }
}

fn call_failed(what: &str, e: zbus::Error) -> Error {
    Error::Platform {
        code: -1,
        message: format!("{what} failed: {e}"),
    }
}

/// Most header objects [`LinuxProvider::rich_table_headers`] asks for, one per
/// row or column, before refusing.
const MAX_HEADER_PROBES: i32 = 1000;

/// A text object, its proxy, and the unit its toolkit counts in.
struct TextAccess<'a> {
    proxy: Proxy<'a>,
    unit: OffsetUnit,
}

impl TextAccess<'_> {
    fn native_count(&self) -> Result<i32> {
        self.proxy
            .get_property::<i32>("CharacterCount")
            .map_err(|e| call_failed("Text.CharacterCount", e))
    }

    /// `GetText(0, end)` in native units.
    fn prefix(&self, end: i32) -> Result<String> {
        let reply = self
            .proxy
            .call_method("GetText", &(0i32, end))
            .map_err(|e| call_failed("Text.GetText", e))?;
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Text.GetText deserialize failed: {e}"),
        })
    }

    fn full(&self) -> Result<String> {
        let count = self.native_count()?;
        self.prefix(count.max(0))
    }

    /// The text from 0 to at least UTF-16 offset `end` (to the end of the
    /// text if it is shorter), so a read near the start does not fetch the
    /// whole document. A code point is one or two UTF-16 units, so `end`
    /// code points always cover `end` units.
    fn prefix_covering(&self, end: u32) -> Result<String> {
        let count = self.native_count()?.max(0);
        let want = i32::try_from(end).unwrap_or(i32::MAX).min(count);
        let text = self.prefix(want)?;
        if utf16_len(&text) >= end || want == count {
            Ok(text)
        } else {
            // Only reachable if the toolkit's count disagrees with its text.
            self.prefix(count)
        }
    }

    /// The text of `range`, validated: past the end or splitting a surrogate
    /// pair fails with `InvalidTextRange`. Reads only up to `range.end`
    /// (plus one unit, to tell whether the end splits a pair).
    fn read_range(&self, range: TextRange) -> Result<String> {
        let text = self.prefix_covering(range.end.saturating_add(1))?;
        // A prefix no longer than `end` is the whole text, so only then is
        // its length the text's length.
        let whole = utf16_len(&text) <= range.end;
        slice_utf16(&text, range).map_err(|e| match e {
            Error::InvalidTextRange {
                start,
                end,
                length,
                message,
            } => Error::InvalidTextRange {
                start,
                end,
                length: if whole { length } else { None },
                message,
            },
            other => other,
        })
    }

    /// Convert native offsets to UTF-16, reading the text before the largest
    /// one when the toolkit counts code points.
    fn to_utf16(&self, native: &[i32]) -> Result<Vec<u32>> {
        let non_negative = |n: i32| {
            u32::try_from(n).map_err(|_| Error::Platform {
                code: -1,
                message: format!("toolkit reported a negative text offset {n}"),
            })
        };
        match self.unit {
            OffsetUnit::Utf16 => native.iter().map(|&n| non_negative(n)).collect(),
            OffsetUnit::CodePoints => {
                let max = native.iter().copied().max().unwrap_or(0);
                let prefix = self.prefix(max.max(0))?;
                native
                    .iter()
                    .map(|&n| char_to_utf16_offset(&prefix, non_negative(n)?))
                    .collect()
            }
        }
    }

    /// Convert a UTF-16 offset into `full` to the native unit.
    fn to_native(&self, full: &str, utf16: u32) -> Result<i32> {
        let n = match self.unit {
            OffsetUnit::Utf16 => utf16,
            OffsetUnit::CodePoints => utf16_to_char_offset(full, utf16)?,
        };
        i32::try_from(n).map_err(|_| Error::InvalidTextRange {
            start: utf16,
            end: utf16,
            length: Some(utf16_len(full)),
            message: "offset does not fit the platform's 32-bit offset".to_string(),
        })
    }
}

impl LinuxProvider {
    /// Table, cell and collection metadata for one element; see the module
    /// docs. Called from `build_element_data_in`.
    pub(super) fn read_table_metadata(
        &self,
        aref: &AccessibleRef,
        role: Role,
        role_num: u32,
        role_name: &str,
        ifaces: Interfaces,
    ) -> TableMeta {
        let table = ifaces.table.then(|| self.read_table_info(aref)).flatten();
        let header = header_kind_for(role_num, role_name);
        let cell = if ifaces.table_cell {
            self.read_cell_info(aref, header)
        } else {
            header.map(|h| {
                CellParts {
                    row: None,
                    column: None,
                    row_span: None,
                    column_span: None,
                    header: Some(h),
                }
                .into()
            })
        };
        let collection = if is_collection_role(role) {
            match self.scan_attributes(aref) {
                Ok(Some(Some(attrs))) => collection_from_attributes(&attrs),
                _ => None,
            }
        } else {
            None
        };
        TableMeta {
            table,
            cell,
            table_row: None,
            collection,
        }
    }

    fn read_table_info(&self, aref: &AccessibleRef) -> Option<TableInfo> {
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")
            .ok()?;
        let count = |p: &str| {
            proxy
                .get_property::<i32>(p)
                .ok()
                .map(|n| Count::from_signed(n.into()))
        };
        let info: TableInfo = TableParts {
            row_count: count("NRows"),
            column_count: count("NColumns"),
        }
        .into();
        (!info.is_empty()).then_some(info)
    }

    fn read_cell_info(&self, aref: &AccessibleRef, header: Option<HeaderKind>) -> Option<CellInfo> {
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.TableCell")
            .ok()?;
        let reply = proxy.call_method("GetRowColumnSpan", &()).ok();
        let spans: Option<(i32, i32, i32, i32)> = reply.and_then(|r| r.body().deserialize().ok());
        let index = |n: i32| u32::try_from(n).ok();
        let span = |n: i32| u32::try_from(n).ok().filter(|&s| s >= 1);
        let info: CellInfo = CellParts {
            row: spans.and_then(|s| index(s.0)),
            column: spans.and_then(|s| index(s.1)),
            row_span: spans.and_then(|s| span(s.2)),
            column_span: spans.and_then(|s| span(s.3)),
            header,
        }
        .into();
        (!info.is_empty()).then_some(info)
    }

    /// The live object behind `element`, refusing one without `iface`.
    fn with_interface(
        &self,
        element: &ElementData,
        iface: &'static str,
        action: &str,
    ) -> Result<AccessibleRef> {
        let aref = self.get_cached(element.handle)?;
        let ifaces = self.get_interfaces(&aref)?;
        let has = match iface {
            "org.a11y.atspi.Table" => ifaces.table,
            "org.a11y.atspi.TableCell" => ifaces.table_cell,
            "org.a11y.atspi.Selection" => ifaces.selection,
            "org.a11y.atspi.Text" => ifaces.text,
            other => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("no interface gate for {other}"),
                })
            }
        };
        if has {
            Ok(aref)
        } else {
            Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            })
        }
    }

    fn build_refs(&self, refs: Vec<AccessibleRef>, pid: Option<u32>) -> Vec<ElementData> {
        refs.iter()
            .map(|r| self.build_element_data(r, pid))
            .collect()
    }

    pub(super) fn rich_table_cell_at(
        &self,
        table: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        let aref = self.with_interface(table, "org.a11y.atspi.Table", "table")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")?;
        let rows: i32 = proxy
            .get_property("NRows")
            .map_err(|e| call_failed("Table.NRows", e))?;
        let cols: i32 = proxy
            .get_property("NColumns")
            .map_err(|e| call_failed("Table.NColumns", e))?;
        // Out of range is an answer, not an error; AT-SPI toolkits disagree on
        // what GetAccessibleAt returns there, so ask only in range.
        let (Ok(r), Ok(c)) = (i32::try_from(row), i32::try_from(column)) else {
            return Ok(None);
        };
        // A negative count is "unknown" (virtualized content): that axis has
        // no known bound, so the platform is asked rather than every slot
        // being answered as out of range.
        if (rows >= 0 && r >= rows) || (cols >= 0 && c >= cols) {
            return Ok(None);
        }
        let reply = proxy
            .call_method("GetAccessibleAt", &(r, c))
            .map_err(|e| call_failed("Table.GetAccessibleAt", e))?;
        Ok(object_ref(reply, "Table.GetAccessibleAt")?
            .map(|cell| self.build_element_data(&cell, table.pid)))
    }

    pub(super) fn rich_table_headers(
        &self,
        table: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(table, "org.a11y.atspi.Table", "table")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")?;
        let (count_prop, method) = match kind {
            HeaderKind::Column => ("NColumns", "GetColumnHeader"),
            HeaderKind::Row => ("NRows", "GetRowHeader"),
        };
        let n: i32 = proxy
            .get_property(count_prop)
            .map_err(|e| call_failed(count_prop, e))?;
        // A negative count is "unknown" (virtualized content): there is no
        // range to probe, and an empty answer would read as "no headers".
        if n < 0 {
            return Err(Error::Unsupported {
                feature: format!(
                    "{method} on a table whose {count_prop} is unknown ({n}); read the headers \
                     of individual cells with column_header_cells / row_header_cells"
                ),
            });
        }
        if n > MAX_HEADER_PROBES {
            return Err(Error::Unsupported {
                feature: format!(
                    "{method} over {n} {}s: more than {MAX_HEADER_PROBES}; read the headers \
                     of individual cells with column_header_cells / row_header_cells",
                    kind.to_snake_case()
                ),
            });
        }
        let mut refs = Vec::new();
        for i in 0..n.max(0) {
            let reply = proxy
                .call_method(method, &(i,))
                .map_err(|e| call_failed(method, e))?;
            if let Some(r) = object_ref(reply, method)? {
                // A header spanning several columns is returned once per column.
                if !refs
                    .iter()
                    .any(|x: &AccessibleRef| x.path == r.path && x.bus_name == r.bus_name)
                {
                    refs.push(r);
                }
            }
        }
        Ok(self.build_refs(refs, table.pid))
    }

    pub(super) fn rich_cell_headers(
        &self,
        cell: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(cell, "org.a11y.atspi.TableCell", "cell_headers")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.TableCell")?;
        let method = match kind {
            HeaderKind::Column => "GetColumnHeaderCells",
            HeaderKind::Row => "GetRowHeaderCells",
        };
        let reply = proxy
            .call_method(method, &())
            .map_err(|e| call_failed(method, e))?;
        let list: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("{method} deserialize failed: {e}"),
            })?;
        let refs = list
            .into_iter()
            .filter_map(|(b, p)| live_ref(b, p.to_string()))
            .collect();
        Ok(self.build_refs(refs, cell.pid))
    }

    pub(super) fn rich_selected_items(&self, container: &ElementData) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(container, "org.a11y.atspi.Selection", "selected_items")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Selection")?;
        let n: i32 = proxy
            .get_property("NSelectedChildren")
            .map_err(|e| call_failed("Selection.NSelectedChildren", e))?;
        // Each item is a full element build (about ten round trips), so a
        // select-all over a large view is refused rather than walked.
        if n > MAX_HEADER_PROBES {
            return Err(Error::Unsupported {
                feature: format!(
                    "selected_items over {n} selected children: more than {MAX_HEADER_PROBES}"
                ),
            });
        }
        let mut refs = Vec::new();
        for i in 0..n.max(0) {
            let reply = proxy
                .call_method("GetSelectedChild", &(i,))
                .map_err(|e| call_failed("Selection.GetSelectedChild", e))?;
            if let Some(r) = object_ref(reply, "Selection.GetSelectedChild")? {
                refs.push(r);
            }
        }
        Ok(self.build_refs(refs, container.pid))
    }

    // ── Text ────────────────────────────────────────────────────────

    /// The offset unit of `bus`'s toolkit, cached per bus.
    pub(super) fn offset_unit(&self, bus: &str) -> Result<OffsetUnit> {
        if let Some(unit) = self
            .gtk4
            .offset_units
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(bus)
        {
            return Ok(*unit);
        }
        let proxy = self.make_proxy(bus, APP_ROOT_PATH, "org.a11y.atspi.Application")?;
        let unit = read_offset_unit(&proxy, bus)?;
        let mut units = self
            .gtk4
            .offset_units
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Bus names are never reused, so entries for exited apps would pile
        // up in a long-running process; bound the map like `toplevels`.
        if units.len() >= GTK4_TOPLEVEL_CACHE_LIMIT {
            units.clear();
        }
        units.insert(bus.to_owned(), unit);
        Ok(unit)
    }

    fn text_access(&self, element: &ElementData) -> Result<TextAccess<'_>> {
        let aref = self.with_interface(element, "org.a11y.atspi.Text", "text")?;
        let unit = self.offset_unit(&aref.bus_name)?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Text")?;
        Ok(TextAccess { proxy, unit })
    }

    pub(super) fn rich_text_length(&self, element: &ElementData) -> Result<u32> {
        let t = self.text_access(element)?;
        match t.unit {
            OffsetUnit::Utf16 => Ok(t.to_utf16(&[t.native_count()?])?[0]),
            OffsetUnit::CodePoints => Ok(utf16_len(&t.full()?)),
        }
    }

    pub(super) fn rich_text_caret(&self, element: &ElementData) -> Result<Option<u32>> {
        let t = self.text_access(element)?;
        let caret: i32 = t
            .proxy
            .get_property("CaretOffset")
            .map_err(|e| call_failed("Text.CaretOffset", e))?;
        if caret < 0 {
            return Ok(None);
        }
        Ok(Some(t.to_utf16(&[caret])?[0]))
    }

    pub(super) fn rich_text_selections(&self, element: &ElementData) -> Result<Vec<TextRange>> {
        let t = self.text_access(element)?;
        let n: i32 = t
            .proxy
            .call_method("GetNSelections", &())
            .map_err(|e| call_failed("Text.GetNSelections", e))?
            .body()
            .deserialize()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetNSelections deserialize failed: {e}"),
            })?;
        let mut native = Vec::new();
        for i in 0..n.max(0) {
            let (start, end): (i32, i32) = t
                .proxy
                .call_method("GetSelection", &(i,))
                .map_err(|e| call_failed("Text.GetSelection", e))?
                .body()
                .deserialize()
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("Text.GetSelection deserialize failed: {e}"),
                })?;
            if start != end {
                native.push(start.min(end));
                native.push(start.max(end));
            }
        }
        let utf16 = t.to_utf16(&native)?;
        let mut ranges: Vec<TextRange> = utf16
            .chunks(2)
            .map(|p| TextRange {
                start: p[0],
                end: p[1],
            })
            .collect();
        ranges.sort_by_key(|r| r.start);
        Ok(ranges)
    }

    pub(super) fn rich_text_read(&self, element: &ElementData, range: TextRange) -> Result<String> {
        let t = self.text_access(element)?;
        t.read_range(range)
    }

    pub(super) fn rich_text_range_at(
        &self,
        element: &ElementData,
        offset: u32,
        unit: TextUnit,
    ) -> Result<TextRange> {
        let t = self.text_access(element)?;
        let full = t.full()?;
        let length = utf16_len(&full);
        check_in_bounds(
            TextRange {
                start: offset,
                end: offset,
            },
            length,
        )?;
        if length == 0 {
            return Ok(TextRange { start: 0, end: 0 });
        }
        // At the end, answer for the unit that ends there.
        let at = if offset == length {
            utf16_last_scalar_start(&full)
        } else {
            offset
        };
        // An offset on the second half of a surrogate pair answers for the
        // pair, for every unit and toolkit: GTK's code-point conversion would
        // otherwise refuse it while Qt's UTF-16 offsets pass it through.
        let units: Vec<u16> = full.encode_utf16().collect();
        let at = if at > 0 && (0xDC00..=0xDFFF).contains(&units[at as usize]) {
            at - 1
        } else {
            at
        };
        if unit == TextUnit::Character {
            // The scalar value at `at` is known from the text itself.
            let start = at;
            let width = if (0xD800..=0xDBFF).contains(&units[start as usize]) {
                2
            } else {
                1
            };
            return Ok(TextRange {
                start,
                end: (start + width).min(length),
            });
        }
        // ATSPI_TEXT_GRANULARITY_{WORD, LINE, PARAGRAPH}.
        let granularity: u32 = match unit {
            TextUnit::Word => 1,
            TextUnit::Line => 3,
            TextUnit::Paragraph => 4,
            TextUnit::Character => 0,
        };
        let native = t.to_native(&full, at)?;
        let reply = t
            .proxy
            .call_method("GetStringAtOffset", &(native, granularity))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::Unsupported {
                        feature: format!(
                            "Text.GetStringAtOffset is not implemented by this toolkit: {e}"
                        ),
                    }
                } else {
                    call_failed("Text.GetStringAtOffset", e)
                }
            })?;
        let (_text, start, end): (String, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetStringAtOffset deserialize failed: {e}"),
            })?;
        // `(-1, -1)` for an in-bounds offset is a toolkit with no notion of
        // this unit: GTK 3's text view answers it for paragraphs.
        if start < 0 || end < 0 {
            return Err(Error::Unsupported {
                feature: format!(
                    "Text.GetStringAtOffset({native}, {}) returned ({start}, {end}): this \
                     toolkit does not report {} boundaries",
                    unit.to_snake_case(),
                    unit.to_snake_case()
                ),
            });
        }
        let to16 = |n: i32| -> Result<u32> {
            let n = u32::try_from(n).map_err(|_| Error::Platform {
                code: -1,
                message: format!("Text.GetStringAtOffset reported a negative offset {n}"),
            })?;
            match t.unit {
                OffsetUnit::Utf16 => Ok(n),
                OffsetUnit::CodePoints => char_to_utf16_offset(&full, n),
            }
        };
        let range = TextRange {
            start: to16(start)?,
            end: to16(end)?,
        };
        Ok(match unit {
            TextUnit::Line | TextUnit::Paragraph => include_line_terminator(&units, range),
            TextUnit::Word | TextUnit::Character => range,
        })
    }

    pub(super) fn rich_text_range_bounds(
        &self,
        element: &ElementData,
        range: TextRange,
    ) -> Result<Option<Rect>> {
        let t = self.text_access(element)?;
        let full = t.full()?;
        check_in_bounds(range, utf16_len(&full))?;
        let start = t.to_native(&full, range.start)?;
        let end = t.to_native(&full, range.end)?;
        let aref = self.get_cached(element.handle)?;
        let gtk4 = self.is_gtk4(&aref.bus_name)?;
        // GTK 4's SCREEN coordinates are wrong (issue #487): ask for WINDOW
        // coordinates and add the window's origin, as element bounds do.
        let coord: u32 = if gtk4 { 1 } else { 0 };
        let reply = t
            .proxy
            .call_method("GetRangeExtents", &(start, end, coord))
            .map_err(|e| call_failed("Text.GetRangeExtents", e))?;
        let (x, y, w, h): (i32, i32, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetRangeExtents deserialize failed: {e}"),
            })?;
        let rect = if gtk4 {
            let Some((ox, oy)) = self.gtk4_window_origin(&aref) else {
                return Ok(None);
            };
            match (x.checked_add(ox), y.checked_add(oy)) {
                (Some(x), Some(y)) => extents_to_rect(x, y, w, h),
                _ => None,
            }
        } else {
            extents_to_rect(x, y, w, h)
        };
        Ok(rect.map(|r| r.to_logical(crate::scale::coordinate_scale())))
    }

    pub(super) fn rich_text_attributes_at(
        &self,
        element: &ElementData,
        offset: u32,
    ) -> Result<TextRun> {
        let t = self.text_access(element)?;
        let full = t.full()?;
        let length = utf16_len(&full);
        if offset >= length {
            return Err(Error::InvalidTextRange {
                start: offset,
                end: offset,
                length: Some(length),
                message: "no text at or after this offset".to_string(),
            });
        }
        let native = t.to_native(&full, offset)?;
        let reply = t
            .proxy
            .call_method("GetAttributeRun", &(native, true))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::Unsupported {
                        feature: format!(
                            "Text.GetAttributeRun is not implemented by this toolkit: {e}"
                        ),
                    }
                } else {
                    call_failed("Text.GetAttributeRun", e)
                }
            })?;
        let (attrs, start, end): (HashMap<String, String>, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetAttributeRun deserialize failed: {e}"),
            })?;
        if start < 0 || end < start {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "Text.GetAttributeRun at {native} reported the run {start}..{end}"
                ),
            });
        }
        let mut ends = t.to_utf16(&[start, end])?;
        let (end16, start16) = (ends.pop(), ends.pop());
        let (Some(start16), Some(end16)) = (start16, end16) else {
            return Err(Error::Platform {
                code: -1,
                message: "Text.GetAttributeRun range did not convert".to_string(),
            });
        };
        Ok(TextRun::new(
            TextRange {
                start: start16,
                end: end16,
            },
            attributes_from_atspi(&attrs).into(),
        ))
    }

    /// `Text.SetSelection` with UTF-16 offsets.
    pub(super) fn rich_set_text_selection(
        &self,
        element: &ElementData,
        start: u32,
        end: u32,
    ) -> Result<()> {
        let t = self.text_access(element)?;
        let full = t.full()?;
        let range = TextRange { start, end };
        // Validate exactly as a read would, then convert.
        slice_utf16(&full, range)?;
        let native_start = t.to_native(&full, start)?;
        let native_end = t.to_native(&full, end)?;
        let target = self.get_cached(element.handle)?;
        // `SetSelection(0, ..)` changes selection 0, so it needs one to exist:
        // GTK 3 answers FALSE when nothing is selected. `AddSelection` is the
        // call that creates one. Which applies is a state read, not a retry.
        let existing: i32 = t
            .proxy
            .call_method("GetNSelections", &())
            .map_err(|e| call_failed("Text.GetNSelections", e))?
            .body()
            .deserialize()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetNSelections deserialize failed: {e}"),
            })?;
        let (member, reply) = if existing > 0 {
            let reply = t
                .proxy
                .call_method("SetSelection", &(0i32, native_start, native_end))
                .map_err(|e| call_failed("Text.SetSelection", e))?;
            ("SetSelection", reply)
        } else {
            let reply = t
                .proxy
                .call_method("AddSelection", &(native_start, native_end))
                .map_err(|e| call_failed("Text.AddSelection", e))?;
            ("AddSelection", reply)
        };
        atspi_bool_reply(
            reply,
            &format!("Text.{member}({start}, {end})"),
            &target,
            element,
        )
    }
}

/// UTF-16 offset of the last scalar value of a non-empty string.
fn utf16_last_scalar_start(s: &str) -> u32 {
    let len = utf16_len(s);
    let last = s.chars().last().map(|c| c.len_utf16() as u32).unwrap_or(0);
    len - last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_counts_utf16_and_everything_else_code_points() {
        assert_eq!(offset_unit_for(Some("Qt")), OffsetUnit::Utf16);
        assert_eq!(offset_unit_for(Some("GTK")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(Some("Chromium")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(Some("Gecko")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(None), OffsetUnit::CodePoints);
    }

    #[test]
    fn header_roles() {
        assert_eq!(header_kind_for(10, ""), Some(HeaderKind::Column));
        assert_eq!(
            header_kind_for(0, "table row header"),
            Some(HeaderKind::Row)
        );
        assert_eq!(header_kind_for(56, "table cell"), None);
    }

    #[test]
    fn collection_attributes_are_one_based_and_minus_one_is_unknown() {
        let attrs: HashMap<String, String> = [("posinset", "3"), ("setsize", "-1"), ("level", "2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let c = collection_from_attributes(&attrs).unwrap();
        assert_eq!(
            (c.index, c.size, c.depth),
            (Some(2), Some(Count::Unknown), Some(1))
        );
        assert!(collection_from_attributes(&HashMap::new()).is_none());
    }

    #[test]
    fn colours_in_both_widths() {
        assert_eq!(
            parse_atspi_color("65535,0,32896").unwrap().to_hex(),
            "#ff0080"
        );
        assert_eq!(parse_atspi_color("255,0,128").unwrap().to_hex(), "#ff0080");
        assert!(parse_atspi_color("1,2").is_none());
    }

    #[test]
    fn attribute_runs_leave_absent_fields_unreported() {
        let attrs: HashMap<String, String> = [("weight", "700"), ("style", "italic")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let a: xa11y_core::TextAttributes = attributes_from_atspi(&attrs).into();
        assert_eq!(a.font_weight, Some(700));
        assert_eq!(a.italic, Some(true));
        assert_eq!(a.underline, None);
        assert_eq!(a.spelling_error, None);
    }

    #[test]
    fn qt_weight_keywords_and_quoted_families() {
        // What Qt 6.12 sends for a bold run (captured over D-Bus).
        let attrs: HashMap<String, String> = [
            ("family-name", "\"Sans Serif\""),
            ("size", "9"),
            ("style", "normal"),
            ("weight", "bold"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let a: xa11y_core::TextAttributes = attributes_from_atspi(&attrs).into();
        assert_eq!(a.font_family.as_deref(), Some("Sans Serif"));
        assert_eq!(a.font_weight, Some(700));
        assert_eq!(a.font_size, Some(9.0));
        assert_eq!(a.italic, Some(false));
        assert_eq!(parse_weight("normal"), Some(400));
        assert_eq!(parse_weight("bolder"), None);
        assert_eq!(unquote_family("'Noto Sans'"), "Noto Sans");
        assert_eq!(unquote_family("Cantarell"), "Cantarell");
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_line_without_its_terminator_gets_it() {
        // Qt: "Hi 😀 wörld 𝄞\nsecond line" reports the first line as 0..14,
        // also when asked at the newline (14) itself.
        let text = u16s("Hi \u{1F600} w\u{f6}rld \u{1D11E}\nsecond line");
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 0, end: 14 }),
            TextRange { start: 0, end: 15 }
        );
        // The last line has no terminator to take.
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 15, end: 26 }),
            TextRange { start: 15, end: 26 }
        );
        // `\r\n` is one terminator.
        let crlf = u16s("ab\r\ncd");
        assert_eq!(
            include_line_terminator(&crlf, TextRange { start: 0, end: 2 }),
            TextRange { start: 0, end: 4 }
        );
        // An empty line between two newlines.
        let blank = u16s("a\n\nb");
        assert_eq!(
            include_line_terminator(&blank, TextRange { start: 2, end: 2 }),
            TextRange { start: 2, end: 3 }
        );
    }

    #[test]
    fn a_line_that_has_its_terminator_is_left_alone() {
        // GTK: "a\n\nb" reports 0..2 and then the blank line 2..3; the
        // newline after the first line is the blank line's, not its own.
        let text = u16s("a\n\nb");
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 0, end: 2 }),
            TextRange { start: 0, end: 2 }
        );
        // A soft-wrapped line is followed by text, not a terminator.
        let wrapped = u16s("one two\n");
        assert_eq!(
            include_line_terminator(&wrapped, TextRange { start: 0, end: 4 }),
            TextRange { start: 0, end: 4 }
        );
    }

    #[test]
    fn last_scalar_start() {
        assert_eq!(utf16_last_scalar_start("ab"), 1);
        assert_eq!(utf16_last_scalar_start("a😀"), 1);
    }
}
