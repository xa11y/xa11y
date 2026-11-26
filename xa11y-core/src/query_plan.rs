//! Query planning shared by native providers. Planning only inspects the parsed
//! selector and caller-supplied facts. It neither reads nor retains UI data.

use crate::SelectorGroup;

/// The result the executor owes its caller. Existence uses a bounded count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum QueryOutput {
    Elements,
    Count,
}

/// The ordinary resolver and the alternative that matches projected records.
/// For element output a projected executor must return complete, live matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::exhaustive_enums,
    reason = "Providers must implement each selected strategy."
)]
pub enum ReadStrategy {
    ResolveElements,
    Projected,
}

/// Inputs shared by legality checks and cost estimates. Providers validate
/// hand-built selectors at their normal entry point before planning.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct Query<'a> {
    pub group: &'a SelectorGroup,
    pub output: QueryOutput,
    pub limit: Option<usize>,
}

impl<'a> Query<'a> {
    pub fn new(group: &'a SelectorGroup, output: QueryOutput, limit: Option<usize>) -> Self {
        Self {
            group,
            output,
            limit,
        }
    }

    /// Bound for a native walk which collects every clause's first segment
    /// before narrowing and ordering its union. A streaming ordered-union
    /// executor can apply the outer limit directly instead.
    fn first_stage_limit(self) -> Option<usize> {
        let [clause] = self.group.clauses.as_slice() else {
            return None;
        };
        let first = clause.segments.first()?;
        let outer = self.limit.filter(|_| clause.segments.len() == 1);
        outer.max(first.simple.nth)
    }

    /// Can a flat projected walk evaluate every clause? The provider supplies
    /// the fields its records actually contain. Role matching, including
    /// platform roles, must already be supported by that walk.
    pub fn covered_by(self, supports_field: impl Fn(&str) -> bool, supports_nth: bool) -> bool {
        self.group.clauses.iter().all(|clause| {
            let [segment] = clause.segments.as_slice() else {
                return false;
            };
            (supports_nth || segment.simple.nth.is_none())
                && segment
                    .simple
                    .filters
                    .iter()
                    .all(|f| supports_field(&f.attr))
        })
    }
}

/// Physical choices consumed by native executors. The stage bound applies to
/// collectors that narrow and merge after their first traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ExecutionPlan {
    pub strategy: ReadStrategy,
    pub first_stage_limit: Option<usize>,
}

/// Plan a native query from the legal reads the provider offers for this
/// scope. All providers use the same production cost policy. An empty set
/// of alternatives still receives the shared traversal-limit analysis.
pub fn plan_query(
    query: Query<'_>,
    alternatives: impl IntoIterator<Item = ReadStrategy>,
) -> ExecutionPlan {
    choose_plan(query, alternatives, read_cost)
}

// Relative ranks retain the measured sparse/dense crossover. Replace this
// policy independently of which native reads are semantically eligible.
fn read_cost(query: Query<'_>, strategy: ReadStrategy) -> u8 {
    match strategy {
        ReadStrategy::ResolveElements => 1,
        ReadStrategy::Projected => {
            let sparse = query.output == QueryOutput::Count
                || query.limit.is_some_and(|limit| limit <= 2)
                || query.group.clauses.iter().all(|clause| {
                    clause.segments[0]
                        .simple
                        .filters
                        .iter()
                        .any(|f| f.attr == "name")
                })
                || query.group.clauses.iter().all(|clause| {
                    matches!(
                        clause.segments[0].simple.role,
                        Some(crate::selector::RoleMatch::Normalized(
                            crate::Role::Window | crate::Role::Dialog
                        ))
                    )
                });
            if sparse {
                0
            } else {
                2
            }
        }
    }
}

/// Costs are comparable within a single query. Equal costs keep the ordinary resolver.
fn choose_plan<C: Ord>(
    query: Query<'_>,
    alternatives: impl IntoIterator<Item = ReadStrategy>,
    cost: impl Fn(Query<'_>, ReadStrategy) -> C,
) -> ExecutionPlan {
    let strategy = std::iter::once(ReadStrategy::ResolveElements)
        .chain(alternatives)
        .min_by_key(|strategy| cost(query, *strategy))
        .unwrap_or(ReadStrategy::ResolveElements);
    ExecutionPlan {
        strategy,
        first_stage_limit: query.first_stage_limit(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_policy_only_ranks_legal_plans() {
        let group = SelectorGroup::parse("button[name='Save']").unwrap();
        let query = Query::new(&group, QueryOutput::Elements, Some(1));
        let cheap_projection = |_: Query<'_>, strategy| strategy == ReadStrategy::ResolveElements;
        assert_eq!(
            choose_plan(query, [ReadStrategy::Projected], cheap_projection).strategy,
            ReadStrategy::Projected
        );
        assert_eq!(
            choose_plan(query, [ReadStrategy::Projected], |_, s| s
                == ReadStrategy::Projected)
            .strategy,
            ReadStrategy::ResolveElements
        );
        assert_eq!(
            choose_plan(query, [ReadStrategy::Projected], |_, _| 0).strategy,
            ReadStrategy::ResolveElements
        );
        assert_eq!(
            choose_plan(query, [], cheap_projection).strategy,
            ReadStrategy::ResolveElements
        );
    }

    #[test]
    fn native_first_stage_respects_union_narrowing_and_ordinals() {
        for (selector, limit, expected) in [
            ("button", Some(2), Some(2)),
            ("window button", Some(2), None),
            ("window:nth(3) button", Some(1), Some(3)),
            ("button:nth(3)", None, Some(3)),
            ("button:nth(3)", Some(5), Some(5)),
            ("window, button:nth(3)", Some(1), None),
        ] {
            let group = SelectorGroup::parse(selector).unwrap();
            assert_eq!(
                plan_query(Query::new(&group, QueryOutput::Elements, limit), []).first_stage_limit,
                expected,
                "{selector}"
            );
        }
    }
}
