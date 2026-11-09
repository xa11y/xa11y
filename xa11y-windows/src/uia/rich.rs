//! UIA grids, collection positions, and live text ranges.
use super::*;
use xa11y_core::table::{
    index_from_one_based, CellInfo, CellParts, CollectionInfo, CollectionParts, Count, HeaderKind,
    RowInfo, RowParts, TableInfo, TableParts,
};
use xa11y_core::text_api::{
    slice_utf16, utf16_len, Color, TextAttributeParts, TextRange, TextScript,
};

fn property(element: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Result<Option<i32>> {
    // Ignore UIA's defaults: an absent grid index must not become row zero.
    let value = uia_call(|| unsafe { element.GetCachedPropertyValueEx(id, true) })?;
    use windows::Win32::System::Variant::VT_I4;
    if unsafe { value.Anonymous.Anonymous.vt } == VT_I4 {
        Ok(Some(i32::try_from(&value).map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("reading cached UIA property {id:?}: {e}"),
            }
        })?))
    } else {
        // UIA's reserved NotSupported value is VT_UNKNOWN.
        Ok(None)
    }
}

pub(super) fn table_info(e: &IUIAutomationElement) -> Result<Option<TableInfo>> {
    let info: TableInfo = TableParts {
        row_count: property(e, UIA_GridRowCountPropertyId)?.map(|n| Count::from_signed(n.into())),
        column_count: property(e, UIA_GridColumnCountPropertyId)?
            .map(|n| Count::from_signed(n.into())),
    }
    .into();
    Ok((!info.is_empty()).then_some(info))
}

pub(super) fn cell_info(e: &IUIAutomationElement) -> Result<Option<CellInfo>> {
    let index =
        |id| -> Result<Option<u32>> { Ok(property(e, id)?.and_then(|n| u32::try_from(n).ok())) };
    let info: CellInfo = CellParts {
        row: index(UIA_GridItemRowPropertyId)?,
        column: index(UIA_GridItemColumnPropertyId)?,
        row_span: index(UIA_GridItemRowSpanPropertyId)?.filter(|n| *n > 0),
        column_span: index(UIA_GridItemColumnSpanPropertyId)?.filter(|n| *n > 0),
        header: header_kind(e),
    }
    .into();
    Ok((!info.is_empty()).then_some(info))
}

fn header_kind(e: &IUIAutomationElement) -> Option<HeaderKind> {
    match uia_cached_bstr(e, UIA_AriaRolePropertyId).as_deref() {
        Some("columnheader") => return Some(HeaderKind::Column),
        Some("rowheader") => return Some(HeaderKind::Row),
        _ => {}
    }
    // WPF's peers publish these native class names, but no orientation.
    // Use only cached properties: enumerating an owning Table's header
    // arrays for every HeaderItem is quadratic and blocks event snapshots.
    // Other providers with neither signal honestly leave the kind unknown.
    // Explicit Table/TableItem header requests label their results below.
    if uia_cached_bstr(e, UIA_FrameworkIdPropertyId).as_deref() == Some("WPF") {
        match uia_cached_bstr(e, UIA_ClassNamePropertyId).as_deref() {
            Some("DataGridColumnHeader") => return Some(HeaderKind::Column),
            Some("DataGridRowHeader") => return Some(HeaderKind::Row),
            _ => {}
        }
    }
    None
}

pub(super) fn parent_header_kind(
    data: &mut ElementData,
    element: &IUIAutomationElement,
    walker: &IUIAutomationTreeWalker,
    cache: &IUIAutomationCacheRequest,
) -> Result<()> {
    if data.cell.is_some_and(|c| c.header.is_some())
        || uia_call(|| unsafe { element.CachedControlType() })? != UIA_HeaderItemControlTypeId
    {
        return Ok(());
    }
    // One parent read, independent of table size. Event snapshots never
    // enter this path. Only an actual Header container defines orientation.
    let parent = uia_call(|| unsafe { walker.GetParentElementBuildCache(element, cache) })?;
    if uia_call(|| unsafe { parent.CachedControlType() })? == UIA_HeaderControlTypeId {
        let kind = match property(&parent, UIA_OrientationPropertyId)? {
            Some(n) if n == OrientationType_Horizontal.0 => Some(HeaderKind::Column),
            Some(n) if n == OrientationType_Vertical.0 => Some(HeaderKind::Row),
            _ => None,
        };
        if let Some(kind) = kind {
            label_headers(std::slice::from_mut(data), kind);
        }
    }
    Ok(())
}

pub(super) fn label_headers(headers: &mut [ElementData], kind: HeaderKind) {
    for header in headers {
        header
            .cell
            .get_or_insert_with(|| {
                CellParts {
                    row: None,
                    column: None,
                    row_span: None,
                    column_span: None,
                    header: None,
                }
                .into()
            })
            .header = Some(kind);
    }
}

pub(super) fn row_info(e: &IUIAutomationElement, role: Role) -> Result<Option<RowInfo>> {
    if role != Role::TableRow {
        return Ok(None);
    }
    Ok(property(e, UIA_GridItemRowPropertyId)?
        .and_then(|n| u32::try_from(n).ok())
        .map(|index| RowParts { index: Some(index) }.into()))
}

pub(super) fn collection_info(e: &IUIAutomationElement) -> Result<Option<CollectionInfo>> {
    let info: CollectionInfo = CollectionParts {
        index: property(e, UIA_PositionInSetPropertyId)?
            .and_then(|n| index_from_one_based(n.into())),
        // Zero is UIA's default, meaning not reported; -1 explicitly means unknown.
        size: property(e, UIA_SizeOfSetPropertyId)?
            .filter(|n| *n != 0)
            .map(|n| Count::from_signed(n.into())),
        depth: property(e, UIA_LevelPropertyId)?.and_then(|n| index_from_one_based(n.into())),
    }
    .into();
    Ok((!info.is_empty()).then_some(info))
}

impl WindowsProvider {
    pub(super) fn rich_pattern<T: Interface>(
        &self,
        e: &ElementData,
        id: UIA_PATTERN_ID,
        action: &str,
    ) -> Result<T> {
        current_pattern(&self.get_cached(e.handle)?, id, action)?.ok_or_else(|| {
            Error::ActionNotSupported {
                action: action.into(),
                role: e.role,
            }
        })
    }

    pub(super) fn rich_element(
        &self,
        dpi: &crate::dpi::DpiScope,
        e: &IUIAutomationElement,
    ) -> Result<ElementData> {
        let fresh = uia_call(|| self.populate_cache(dpi, e))?;
        let clip = self.walk_inherited_clip(dpi, &fresh)?;
        let host = self.walk_to_host_hwnd(&fresh)?;
        let pid = uia_call(|| unsafe { fresh.CachedProcessId() })?;
        self.build_element_data_in(
            dpi,
            &fresh,
            u32::try_from(pid).ok(),
            host,
            HostHint::Parent,
            &clip,
        )
    }

    pub(super) fn rich_elements(
        &self,
        dpi: &crate::dpi::DpiScope,
        arr: &IUIAutomationElementArray,
    ) -> Result<Vec<ElementData>> {
        (0..uia_call(|| unsafe { arr.Length() })?)
            .map(|i| self.rich_element(dpi, &uia_call(|| unsafe { arr.GetElement(i) })?))
            .collect()
    }

    pub(super) fn document(&self, e: &ElementData) -> Result<IUIAutomationTextRange> {
        let pattern: IUIAutomationTextPattern = self.rich_pattern(e, UIA_TextPatternId, "text")?;
        uia_call(|| unsafe { pattern.DocumentRange() })
    }
}

pub(super) fn contents(range: &IUIAutomationTextRange) -> Result<String> {
    Ok(uia_call(|| unsafe { range.GetText(-1) })?.to_string())
}

/// Measure endpoints from the document start. UIA character units vary by
/// provider (UTF-16, scalar values, or grapheme clusters); never expose them
/// directly as numeric offsets.
pub(super) fn offsets(
    doc: &IUIAutomationTextRange,
    range: &IUIAutomationTextRange,
) -> Result<TextRange> {
    let prefix = uia_call(|| unsafe { doc.Clone() })?;
    let offset = |endpoint| -> Result<u32> {
        uia_call(|| unsafe {
            prefix.MoveEndpointByRange(TextPatternRangeEndpoint_End, range, endpoint)
        })?;
        Ok(utf16_len(&contents(&prefix)?))
    };
    Ok(TextRange {
        start: offset(TextPatternRangeEndpoint_Start)?,
        end: offset(TextPatternRangeEndpoint_End)?,
    })
}

/// Seek in provider character units using exponential bracketing and bisection.
/// Each observation measures a UTF-16 prefix; never correct one unit at a time.
/// `advance` returns the actual provider-unit movement and measured endpoint.
fn seek_endpoint(
    target: u32,
    estimate: i32,
    mut advance: impl FnMut(i32) -> Result<(i32, u32)>,
) -> Result<()> {
    let mut current = 0i64;
    let mut lower = 0i64;
    let mut upper = None;
    let mut next = i64::from(estimate.max(1));
    loop {
        let step = i32::try_from(next - current).map_err(|_| Error::Unsupported {
            feature: "UIA text movement exceeds i32::MAX characters".into(),
        })?;
        let (moved, reached) = advance(step)?;
        current += i64::from(moved);
        if reached == target {
            return Ok(());
        }
        if moved == 0 {
            return Err(unreachable_endpoint(target, reached));
        }
        if reached < target {
            lower = current;
        } else {
            upper = Some(current);
        }
        next = if let Some(upper) = upper {
            if upper - lower <= 1 {
                return Err(unreachable_endpoint(target, reached));
            }
            lower + (upper - lower) / 2
        } else {
            // The initial scalar estimate undershot (e.g. a UTF-16-unit
            // provider). Double the distance until the endpoint is bracketed.
            (current * 2).min(i64::from(i32::MAX))
        };
        if next == current {
            return Err(unreachable_endpoint(target, reached));
        }
    }
}

fn unreachable_endpoint(target: u32, reached: u32) -> Error {
    Error::Unsupported {
        feature: format!(
            "UIA provider cannot represent UTF-16 endpoint {target} (reached {reached})"
        ),
    }
}

pub(super) fn range(
    doc: &IUIAutomationTextRange,
    wanted: TextRange,
) -> Result<IUIAutomationTextRange> {
    let text = contents(doc)?;
    slice_utf16(&text, wanted)?;
    range_in(doc, &text, wanted)
}

fn range_in(
    doc: &IUIAutomationTextRange,
    text: &str,
    wanted: TextRange,
) -> Result<IUIAutomationTextRange> {
    let start = point(doc, text, None, wanted.start)?;
    // Reuse the start endpoint rather than seeking from zero twice.
    let end = point(doc, text, Some((&start, wanted.start)), wanted.end)?;
    uia_call(|| unsafe {
        start.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &end,
            TextPatternRangeEndpoint_End,
        )
    })?;
    Ok(start)
}

fn point(
    doc: &IUIAutomationTextRange,
    text: &str,
    origin: Option<(&IUIAutomationTextRange, u32)>,
    target: u32,
) -> Result<IUIAutomationTextRange> {
    let p = uia_call(|| unsafe { doc.Clone() })?;
    if target == utf16_len(text) {
        // WPF Character movement stops before a FlowDocument's final CRLF.
        // The document End is nevertheless an addressable native endpoint.
        uia_call(|| unsafe {
            p.MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                doc,
                TextPatternRangeEndpoint_End,
            )
        })?;
        return Ok(p);
    }
    let (base, offset) = origin.unwrap_or((doc, 0));
    uia_call(|| unsafe {
        p.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            base,
            TextPatternRangeEndpoint_Start,
        )
    })?;
    if target > offset {
        let scalars = |n| xa11y_core::text_api::utf16_to_char_offset(text, n);
        let count =
            i32::try_from(scalars(target)? - scalars(offset)?).map_err(|_| Error::Unsupported {
                feature: "UIA text offsets exceeding i32::MAX characters".into(),
            })?;
        seek_endpoint(target, count, |step| {
            let moved = uia_call(|| unsafe {
                p.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, step)
            })?;
            Ok((moved, utf16_len(&contents(&p)?)))
        })?;
    }
    uia_call(|| unsafe {
        p.MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            &p,
            TextPatternRangeEndpoint_End,
        )
    })?;
    Ok(p)
}

/// A nondegenerate provider character avoids backward affinity at format
/// boundaries and does not require an endpoint inside a CRLF or grapheme.
pub(super) fn format_sample(
    doc: &IUIAutomationTextRange,
    offset: u32,
) -> Result<IUIAutomationTextRange> {
    let text = contents(doc)?;
    slice_utf16(
        &text,
        TextRange {
            start: offset,
            end: offset,
        },
    )?;
    let sample = point(doc, &text, None, offset)?;
    uia_call(|| unsafe {
        sample.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1)
    })?;
    Ok(sample)
}

pub(super) fn unit_at(
    doc: &IUIAutomationTextRange,
    text: &str,
    offset: u32,
    unit: windows::Win32::UI::Accessibility::TextUnit,
) -> Result<IUIAutomationTextRange> {
    let p = range_in(
        doc,
        text,
        TextRange {
            start: offset,
            end: offset,
        },
    )?;
    if offset > 0 && offset == utf16_len(text) {
        // End has backward affinity in our API: ask for the last unit,
        // rather than the empty paragraph after a trailing newline. Use a
        // native Character step, not length-1 (which may be inside CRLF).
        uia_call(|| unsafe {
            p.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)
        })?;
        uia_call(|| unsafe {
            p.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &p,
                TextPatternRangeEndpoint_Start,
            )
        })?;
    }
    uia_call(|| unsafe { p.ExpandToEnclosingUnit(unit) })?;
    Ok(p)
}

fn attribute(range: &IUIAutomationTextRange, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
    uia_call(|| unsafe { range.GetAttributeValue(id) })
}

pub(super) fn attributes(
    range: &IUIAutomationTextRange,
    wpf_colors: bool,
) -> Result<xa11y_core::text_api::TextAttributes> {
    use windows::Win32::System::Variant::{VT_BOOL, VT_BSTR, VT_I4, VT_R8};
    let integer = |id| -> Result<Option<i32>> {
        let v = attribute(range, id)?;
        Ok((unsafe { v.Anonymous.Anonymous.vt } == VT_I4)
            .then(|| variant_i32(&v))
            .flatten())
    };
    let boolean = |id| -> Result<Option<bool>> {
        let v = attribute(range, id)?;
        Ok((unsafe { v.Anonymous.Anonymous.vt } == VT_BOOL)
            .then(|| variant_bool(&v))
            .flatten())
    };
    let color = |id| -> Result<Option<Color>> {
        // WPF's TextRangeAdaptor.ColorFromBrush emits 0xRRGGBB,
        // unlike UIA's documented COLORREF (0xBBGGRR). This is a provider
        // convention, selected by FrameworkId, never by the observed value.
        // https://source.dot.net/PresentationFramework/System/Windows/Controls/TextRangeAdaptor.cs.html
        Ok(integer(id)?.map(|n| decode_color(n, wpf_colors)))
    };
    let font = attribute(range, UIA_FontNameAttributeId)?;
    let size = attribute(range, UIA_FontSizeAttributeId)?;
    let super_script = boolean(UIA_IsSuperscriptAttributeId)?;
    let sub_script = boolean(UIA_IsSubscriptAttributeId)?;
    Ok(TextAttributeParts {
        font_family: if unsafe { font.Anonymous.Anonymous.vt } == VT_BSTR {
            Some(
                windows::core::BSTR::try_from(&font)
                    .map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: format!("UIA font name: {e}"),
                    })?
                    .to_string(),
            )
        } else {
            None
        },
        font_size: if unsafe { size.Anonymous.Anonymous.vt } == VT_R8 {
            Some(unsafe { size.Anonymous.Anonymous.Anonymous.dblVal })
        } else {
            None
        },
        font_weight: integer(UIA_FontWeightAttributeId)?.and_then(|n| u16::try_from(n).ok()),
        italic: boolean(UIA_IsItalicAttributeId)?,
        underline: integer(UIA_UnderlineStyleAttributeId)?
            .map(|n| n != TextDecorationLineStyle_None.0),
        strikethrough: integer(UIA_StrikethroughStyleAttributeId)?
            .map(|n| n != TextDecorationLineStyle_None.0),
        foreground: color(UIA_ForegroundColorAttributeId)?,
        background: color(UIA_BackgroundColorAttributeId)?,
        script: match (super_script, sub_script) {
            (Some(true), _) => Some(TextScript::Superscript),
            (_, Some(true)) => Some(TextScript::Subscript),
            (Some(false), Some(false)) => Some(TextScript::Baseline),
            _ => None,
        },
        spelling_error: spelling_error(&attribute(range, UIA_AnnotationTypesAttributeId)?)?,
    }
    .into())
}

fn spelling_error(value: &VARIANT) -> Result<Option<bool>> {
    use windows::Win32::System::Ole::{
        SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
    };
    use windows::Win32::System::Variant::{VT_ARRAY, VT_I4};
    if unsafe { value.Anonymous.Anonymous.vt }.0 != VT_ARRAY.0 | VT_I4.0 {
        // NotSupported and MixedAttributeValue are reserved VT_UNKNOWN values.
        return Ok(None);
    }
    // The VARIANT owns this array for the entire read. It must not be destroyed
    // separately. The exact element type was checked before copying any i32.
    let array = unsafe { value.Anonymous.Anonymous.Anonymous.parray };
    if array.is_null() || unsafe { SafeArrayGetDim(array) } != 1 {
        return Err(Error::Platform {
            code: -1,
            message: "UIA AnnotationTypes must be a one-dimensional i32 array".into(),
        });
    }
    let lower = uia_call(|| unsafe { SafeArrayGetLBound(array, 1) })?;
    let upper = uia_call(|| unsafe { SafeArrayGetUBound(array, 1) })?;
    for index in lower..=upper {
        let annotation = uia_call(|| {
            let mut n = 0i32;
            unsafe { SafeArrayGetElement(array, &index, (&raw mut n).cast()) }?;
            Ok(n)
        })?;
        if annotation == AnnotationType_SpellingError.0 {
            return Ok(Some(true));
        }
    }
    Ok(Some(false))
}

fn decode_color(n: i32, wpf: bool) -> Color {
    let (low, high) = (n as u8, (n >> 16) as u8);
    Color {
        r: if wpf { high } else { low },
        g: (n >> 8) as u8,
        b: if wpf { low } else { high },
        a: 255,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_search_is_logarithmic_for_different_provider_units() {
        // 30k UTF-16 units: scalars, CRLF/grapheme pairs, and surrogate units.
        for (width, estimate) in [(1, 15_000), (2, 30_000), (3, 30_000)] {
            let target = 30_000;
            let mut cursor = 0i32;
            let mut calls = 0;
            seek_endpoint(target, estimate, |step| {
                calls += 1;
                let next = (cursor + step).clamp(0, 50_000 / width);
                let moved = next - cursor;
                cursor = next;
                Ok((moved, (cursor * width) as u32))
            })
            .expect("reachable endpoint");
            assert_eq!(cursor * width, target as i32);
            assert!(calls < 25, "width {width}: {calls} remote prefix reads");
        }
    }

    #[test]
    fn endpoint_search_rejects_indivisible_clusters_and_stopped_providers() {
        for target in [15, 50] {
            let mut cursor = 0i32;
            let result = seek_endpoint(target, target as i32, |step| {
                let next = (cursor + step).clamp(0, 10);
                let moved = next - cursor;
                cursor = next;
                Ok((moved, (cursor * 2) as u32))
            });
            assert!(matches!(result, Err(Error::Unsupported { .. })));
        }
        let error = seek_endpoint(1, 1, |_| {
            Err(Error::stale(
                xa11y_core::StaleReason::Removed,
                "native endpoint",
            ))
        });
        assert!(matches!(error, Err(Error::ElementStale { .. })));
    }

    #[test]
    fn color_conventions_keep_red_and_blue_distinct() {
        assert_eq!(decode_color(0x123456, false).to_hex(), "#563412");
        assert_eq!(decode_color(0x123456, true).to_hex(), "#123456");
    }

    #[test]
    fn spelling_annotations_preserve_missing_false_and_true() {
        use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
        use windows::Win32::System::Variant::{VT_ARRAY, VT_I4};
        assert_eq!(spelling_error(&VARIANT::default()).expect("missing"), None);
        for (annotation, expected) in [
            (AnnotationType_Comment.0, false),
            (AnnotationType_SpellingError.0, true),
        ] {
            // A nonzero lower bound catches clients that assume array indices
            // always start at zero. The VARIANT takes ownership of the array.
            let array = unsafe { SafeArrayCreateVector(VT_I4, 7, 1) };
            assert!(!array.is_null(), "create annotation array");
            let mut value = VARIANT::default();
            unsafe {
                (*value.Anonymous.Anonymous).vt = VT_ARRAY | VT_I4;
                (*value.Anonymous.Anonymous).Anonymous.parray = array;
                SafeArrayPutElement(array, &7, (&raw const annotation).cast())
                    .expect("set annotation");
            }
            assert_eq!(spelling_error(&value).expect("annotation"), Some(expected));
        }
    }
}
