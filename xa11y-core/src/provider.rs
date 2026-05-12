use crate::element::ElementData;
use crate::error::Result;
use crate::event_provider::Subscription;
use crate::selector::{matches_simple, Combinator, Selector, SelectorGroup, SelectorSegment};

/// Platform backend trait for accessibility tree access.
///
/// Providers implement lazy, on-demand tree navigation. Elements are identified
/// by their [`ElementData`] (which contains a provider-specific `handle` for
/// looking up the underlying platform object).
///
/// # Action model
///
/// Common actions are first-class methods with proper typed signatures.
/// Platform-specific or custom actions use [`perform_action`](Self::perform_action)
/// as an escape hatch — it takes a `snake_case` action name string.
///
/// Providers should check platform permissions in their constructor (`new()`)
/// and return [`Error::PermissionDenied`](crate::Error::PermissionDenied) if
/// required permissions are not granted.
pub trait Provider: Send + Sync {
    // ── Tree navigation ─────────────────────────────────────────────

    /// Get direct children of an element.
    ///
    /// If `element` is `None`, returns top-level application elements.
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>>;

    /// Get the parent of an element.
    ///
    /// Returns `None` for top-level (application) elements.
    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>>;

    /// Search for elements matching a selector.
    ///
    /// The selector is already parsed by the core — providers match against it
    /// during traversal and can prune subtrees that can't match.
    ///
    /// If `root` is `None`, searches from the system root (all applications).
    /// If `limit` is `Some(n)`, stops after finding `n` matches.
    /// If `max_depth` is `Some(d)`, does not descend deeper than `d` levels.
    ///
    /// This is a thin convenience wrapper that re-uses
    /// [`find_elements_group`](Self::find_elements_group) with a single-clause
    /// group. Backends do **not** override this method — they override
    /// `find_elements_group`, and the single-clause case runs through the
    /// same native code path as multi-clause queries.
    fn find_elements(
        &self,
        root: Option<&ElementData>,
        selector: &Selector,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        let group = SelectorGroup {
            clauses: vec![selector.clone()],
        };
        self.find_elements_group(root, &group, limit, max_depth)
    }

    /// Search for elements matching any clause of a comma-separated selector
    /// group.
    ///
    /// This is the **primary search primitive** each backend should override.
    /// A native implementation performs ONE platform-level subtree query/walk
    /// (e.g. `FindAllBuildCache(TreeScope_Subtree)` on Windows, one DFS over
    /// AT-SPI children on Linux, one AX walk on macOS) and evaluates every
    /// clause inline against each visited element. This avoids the
    /// per-clause-walk perf cliff and also keeps the cross-clause dedup
    /// correct: because everything happens inside one call, each platform
    /// node is visited once and platform identity is stable.
    ///
    /// The default implementation traverses via [`get_children`](Self::get_children)
    /// and uses tree-path identity for cross-clause merging. It's the
    /// fallback for backends that haven't shipped a native override yet —
    /// the same path the test/mock provider exercises.
    fn find_elements_group(
        &self,
        root: Option<&ElementData>,
        group: &SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        crate::selector::find_elements_in_tree_group(
            |el| self.get_children(el),
            root,
            group,
            limit,
            max_depth,
        )
    }

    /// Narrow candidates through remaining selector segments (Child/Descendant
    /// combinators), deduplicate, apply final :nth and limit.
    fn narrow_multi_segment(
        &self,
        mut candidates: Vec<ElementData>,
        segments: &[SelectorSegment],
        max_depth: u32,
        limit: Option<usize>,
    ) -> Result<Vec<ElementData>> {
        for segment in segments {
            let mut next_candidates = Vec::new();
            for candidate in &candidates {
                match segment.combinator {
                    Combinator::Child => {
                        let children = self.get_children(Some(candidate))?;
                        for child in children {
                            if matches_simple(&child, &segment.simple) {
                                next_candidates.push(child);
                            }
                        }
                    }
                    Combinator::Descendant => {
                        let sub_selector = Selector {
                            segments: vec![SelectorSegment {
                                combinator: Combinator::Root,
                                simple: segment.simple.clone(),
                            }],
                        };
                        let mut sub_results = self.find_elements(
                            Some(candidate),
                            &sub_selector,
                            None,
                            Some(max_depth),
                        )?;
                        next_candidates.append(&mut sub_results);
                    }
                    Combinator::Root => unreachable!(),
                }
            }
            let mut seen = std::collections::HashSet::new();
            next_candidates.retain(|e| seen.insert(e.handle));
            candidates = next_candidates;
        }

        // Apply :nth on last segment
        if let Some(nth) = segments.last().and_then(|s| s.simple.nth) {
            if nth <= candidates.len() {
                candidates = vec![candidates.remove(nth - 1)];
            } else {
                candidates.clear();
            }
        }

        if let Some(limit) = limit {
            candidates.truncate(limit);
        }

        Ok(candidates)
    }

    // ── Common actions ──────────────────────────────────────────────

    /// Click / tap / invoke the element.
    fn press(&self, element: &ElementData) -> Result<()>;

    /// Set keyboard focus to the element.
    fn focus(&self, element: &ElementData) -> Result<()>;

    /// Remove keyboard focus from the element.
    fn blur(&self, element: &ElementData) -> Result<()>;

    /// Toggle a checkbox or switch.
    fn toggle(&self, element: &ElementData) -> Result<()>;

    /// Select an item in a list, tab group, or menu.
    fn select(&self, element: &ElementData) -> Result<()>;

    /// Expand a collapsible element (combo box, tree item, disclosure).
    fn expand(&self, element: &ElementData) -> Result<()>;

    /// Collapse an expanded element.
    fn collapse(&self, element: &ElementData) -> Result<()>;

    /// Show the element's context menu or dropdown.
    fn show_menu(&self, element: &ElementData) -> Result<()>;

    /// Increment a slider or spinner by one step.
    fn increment(&self, element: &ElementData) -> Result<()>;

    /// Decrement a slider or spinner by one step.
    fn decrement(&self, element: &ElementData) -> Result<()>;

    /// Scroll the element into the visible area.
    fn scroll_into_view(&self, element: &ElementData) -> Result<()>;

    // ── Typed operations ────────────────────────────────────────────

    /// Set the text value of the element.
    fn set_value(&self, element: &ElementData, value: &str) -> Result<()>;

    /// Set the numeric value of the element (slider, spinner).
    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()>;

    /// Insert text at the current cursor position.
    fn type_text(&self, element: &ElementData, text: &str) -> Result<()>;

    /// Select a text range (0-based character offsets).
    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()>;

    // ── Generic action escape hatch ─────────────────────────────────

    /// Perform an action by `snake_case` name.
    ///
    /// This is the escape hatch for platform-specific actions not covered by
    /// the first-class methods above. The provider converts the name to the
    /// platform's convention (e.g. `"custom_thing"` → `"AXCustomThing"` on
    /// macOS) and invokes it.
    ///
    /// Well-known action names (`"press"`, `"focus"`, etc.) should also work
    /// here — providers should delegate to the corresponding method.
    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()>;

    // ── Events ──────────────────────────────────────────────────────

    /// Subscribe to all accessibility events for an application.
    ///
    /// The element should be an application-level element (role=Application).
    /// The provider extracts the PID from `element.pid`.
    ///
    /// Returns a [`Subscription`] that receives events until dropped.
    fn subscribe(&self, element: &ElementData) -> Result<Subscription>;
}

// Blanket impl so shared references to a provider act as providers themselves.
// Used by the umbrella crate's singleton (a `&'static dyn Provider` wrapped in
// `Arc<_>`) and by any caller that wants to share a provider via `&T`. The
// orphan rules keep this collision-free for downstream crates because `xa11y-core`
// owns the `Provider` trait.
impl<T: Provider + ?Sized> Provider for &T {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        (**self).get_children(element)
    }
    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        (**self).get_parent(element)
    }
    fn find_elements(
        &self,
        root: Option<&ElementData>,
        selector: &Selector,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        (**self).find_elements(root, selector, limit, max_depth)
    }
    fn find_elements_group(
        &self,
        root: Option<&ElementData>,
        group: &SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        (**self).find_elements_group(root, group, limit, max_depth)
    }
    fn narrow_multi_segment(
        &self,
        candidates: Vec<ElementData>,
        segments: &[SelectorSegment],
        max_depth: u32,
        limit: Option<usize>,
    ) -> Result<Vec<ElementData>> {
        (**self).narrow_multi_segment(candidates, segments, max_depth, limit)
    }
    fn press(&self, element: &ElementData) -> Result<()> {
        (**self).press(element)
    }
    fn focus(&self, element: &ElementData) -> Result<()> {
        (**self).focus(element)
    }
    fn blur(&self, element: &ElementData) -> Result<()> {
        (**self).blur(element)
    }
    fn toggle(&self, element: &ElementData) -> Result<()> {
        (**self).toggle(element)
    }
    fn select(&self, element: &ElementData) -> Result<()> {
        (**self).select(element)
    }
    fn expand(&self, element: &ElementData) -> Result<()> {
        (**self).expand(element)
    }
    fn collapse(&self, element: &ElementData) -> Result<()> {
        (**self).collapse(element)
    }
    fn show_menu(&self, element: &ElementData) -> Result<()> {
        (**self).show_menu(element)
    }
    fn increment(&self, element: &ElementData) -> Result<()> {
        (**self).increment(element)
    }
    fn decrement(&self, element: &ElementData) -> Result<()> {
        (**self).decrement(element)
    }
    fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
        (**self).scroll_into_view(element)
    }
    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        (**self).set_value(element, value)
    }
    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        (**self).set_numeric_value(element, value)
    }
    fn type_text(&self, element: &ElementData, text: &str) -> Result<()> {
        (**self).type_text(element, text)
    }
    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        (**self).set_text_selection(element, start, end)
    }
    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()> {
        (**self).perform_action(element, action)
    }
    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        (**self).subscribe(element)
    }
}
