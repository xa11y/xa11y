//! Native UTF-16 IA2 Text/Text2 and Table2/TableCell contracts.
use super::*;
use std::ffi::c_void;
use windows::Win32::System::Com::CoTaskMemFree;

pub(super) fn metadata(node: &Node, role: i32) -> Result<(Option<TableInfo>, Option<CellInfo>)> {
    let Some(a) = &node.ia2 else {
        return Ok((None, None));
    };
    let table = if let Some(t) = capability::<AccessibleTable2>(a)? {
        let mut rows = 0;
        let mut columns = 0;
        unsafe {
            (t.vtable().nRows)(t.as_raw(), &mut rows)
                .ok()
                .map_err(native)?;
            (t.vtable().nColumns)(t.as_raw(), &mut columns)
                .ok()
                .map_err(native)?;
        }
        Some(
            TableParts {
                row_count: Some(Count::from_signed(rows.into())),
                column_count: Some(Count::from_signed(columns.into())),
            }
            .into(),
        )
    } else {
        None
    };
    // MSAA ROLE_SYSTEM_COLUMNHEADER / ROWHEADER / CELL.
    let role_kind = match role {
        0x19 => Some(CellKind::ColumnHeader),
        0x1a => Some(CellKind::RowHeader),
        0x1d => Some(CellKind::Data),
        _ => None,
    };
    let cell = if let Some(c) = capability::<AccessibleTableCell>(a)? {
        let (mut row, mut column, mut rows, mut columns) = (0, 0, 0, 0);
        unsafe {
            (c.vtable().rowIndex)(c.as_raw(), &mut row)
                .ok()
                .map_err(native)?;
            (c.vtable().columnIndex)(c.as_raw(), &mut column)
                .ok()
                .map_err(native)?;
            (c.vtable().rowExtent)(c.as_raw(), &mut rows)
                .ok()
                .map_err(native)?;
            (c.vtable().columnExtent)(c.as_raw(), &mut columns)
                .ok()
                .map_err(native)?;
        }
        if row < 0 || column < 0 || rows <= 0 || columns <= 0 {
            return Err(platform(
                "IA2 TableCell returned invalid row/column extents",
            ));
        }
        Some(
            CellParts {
                row: Some(row as u32),
                column: Some(column as u32),
                row_span: Some(rows as u32),
                column_span: Some(columns as u32),
                // Implementing IA2 TableCell is the platform saying "cell";
                // without a header role it is a data cell.
                kind: Some(role_kind.unwrap_or(CellKind::Data)),
            }
            .into(),
        )
    } else {
        role_kind.map(|kind| {
            CellParts {
                row: None,
                column: None,
                row_span: None,
                column_span: None,
                kind: Some(kind),
            }
            .into()
        })
    };
    Ok((table, cell))
}
fn text(node: &Node, e: &ElementData) -> Result<AccessibleText> {
    node.interface(e, "text")
}
fn length(t: &AccessibleText) -> Result<u32> {
    let mut count = 0;
    unsafe {
        (t.vtable().nCharacters)(t.as_raw(), &mut count)
            .ok()
            .map_err(native)?;
    }
    u32::try_from(count).map_err(|_| platform("IA2 Text returned a negative length"))
}
fn substring(t: &AccessibleText, start: u32, end: u32) -> Result<String> {
    if end.saturating_sub(start) > 1024 * 1024 {
        return Err(Error::Unsupported {
            feature: "IA2 text read exceeds 1 Mi UTF-16 units; page the read".into(),
        });
    }
    let mut value = BSTR::default();
    unsafe {
        (t.vtable().text)(t.as_raw(), start as i32, end as i32, &mut value)
            .ok()
            .map_err(native)?;
    }
    if value.len() != end.saturating_sub(start) as usize {
        return Err(platform(
            "IA2 Text changed length while reading a requested range",
        ));
    }
    Ok(value.to_string())
}
fn read(t: &AccessibleText, r: TextRange) -> Result<String> {
    let len = length(t)?;
    check_in_bounds(r, len)?;
    let start = r.start.saturating_sub(1);
    let end = r.end.saturating_add(1).min(len);
    let observed = substring(t, start, end)?;
    slice_utf16(
        &observed,
        TextRange {
            start: r.start - start,
            end: r.end - start,
        },
    )
    .map_err(|error| match error {
        Error::InvalidTextRange { message, .. } => Error::InvalidTextRange {
            start: r.start,
            end: r.end,
            length: Some(len),
            message,
        },
        error => error,
    })
}
impl State {
    pub(super) fn text_length(&mut self, e: &ElementData) -> Result<u32> {
        length(&text(&self.node(e)?, e)?)
    }
    pub(super) fn text_read(&mut self, e: &ElementData, r: TextRange) -> Result<String> {
        read(&text(&self.node(e)?, e)?, r)
    }
    pub(super) fn text_caret(&mut self, e: &ElementData) -> Result<Option<u32>> {
        let node = self.node(e)?;
        let t = text(&node, e)?;
        // Core's caret contract: a caret belongs to the focused text element,
        // so an unfocused one has none (UIA gates on HasKeyboardFocus the
        // same way). MSAA STATE_SYSTEM_FOCUSED.
        let states = integer(
            &unsafe { node.object.get_accState(&VARIANT::from(node.child)) }.map_err(native)?,
        )?;
        if states & 0x4 == 0 {
            return Ok(None);
        }
        let mut offset = 0;
        let status = unsafe { (t.vtable().caretOffset)(t.as_raw(), &mut offset) };
        caret_answer(status, offset, || length(&t))
    }
    pub(super) fn text_selections(&mut self, e: &ElementData) -> Result<Vec<TextRange>> {
        let t = text(&self.node(e)?, e)?;
        let mut count = 0;
        let status = unsafe { (t.vtable().nSelections)(t.as_raw(), &mut count) };
        status.ok().map_err(native)?;
        // S_FALSE is IA2's "no value": no selection to report.
        if status == windows::Win32::Foundation::S_FALSE {
            return Ok(Vec::new());
        }
        let count = bounded(count, 1024, "text selections")?;
        let len = length(&t)?;
        let mut out = Vec::new();
        for i in 0..count {
            let mut start = 0;
            let mut end = 0;
            let status =
                unsafe { (t.vtable().selection)(t.as_raw(), i as i32, &mut start, &mut end) };
            status.ok().map_err(native)?;
            if status == windows::Win32::Foundation::S_FALSE {
                continue;
            }
            let r = selection_range(start, end, len)?;
            if !r.is_empty() {
                out.push(r);
            }
        }
        out.sort_by_key(|r| (r.start, r.end));
        Ok(out)
    }
    pub(super) fn set_selection(&mut self, e: &ElementData, r: TextRange) -> Result<()> {
        let t = text(&self.actionable(e)?, e)?;
        read(&t, r)?;
        let mut count = 0;
        unsafe {
            (t.vtable().nSelections)(t.as_raw(), &mut count)
                .ok()
                .map_err(native)?;
        }
        bounded(count, 1024, "text selections")?;
        self.check_deadline()?;
        if r.is_empty() {
            for i in (0..count).rev() {
                self.before_mutation()?;
                unsafe {
                    (t.vtable().removeSelection)(t.as_raw(), i)
                        .ok()
                        .map_err(native)?;
                }
            }
            self.before_mutation()?;
            unsafe {
                (t.vtable().setCaretOffset)(t.as_raw(), r.start as i32)
                    .ok()
                    .map_err(native)
            }
        } else {
            if count == 0 {
                self.before_mutation()?;
                unsafe {
                    (t.vtable().addSelection)(t.as_raw(), r.start as i32, r.end as i32)
                        .ok()
                        .map_err(native)?;
                }
            } else {
                self.before_mutation()?;
                unsafe {
                    (t.vtable().setSelection)(t.as_raw(), 0, r.start as i32, r.end as i32)
                        .ok()
                        .map_err(native)?;
                }
            }
            for i in (1..count).rev() {
                self.before_mutation()?;
                unsafe {
                    (t.vtable().removeSelection)(t.as_raw(), i)
                        .ok()
                        .map_err(native)?;
                }
            }
            Ok(())
        }
    }
    pub(super) fn text_range_at(
        &mut self,
        e: &ElementData,
        offset: u32,
        unit: TextUnit,
    ) -> Result<TextRange> {
        let t = text(&self.node(e)?, e)?;
        let len = length(&t)?;
        check_in_bounds(
            TextRange {
                start: offset,
                end: offset,
            },
            len,
        )?;
        if len == 0 {
            return Ok(TextRange { start: 0, end: 0 });
        }
        let mut offset = offset.min(len - 1);
        let begin = offset.saturating_sub(1);
        let end = (offset + 2).min(len);
        let around = substring(&t, begin, end)?;
        let units: Vec<u16> = around.encode_utf16().collect();
        let i = (offset - begin) as usize;
        if offset > 0
            && (0xdc00..=0xdfff).contains(&units[i])
            && (0xd800..=0xdbff).contains(&units[i - 1])
        {
            offset -= 1;
        }
        if unit == TextUnit::Character {
            let i = (offset - begin) as usize;
            let width = if (0xd800..=0xdbff).contains(&units[i]) {
                2
            } else {
                1
            };
            return Ok(TextRange {
                start: offset,
                end: offset + width,
            });
        }
        let boundary = match unit {
            TextUnit::Character => 0,
            TextUnit::Word => 1,
            TextUnit::Paragraph => 3,
            TextUnit::Line => 4,
        };
        let mut start = 0;
        let mut end = 0;
        let mut value = BSTR::default();
        let status = unsafe {
            (t.vtable().textAtOffset)(
                t.as_raw(),
                offset as i32,
                boundary,
                &mut start,
                &mut end,
                &mut value,
            )
        };
        available(status, "IA2 text boundary")?;
        let mut range = checked_range(start, end, len)?;
        if value.len() > 1024 * 1024 {
            return Err(Error::Unsupported {
                feature: "IA2 text boundary exceeds 1 Mi UTF-16 units".into(),
            });
        }
        if matches!(unit, TextUnit::Line | TextUnit::Paragraph) {
            let observed = substring(&t, range.start, range.end.saturating_add(2).min(len))?;
            range = normalize_line(range, offset, &observed.encode_utf16().collect::<Vec<_>>())?;
        }
        // IA2 Word intentionally returns the preceding word at whitespace
        // offsets. Only line/paragraph walking requires containment; the
        // normalizer enforces it without rejecting valid Word responses.
        Ok(range)
    }
    pub(super) fn text_attributes(&mut self, e: &ElementData, offset: u32) -> Result<TextRun> {
        let node = self.node(e)?;
        let t = text(&node, e)?;
        let len = length(&t)?;
        // A run is the one holding the character at `offset`, so there must
        // be one: the end of the text is refused, as on Linux and macOS.
        if offset >= len {
            return Err(Error::InvalidTextRange {
                start: offset,
                end: offset,
                length: Some(len),
                message: "no text at or after this offset".to_string(),
            });
        }
        // Refuses an offset that splits a surrogate pair.
        read(
            &t,
            TextRange {
                start: offset,
                end: offset,
            },
        )?;
        let mut start = 0;
        let mut end = 0;
        let mut attrs = BSTR::default();
        if let Some(t2) =
            capability::<AccessibleText2>(node.ia2.as_ref().ok_or_else(|| unsupported(e, "text"))?)?
        {
            let filter=BSTR::from("font-family, font-size, font-weight, font-style, text-underline-style, text-line-through-style, color, background-color, text-position, invalid");
            let status = unsafe {
                (t2.vtable().attributeRange)(
                    t2.as_raw(),
                    offset as i32,
                    filter.as_ptr(),
                    &mut start,
                    &mut end,
                    &mut attrs,
                )
            };
            available(status, "IA2 formatting attributes")?;
        } else {
            let status = unsafe {
                (t.vtable().attributes)(t.as_raw(), offset as i32, &mut start, &mut end, &mut attrs)
            };
            available(status, "IA2 formatting attributes")?;
        }
        if start < 0 || end < 0 {
            return Err(unsupported(e, "IA2 formatting attributes"));
        }
        let range = checked_range(start, end, len)?;
        if attrs.len() > 65536 {
            return Err(platform("IA2 text attribute payload exceeds 65536 units"));
        }
        Ok(TextRun::new(
            range,
            attributes(&parse_attributes(&attrs.to_string())?)?,
        ))
    }
    pub(super) fn text_bounds(&mut self, e: &ElementData, r: TextRange) -> Result<Option<Rect>> {
        let t = text(&self.node(e)?, e)?;
        check_in_bounds(r, length(&t)?)?;
        if r.len() > 4096 {
            return Err(Error::Unsupported {
                feature: "IA2 geometry is limited to 4096 UTF-16 units per rendered line".into(),
            });
        }
        let observed = read(&t, r)?;
        let mut scalar_offsets = Vec::new();
        let mut offset = r.start;
        if r.is_empty() {
            scalar_offsets.push(offset);
        } else {
            for c in observed.chars() {
                scalar_offsets.push(offset);
                offset += c.len_utf16() as u32;
            }
        }
        let dpi = crate::dpi::DpiScope::enter()?;
        let mut out: Option<Rect> = None;
        for offset in scalar_offsets {
            self.check_deadline()?;
            let next =
                crate::coords::ia2_character_bounds(&dpi, &t, offset as i32).map_err(native)?;
            if next.height == 0 || (!r.is_empty() && next.width == 0) {
                continue;
            }
            out = Some(if let Some(prev) = out {
                union(prev, next)?
            } else {
                next
            });
        }
        Ok(out)
    }
    pub(super) fn cell_at(
        &mut self,
        e: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        let node = self.node(e)?;
        let t: AccessibleTable2 = node.interface(e, "table")?;
        let mut rows = 0;
        let mut columns = 0;
        unsafe {
            (t.vtable().nRows)(t.as_raw(), &mut rows)
                .ok()
                .map_err(native)?;
            (t.vtable().nColumns)(t.as_raw(), &mut columns)
                .ok()
                .map_err(native)?;
        }
        if row > i32::MAX as u32
            || column > i32::MAX as u32
            || (rows >= 0 && row >= rows as u32)
            || (columns >= 0 && column >= columns as u32)
        {
            return Ok(None);
        }
        let mut raw = std::ptr::null_mut();
        unsafe {
            (t.vtable().cellAt)(t.as_raw(), row as i32, column as i32, &mut raw)
                .ok()
                .map_err(native)?;
        }
        if raw.is_null() {
            return Ok(None);
        }
        let object: IAccessible = unsafe { IUnknown::from_raw(raw) }.cast().map_err(native)?;
        Ok(Some(self.snapshot(Node::new(object, 0, node.owner, 0)?)?))
    }
    pub(super) fn cell_headers(
        &mut self,
        e: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let node = self.node(e)?;
        let c: AccessibleTableCell = node.interface(e, "cell_headers")?;
        let mut headers = Vec::new();
        for object in self.header_objects(&c, kind)? {
            self.check_deadline()?;
            let header = Node::new(object, 0, node.owner.clone(), 0)?;
            headers.push(self.header_element(header, kind)?);
        }
        Ok(headers)
    }
    /// The native header objects a cell reports for `kind`, not yet read.
    fn header_objects(
        &self,
        c: &AccessibleTableCell,
        kind: HeaderKind,
    ) -> Result<Vec<IAccessible>> {
        let mut raw = std::ptr::null_mut();
        let mut count = 0;
        unsafe {
            match kind {
                HeaderKind::Column => {
                    (c.vtable().columnHeaderCells)(c.as_raw(), &mut raw, &mut count)
                }
                HeaderKind::Row => (c.vtable().rowHeaderCells)(c.as_raw(), &mut raw, &mut count),
            }
            .ok()
            .map_err(native)?;
        }
        self.objects(raw, count)
    }
    /// Snapshot a header object reported by a cell, marked as a `kind` header.
    fn header_element(&mut self, node: Node, kind: HeaderKind) -> Result<ElementData> {
        let mut header = self.snapshot(node)?;
        header.role = Role::TableCell;
        if let Some(cell) = &mut header.cell {
            cell.kind = Some(kind.into());
        } else {
            header.cell = Some(
                CellParts {
                    row: None,
                    column: None,
                    row_span: None,
                    column_span: None,
                    kind: Some(kind.into()),
                }
                .into(),
            );
        }
        Ok(header)
    }
    pub(super) fn selected(&mut self, e: &ElementData) -> Result<Vec<ElementData>> {
        let node = self.node(e)?;
        let t: AccessibleTable2 = node.interface(e, "selected_items")?;
        let mut count = 0;
        unsafe {
            (t.vtable().nSelectedCells)(t.as_raw(), &mut count)
                .ok()
                .map_err(native)?;
        }
        bounded(count, LIMIT, "selected cells")?;
        let mut raw = std::ptr::null_mut();
        unsafe {
            (t.vtable().selectedCells)(t.as_raw(), &mut raw, &mut count)
                .ok()
                .map_err(native)?;
        }
        self.object_array(raw, count, &node.owner, 0)
    }
    fn object_array(
        &mut self,
        raw: *mut *mut c_void,
        count: i32,
        owner: &Owner,
        parent: u64,
    ) -> Result<Vec<ElementData>> {
        let mut out = Vec::new();
        for object in self.objects(raw, count)? {
            self.check_deadline()?;
            out.push(self.snapshot(Node::new(object, 0, owner.clone(), parent)?)?);
        }
        Ok(out)
    }
    /// Take ownership of a CoTaskMem array of IA2 objects, without reading them.
    fn objects(&self, raw: *mut *mut c_void, count: i32) -> Result<Vec<IAccessible>> {
        if count < 0 {
            unsafe { CoTaskMemFree(Some(raw.cast())) };
            return Err(platform("IA2 returned a negative object array count"));
        }
        struct Objects {
            raw: *mut *mut c_void,
            count: usize,
        }
        impl Drop for Objects {
            fn drop(&mut self) {
                if !self.raw.is_null() {
                    for i in 0..self.count {
                        let p = unsafe { *self.raw.add(i) };
                        if !p.is_null() {
                            drop(unsafe { IUnknown::from_raw(p) });
                        }
                    }
                    unsafe {
                        CoTaskMemFree(Some(self.raw.cast()));
                    }
                }
            }
        }
        let array = Objects {
            raw,
            count: count as usize,
        };
        bounded(count, LIMIT, "object array")?;
        if raw.is_null() {
            if count == 0 {
                return Ok(Vec::new());
            }
            return Err(platform("IA2 object array is null with nonzero count"));
        }
        let mut out = Vec::new();
        for i in 0..array.count {
            self.check_deadline()?;
            let p = unsafe { *array.raw.add(i) };
            if p.is_null() {
                return Err(platform("IA2 object array contains a null interface"));
            }
            // Borrow the array's reference, clone it before the array's Drop.
            let unknown = std::mem::ManuallyDrop::new(unsafe { IUnknown::from_raw(p) });
            let object: IAccessible = unknown.cast().map_err(native)?;
            out.push(object);
        }
        Ok(out)
    }
    /// Every `kind` header of the table, each once, in column (or row) order.
    ///
    /// Headers are per column (or per row), so this reads one cell per
    /// column (row): the first cell from the top (left) that is not itself a
    /// `kind` header. A header cell's own header list names only the headers
    /// above it (Firefox reports none for a `th` in row 0), so the first data
    /// cell is the one whose list holds the whole column's headers. A cell
    /// spanning several columns (rows) is read once. Each header is
    /// snapshotted once, however many cells reference it.
    ///
    /// That is `rows + columns` cell lookups for a table with one header row
    /// and column, instead of reading every cell. A missing cell along the
    /// walk (a sparse or virtualized table) fails explicitly rather than
    /// skipping the column, because a skipped column would silently drop its
    /// headers.
    pub(super) fn table_headers(
        &mut self,
        e: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let node = self.node(e)?;
        let t: AccessibleTable2 = node.interface(e, "table")?;
        let mut rows = 0;
        let mut columns = 0;
        unsafe {
            (t.vtable().nRows)(t.as_raw(), &mut rows)
                .ok()
                .map_err(native)?;
            (t.vtable().nColumns)(t.as_raw(), &mut columns)
                .ok()
                .map_err(native)?;
        }
        let rows = bounded(rows, LIMIT, "header table rows")?;
        let columns = bounded(columns, LIMIT, "header table columns")?;
        // `lines` are what headers label; `depth` is the walk along each one.
        let (lines, depth, line_name, own_role) = match kind {
            HeaderKind::Column => (columns, rows, "column", 0x19),
            HeaderKind::Row => (rows, columns, "row", 0x1a),
        };
        let mut out = Vec::new();
        let mut cells_read = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        for line in 0..lines {
            let mut labelled = None;
            for step in 0..depth {
                self.check_deadline()?;
                let (row, column) = match kind {
                    HeaderKind::Column => (step, line),
                    HeaderKind::Row => (line, step),
                };
                let object = table_cell(&t, row, column)?.ok_or_else(|| Error::Unsupported {
                    feature: format!(
                        "IA2 table-wide {line_name} header discovery: the table reports no cell at row {row}, column {column}; read headers per cell with cell_headers"
                    ),
                })?;
                let role =
                    integer(&unsafe { object.get_accRole(&VARIANT::from(0)) }.map_err(native)?)?;
                if role != own_role {
                    labelled = Some(object);
                    break;
                }
            }
            // Every cell along this line is itself a header of this kind:
            // nothing in it is labelled.
            let Some(object) = labelled else {
                continue;
            };
            let a = query(&object)?.ok_or_else(|| {
                unsupported(e, "table header discovery on a cell without IAccessible2")
            })?;
            let (mut id, mut hwnd) = (0, HWND::default());
            unsafe {
                (a.vtable().unique_id)(a.as_raw(), &mut id)
                    .ok()
                    .map_err(native)?;
                (a.vtable().window_handle)(a.as_raw(), &mut hwnd)
                    .ok()
                    .map_err(native)?;
            }
            // A cell spanning several lines answers for each; read it once.
            if !cells_read.insert((id, hwnd.0 as usize)) {
                continue;
            }
            let c = capability::<AccessibleTableCell>(&a)?.ok_or_else(|| {
                unsupported(
                    e,
                    "table header discovery on a cell without IAccessibleTableCell",
                )
            })?;
            for header in self.header_objects(&c, kind)? {
                self.check_deadline()?;
                let header = Node::new(header, 0, node.owner.clone(), 0)?;
                // IA2 header objects must carry identity; do not dedup by name.
                let key = header
                    .identity
                    .ok_or_else(|| unsupported(e, "header identity"))?;
                if seen.insert(key) {
                    out.push(self.header_element(header, kind)?);
                }
            }
        }
        Ok(out)
    }
}
/// The cell at (`row`, `column`), unread; `None` when the table has none there.
fn table_cell(t: &AccessibleTable2, row: usize, column: usize) -> Result<Option<IAccessible>> {
    let (Ok(row), Ok(column)) = (i32::try_from(row), i32::try_from(column)) else {
        return Err(platform("IA2 table index exceeds i32"));
    };
    let mut raw = std::ptr::null_mut();
    unsafe {
        (t.vtable().cellAt)(t.as_raw(), row, column, &mut raw)
            .ok()
            .map_err(native)?;
    }
    if raw.is_null() {
        return Ok(None);
    }
    let object: IAccessible = unsafe { IUnknown::from_raw(raw) }.cast().map_err(native)?;
    Ok(Some(object))
}
fn available(status: windows::core::HRESULT, feature: &str) -> Result<()> {
    status.ok().map_err(native)?;
    if status == windows::Win32::Foundation::S_FALSE {
        return Err(Error::Unsupported {
            feature: feature.into(),
        });
    }
    Ok(())
}

/// The observation starts at native.start and includes up to two guard units
/// after native.end. Honor native soft wraps, include explicit terminators,
/// and subdivide overbroad ranges at definite hard line breaks.
fn normalize_line(native: TextRange, offset: u32, units: &[u16]) -> Result<TextRange> {
    let n = native.len() as usize;
    if n > units.len() {
        return Err(platform(
            "IA2 line observation is shorter than its native range",
        ));
    }
    let mut end = n;
    if n > 0 && units[n - 1] == 13 && units.get(n) == Some(&10) {
        end += 1;
    } else if n == 0 || !matches!(units[n - 1], 10 | 13) {
        if units.get(n) == Some(&13) && units.get(n + 1) == Some(&10) {
            end += 2;
        } else if units.get(n).is_some_and(|u| matches!(*u, 10 | 13)) {
            end += 1;
        }
    }
    let mut range = TextRange {
        start: native.start,
        end: native.start + end as u32,
    };
    let mut i = 0;
    while i < end {
        if matches!(units[i], 10 | 13) {
            let width = if units[i] == 13 && units.get(i + 1) == Some(&10) {
                2
            } else {
                1
            };
            let next = native.start + (i + width) as u32;
            if next <= offset {
                range.start = next;
            } else {
                range.end = next;
                break;
            }
            i += width;
        } else {
            i += 1;
        }
    }
    if range.start > offset || range.end <= offset {
        return Err(platform(format!(
            "IA2 line boundary {range} does not advance at offset {offset}"
        )));
    }
    Ok(range)
}

/// Interpret `IAccessibleText::caretOffset` for a focused element.
///
/// IA2 answers S_FALSE when the caret is not active on this object, and
/// documents the offset as -1 then; Chromium answers S_FALSE with offset 0,
/// so the status, not the offset, decides. A failing HRESULT keeps its code.
fn caret_answer(
    status: windows::core::HRESULT,
    offset: i32,
    length: impl FnOnce() -> Result<u32>,
) -> Result<Option<u32>> {
    status.ok().map_err(native)?;
    if status == windows::Win32::Foundation::S_FALSE || offset == -1 {
        return Ok(None);
    }
    if offset < 0 || offset as u32 > length()? {
        return Err(platform(format!(
            "IA2 returned an invalid caret offset {offset}"
        )));
    }
    Ok(Some(offset as u32))
}
/// A selection as a range in document order. IA2 reports a backwards
/// selection (extended towards the start) with `start > end`; core's
/// [`TextRange`] has no direction, and the caret reports the active end.
fn selection_range(start: i32, end: i32, len: u32) -> Result<TextRange> {
    checked_range(start.min(end), start.max(end), len)
}
fn checked_range(start: i32, end: i32, len: u32) -> Result<TextRange> {
    if start < 0 || end < 0 {
        return Err(platform("IA2 returned negative text endpoints"));
    }
    let r = TextRange {
        start: start as u32,
        end: end as u32,
    };
    check_in_bounds(r, len)?;
    Ok(r)
}
fn union(a: Rect, b: Rect) -> Result<Rect> {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (i64::from(a.x) + i64::from(a.width)).max(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).max(i64::from(b.y) + i64::from(b.height));
    Ok(Rect {
        x,
        y,
        width: u32::try_from(right - i64::from(x)).map_err(|_| platform("IA2 bounds overflow"))?,
        height: u32::try_from(bottom - i64::from(y))
            .map_err(|_| platform("IA2 bounds overflow"))?,
    })
}
pub(super) fn parse_attributes(input: &str) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut escaped = false;
    for c in input.chars().chain(std::iter::once(';')) {
        if escaped {
            if in_value {
                value.push(c);
            } else {
                key.push(c);
            }
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            ':' if !in_value => in_value = true,
            ';' => {
                if key.is_empty() && !in_value {
                    continue;
                }
                if !in_value || key.is_empty() {
                    return Err(platform("malformed IA2 attribute entry"));
                }
                if out
                    .insert(std::mem::take(&mut key), std::mem::take(&mut value))
                    .is_some()
                {
                    return Err(platform("duplicate IA2 attribute name"));
                }
                in_value = false;
            }
            _ => {
                if in_value {
                    value.push(c);
                } else {
                    key.push(c);
                }
            }
        }
    }
    if escaped || !key.is_empty() || in_value {
        return Err(platform("incomplete IA2 attribute escape or entry"));
    }
    Ok(out)
}
fn attributes(a: &HashMap<String, String>) -> Result<TextAttributes> {
    let get = |key| a.get(key).map(String::as_str);
    let color = |key| -> Result<Option<Color>> {
        let Some(s) = get(key) else { return Ok(None) };
        let channels = s
            .strip_prefix("rgb(")
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| platform(format!("invalid IA2 color {s}")))?;
        let channels: Vec<u8> = channels
            .split(',')
            .map(|n| {
                n.trim()
                    .parse::<u8>()
                    .map_err(|_| platform(format!("invalid IA2 color {s}")))
            })
            .collect::<Result<_>>()?;
        if channels.len() != 3 {
            return Err(platform("IA2 color needs three channels"));
        }
        Ok(Some(Color {
            r: channels[0],
            g: channels[1],
            b: channels[2],
            a: 255,
        }))
    };
    let size = get("font-size")
        .map(|s| {
            let (v, scale) = if let Some(v) = s.strip_suffix("pt") {
                (v, 1.0)
            } else if let Some(v) = s.strip_suffix("px") {
                (v, 0.75)
            } else {
                return Err(platform(format!("unknown IA2 font size unit {s}")));
            };
            let size = v
                .parse::<f64>()
                .map_err(|_| platform(format!("invalid IA2 font size {s}")))?;
            if !size.is_finite() || size < 0.0 {
                return Err(platform("invalid IA2 font size"));
            }
            Ok(size * scale)
        })
        .transpose()?;
    let weight = get("font-weight")
        .map(|s| match s {
            "normal" => Ok(400),
            "bold" => Ok(700),
            _ => s
                .parse::<u16>()
                .map_err(|_| platform(format!("invalid IA2 font weight {s}"))),
        })
        .transpose()?;
    Ok(TextAttributeParts {
        font_family: get("font-family").map(str::to_owned),
        font_size: size,
        font_weight: weight,
        italic: get("font-style").map(|s| matches!(s, "italic" | "oblique")),
        underline: get("text-underline-style").map(|s| s != "none"),
        strikethrough: get("text-line-through-style").map(|s| s != "none"),
        foreground: color("color")?,
        background: color("background-color")?,
        script: get("text-position").and_then(|s| match s {
            "super" => Some(TextScript::Superscript),
            "sub" => Some(TextScript::Subscript),
            "baseline" => Some(TextScript::Baseline),
            _ => None,
        }),
        spelling_error: get("invalid").map(|s| s == "spelling"),
    }
    .into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_boundaries_preserve_wraps_and_include_crlf_or_blank_lines() {
        let r = |start, end| TextRange { start, end };
        let normalize = |text: &str, native, offset| {
            normalize_line(native, offset, &text.encode_utf16().collect::<Vec<_>>()).expect("line")
        };
        assert_eq!(normalize("a\r\nb", r(0, 4), 2), r(0, 3));
        assert_eq!(normalize("a\r\nb", r(0, 4), 3), r(3, 4));
        assert_eq!(normalize("a\r\n", r(0, 1), 0), r(0, 3));
        assert_eq!(normalize("a\r\nb", r(0, 2), 1), r(0, 3));
        assert_eq!(normalize("\n\nx", r(0, 3), 1), r(1, 2));
        assert_eq!(normalize("\n\n", r(0, 0), 0), r(0, 1));
        assert_eq!(normalize("abcde", r(0, 3), 2), r(0, 3), "native soft wrap");
        assert!(normalize_line(r(0, 0), 0, &[]).is_err());
    }
    #[test]
    fn absent_native_boundaries_are_unsupported_and_failures_keep_hresult() {
        use windows::Win32::Foundation::{E_ACCESSDENIED, S_FALSE};
        assert!(matches!(
            available(S_FALSE, "IA2 line"),
            Err(Error::Unsupported { .. })
        ));
        assert!(
            matches!(available(E_ACCESSDENIED,"IA2 line"),Err(Error::Platform{code,..})if code==E_ACCESSDENIED.0 as i64)
        );
    }
    #[test]
    fn caret_is_none_on_s_false_whatever_the_offset() {
        use windows::Win32::Foundation::{E_FAIL, S_FALSE, S_OK};
        assert_eq!(caret_answer(S_FALSE, 0, || Ok(5)).unwrap(), None);
        assert_eq!(caret_answer(S_FALSE, -1, || Ok(5)).unwrap(), None);
        assert_eq!(caret_answer(S_OK, -1, || Ok(5)).unwrap(), None);
        assert_eq!(caret_answer(S_OK, 3, || Ok(5)).unwrap(), Some(3));
        assert_eq!(caret_answer(S_OK, 5, || Ok(5)).unwrap(), Some(5));
        assert!(caret_answer(S_OK, 6, || Ok(5)).is_err());
        assert!(caret_answer(S_OK, -2, || Ok(5)).is_err());
        assert!(
            matches!(caret_answer(E_FAIL, 0, || Ok(5)), Err(Error::Platform { code, .. }) if code == E_FAIL.0 as i64)
        );
    }
    #[test]
    fn backwards_selections_read_in_document_order() {
        assert_eq!(
            selection_range(4, 1, 5).unwrap(),
            TextRange { start: 1, end: 4 }
        );
        assert_eq!(
            selection_range(1, 4, 5).unwrap(),
            TextRange { start: 1, end: 4 }
        );
        assert!(selection_range(6, 1, 5).is_err());
        assert!(selection_range(-1, 1, 5).is_err());
    }
    #[test]
    fn escaped_attributes_are_split_only_at_unescaped_delimiters() {
        let a = parse_attributes(r"font-family:Name\: Part\; Two\\Three;color:rgb(1,2,3);")
            .expect("escaped attrs");
        assert_eq!(a["font-family"], r"Name: Part; Two\Three");
        assert_eq!(
            attributes(&a)
                .expect("mapped attrs")
                .foreground
                .expect("color")
                .to_hex(),
            "#010203"
        );
    }
    #[test]
    fn malformed_attributes_and_numbers_fail() {
        for s in ["font-size", ":value;", "x:y;x:z;", r"key:value\"] {
            assert!(parse_attributes(s).is_err(), "{s:?}");
        }
        for s in [
            "font-size:NaNpt;",
            "color:rgb(1,2,999);",
            "font-weight:bad;",
        ] {
            assert!(
                attributes(&parse_attributes(s).expect("syntax")).is_err(),
                "{s}"
            );
        }
    }
}
