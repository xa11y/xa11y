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

pub(super) fn cell_info(
    e: &IUIAutomationElement,
    walker: Option<&IUIAutomationTreeWalker>,
) -> Result<Option<CellInfo>> {
    let index =
        |id| -> Result<Option<u32>> { Ok(property(e, id)?.and_then(|n| u32::try_from(n).ok())) };
    let info: CellInfo = CellParts {
        row: index(UIA_GridItemRowPropertyId)?,
        column: index(UIA_GridItemColumnPropertyId)?,
        row_span: index(UIA_GridItemRowSpanPropertyId)?.filter(|n| *n > 0),
        column_span: index(UIA_GridItemColumnSpanPropertyId)?.filter(|n| *n > 0),
        header: header_kind(e, walker)?,
    }
    .into();
    Ok((!info.is_empty()).then_some(info))
}

fn header_kind(
    e: &IUIAutomationElement,
    walker: Option<&IUIAutomationTreeWalker>,
) -> Result<Option<HeaderKind>> {
    match uia_cached_bstr(e, UIA_AriaRolePropertyId).as_deref() {
        Some("columnheader") => return Ok(Some(HeaderKind::Column)),
        Some("rowheader") => return Ok(Some(HeaderKind::Row)),
        _ => {}
    }
    // Only HeaderItems need the ancestor query. UIA's HeaderItem role does
    // not encode orientation; the owning Table's header arrays do.
    if uia_call(|| unsafe { e.CachedControlType() })? != UIA_HeaderItemControlTypeId {
        return Ok(None);
    }
    let Some(walker) = walker else {
        return Ok(None);
    };
    let identity = WindowsProvider::runtime_id(e, 0)?;
    if identity.is_empty() {
        return Ok(None);
    }
    let mut ancestor = e.clone();
    for _ in 0..HOST_WINDOW_WALK_LIMIT {
        ancestor = match unsafe { walker.GetParentElement(&ancestor) } {
            Ok(parent) => parent,
            Err(e) if e.code().0 == 0 => return Ok(None),
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("finding a header's owning Table: {e}"),
                })
            }
        };
        if let Some(table) = current_pattern::<IUIAutomationTablePattern>(
            &ancestor,
            UIA_TablePatternId,
            "TablePattern",
        )? {
            for kind in [HeaderKind::Column, HeaderKind::Row] {
                let headers = uia_call(|| unsafe {
                    match kind {
                        HeaderKind::Column => table.GetCurrentColumnHeaders(),
                        HeaderKind::Row => table.GetCurrentRowHeaders(),
                    }
                })?;
                for i in 0..uia_call(|| unsafe { headers.Length() })? {
                    let header = uia_call(|| unsafe { headers.GetElement(i) })?;
                    if WindowsProvider::runtime_id(&header, 0)? == identity {
                        return Ok(Some(kind));
                    }
                }
            }
            return Ok(None);
        }
    }
    Err(Error::Unsupported {
        feature: "UIA header ancestry exceeds 256 elements".into(),
    })
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

pub(super) fn range(
    doc: &IUIAutomationTextRange,
    wanted: TextRange,
) -> Result<IUIAutomationTextRange> {
    let text = contents(doc)?;
    slice_utf16(&text, wanted)?;
    let point = |target: u32| -> Result<IUIAutomationTextRange> {
        let p = uia_call(|| unsafe { doc.Clone() })?;
        uia_call(|| unsafe {
            p.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                doc,
                TextPatternRangeEndpoint_Start,
            )
        })?;
        // Start with the scalar count, then measure what the provider actually
        // moved. Usually this needs one call, including text after emoji.
        let chars = xa11y_core::text_api::utf16_to_char_offset(&text, target)?;
        let count = i32::try_from(chars).map_err(|_| Error::Unsupported {
            feature: "UIA text offsets exceeding i32::MAX characters".into(),
        })?;
        if count > 0 {
            uia_call(|| unsafe {
                p.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, count)
            })?;
        }
        let mut pos = utf16_len(&contents(&p)?);
        while pos != target {
            let direction = if pos < target { 1 } else { -1 };
            let moved = uia_call(|| unsafe {
                p.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, direction)
            })?;
            let next = utf16_len(&contents(&p)?);
            if moved == 0
                || next == pos
                || (pos < target && next > target)
                || (pos > target && next < target)
            {
                return Err(Error::Unsupported {
                    feature: format!(
                        "UIA provider cannot represent UTF-16 endpoint {target} (reached {next})"
                    ),
                });
            }
            pos = next;
        }
        uia_call(|| unsafe {
            p.MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                &p,
                TextPatternRangeEndpoint_End,
            )
        })?;
        Ok(p)
    };
    let result = point(wanted.start)?;
    let end = point(wanted.end)?;
    uia_call(|| unsafe {
        result.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &end,
            TextPatternRangeEndpoint_End,
        )
    })?;
    Ok(result)
}

pub(super) fn unit_at(
    doc: &IUIAutomationTextRange,
    offset: u32,
    unit: windows::Win32::UI::Accessibility::TextUnit,
) -> Result<IUIAutomationTextRange> {
    let p = range(
        doc,
        TextRange {
            start: offset,
            end: offset,
        },
    )?;
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
