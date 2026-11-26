# Shared query planning and lightweight Windows counts

This follow-up is measured against the combined changes from PRs #723-#726
(commit `a752f650`). It adds the following improvements above the bindings:

* Each Locator lazily parses its selector once. Clones share the immutable
  syntax; child/descendant chaining starts a new syntax cache. Every operation
  still reads fresh provider data. Invalid-selector errors remain lazy and keep
  their original selector context.
* Flat selector unions share one document-order walk and independent per-clause
  `:nth` counters. A first/existence/outer-nth query can stop after the ordered
  union contains enough results. Compound selectors keep the existing narrowing
  algorithm. Query-local child snapshots are retained only where multiple
  clauses need to share them.
* The same flat walker has a count-only visitor that does not retain or clone
  matching result snapshots. This is backend plumbing behind the existing
  Provider count interface from #724; no language-specific API is added.
* Windows application counts/existence use role/name metadata for flat queries
  involving normalized roles, platform classes, name, role, class_name or
  control_type_id. The request includes ten role/name properties and no bounds,
  values or action patterns. Full and projected reads share exactly the same
  role-refinement implementation (ARIA, MSAA, document roles and DataItem
  parents). Partial query records never escape as public Elements.
* State/value/geometry/raw-property queries outside that metadata set, compound
  queries, and non-application roots retain the existing full resolver. In
  particular, the HWND subtree resolver's root inclusion and runtime-ID merge
  behavior are preserved; substituting a descendants walk would change results.
* The shell visibility probe fetches only IsOffscreen instead of full snapshots.
  Missing/unreadable-property and native query errors keep their existing meaning.
* The Rust integration `named` helper requests the first match rather than
  enumerating all matches and throwing the rest away. Uniqueness assertions
  continue to use their full match set.

The parser and union changes apply through core to Rust, Python, JS, CLI and
MCP. Native providers may still need their own short-circuit implementation for
native bulk traversal; the shared engine already benefits Windows application
walks, generic providers and multi-root traversal. The count interface is shared
across platforms, while the projection respects each backend's native semantics.

No locator interval, timeout, stability window or actionability check is reduced.
There are no new event subscriptions, workers or persistent UI snapshots.

## Measurements

Windows, Python 3.12.13, Qt/PySide6 6.12.0, release wheels. A/B/B/A with a fresh
fixture per round, one warmup and five validated samples per workload per round:
ten timed samples per variant. The same modified benchmark source and fixture
ran against both wheels. No builds or other suites ran during the four rounds.

| Workload | Before ms | After ms | Change |
| --- | ---: | ---: | ---: |
| attach by PID | 4.160 | 4.176 | +0.4% |
| enumerate windows | 2.191 | 2.175 | -0.7% |
| read direct children | 2.142 | 2.155 | +0.6% |
| read shallow tree | 6.902 | 7.046 | +2.1% |
| capture 32 nodes | 27.063 | 27.408 | +1.3% |
| exact name | 27.407 | 27.513 | +0.4% |
| exists miss | 86.767 | 40.046 | -53.8% |
| first button | 10.534 | 10.679 | +1.4% |
| count buttons | 85.797 | 40.070 | -53.3% |
| list buttons | 86.415 | 86.694 | +0.3% |
| state filter | 83.725 | 86.625 | +3.5% |
| descendant selector | 102.086 | 86.005 | -15.8% |
| selector union | 100.055 | 100.451 | +0.4% |
| first union match | 99.976 | 11.352 | -88.6% |
| union exists | 99.408 | 7.630 | -92.3% |
| count metadata union | 100.243 | 44.639 | -55.5% |
| count named buttons | 100.203 | 45.163 | -54.9% |
| ready wait | 30.209 | 30.677 | +1.6% |
| resolve stable ID | 30.828 | 32.364 | +5.0% |
| set text | 41.508 | 41.578 | +0.2% |
| press to visible dialog control | 289.052 | 286.910 | -0.7% |
| multi-window tree roots | 8.625 | 8.674 | +0.6% |
| multi-window unique dialog control | 99.449 | 100.814 | +1.4% |

The untouched paths vary between runs; these are observations on one desktop,
not cross-platform performance guarantees. The added union/count workloads
validate identity/order or compare cardinality against full materialization
outside the timed operation.

Raw samples are in [shared-query-performance-results.json](shared-query-performance-results.json).
Reproduce by installing a release wheel for the chosen revision and running:

```powershell
$env:XA11Y_TEST_APP = "qt"
python -m pytest tests/suites/python/test_performance.py -q --timeout=120
```

The existing `cargo bench -p xa11y-core --bench selector_scan` now includes
limited union workloads, placed after the original workloads to avoid allocator
warming bias from the baseline's full scans. In the 10k fixture, limited unions
used 2 child reads instead of 10,101. One release comparison measured first-union
resolution at 17,146 vs 71 microseconds, and union nth at 15,127 vs 76 microseconds.
These synthetic timings are observations, not CI thresholds. Full CPU samples
and unchanged-workload results are in
[shared-query-cpu-results.json](shared-query-cpu-results.json).

## Boundaries and next choices

A native projected HWND count path can remove its remaining snapshot work while
retaining root inclusion, per-clause nth and runtime-ID dedup order. macOS can
use the same provider count interface, but AX churn/liveness handling must be
retained before counting refs. Neither requires new Python/JS/CLI methods.

A more detailed property-read plan could cover selected/enabled/value queries.
Visibility requires ancestor clipping/minimization, checked/expanded require
pattern reads, and arbitrary raw properties require their defined representation.
They must remain explicit read dependencies rather than defaulted missing data.

Event-triggered locator waits remain a separate option: they need reusable,
coalesced subscriptions, subscribe-before-read ordering, and bounded backup
polling for providers that omit events. The current 100 ms schedule is preserved.

## Validation

Regression tests compare flat queries/counts with the path-based resolver over
clause ordering, overlap, nth, depth and limits; verify early termination and
propagation of needed read errors; and verify cached locator branches do not
reuse a different selector. Real integration cases compare counts/existence with
materialized results for role/name, union, nth, state-filter and HWND roots.

Local validation: 496 core unit tests passed; core/Windows clippy with warnings
denied and bindings parity passed; the full Windows/Qt Python suite passed
(119 passed, with existing skips and expected-failure markers). All 201 main
Windows integration tests, STA-host and both DPI tests passed after fixing the
tray test's visibility/cleanup race. The standalone UIPI test still requires an
elevated runner and fails at that prerequisite here.

The tray regression was in test synchronization: the overflow root may appear
before its icons become visible, and an early failure previously left the popup
open for the next run. The test now installs cleanup first, resets a leftover
popup and waits for its visible fixture. The same baseline probe reproduced the
intermediate root-with-invisible-icons state.

CI will verify the combined changes before merge.
