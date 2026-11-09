//! JS `Text` class: an element's text, read by range.
//!
//! Every method is a live platform read, so each one is async and runs on
//! napi's worker pool like the rest of the provider-touching API. Arguments
//! are parsed synchronously first (a bad one throws before any OS call),
//! matching `Element.moveTo`.

use napi::bindgen_prelude::{AsyncTask, Env, Task, ToNapiValue, TypeName};

use crate::map_err;
use crate::types::{
    checked_offset, nullable, Rect, TextFindOptions, TextLines, TextRange, TextRuns,
};

/// Default page size for `Text.lines` / `Text.runs`, matching the Python
/// binding's `limit=200`.
const DEFAULT_PAGE_LIMIT: usize = 200;

/// A blocking xa11y call run on the worker pool, resolving to `T`.
///
/// One generic task instead of a struct per method: the work is a closure
/// built after the arguments are parsed, so `compute` only ever performs the
/// read and converts the result.
pub struct Blocking<T: Send + 'static> {
    work: Option<Box<dyn FnOnce() -> xa11y::Result<T> + Send>>,
}

impl<T: Send + 'static> Blocking<T> {
    pub(crate) fn new(work: impl FnOnce() -> xa11y::Result<T> + Send + 'static) -> Self {
        Self {
            work: Some(Box::new(work)),
        }
    }
}

impl<T> Task for Blocking<T>
where
    T: ToNapiValue + TypeName + Send + 'static,
{
    type Output = T;
    type JsValue = T;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // napi calls `compute` exactly once per task; a second call would be a
        // runtime bug, reported rather than panicking (tenet 4).
        let work = self
            .work
            .take()
            .ok_or_else(|| napi::Error::from_reason("Blocking task computed twice"))?;
        work().map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Parse a page limit: a whole, non-negative number, defaulting to 200.
fn page_limit(limit: Option<f64>) -> napi::Result<usize> {
    match limit {
        None => Ok(DEFAULT_PAGE_LIMIT),
        Some(n) => {
            let n = checked_offset(n, "limit")?;
            if n == 0 {
                // A page that holds nothing never advances.
                return Err(crate::map_err(xa11y::Error::invalid_action_data("limit must be at least 1")));
            }
            Ok(n as usize)
        }
    }
}

/// The text of one element, read by range.
///
/// Returned by `Element.text()` (bound to that element: a re-rendered
/// control rejects with `ElementStaleError`) and `Locator.text()`
/// (re-resolved on every call, waiting until the selector matches; reads do
/// not require the element to be visible or enabled).
///
/// Every method is a live platform read and caches nothing, so polling
/// `caret()` sees the current caret without fetching the whole value.
/// Offsets are UTF-16 code units — the unit JS string indices use — and
/// ranges are plain `{ start, end }` objects (half-open). A range past the
/// end of the text, or one that splits a surrogate pair, rejects with
/// `InvalidTextRangeError`.
///
/// Exported as `Text`, like the core and Python type. In a TypeScript
/// project that includes the DOM lib, import it explicitly
/// (`import type { Text } from '@crowecawcaw/xa11y'`): the global DOM `Text`
/// node type shares the name, and an unimported annotation resolves to it.
#[napi]
pub struct Text {
    inner: xa11y::Text,
}

impl Text {
    pub(crate) fn from_core(inner: xa11y::Text) -> Self {
        Self { inner }
    }
}

#[napi]
impl Text {
    /// Length of the text in UTF-16 code units. Reads the text only where
    /// the platform counts something else (GTK on Linux, Windows).
    #[napi(ts_return_type = "Promise<number>")]
    pub fn length(&self) -> AsyncTask<Blocking<u32>> {
        let t = self.inner.clone();
        AsyncTask::new(Blocking::new(move || t.length()))
    }

    /// The caret offset, or `null` when the element has no caret (not
    /// focused, or not editable). Rejects with `ActionNotSupportedError` when
    /// the platform cannot report a caret at all.
    #[napi(ts_return_type = "Promise<number | null>")]
    pub fn caret(&self) -> AsyncTask<Blocking<Option<u32>>> {
        let t = self.inner.clone();
        AsyncTask::new(Blocking::new(move || t.caret()))
    }

    /// The selected ranges, in document order. Empty when nothing is
    /// selected; a caret alone is not a selection.
    #[napi(ts_return_type = "Promise<TextRange[]>")]
    pub fn selections(&self) -> AsyncTask<Blocking<Vec<TextRange>>> {
        let t = self.inner.clone();
        AsyncTask::new(Blocking::new(move || {
            Ok(t.selections()?.into_iter().map(Into::into).collect())
        }))
    }

    /// The text in `range`, exactly as the platform reports it (bidi
    /// controls are not stripped, unlike `Element.value`).
    #[napi(ts_return_type = "Promise<string>")]
    pub fn read(&self, range: TextRange) -> napi::Result<AsyncTask<Blocking<String>>> {
        let range = range.to_core("range")?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || t.read(range))))
    }

    /// The whole text. Reads every unit, so prefer `read(range)` on a large
    /// document.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn read_all(&self) -> AsyncTask<Blocking<String>> {
        let t = self.inner.clone();
        AsyncTask::new(Blocking::new(move || t.read_all()))
    }

    /// Find an exact match of `needle`, or `null` when there is none.
    ///
    /// Matching is case-sensitive on UTF-16 code units, with no Unicode
    /// normalization, and matches do not overlap. `options.occurrence` picks
    /// the n-th match, 0-based (default `0`, the first); `options.within`
    /// limits the search to a range. An empty `needle` rejects with
    /// `InvalidActionDataError`.
    #[napi(
        ts_args_type = "needle: string, options?: TextFindOptions | null",
        ts_return_type = "Promise<TextRange | null>"
    )]
    pub fn find(
        &self,
        needle: String,
        options: Option<TextFindOptions>,
    ) -> napi::Result<AsyncTask<Blocking<Option<TextRange>>>> {
        // `Text::find_with` unconditionally, so the plain call cannot drift
        // from the optioned one (AGENTS.md: options structs fold into the
        // primary verb).
        let mut opts = xa11y::FindOptions::new();
        if let Some(o) = options {
            if let Some(n) = nullable(o.occurrence) {
                opts = opts.occurrence(checked_offset(n, "occurrence")? as usize);
            }
            if let Some(r) = nullable(o.within) {
                opts = opts.within(r.to_core("within")?);
            }
        }
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.find_with(&needle, opts)?.map(Into::into))
        })))
    }

    /// Live embedded object or link at a UTF-16 offset; null for plain text.
    #[napi(ts_return_type = "Promise<Element | null>")]
    pub fn object_at(
        &self,
        offset: f64,
    ) -> napi::Result<AsyncTask<Blocking<Option<crate::element::Element>>>> {
        let offset = checked_offset(offset, "offset")?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.object_at(offset)?
                .map(|e| crate::element::Element::new(e.data().clone(), e.provider().clone())))
        })))
    }

    /// The `unit` (`'character'`, `'word'`, `'line'` — a rendered line — or
    /// `'paragraph'`) containing `offset`. At `offset === length`, the unit
    /// that ends there. An unknown unit throws `InvalidActionDataError`.
    #[napi(ts_return_type = "Promise<TextRange>")]
    pub fn range_at(
        &self,
        offset: f64,
        unit: String,
    ) -> napi::Result<AsyncTask<Blocking<TextRange>>> {
        let offset = checked_offset(offset, "offset")?;
        let unit = parse_text_unit(&unit)?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.range_at(offset, unit)?.into())
        })))
    }

    /// The rendered lines that cover `range`, each clipped to it, at most
    /// `limit` (default 200) per page. When `truncated`, continue from
    /// `nextStart`.
    #[napi(
        ts_args_type = "range: TextRange, limit?: number | null",
        ts_return_type = "Promise<TextLines>"
    )]
    pub fn lines(
        &self,
        range: TextRange,
        limit: Option<f64>,
    ) -> napi::Result<AsyncTask<Blocking<TextLines>>> {
        let range = range.to_core("range")?;
        let limit = page_limit(limit)?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.lines(range, limit)?.into())
        })))
    }

    /// Screen rectangles of `range`, one per rendered line fragment, in the
    /// same desktop coordinates as `Element.bounds`. Fragments without
    /// geometry (scrolled out, not laid out) are left out. A range covering
    /// too many lines rejects with `InvalidTextRangeError`: page it with
    /// `lines()` first.
    #[napi(ts_return_type = "Promise<Rect[]>")]
    pub fn bounds(&self, range: TextRange) -> napi::Result<AsyncTask<Blocking<Vec<Rect>>>> {
        let range = range.to_core("range")?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.bounds(range)?.into_iter().map(Into::into).collect())
        })))
    }

    /// Formatting runs covering `range`, each clipped to it, at most `limit`
    /// (default 200) per page. When `truncated`, continue from `nextStart`.
    #[napi(
        ts_args_type = "range: TextRange, limit?: number | null",
        ts_return_type = "Promise<TextRuns>"
    )]
    pub fn runs(
        &self,
        range: TextRange,
        limit: Option<f64>,
    ) -> napi::Result<AsyncTask<Blocking<TextRuns>>> {
        let range = range.to_core("range")?;
        let limit = page_limit(limit)?;
        let t = self.inner.clone();
        Ok(AsyncTask::new(Blocking::new(move || {
            Ok(t.runs(range, limit)?.into())
        })))
    }
}

/// Parse a text-unit name. Arm by arm, so `[[types.variant_coverage]]` sees
/// every `TextUnit` variant in this binding; the error lists the valid names.
pub fn parse_text_unit(name: &str) -> napi::Result<xa11y::TextUnit> {
    Ok(match name {
        "character" => xa11y::TextUnit::Character,
        "word" => xa11y::TextUnit::Word,
        "line" => xa11y::TextUnit::Line,
        "paragraph" => xa11y::TextUnit::Paragraph,
        other => {
            return Err(map_err(xa11y::Error::invalid_action_data(format!(
                    "unknown text unit {other:?}; expected one of character, word, line, paragraph"
                ))))
        }
    })
}
