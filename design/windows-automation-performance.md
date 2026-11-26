# Windows automation performance

## Scope

Survey against main `a2fedad3`: Windows UIA discovery, snapshot construction,
root deduplication and selector traversal; core locators and event channels;
Python/JS subscriptions; pytest event recording; integration helpers and
recommended usage. IA2's separate worker/deadline architecture is unchanged.

## Changes

* Discovery fetches four window properties instead of the full element property
  set and six action patterns. This covers desktop app enumeration, direct PID
  attach and its UWP attribution path. Returned application data is still
  synthesized by the same implementation. Full element reads keep full caches.
* PID attach checks interactivity/hung status only for windows owned by that
  PID. Desktop-wide discovery still checks all interactive windows.
* A single native root cannot duplicate another root. Return it without fetching
  its children or walking their parent chains. Multiple roots still use UIA
  ancestry and identity comparisons, including the popup coverage checks.
* Those popup checks fetch references in raw view without requesting properties
  or action patterns that the checks never inspect.
* Radio/check-box `checked` and `selected` reuse one SelectionItem observation.
  TogglePattern remains authoritative for `checked` when present. Pattern read
  failures still propagate.
* Rust `Subscription::wait_for` blocks on the event receiver for the remaining
  deadline instead of waking every 10 ms. Delivery and disconnect both wake the
  receiver. Predicate filtering and timeout diagnosis remain in place.
  The Rust blocking iterator likewise waits for delivery/disconnect instead of
  waking every 100 ms. Python signal checks and JS worker cancellation remain
  unchanged.
* Qt popup tests now verify cleanup before proceeding. The fixture's New action
  explicitly closes its menu: UIA invokes `QAction.trigger`, which alone does
  not perform QMenu's pointer-driven dismissal. Previously a later dialog could
  happen to hide the leftover popup, making the window-activation test depend
  on traversal timing. Cleanup waits for native popup disappearance or dialog
  invisibility, without a fixed settle delay.

No shorter locator polling intervals, reduced deadlines, skipped actionability
checks, persistent snapshots, or extra background workers are introduced.

## Choices left open

| Option | Benefit | Cost / reason to keep separate |
| --- | --- | --- |
| Wake locator waits on UIA events, with a bounded polling backup | Avoid much of the 100 ms reaction interval | Some providers omit events. Subscribing can itself be expensive and race with actions. Needs subscription reuse, subscribe-before-read ordering, event coalescing and provider-specific tests. |
| Delegate `EventRecorder.drain` to native event waits | Avoid its 50 ms delivery delay and sleep rounding | The recorder currently tolerates a disconnected source for the entire requested recording duration, while native predicate waits surface disconnect as a platform error. Preserve that behavior explicitly before replacing the helper; do not ship a shorter wait by changing its contract. |
| Fetch pattern values in bulk | Fewer live state/value calls on rich controls | Adds requested properties to every node, even controls that do not use them. Must benchmark across Qt, AccessKit, WPF and WinForms before choosing a request shape. |
| Provider count-only / lightweight query results | Avoid constructing full ElementData when only a count is needed | Requires a new cross-platform provider contract and equivalent selector, identity, error and binding behavior. Current count semantics are preserved. |
| Replace the integration `act` helper's fixed settle | Remove its default 100 ms delay | `act` takes no expected postcondition. Removing the delay alone is unreliable; migrate individual callers to explicit state/value predicates. |
| Shorten `wait_stable`'s 750 ms observation window | Earlier completion | Changes the meaning of stability. Use a specific readiness condition when stability is not required. |
| Flatten every app query into a UIA subtree call | Fewer level-by-level reads | Fragment/provider activation boundaries and scroll clipping require the existing guarded traversal. Do not remove its correctness paths for speed. |

Python subscription waits retain periodic signal checks, and the JS event worker
retains its cancellation checks. A blocking receive already wakes on an event;
these timeout slices do not add event-delivery latency.

## Consumer patterns

* Attach by PID when known and keep the App/provider for the automation session.
  Rediscovering the entire desktop between actions adds unrelated work.
* Scope repeated queries to a known live window or container when the application
  contract allows it. App scope intentionally includes all its windows.
* Use `first().element()` or `exists()` when only one answer is needed. Use
  `elements()` when the test must verify the complete match set. An exactly-one
  assertion must still check uniqueness.
* Wait for the expected state/value rather than sleeping or requesting stability
  after every action. A locator action already waits for its actionability gate;
  the postcondition still needs its own assertion.
* Subscribe before the action when an emitted event is the assertion. Verify the
  resulting state as well when the test promises a state change rather than just
  event delivery. Event support varies by provider.
* Request a tree, capture, or screenshot only when it is part of the assertion or
  a failure artifact. Do not re-fetch a tree solely to read a value already in
  the returned Element snapshot; re-query when a fresh observation is required.

## Benchmark and validation

Use the existing `tests/suites/python/test_performance.py` on Windows/Qt with
release Python wheels. Each workload warms once and validates five timed
samples. This change adds open-dialog multi-window reads and uniqueness checks.
Compare the same test source with baseline and changed wheels; package build,
installation, application startup and validation/reset stay outside operation
timings. Timings are observations on one desktop, not CI performance thresholds.

Release-wheel A/B/B/A comparison on Windows, Python 3.12.13 and Qt/PySide6
6.12.0. Each variant has ten timed samples per workload across two fresh app
sessions. The same modified benchmark source ran against both wheels. No builds
or other test suites ran during these four rounds. Baseline is `a2fedad3`.

| Workload | Baseline ms | Changed ms | Change |
| --- | ---: | ---: | ---: |
| attach by PID | 4.666 | 4.179 | -10.4% |
| enumerate windows | 2.192 | 2.228 | +1.6% |
| read direct children | 6.864 | 2.104 | -69.3% |
| read shallow tree | 11.463 | 7.037 | -38.6% |
| capture 32 nodes | 31.502 | 27.203 | -13.6% |
| exact name | 31.941 | 27.496 | -13.9% |
| exists miss | 91.196 | 87.220 | -4.4% |
| first button | 14.998 | 10.558 | -29.6% |
| count buttons | 91.320 | 87.451 | -4.2% |
| list buttons | 88.130 | 86.922 | -1.4% |
| state filter | 97.595 | 83.618 | -14.3% |
| descendant selector | 111.411 | 102.289 | -8.2% |
| selector union | 109.299 | 100.305 | -8.2% |
| ready wait | 35.830 | 30.639 | -14.5% |
| set text | 45.492 | 41.258 | -9.3% |
| press to visible dialog control | 297.613 | 286.594 | -3.7% |
| multi-window tree roots | 10.460 | 8.685 | -17.0% |
| multi-window unique dialog control | 101.658 | 99.731 | -1.9% |

Raw samples: [windows-automation-performance-results.json](windows-automation-performance-results.json).
Different queries have different costs; these results do not imply a uniform
speedup. In particular, native window enumeration is effectively unchanged.

To reproduce each round, build a release wheel for the chosen revision, install
it into the same environment, then run from this checkout:

```powershell
$env:XA11Y_TEST_APP = "qt"
python -m pytest tests/suites/python/test_performance.py -q --timeout=120
```

Repeat in baseline/changed/changed/baseline order. Each operation validates its
result; the open-dialog workload also verifies multiple native windows and a
unique dialog control. The raw sample JSON records each round rather than only
its aggregate.

## Validation

* `cargo test --workspace` passed with a stationary AccessKit fixture present.
  Some existing Windows unit tests require at least one desktop application;
  an empty desktop is not a valid environment for those tests.
* Targeted core/Windows clippy with warnings denied passed. Changed Rust files
  pass rustfmt, and changed Python files compile.
* The Windows integration harness's 199 main tests passed, as did its STA-host
  and two system-aware DPI tests. Its separate UIPI test requires an elevated
  runner, unavailable in this session, and fails at that prerequisite assertion.
* The full Windows/Qt Python suite passed: 118 passed, 209 skipped, 2 xfailed,
  2 xpassed. The existing skips and expected-failure markers were retained.
* Python binding tests (436), JS binding tests (263), pytest plugin tests (129),
  Strands tests (263), and harness/matrix self-tests (126) passed during validation.
  Bindings parity passed.
* `cargo xtask check` was run. Its aggregate result remains nonzero due to
  existing generated capability-table drift and an unrelated rustfmt import
  ordering difference in `dpi_system_aware.rs`. The formatting difference also
  reproduces on the unmodified baseline. A missing YAML dependency was installed
  and harness tests rerun; Rust tests were rerun with a stable desktop fixture.
