//! AX tables, collection positions, and text by range (#477, #478, #479).
//!
//! # Offsets
//!
//! AX counts text in UTF-16 code units (`NSString` indices), which is the
//! unit every xa11y offset uses, so nothing here converts. What AX does not
//! do is refuse a range that splits a surrogate pair, so the reads below
//! check the units on each side of a boundary themselves.
//!
//! # What AX reports
//!
//! * Tables: `AXRowCount` / `AXColumnCount` (WebKit, Chromium), or else the
//!   length of `AXRows` / `AXColumns` (AppKit's `NSTableView`, which has no
//!   count attribute). A cell's position and span are its `AXRowIndexRange` /
//!   `AXColumnIndexRange`. A row's index is `AXIndex`. An `AXOutline`
//!   (`NSOutlineView`, an `NSTableView` subclass) keeps the list role but
//!   answers every table call the same way; see [`has_table_api`].
//! * Collections: `AXARIAPosInSet` / `AXARIASetSize` (1-based) on rows, tabs
//!   and radio buttons, and `AXDisclosureLevel` (0-based) for the depth of an
//!   outline row.
//! * Header cells carry no header marker on the cell itself, so
//!   [`CellInfo::header`] stays `None`; `table_headers` and `cell_headers`
//!   answer from the table's and cell's header attributes instead.
//! * Text units: a character is one Unicode scalar, read from the text
//!   itself (AX's `AXRangeForIndex` answers a grapheme cluster instead). AX
//!   has parameterized attributes for a rendered line (`AXLineForIndex` +
//!   `AXRangeForLine`), and none for a word or a paragraph, so those two
//!   units are `Unsupported`.
//! * Selection: `AXSelectedTextRanges` where the element has it, else
//!   `AXSelectedTextRange`. Web content (an element listing
//!   `AXSelectedTextMarkerRange`: Chromium, WebKit) has one DOM selection,
//!   and Chromium fails a read of the plural attribute with
//!   `kAXErrorFailure` whenever nothing is selected, so web content reads
//!   the singular one (#665).
//! * Geometry: `AXBoundsForRange`. In web content an editor's text is
//!   rendered by `AXStaticText` descendants and only they answer it (a
//!   Chromium `contenteditable` answers nothing), so a range is split
//!   across them; see `text_leaves`.
use super::*;
use xa11y_core::table::{
    index_from_one_based, CellInfo, CellParts, CollectionInfo, CollectionParts, Count, HeaderKind,
    RowInfo, RowParts, TableInfo, TableParts,
};
use xa11y_core::text_api::{
    check_in_bounds, Color, TextAttributeParts, TextRange, TextRun, TextScript, TextUnit,
};

const AX_ERROR_ILLEGAL_ARGUMENT: i32 = -25201;
const AX_ERROR_PARAMETERIZED_ATTRIBUTE_UNSUPPORTED: i32 = -25213;

/// How far `cell_headers` climbs from a cell to find its table.
const TABLE_ANCESTOR_LIMIT: usize = 8;

/// Mirrors `XA11yTextRun` in `exception_safe.m`.
#[repr(C)]
struct XA11yTextRun {
    family: CFStringRef,
    size: f64,
    weight: i32,
    italic: i32,
    underline: i32,
    strikethrough: i32,
    script: i32,
    misspelled: i32,
    has_foreground: i32,
    foreground: [u8; 4],
    has_background: i32,
    background: [u8; 4],
}

extern "C" {
    fn safe_ax_copy_parameterized_attribute_value(
        element: AXUIElementRef,
        attribute: CFStringRef,
        parameter: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn safe_ax_copy_attribute_names(element: AXUIElementRef, names: *mut CFArrayRef) -> i32;
    fn safe_ax_get_attribute_value_count(
        element: AXUIElementRef,
        attribute: CFStringRef,
        count: *mut CFIndex,
    ) -> i32;
    fn safe_ax_copy_attribute_values(
        element: AXUIElementRef,
        attribute: CFStringRef,
        index: CFIndex,
        max_values: CFIndex,
        values: *mut CFArrayRef,
    ) -> i32;
    fn safe_ax_value_get_range(
        value: CFTypeRef,
        location: *mut CFIndex,
        length: *mut CFIndex,
    ) -> bool;
    fn safe_ax_value_get_rect(
        value: CFTypeRef,
        x: *mut f64,
        y: *mut f64,
        w: *mut f64,
        h: *mut f64,
    ) -> bool;
    fn safe_cf_string_copy_utf16(value: CFTypeRef, buffer: *mut u16, capacity: CFIndex) -> CFIndex;
    fn safe_ax_decode_text_run(attributed: CFTypeRef, out: *mut XA11yTextRun) -> i32;
    #[cfg(test)]
    fn xa11y_test_font_traits(name: CFStringRef, size: f64, italic: *mut i32) -> i32;
}

// ── CF values ───────────────────────────────────────────────────────────────

/// An owned (+1) CF value, released on drop.
struct Owned(CFTypeRef);

impl Owned {
    fn ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { safe_cf_release(self.0) };
        }
    }
}

/// A borrowed CFNumber as an integer. `None` for null or any other type.
pub(super) fn cf_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    unsafe {
        if safe_cf_get_type_id(v) != safe_cf_number_get_type_id() {
            return None;
        }
        let mut n: i64 = 0;
        safe_cf_number_get_value(v, CF_NUMBER_SINT64, &mut n as *mut _ as *mut c_void).then_some(n)
    }
}

/// A borrowed AXValue holding a CFRange, as `(location, length)`.
pub(super) fn cf_range(v: CFTypeRef) -> Option<(i64, i64)> {
    let (mut location, mut length): (CFIndex, CFIndex) = (0, 0);
    unsafe { safe_ax_value_get_range(v, &mut location, &mut length) }
        .then_some((location as i64, length as i64))
}

fn cf_rect(v: CFTypeRef) -> Option<(f64, f64, f64, f64)> {
    let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
    unsafe { safe_ax_value_get_rect(v, &mut x, &mut y, &mut w, &mut h) }.then_some((x, y, w, h))
}

/// A borrowed CFString's UTF-16 code units.
fn cf_utf16(v: CFTypeRef) -> Option<Vec<u16>> {
    let n = unsafe { safe_cf_string_copy_utf16(v, std::ptr::null_mut(), 0) };
    if n < 0 {
        return None;
    }
    let mut buf = vec![0u16; n as usize];
    let copied = unsafe { safe_cf_string_copy_utf16(v, buf.as_mut_ptr(), n) };
    (copied == n).then_some(buf)
}

fn range_value(range: TextRange) -> Result<Owned> {
    let v = unsafe {
        safe_ax_value_create_cf_range(range.start as isize, (range.end - range.start) as isize)
    };
    if v.is_null() {
        return Err(Error::Platform {
            code: -1,
            message: format!("could not create a CFRange AXValue for {range}"),
        });
    }
    Ok(Owned(v))
}

/// `NSNotFound`, which AppKit answers as the location of "no selection".
const NS_NOT_FOUND: i64 = i64::MAX;

/// Retain `value` as an element, refusing any other CF type: an app can
/// answer an element-valued attribute with `NSNull` or a string, and AX
/// calls on a non-element are undefined.
fn element_from(value: CFTypeRef, attribute: &str) -> Result<AXElement> {
    if unsafe { safe_ax_is_element(value) } {
        Ok(AXElement::from_borrowed(value as AXUIElementRef))
    } else {
        Err(Error::Platform {
            code: -1,
            message: format!("{attribute} answered a value that is not an accessibility element"),
        })
    }
}

/// A CFRange attribute, keeping read errors.
fn read_range(element: AXUIElementRef, attribute: &str) -> Result<Option<(i64, i64)>> {
    Ok(read(element, attribute)?
        .value()
        .and_then(|v| cf_range(v.ptr())))
}

/// Whether `[loc, loc + len)` (a span of at least 1) covers `i`, without
/// overflowing on an NSNotFound location.
fn covers(range: Option<(i64, i64)>, i: u32) -> bool {
    range.is_some_and(|(loc, len)| {
        let i = i64::from(i);
        loc <= i && loc.checked_add(len.max(1)).is_some_and(|end| i < end)
    })
}

fn platform(attribute: &str, code: i32) -> Error {
    Error::Platform {
        code: code as i64,
        message: format!("reading {attribute} failed (AXError {code})"),
    }
}

/// The answer to one AX read.
enum Read {
    Value(Owned),
    /// The attribute exists but holds nothing (`kAXErrorNoValue`, or a null
    /// value on success).
    NoValue,
    /// The element does not have this attribute.
    Unsupported,
}

impl Read {
    fn value(self) -> Option<Owned> {
        match self {
            Read::Value(v) => Some(v),
            Read::NoValue | Read::Unsupported => None,
        }
    }
}

fn read(element: AXUIElementRef, attribute: &str) -> Result<Read> {
    let name = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, name.as_concrete_TypeRef() as CFTypeRef, &mut value);
    match err {
        AX_ERROR_SUCCESS if value.is_null() => Ok(Read::NoValue),
        AX_ERROR_SUCCESS => Ok(Read::Value(Owned(value))),
        AX_ERROR_NO_VALUE => Ok(Read::NoValue),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED => Ok(Read::Unsupported),
        code => Err(platform(attribute, code)),
    }
}

fn read_param(element: AXUIElementRef, attribute: &str, parameter: CFTypeRef) -> Result<Read> {
    let name = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err = ax_bounded(
        element,
        || format!("reading {attribute}"),
        || unsafe {
            safe_ax_copy_parameterized_attribute_value(
                element,
                name.as_concrete_TypeRef() as CFStringRef,
                parameter,
                &mut value,
            )
        },
    );
    match err {
        AX_ERROR_SUCCESS if value.is_null() => Ok(Read::NoValue),
        AX_ERROR_SUCCESS => Ok(Read::Value(Owned(value))),
        AX_ERROR_NO_VALUE => Ok(Read::NoValue),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_PARAMETERIZED_ATTRIBUTE_UNSUPPORTED => {
            Ok(Read::Unsupported)
        }
        code => Err(platform(attribute, code)),
    }
}

/// A parameterized read the element must answer: an unsupported attribute or
/// an empty answer is `Unsupported`, naming the attribute.
fn require_param(element: AXUIElementRef, attribute: &str, parameter: CFTypeRef) -> Result<Owned> {
    match read_param(element, attribute, parameter)? {
        Read::Value(v) => Ok(v),
        Read::NoValue | Read::Unsupported => Err(Error::Unsupported {
            feature: format!("this element does not answer {attribute}"),
        }),
    }
}

/// Whether `element` lists `attribute` among its attribute names.
///
/// For a capability decided by what kind of element this is rather than by
/// the error one read returns (see `rich_text_selections`).
fn advertises(element: AXUIElementRef, attribute: &str) -> Result<bool> {
    let mut names: CFArrayRef = std::ptr::null();
    let err = ax_bounded(
        element,
        || "reading an element's attribute names".to_string(),
        || unsafe { safe_ax_copy_attribute_names(element, &mut names) },
    );
    if err != AX_ERROR_SUCCESS {
        return Err(Error::Platform {
            code: err as i64,
            message: format!(
                "reading the attribute names (to check for {attribute}) failed (AXError {err})"
            ),
        });
    }
    if names.is_null() {
        return Ok(false);
    }
    let names = Owned(names);
    unsafe {
        if safe_cf_get_type_id(names.ptr()) != safe_cf_array_get_type_id() {
            return Err(Error::Platform {
                code: -1,
                message: "the attribute names are not an array".to_string(),
            });
        }
        let want: Vec<u16> = attribute.encode_utf16().collect();
        for i in 0..safe_cf_array_get_count(names.ptr()) {
            if cf_utf16(safe_cf_array_get_value(names.ptr(), i)).as_deref() == Some(&want[..]) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Upper bound on the descendants [`text_leaves`] visits, so a bounds call
/// on a huge editor fails loudly rather than walking the whole document.
const TEXT_LEAF_NODE_LIMIT: usize = 4096;

/// `"AXStaticText"` as UTF-16, for comparing a role read as a CFString.
const STATIC_TEXT: &[u16] = &[
    0x41, 0x58, 0x53, 0x74, 0x61, 0x74, 0x69, 0x63, 0x54, 0x65, 0x78, 0x74,
];

/// One `AXStaticText` descendant of a text element and the span of the
/// element's text it renders.
struct TextLeaf {
    element: AXElement,
    start: u32,
    end: u32,
}

/// Whether `u` is text the element holds that no `AXStaticText` renders: a
/// line break between blocks or at a `<br>`, or an embedded object.
fn is_unrendered_gap(u: u16) -> bool {
    is_line_terminator(u) || u == 0xFFFC
}

/// The `AXStaticText` descendants of `ax`, in document order, each placed
/// at the span of the element's own text (its first `length` units, read
/// only once a static text is found) it renders. The walk stops at the
/// first leaf that ends past `until`, so a range near the start of a long
/// editor does not visit the rest of it.
///
/// Web content keeps an editor's rendered text in static-text children, and
/// only those answer `AXBoundsForRange`: Chromium answers it on a node that
/// is text or an atomic text field (`<input>`, `<textarea>`), and a
/// `contenteditable` is neither (#665). A leaf's text must start where the
/// previous one ended, after at most a run of unrendered characters (see
/// [`is_unrendered_gap`]); any other mismatch is an error rather than a
/// guessed position.
fn text_leaves(ax: AXUIElementRef, length: u32, until: u32) -> Result<Vec<TextLeaf>> {
    let mut leaves = Vec::new();
    let mut text: Option<Vec<u16>> = None;
    let mut cursor = 0usize;
    let mut visited = 0usize;
    // Depth-first, children in order.
    let mut stack: Vec<AXElement> = ax_element_list_strict(ax, "AXChildren")?;
    stack.reverse();
    while let Some(node) = stack.pop() {
        // Leaves past the range cannot change its bounds; an empty range at
        // `until` still needs the leaf that starts there.
        if leaves.last().is_some_and(|l: &TextLeaf| l.end > until) {
            break;
        }
        visited += 1;
        if visited > TEXT_LEAF_NODE_LIMIT {
            return Err(Error::Unsupported {
                feature: format!(
                    "text bounds in an element with more than {TEXT_LEAF_NODE_LIMIT} descendants"
                ),
            });
        }
        let is_static_text = match read(node.as_ptr(), "AXRole")? {
            Read::Value(v) => cf_utf16(v.ptr()).as_deref() == Some(STATIC_TEXT),
            Read::NoValue | Read::Unsupported => false,
        };
        if !is_static_text {
            let mut children = ax_element_list_strict(node.as_ptr(), "AXChildren")?;
            children.reverse();
            stack.extend(children);
            continue;
        }
        let units = match read(node.as_ptr(), "AXValue")? {
            Read::Value(v) => cf_utf16(v.ptr()).ok_or_else(|| Error::Platform {
                code: -1,
                message: "a static text's AXValue is not a string".to_string(),
            })?,
            Read::NoValue | Read::Unsupported => Vec::new(),
        };
        if units.is_empty() {
            continue;
        }
        let text = match &mut text {
            Some(t) => t,
            None => text.insert(read_units(
                ax,
                TextRange {
                    start: 0,
                    end: length,
                },
            )?),
        };
        let mut at = cursor;
        while !text[at..].starts_with(&units) {
            match text.get(at) {
                Some(&u) if is_unrendered_gap(u) => at += 1,
                _ => {
                    return Err(Error::Platform {
                        code: -1,
                        message: format!(
                            "a static text descendant's text ({:?}) does not continue the \
                             element's text at offset {cursor}",
                            String::from_utf16_lossy(&units)
                        ),
                    })
                }
            }
        }
        cursor = at + units.len();
        leaves.push(TextLeaf {
            element: node,
            start: at as u32,
            end: cursor as u32,
        });
    }
    Ok(leaves)
}

/// `AXBoundsForRange` on one element, as `(x, y, w, h)`; `None` when it
/// reports no geometry.
fn bounds_for_range(ax: AXUIElementRef, range: TextRange) -> Result<Option<(f64, f64, f64, f64)>> {
    let param = range_value(range)?;
    let Some(v) = read_param(ax, "AXBoundsForRange", param.ptr())?.value() else {
        return Ok(None);
    };
    let Some((x, y, w, h)) = cf_rect(v.ptr()) else {
        return Err(Error::Platform {
            code: -1,
            message: format!("AXBoundsForRange({range}) is not a CGRect"),
        });
    };
    if w <= 0.0 && h <= 0.0 {
        return Ok(None);
    }
    Ok(Some((x, y, w.max(0.0), h.max(0.0))))
}

/// The union of what each leaf overlapping `range` reports for its part of
/// it. An empty range asks the leaf it falls in (the one it starts, at a
/// boundary between two). `None` when no part has geometry, which includes
/// a range over unrendered characters only.
fn leaves_bounds(leaves: &[TextLeaf], range: TextRange) -> Result<Option<(f64, f64, f64, f64)>> {
    let mut union: Option<(f64, f64, f64, f64)> = None;
    for leaf in leaves {
        let part = if range.is_empty() {
            let last = std::ptr::eq(leaf, &leaves[leaves.len() - 1]);
            if !(leaf.start <= range.start
                && (range.start < leaf.end || (last && range.start == leaf.end)))
            {
                continue;
            }
            TextRange {
                start: range.start - leaf.start,
                end: range.start - leaf.start,
            }
        } else {
            let start = range.start.max(leaf.start);
            let end = range.end.min(leaf.end);
            if start >= end {
                continue;
            }
            TextRange {
                start: start - leaf.start,
                end: end - leaf.start,
            }
        };
        if let Some((x, y, w, h)) = bounds_for_range(leaf.element.as_ptr(), part)? {
            union = Some(match union {
                None => (x, y, w, h),
                Some((ux, uy, uw, uh)) => {
                    let (l, t) = (ux.min(x), uy.min(y));
                    let (r, b) = ((ux + uw).max(x + w), (uy + uh).max(y + h));
                    (l, t, r - l, b - t)
                }
            });
        }
        if range.is_empty() {
            break;
        }
    }
    Ok(union)
}

fn number_param(n: u32) -> CFNumber {
    CFNumber::from(i64::from(n))
}

/// Length of an array attribute without copying it. `None` when the element
/// has no such attribute.
fn attribute_count(element: AXUIElementRef, attribute: &str) -> Result<Option<u32>> {
    let name = CFString::new(attribute);
    let mut count: CFIndex = 0;
    let err = ax_bounded(
        element,
        || format!("counting {attribute}"),
        || unsafe {
            safe_ax_get_attribute_value_count(
                element,
                name.as_concrete_TypeRef() as CFStringRef,
                &mut count,
            )
        },
    );
    match err {
        AX_ERROR_SUCCESS => Ok(u32::try_from(count).ok()),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(None),
        code => Err(platform(attribute, code)),
    }
}

/// The element at `index` of an array attribute, fetching only that one.
/// `None` past the end; `Unsupported` when the element has no such
/// attribute, which is not the same answer as "no element there".
fn attribute_element_at(
    element: AXUIElementRef,
    attribute: &str,
    index: u32,
) -> Result<Option<AXElement>> {
    let name = CFString::new(attribute);
    let mut values: CFArrayRef = std::ptr::null();
    let err = ax_bounded(
        element,
        || format!("reading {attribute} at index {index}"),
        || unsafe {
            safe_ax_copy_attribute_values(
                element,
                name.as_concrete_TypeRef() as CFStringRef,
                index as CFIndex,
                1,
                &mut values,
            )
        },
    );
    match err {
        AX_ERROR_SUCCESS if values.is_null() => Ok(None),
        AX_ERROR_SUCCESS => {
            let values = Owned(values);
            unsafe {
                if safe_cf_get_type_id(values.ptr()) != safe_cf_array_get_type_id()
                    || safe_cf_array_get_count(values.ptr()) < 1
                {
                    return Ok(None);
                }
                let first = safe_cf_array_get_value(values.ptr(), 0);
                if first.is_null() {
                    return Ok(None);
                }
                element_from(first, attribute).map(Some)
            }
        }
        // Past the end of the array.
        AX_ERROR_ILLEGAL_ARGUMENT | AX_ERROR_NO_VALUE => Ok(None),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED => Err(Error::Unsupported {
            feature: format!("this element has no {attribute} to index"),
        }),
        code => Err(platform(attribute, code)),
    }
}

// ── Snapshot metadata ───────────────────────────────────────────────────────

/// The table / collection attributes of one element, as AX reported them.
#[derive(Default)]
pub(super) struct PositionAttrs {
    pub(super) row_count: Option<i64>,
    pub(super) column_count: Option<i64>,
    pub(super) row_range: Option<(i64, i64)>,
    pub(super) column_range: Option<(i64, i64)>,
    pub(super) index: Option<i64>,
    pub(super) disclosure_level: Option<i64>,
    pub(super) pos_in_set: Option<i64>,
    pub(super) set_size: Option<i64>,
}

fn individual_i64(element: AXUIElementRef, attribute: &str) -> Option<i64> {
    let v = ax_attr(element, attribute)?;
    let n = cf_i64(v);
    unsafe { safe_cf_release(v) };
    n
}

fn individual_range(element: AXUIElementRef, attribute: &str) -> Option<(i64, i64)> {
    let v = ax_attr(element, attribute)?;
    let r = cf_range(v);
    unsafe { safe_cf_release(v) };
    r
}

/// The `table`, `cell`, `table_row` and `collection` fields of
/// `ElementParts`, in that order.
pub(super) type Metadata = (
    Option<TableInfo>,
    Option<CellInfo>,
    Option<RowInfo>,
    Option<CollectionInfo>,
);

/// Whether an element answers the table APIs (`table` info, `cell_at`, table
/// headers). A table does, and so does an AppKit outline: `AXOutline` maps to
/// [`Role::List`] (selectors written against that role keep working), but
/// `NSOutlineView` is an `NSTableView` and has the same `AXRows`,
/// `AXColumns` and per-column `AXHeader` a table has.
fn has_table_api(role: Role, ax_role: &str) -> bool {
    role == Role::Table || ax_role == "AXOutline"
}

/// The roles whose snapshot carries a [`CollectionInfo`].
fn is_collection_role(role: Role) -> bool {
    matches!(
        role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::RadioButton | Role::TableRow
    )
}

impl PositionAttrs {
    /// The individual-read path's counterpart of the batch slots. Reads only
    /// the attributes the element's AX role can carry, so the fallback does
    /// not pay eight extra round trips on every element.
    pub(super) fn read_individually(
        element: AXUIElementRef,
        ax_role: &str,
        subrole: Option<&str>,
    ) -> Self {
        let mut attrs = Self::default();
        match ax_role {
            "AXTable" | "AXGrid" | "AXOutline" => {
                attrs.row_count = individual_i64(element, "AXRowCount");
                attrs.column_count = individual_i64(element, "AXColumnCount");
            }
            "AXCell" => {
                attrs.row_range = individual_range(element, "AXRowIndexRange");
                attrs.column_range = individual_range(element, "AXColumnIndexRange");
            }
            "AXRow" => {
                attrs.index = individual_i64(element, "AXIndex");
                attrs.disclosure_level = individual_i64(element, "AXDisclosureLevel");
            }
            _ => {}
        }
        // Only where `to_metadata` builds a collection; a position read
        // elsewhere is discarded. Decided on the mapped role, as there, so a
        // tab that is an `AXButton` with the `AXTabButton` subrole keeps
        // its position on this path as it does on the batch one.
        if is_collection_role(map_ax_role(ax_role, subrole)) {
            attrs.pos_in_set = individual_i64(element, "AXARIAPosInSet");
            attrs.set_size = individual_i64(element, "AXARIASetSize");
        }
        attrs
    }

    /// The four metadata fields of `ElementParts`.
    ///
    /// Fails when counting `AXRows` / `AXColumns` fails with anything but
    /// "no such attribute" or "no value" (a deadline cut included, which
    /// `ax_bounded` has already recorded): a count that could not be read is
    /// not the same answer as one the platform does not report.
    pub(super) fn to_metadata(
        &self,
        element: AXUIElementRef,
        role: Role,
        ax_role: &str,
        subrole: Option<&str>,
    ) -> Result<Metadata> {
        let table = if has_table_api(role, ax_role) {
            // AppKit's NSTableView and NSOutlineView report no AXRowCount /
            // AXColumnCount; their AXRows / AXColumns arrays hold every row
            // and column (AXVisibleRows is the realized subset), so their
            // length is the platform's total. `attribute_count` answers
            // `None` only for an attribute the element does not have or that
            // holds no value; any other failure is the read's error.
            let count = |reported: Option<i64>, array: &str| -> Result<Option<Count>> {
                match reported {
                    Some(n) => Ok(Some(Count::from_signed(n))),
                    None => Ok(attribute_count(element, array)?.map(Count::Known)),
                }
            };
            let info: TableInfo = TableParts {
                row_count: count(self.row_count, "AXRows")?,
                column_count: count(self.column_count, "AXColumns")?,
            }
            .into();
            Some(info)
        } else {
            None
        };
        let cell = (role == Role::TableCell).then(|| {
            let start = |r: Option<(i64, i64)>| r.and_then(|(loc, _)| u32::try_from(loc).ok());
            let span = |r: Option<(i64, i64)>| {
                r.and_then(|(_, len)| u32::try_from(len).ok())
                    .filter(|n| *n > 0)
            };
            let info: CellInfo = CellParts {
                row: start(self.row_range),
                column: start(self.column_range),
                row_span: span(self.row_range),
                column_span: span(self.column_range),
                // AX marks no cell as a header; see the module docs.
                header: None,
            }
            .into();
            info
        });
        let table_row = (ax_role == "AXRow")
            .then(|| self.index.and_then(|n| u32::try_from(n).ok()))
            .flatten()
            .map(|index| RowParts { index: Some(index) }.into());
        let collection = is_collection_role(role).then(|| {
            let info: CollectionInfo = CollectionParts {
                index: self.pos_in_set.and_then(index_from_one_based),
                // 0 is "not set"; -1 explicitly "unknown".
                size: self.set_size.filter(|n| *n != 0).map(Count::from_signed),
                // AXDisclosureLevel is already 0-based. Only an outline row's
                // level is a tree depth: AppKit reports 0 on every row of a
                // flat table too.
                depth: (role == Role::TreeItem || subrole == Some("AXOutlineRow"))
                    .then_some(self.disclosure_level)
                    .flatten()
                    .and_then(|n| u32::try_from(n).ok()),
            }
            .into();
            info
        });
        Ok((
            table.filter(|t| !t.is_empty()),
            cell.filter(|c| !c.is_empty()),
            table_row,
            collection.filter(|c| !c.is_empty()),
        ))
    }
}

// ── Text helpers ────────────────────────────────────────────────────────────

fn is_high(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

fn is_low(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

fn is_line_terminator(u: u16) -> bool {
    matches!(u, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029)
}

fn splits_pair(range: TextRange, length: u32, at: u32) -> Error {
    Error::InvalidTextRange {
        start: range.start,
        end: range.end,
        length: Some(length),
        message: format!("offset {at} splits a surrogate pair"),
    }
}

fn ax_text_length(ax: AXUIElementRef, element: &ElementData) -> Result<u32> {
    match read(ax, "AXNumberOfCharacters")? {
        Read::Value(v) => cf_i64(v.ptr())
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: "AXNumberOfCharacters is not a non-negative number".to_string(),
            }),
        Read::NoValue | Read::Unsupported => Err(Error::ActionNotSupported {
            action: "text".to_string(),
            role: element.role,
        }),
    }
}

/// The UTF-16 units of `range`, which the caller has bounds-checked.
fn read_units(ax: AXUIElementRef, range: TextRange) -> Result<Vec<u16>> {
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let param = range_value(range)?;
    let v = require_param(ax, "AXStringForRange", param.ptr())?;
    let units = cf_utf16(v.ptr()).ok_or_else(|| Error::Platform {
        code: -1,
        message: format!("AXStringForRange({range}) did not return a string"),
    })?;
    if units.len() as u32 != range.len() {
        return Err(Error::Platform {
            code: -1,
            message: format!(
                "AXStringForRange({range}) returned {} units, expected {}",
                units.len(),
                range.len()
            ),
        });
    }
    Ok(units)
}

/// Refuse `range` when `boundary` (strictly inside the text) falls between
/// the two halves of a surrogate pair.
fn check_boundary(ax: AXUIElementRef, range: TextRange, length: u32, boundary: u32) -> Result<()> {
    if boundary == 0 || boundary >= length {
        return Ok(());
    }
    let pair = read_units(
        ax,
        TextRange {
            start: boundary - 1,
            end: boundary + 1,
        },
    )?;
    if is_high(pair[0]) && is_low(pair[1]) {
        return Err(splits_pair(range, length, boundary));
    }
    Ok(())
}

/// Bounds- and surrogate-check `range`; returns the text length.
fn validate(ax: AXUIElementRef, element: &ElementData, range: TextRange) -> Result<u32> {
    let length = ax_text_length(ax, element)?;
    check_in_bounds(range, length)?;
    check_boundary(ax, range, length, range.start)?;
    if range.end != range.start {
        check_boundary(ax, range, length, range.end)?;
    }
    Ok(length)
}

/// A selected range, or `None` for AppKit's "no selection" (an NSNotFound
/// location).
fn selection_from(value: CFTypeRef, attribute: &str) -> Result<Option<TextRange>> {
    if cf_range(value).is_some_and(|(location, _)| location == NS_NOT_FOUND) {
        return Ok(None);
    }
    range_from(value, attribute).map(Some)
}

fn ranges_in(value: CFTypeRef) -> Result<Vec<TextRange>> {
    let mut out = Vec::new();
    unsafe {
        if safe_cf_get_type_id(value) != safe_cf_array_get_type_id() {
            return Err(Error::Platform {
                code: -1,
                message: "AXSelectedTextRanges is not an array".to_string(),
            });
        }
        for i in 0..safe_cf_array_get_count(value) {
            out.extend(selection_from(
                safe_cf_array_get_value(value, i),
                "AXSelectedTextRanges",
            )?);
        }
    }
    Ok(out)
}

fn range_from(value: CFTypeRef, attribute: &str) -> Result<TextRange> {
    let (location, length) = cf_range(value).ok_or_else(|| Error::Platform {
        code: -1,
        message: format!("{attribute} is not a CFRange"),
    })?;
    let start = u32::try_from(location);
    let end = location.checked_add(length).map(u32::try_from);
    match (start, end) {
        (Ok(start), Some(Ok(end))) if start <= end => Ok(TextRange { start, end }),
        _ => Err(Error::Platform {
            code: -1,
            message: format!("{attribute} reported the range ({location}, {length})"),
        }),
    }
}

fn color(rgba: [u8; 4]) -> Color {
    Color {
        r: rgba[0],
        g: rgba[1],
        b: rgba[2],
        a: rgba[3],
    }
}

fn flag(n: i32) -> Option<bool> {
    match n {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn run_attributes(run: &XA11yTextRun) -> TextAttributeParts {
    let family = (!run.family.is_null())
        .then(|| unsafe { CFString::wrap_under_create_rule(run.family as *const _) }.to_string());
    TextAttributeParts {
        font_family: family,
        font_size: (run.size >= 0.0).then_some(run.size),
        font_weight: u16::try_from(run.weight).ok().filter(|w| *w > 0),
        italic: flag(run.italic),
        underline: flag(run.underline),
        strikethrough: flag(run.strikethrough),
        foreground: (run.has_foreground != 0).then(|| color(run.foreground)),
        background: (run.has_background != 0).then(|| color(run.background)),
        script: match run.script {
            1 => Some(TextScript::Superscript),
            -1 => Some(TextScript::Subscript),
            0 => Some(TextScript::Baseline),
            _ => None,
        },
        spelling_error: flag(run.misspelled),
    }
}

// ── Provider methods ────────────────────────────────────────────────────────

fn ax_role_of(element: &ElementData) -> &str {
    element
        .raw
        .get("ax_role")
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

impl MacOSProvider {
    fn build_all(&self, elements: Vec<AXElement>, pid: Option<u32>) -> Result<Vec<ElementData>> {
        elements
            .iter()
            .map(|e| self.build_element_data(e, pid))
            .collect()
    }

    fn require_table(&self, table: &ElementData) -> Result<AXElement> {
        if !has_table_api(table.role, ax_role_of(table)) {
            return Err(Error::ActionNotSupported {
                action: "table".to_string(),
                role: table.role,
            });
        }
        self.get_cached(table.handle)
    }

    pub(super) fn rich_table_cell_at(
        &self,
        table: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        let ax = self.require_table(table)?;
        // WebKit and Chromium answer the slot directly.
        let slot = unsafe {
            let numbers = [number_param(column), number_param(row)];
            let ptrs: Vec<CFTypeRef> = numbers
                .iter()
                .map(|n| n.as_concrete_TypeRef() as CFTypeRef)
                .collect();
            Owned(safe_cf_array_create(ptrs.as_ptr(), ptrs.len() as CFIndex))
        };
        if slot.ptr().is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "could not create the AXCellForColumnAndRow parameter".to_string(),
            });
        }
        let name = CFString::new("AXCellForColumnAndRow");
        let mut value: CFTypeRef = std::ptr::null();
        let err = ax_bounded(
            ax.as_ptr(),
            || "reading AXCellForColumnAndRow".to_string(),
            || unsafe {
                safe_ax_copy_parameterized_attribute_value(
                    ax.as_ptr(),
                    name.as_concrete_TypeRef() as CFStringRef,
                    slot.ptr(),
                    &mut value,
                )
            },
        );
        match err {
            AX_ERROR_SUCCESS if value.is_null() => return Ok(None),
            AX_ERROR_SUCCESS => {
                let value = Owned(value);
                let cell = element_from(value.ptr(), "AXCellForColumnAndRow")?;
                return Ok(Some(self.build_element_data(&cell, table.pid)?));
            }
            // Out of range, or no cell at that slot.
            AX_ERROR_NO_VALUE | AX_ERROR_ILLEGAL_ARGUMENT => return Ok(None),
            AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_PARAMETERIZED_ATTRIBUTE_UNSUPPORTED => {}
            code => return Err(platform("AXCellForColumnAndRow", code)),
        }
        // AppKit's NSTableView and NSOutlineView have no slot query. Their
        // rows are `AXRows` in row order, and a view-based table's row holds
        // one AXCell per column, each reporting its own index ranges. An
        // outline row also holds its disclosure triangle, which reports no
        // column range and so never matches. The cell is the one
        // whose ranges cover the slot, so a row whose cells report no ranges
        // (a cell-based table, which has no AXCell layer) has no cell there.
        let Some(row_el) = attribute_element_at(ax.as_ptr(), "AXRows", row)? else {
            return Ok(None);
        };
        for child in ax_element_list_strict(row_el.as_ptr(), "AXChildren")? {
            let column_range = read_range(child.as_ptr(), "AXColumnIndexRange")?;
            if !covers(column_range, column) {
                continue;
            }
            let row_range = read_range(child.as_ptr(), "AXRowIndexRange")?;
            if row_range.is_some() && !covers(row_range, row) {
                continue;
            }
            return Ok(Some(self.build_element_data(&child, table.pid)?));
        }
        Ok(None)
    }

    pub(super) fn rich_table_headers(
        &self,
        table: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let ax = self.require_table(table)?;
        let attribute = match kind {
            HeaderKind::Column => "AXColumnHeaderUIElements",
            HeaderKind::Row => "AXRowHeaderUIElements",
        };
        let headers = ax_element_list_strict(ax.as_ptr(), attribute)?;
        if !headers.is_empty() || kind == HeaderKind::Row {
            // AppKit has no row headers; an empty answer is "none".
            return self.build_all(headers, table.pid);
        }
        // AppKit's NSTableView answers AXColumnHeaderUIElements with
        // nothing: each header is the `AXHeader` of its AXColumn, the same
        // place `cell_headers` reads it from.
        let mut headers = Vec::new();
        for column in ax_element_list_strict(ax.as_ptr(), "AXColumns")? {
            if let Some(h) = read(column.as_ptr(), "AXHeader")?.value() {
                headers.push(element_from(h.ptr(), "AXHeader")?);
            }
        }
        self.build_all(headers, table.pid)
    }

    pub(super) fn rich_cell_headers(
        &self,
        cell: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        if cell.role != Role::TableCell {
            return Err(Error::ActionNotSupported {
                action: "cell_headers".to_string(),
                role: cell.role,
            });
        }
        let ax = self.get_cached(cell.handle)?;
        let attribute = match kind {
            HeaderKind::Column => "AXColumnHeaderUIElements",
            HeaderKind::Row => "AXRowHeaderUIElements",
        };
        // WebKit and Chromium cells name their headers. An AppKit cell may
        // list the attribute and answer no value, so only a value counts.
        if let Read::Value(_) = read(ax.as_ptr(), attribute)? {
            let headers = ax_element_list_strict(ax.as_ptr(), attribute)?;
            return self.build_all(headers, cell.pid);
        }
        // AppKit cells do not. An NSTableView's header is per column: the
        // `AXHeader` of the AXColumn at the cell's column index. AppKit has
        // no row headers.
        let column = match (kind, cell.cell.and_then(|c| c.column)) {
            (HeaderKind::Column, Some(column)) => column,
            _ => return Ok(Vec::new()),
        };
        let mut ancestor = ax.clone();
        for _ in 0..TABLE_ANCESTOR_LIMIT {
            let parent = match read(ancestor.as_ptr(), "AXParent")?.value() {
                Some(p) => element_from(p.ptr(), "AXParent")?,
                None => break,
            };
            let role = read(parent.as_ptr(), "AXRole")?
                .value()
                .and_then(|v| cf_utf16(v.ptr()).and_then(|u| String::from_utf16(&u).ok()));
            if matches!(role.as_deref(), Some("AXTable" | "AXOutline" | "AXGrid")) {
                let Some(col) = attribute_element_at(parent.as_ptr(), "AXColumns", column)? else {
                    // The column the cell reports is not in its table.
                    return Err(Error::Platform {
                        code: -1,
                        message: format!(
                            "the cell reports column {column}, which its table's AXColumns does not have"
                        ),
                    });
                };
                let header = match read(col.as_ptr(), "AXHeader")?.value() {
                    Some(h) => vec![element_from(h.ptr(), "AXHeader")?],
                    // A column without a header.
                    None => Vec::new(),
                };
                return self.build_all(header, cell.pid);
            }
            ancestor = parent;
        }
        Err(Error::Unsupported {
            feature: format!(
                "column headers of a cell with no table within {TABLE_ANCESTOR_LIMIT} ancestors"
            ),
        })
    }

    pub(super) fn rich_selected_items(&self, container: &ElementData) -> Result<Vec<ElementData>> {
        let ax = self.get_cached(container.handle)?;
        // Tables and outlines select rows; lists, tab groups and the rest
        // select children.
        let attribute = match ax_role_of(container) {
            "AXTable" | "AXOutline" | "AXGrid" => "AXSelectedRows",
            _ => "AXSelectedChildren",
        };
        match read(ax.as_ptr(), attribute)? {
            Read::Unsupported => Err(Error::ActionNotSupported {
                action: "selected_items".to_string(),
                role: container.role,
            }),
            Read::NoValue => Ok(Vec::new()),
            Read::Value(_) => {
                let items = ax_element_list_strict(ax.as_ptr(), attribute)?;
                self.build_all(items, container.pid)
            }
        }
    }

    pub(super) fn rich_text_length(&self, element: &ElementData) -> Result<u32> {
        let ax = self.get_cached(element.handle)?;
        ax_text_length(ax.as_ptr(), element)
    }

    pub(super) fn rich_text_caret(&self, element: &ElementData) -> Result<Option<u32>> {
        let ax = self.get_cached(element.handle)?;
        match read(ax.as_ptr(), "AXSelectedTextRange")? {
            // AX has no separate caret: the insertion point is an empty
            // selection, and AppKit draws none while a range is selected.
            Read::Value(v) => Ok(selection_from(v.ptr(), "AXSelectedTextRange")?
                .filter(|r| r.is_empty())
                .map(|r| r.start)),
            Read::NoValue | Read::Unsupported => {
                ax_text_length(ax.as_ptr(), element)?;
                Ok(None)
            }
        }
    }

    pub(super) fn rich_text_selections(&self, element: &ElementData) -> Result<Vec<TextRange>> {
        let ax = self.get_cached(element.handle)?;
        // AXSelectedTextRanges (multiple selection) where the element has it;
        // AXSelectedTextRange is the single-range attribute every text
        // element has. Which one applies is a capability, not a retry.
        //
        // Web content has one DOM selection, and its element says so by
        // listing AXSelectedTextMarkerRange. Chromium also lists
        // AXSelectedTextRanges, but builds it as an array holding
        // AXSelectedTextRange, which is nil whenever there is no visible
        // caret or selection; the array literal then throws and AX answers
        // kAXErrorFailure (#665). So web content reads the singular one.
        let multiple = if advertises(ax.as_ptr(), "AXSelectedTextMarkerRange")? {
            Read::Unsupported
        } else {
            read(ax.as_ptr(), "AXSelectedTextRanges")?
        };
        let mut ranges = match multiple {
            Read::Value(v) => ranges_in(v.ptr())?,
            Read::NoValue => Vec::new(),
            Read::Unsupported => match read(ax.as_ptr(), "AXSelectedTextRange")? {
                Read::Value(v) => selection_from(v.ptr(), "AXSelectedTextRange")?
                    .into_iter()
                    .collect(),
                Read::NoValue => Vec::new(),
                Read::Unsupported => {
                    ax_text_length(ax.as_ptr(), element)?;
                    Vec::new()
                }
            },
        };
        ranges.retain(|r| !r.is_empty());
        ranges.sort_by_key(|r| r.start);
        Ok(ranges)
    }

    pub(super) fn rich_text_read(&self, element: &ElementData, range: TextRange) -> Result<String> {
        let ax = self.get_cached(element.handle)?;
        let length = ax_text_length(ax.as_ptr(), element)?;
        check_in_bounds(range, length)?;
        if range.is_empty() {
            // Still refused inside a surrogate pair, as a non-empty read is.
            check_boundary(ax.as_ptr(), range, length, range.start)?;
            return Ok(String::new());
        }
        // One read covering a unit on each side, so both boundaries are
        // checked without extra round trips.
        let lo = range.start.saturating_sub(1);
        let hi = (range.end + 1).min(length);
        let units = read_units(ax.as_ptr(), TextRange { start: lo, end: hi })?;
        let at = |offset: u32| units[(offset - lo) as usize];
        for boundary in [range.start, range.end] {
            if boundary > 0
                && boundary < length
                && is_high(at(boundary - 1))
                && is_low(at(boundary))
            {
                return Err(splits_pair(range, length, boundary));
            }
        }
        String::from_utf16(&units[(range.start - lo) as usize..(range.end - lo) as usize]).map_err(
            |e| Error::InvalidTextRange {
                start: range.start,
                end: range.end,
                length: Some(length),
                message: format!("the range holds a lone surrogate: {e}"),
            },
        )
    }

    pub(super) fn rich_text_range_at(
        &self,
        element: &ElementData,
        offset: u32,
        unit: TextUnit,
    ) -> Result<TextRange> {
        let ax = self.get_cached(element.handle)?;
        let length = ax_text_length(ax.as_ptr(), element)?;
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
        // At the end, answer for the unit that ends there. An offset on the
        // second half of a surrogate pair answers for the pair, for every
        // unit, the same as on Linux and Windows.
        let at = offset.min(length - 1);
        let lo = at.saturating_sub(1);
        let window = read_units(
            ax.as_ptr(),
            TextRange {
                start: lo,
                end: (at + 2).min(length),
            },
        )?;
        let unit_at = |o: u32| window[(o - lo) as usize];
        let at = if at > 0 && is_low(unit_at(at)) && is_high(unit_at(at - 1)) {
            at - 1
        } else {
            at
        };
        let position = number_param(at);
        let position = position.as_concrete_TypeRef() as CFTypeRef;
        match unit {
            // One Unicode scalar, as the core contract defines a character:
            // not AX's composed character sequence (`AXRangeForIndex`), which
            // is a grapheme cluster.
            TextUnit::Character => {
                let width = if is_high(unit_at(at)) && at + 1 < length {
                    2
                } else {
                    1
                };
                Ok(TextRange {
                    start: at,
                    end: at + width,
                })
            }
            TextUnit::Line => {
                let line = require_param(ax.as_ptr(), "AXLineForIndex", position)?;
                let v = require_param(ax.as_ptr(), "AXRangeForLine", line.ptr())?;
                let line = range_from(v.ptr(), "AXRangeForLine")?;
                self.with_line_terminator(ax.as_ptr(), line, length)
            }
            TextUnit::Word | TextUnit::Paragraph => Err(Error::Unsupported {
                feature: format!(
                    "macOS accessibility reports no {} boundaries for text elements",
                    unit.to_snake_case()
                ),
            }),
        }
    }

    /// Extend a line range to cover the terminator that follows it, so
    /// lines tile the text. AppKit already includes it; a toolkit that does
    /// not would otherwise leave the newline in no line at all.
    fn with_line_terminator(
        &self,
        ax: AXUIElementRef,
        line: TextRange,
        length: u32,
    ) -> Result<TextRange> {
        if line.end >= length {
            return Ok(line);
        }
        let start = line.end.saturating_sub(1).max(line.start);
        let around = read_units(
            ax,
            TextRange {
                start,
                end: (line.end + 2).min(length),
            },
        )?;
        let ends_with_terminator = line.end > line.start && is_line_terminator(around[0]);
        let next = &around[(line.end - start) as usize..];
        if ends_with_terminator || next.is_empty() || !is_line_terminator(next[0]) {
            return Ok(line);
        }
        let width = if next[0] == 0x0D && next.get(1) == Some(&0x0A) {
            2
        } else {
            1
        };
        Ok(TextRange {
            start: line.start,
            end: line.end + width,
        })
    }

    pub(super) fn rich_text_range_bounds(
        &self,
        element: &ElementData,
        range: TextRange,
    ) -> Result<Option<Rect>> {
        let ax = self.get_cached(element.handle)?;
        // Refused inside a surrogate pair, as a read or a selection is, so a
        // range is valid or not the same way on every platform.
        let length = validate(ax.as_ptr(), element, range)?;
        // Where the element's text is rendered by static-text descendants
        // (web content: a `contenteditable`), they are what reports its
        // geometry, and the range is split across them. Otherwise (AppKit,
        // and Chromium's atomic text fields) the element answers itself.
        // Which applies is read from the tree, not tried in turn.
        let leaves = text_leaves(ax.as_ptr(), length, range.end)?;
        let rect = if leaves.is_empty() {
            bounds_for_range(ax.as_ptr(), range)?
        } else {
            leaves_bounds(&leaves, range)?
        };
        // Same top-left desktop points as AXPosition / AXSize.
        Ok(rect.map(|(x, y, w, h)| Rect {
            x: x as i32,
            y: y as i32,
            width: w as u32,
            height: h as u32,
        }))
    }

    pub(super) fn rich_text_attributes_at(
        &self,
        element: &ElementData,
        offset: u32,
    ) -> Result<TextRun> {
        let ax = self.get_cached(element.handle)?;
        let length = ax_text_length(ax.as_ptr(), element)?;
        if offset >= length {
            return Err(Error::InvalidTextRange {
                start: offset,
                end: offset,
                length: Some(length),
                message: "no text at or after this offset".to_string(),
            });
        }
        let position = number_param(offset);
        let v = require_param(
            ax.as_ptr(),
            "AXStyleRangeForIndex",
            position.as_concrete_TypeRef() as CFTypeRef,
        )?;
        let run = range_from(v.ptr(), "AXStyleRangeForIndex")?;
        if !(run.start <= offset && offset < run.end) || run.end > length {
            return Err(Error::Platform {
                code: -1,
                message: format!("AXStyleRangeForIndex({offset}) reported the run {run}"),
            });
        }
        let param = range_value(run)?;
        let attributed = require_param(ax.as_ptr(), "AXAttributedStringForRange", param.ptr())?;
        let mut decoded = XA11yTextRun {
            family: std::ptr::null(),
            size: -1.0,
            weight: -1,
            italic: -1,
            underline: -1,
            strikethrough: -1,
            script: -2,
            misspelled: -1,
            has_foreground: 0,
            foreground: [0; 4],
            has_background: 0,
            background: [0; 4],
        };
        match unsafe { safe_ax_decode_text_run(attributed.ptr(), &mut decoded) } {
            0 => {}
            1 => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!(
                        "AXAttributedStringForRange({run}) did not return an attributed string"
                    ),
                })
            }
            code => {
                return Err(Error::Platform {
                    code: code as i64,
                    message: format!("decoding AXAttributedStringForRange({run}) threw"),
                })
            }
        }
        // `run_attributes` adopts the +1 family string.
        Ok(TextRun::new(run, run_attributes(&decoded).into()))
    }

    /// Validate a selection the way a read is validated: in bounds, and not
    /// splitting a surrogate pair (#646).
    pub(super) fn rich_validate_selection(
        &self,
        element: &ElementData,
        start: u32,
        end: u32,
    ) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        match validate(ax.as_ptr(), element, TextRange::new(start, end)?) {
            Ok(_) => Ok(()),
            // Name the action the caller asked for, not the read under it.
            Err(Error::ActionNotSupported { role, .. }) => Err(Error::ActionNotSupported {
                action: "set_text_selection".to_string(),
                role,
            }),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrogate_and_terminator_classes() {
        let units: Vec<u16> = "a😀\u{2028}\r\n".encode_utf16().collect();
        assert!(is_high(units[1]) && is_low(units[2]));
        assert!(!is_high(units[0]) && !is_low(units[0]));
        assert!(is_line_terminator(units[3]));
        assert!(is_line_terminator(units[4]) && is_line_terminator(units[5]));
        assert!(!is_line_terminator(u16::from(b'a')));
    }

    #[test]
    fn cf_readers_refuse_null() {
        assert_eq!(cf_i64(std::ptr::null()), None);
        assert_eq!(cf_range(std::ptr::null()), None);
        assert_eq!(cf_rect(std::ptr::null()), None);
        assert_eq!(cf_utf16(std::ptr::null()), None);
    }

    #[test]
    fn cf_range_reads_a_real_axvalue() {
        let v = Owned(unsafe { safe_ax_value_create_cf_range(3, 4) });
        assert_eq!(cf_range(v.ptr()), Some((3, 4)));
        assert_eq!(
            range_from(v.ptr(), "AXSelectedTextRange").unwrap(),
            TextRange { start: 3, end: 7 }
        );
        // A CGPoint AXValue is not a range.
        let p = Owned(unsafe { safe_ax_value_create_cg_point(1.0, 2.0) });
        assert_eq!(cf_range(p.ptr()), None);
        assert!(range_from(p.ptr(), "AXSelectedTextRange").is_err());
    }

    #[test]
    fn cf_utf16_keeps_surrogates() {
        let s = CFString::new("a😀");
        assert_eq!(
            cf_utf16(s.as_concrete_TypeRef() as CFTypeRef),
            Some(vec![0x61, 0xD83D, 0xDE00])
        );
        let n = CFNumber::from(7i64);
        assert_eq!(cf_utf16(n.as_concrete_TypeRef() as CFTypeRef), None);
        assert_eq!(cf_i64(n.as_concrete_TypeRef() as CFTypeRef), Some(7));
    }

    #[test]
    fn decoding_a_non_attributed_string_is_refused() {
        let s = CFString::new("plain");
        let mut run = XA11yTextRun {
            family: std::ptr::null(),
            size: 0.0,
            weight: 0,
            italic: 0,
            underline: 0,
            strikethrough: 0,
            script: 0,
            misspelled: 0,
            has_foreground: 0,
            foreground: [0; 4],
            has_background: 0,
            background: [0; 4],
        };
        let code =
            unsafe { safe_ax_decode_text_run(s.as_concrete_TypeRef() as CFTypeRef, &mut run) };
        assert_eq!(code, 1);
        // Fields are reset to "not reported".
        let attrs = run_attributes(&run);
        assert_eq!(attrs.font_weight, None);
        assert_eq!(attrs.underline, None);
        assert_eq!(attrs.script, None);
    }

    fn font_traits(name: &str) -> (i32, i32) {
        let name = CFString::new(name);
        let mut italic = -2;
        let weight = unsafe {
            xa11y_test_font_traits(name.as_concrete_TypeRef() as CFStringRef, 13.0, &mut italic)
        };
        (weight, italic)
    }

    #[test]
    fn font_weight_and_italic_resolve_from_the_run_font_name() {
        // AppKit's system font aliases, which CoreText resolves to a face.
        assert_eq!(font_traits(".AppleSystemUIFont"), (400, 0));
        assert_eq!(font_traits(".AppleSystemUIFontBold"), (700, 0));
        // Faces CoreText will not resolve by name, from the variant table.
        assert_eq!(font_traits(".SFNS-RegularItalic"), (400, 1));
        assert_eq!(font_traits(".SFNS-Semibold"), (600, 0));
        // An ordinary font, by exact PostScript name.
        assert_eq!(font_traits("Helvetica-Bold"), (700, 0));
        assert_eq!(font_traits("Helvetica"), (400, 0));
        // A name CoreText would replace with a fallback is not reported.
        assert_eq!(font_traits("NoSuchFont-Bold").0, -1);
        assert_eq!(font_traits(".NoSuchSystemFace").0, -1);
    }

    #[test]
    fn metadata_maps_ranges_and_one_based_positions() {
        let cell = PositionAttrs {
            row_range: Some((2, 1)),
            column_range: Some((1, 3)),
            ..Default::default()
        };
        let (table, info, row, collection) = cell
            .to_metadata(std::ptr::null(), Role::TableCell, "AXCell", None)
            .expect("no read for a cell");
        let info = info.expect("cell info");
        assert_eq!(
            (info.row, info.column, info.row_span, info.column_span),
            (Some(2), Some(1), Some(1), Some(3))
        );
        assert!(table.is_none() && row.is_none() && collection.is_none());

        let item = PositionAttrs {
            index: Some(4),
            pos_in_set: Some(1),
            set_size: Some(-1),
            disclosure_level: Some(1),
            ..Default::default()
        };
        let (_, _, row, collection) = item
            .to_metadata(
                std::ptr::null(),
                Role::TreeItem,
                "AXRow",
                Some("AXOutlineRow"),
            )
            .expect("no read for a row");
        assert_eq!(row.expect("row").index, Some(4));
        let collection = collection.expect("collection");
        assert_eq!(collection.index, Some(0));
        assert_eq!(collection.size, Some(Count::Unknown));
        assert_eq!(collection.depth, Some(1));

        // A flat table row's AXDisclosureLevel is not a depth, and a row with
        // nothing to report has no collection info.
        let flat = PositionAttrs {
            index: Some(0),
            disclosure_level: Some(0),
            ..Default::default()
        };
        let (_, _, _, collection) = flat
            .to_metadata(std::ptr::null(), Role::TableRow, "AXRow", None)
            .expect("no read for a row");
        assert!(collection.is_none());
    }

    /// An outline keeps the list role but carries table info like a table,
    /// from the counts AX reported, without another read.
    #[test]
    fn outline_carries_table_info() {
        assert!(has_table_api(Role::Table, "AXTable"));
        assert!(has_table_api(Role::List, "AXOutline"));
        assert!(!has_table_api(Role::List, "AXList"));

        let outline = PositionAttrs {
            row_count: Some(5),
            column_count: Some(2),
            ..Default::default()
        };
        let (table, _, _, _) = outline
            .to_metadata(std::ptr::null(), Role::List, "AXOutline", None)
            .expect("both counts were reported");
        let table = table.expect("table info");
        assert_eq!(
            (table.row_count, table.column_count),
            (Some(Count::Known(5)), Some(Count::Known(2)))
        );
    }
}
