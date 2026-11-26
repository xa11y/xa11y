# Windows automation performance

## Scope

Survey against main `e6c1842a`: Windows UIA discovery, snapshot construction,
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
or other test suites ran during these four rounds. Baseline is `e6c1842a`.

| Workload | Baseline ms | Changed ms | Change |
| --- | ---: | ---: | ---: |
| attach by PID | 4.464 | 4.160 | -6.8% |
| enumerate windows | 2.288 | 2.220 | -3.0% |
| read direct children | 6.957 | 2.159 | -69.0% |
| read shallow tree | 11.692 | 6.983 | -40.3% |
| capture 32 nodes | 32.225 | 27.575 | -14.4% |
| exact name | 32.418 | 27.605 | -14.8% |
| exists miss | 92.123 | 87.416 | -5.1% |
| first button | 15.269 | 10.723 | -29.8% |
| count buttons | 90.794 | 86.593 | -4.6% |
| list buttons | 89.633 | 87.489 | -2.4% |
| state filter | 105.791 | 93.132 | -12.0% |
| descendant selector | 107.257 | 102.632 | -4.3% |
| selector union | 105.829 | 101.396 | -4.2% |
| ready wait | 35.885 | 31.052 | -13.5% |
| set text | 45.999 | 41.389 | -10.0% |
| press to visible dialog control | 298.529 | 286.509 | -4.0% |
| multi-window tree roots | 10.643 | 9.140 | -14.1% |
| multi-window unique dialog control | 102.251 | 101.168 | -1.1% |

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

Validation details and local environment limitations are recorded in the PR.

