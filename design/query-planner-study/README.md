# Query planning on a scrolling WPF form

Windows UI Automation can walk full records in batches or walk projected
role/name records and fetch each matching element in full. This experiment
forces both legal strategies for the same selector. It also tests two
short-lived caches and a replaceable cost function. All three native providers
now use one planner and cost policy. Each supplies the read strategies it can execute for the given scope and selector.

## Workload and controls

A PowerShell-hosted WPF window contains 32 buttons in a StackPanel inside one
overflowing ScrollViewer. Changing their names changes the number of matches
for `button[name^='Probe']`. UIA also exposes window chrome, scrolling controls
and button children. Each traversal issued 79 child-list reads in the trace.

One release binary queried the same PID, provider and application root for
every strategy at a given density. Each density started a fresh fixture.
The timer covered the native provider query, including planning and snapshot
construction. It excluded fixture setup, payload validation and trace I/O.
After warming each mode once, the driver interleaved eight samples per mode
in balanced order. It checked cardinality, document order and every serialized
element field against the full resolver after clearing the per-read handle.
The fixture remained stationary. Windows 11 Pro build 26200, x86-64.

## Measured query time

Medians in milliseconds. **Full + viewport cache** reused the result of
BuildUpdatedCache for a given runtime ID and scroll-property request during
one resolution. **Projected + ancestor cache** memoized the inherited clip
including a visited ancestor's own viewport, keyed by runtime ID during one
hydration pass. The prototypes paid for identity lookups and retained native
references. Empty IDs were ineligible for reuse.

| Matching buttons | Full | Projected | Full + viewport cache | Projected + ancestor cache |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 366.4 | 141.6 | 343.1 | 140.5 |
| 1 | 374.1 | 147.0 | 340.6 | 148.5 |
| 4 | 372.7 | 172.1 | 343.2 | 168.2 |
| 8 | 371.5 | 208.3 | 346.7 | 199.3 |
| 16 | 381.0 | 271.9 | 350.6 | 255.0 |
| 24 | 381.2 | 340.6 | 355.4 | 309.4 |
| 32 | 374.6 | 396.8 | 349.7 | 360.9 |

## Duplicate reads

One additional observation per mode/density traced selected provider calls
by UIA runtime ID and operation. The identity probes were excluded from the
timings above. Durations in the trace time individual native calls and can
include OS scheduling. They cover only the instrumented sites.

* With 32 matches the projected walk fetched 79 child lists from 79 nodes,
  hydrated 32 distinct controls, and built 33 full snapshots including the
  window. A general child-list or full-snapshot cache would get zero hits
  at those sites. The count control also read 79 distinct lists, built only
  the window snapshot and issued zero scroll-parent reads.
* Finding inherited clips made 64 GetParentElementBuildCache calls from 33
  distinct input nodes. Repeated calls to the same input accounted for
  38.3 ms in the trace. Caching shared ancestor clips reduced the count to
  33 and cut median end-to-end latency by 35.9 ms (9.0%).
* The full walk made 70 viewport refreshes of 37 distinct nodes. The 33
  duplicates took 24.1 ms in the 32-match trace. Caching the viewport read
  reduced the calls to 37 and saved 24.9 ms (6.6%) end to end.

## Held-out cost function

We fixed the model using only the 0- and 16-match uncached medians. The full
cost is their average full time. Projected cost grows linearly with hits.
The estimate comes from the preceding completed element query on the same
unchanged fixture. The model never counts the tree to acquire it.

```text
full_us      = 373700
projected_us = 141600 + 8140 * prior_hits
```

Four new match densities tested that frozen function, using the same two
uncached executors. The existing name heuristic selected projected for all
four. The trace confirmed the strategy used by the estimated-cost run.

| Matching buttons | Forced full ms | Forced projected ms | Estimated choice | Estimated ms |
| ---: | ---: | ---: | --- | ---: |
| 26 | 359.5 | 346.5 | projected | 345.4 |
| 28 | 376.2 | 373.9 | projected | 367.6 |
| 30 | 372.0 | 389.2 | full | 374.7 |
| 31 | 377.8 | 399.0 | full | 375.9 |

The model selected the lower forced median at all four densities. The
2.3 ms separation at 28 hits is small relative to run variation. Any
difference between two runs selecting the same strategy is timing variation.
At 31 hits, switching strategies saved 23.1 ms (5.8%) against the existing
choice. These coefficients describe one tree shape and provider on one host.

## Production implementation

UIA owns one scroll observation per tree walk. Siblings share viewport
validation for the same retained native source. When several projected hits
need full records, hydration also shares inherited clips by ancestor runtime
ID. A new traversal, retry or poll starts with empty maps. Single-hit
hydration skips the ancestor identity lookup. The Windows integration
fixture scrolls, destroys the scroll container, reparents its surviving
controls in reverse order and repeats the same selector after each action.

The shared planner accepts cost estimates only for ranking legal reads.
The Windows provider keeps up to 128 query fingerprints with the visited
node count and match count from completed, unbounded, app-scoped searches
with a normalized role and one name filter. Estimates apply to the same
scope, selector and depth when the previous matches reported WPF. Empty
or mixed-toolkit results discard an existing estimate. The history contains
only cardinalities and lasts for the lifetime of the provider.

With both caches active, the 0- and 16-hit measurements above estimate
full work at 4,390 units per visited node and projected work at 1,780 per
node plus 7,156 per hydrated hit. The estimate describes relative work on
the measured WPF fixture. A changed tree or different shape can make the
choice slower. Every execution still reads and validates its live matches.
Other backends, cold queries, counts and bounded searches use the same
planner with their existing eligibility and cost ordering.

[Individual timings](timings.csv) and [per-observation read counts](read-counts.csv)
include both series. The raw mode names are full, projected, view (viewport
cache), memo (ancestor cache), estimated and count. Only scroll-parent and
viewport-refresh have instrumented durations. A missing operation had no
observed calls. The [benchmark patch and fixture](https://github.com/xa11y/xa11y/pull/728#issuecomment-7198133250)
apply to the measured revision, `2f08e3d`.

## Production versus the preceding build

The same fixture and selector ran against the shared-planner baseline
(`d00260d`) and the production implementation including both caches and
history. A separate measurement build forced each eligible read strategy.
Each mode used its own long-lived provider. After four warm-up rounds, we
interleaved twelve timed queries per mode per density in balanced order.
Every result had the same normalized payload hash as the other modes.

| Matches | Baseline ms | Production ms | Forced full ms | Forced projected ms | Warm production plan |
| ---: | ---: | ---: | ---: | ---: | --- |
| 0 | 150.4 | 150.6 | 359.6 | 149.6 | projected |
| 1 | 156.3 | 158.9 | 358.3 | 156.6 | projected |
| 16 | 285.4 | 264.0 | 358.7 | 263.5 | projected |
| 26 | 355.7 | 324.7 | 353.0 | 326.1 | projected |
| 28 | 384.8 | 349.3 | 364.9 | 349.0 | projected |
| 30 | 394.6 | 356.7 | 358.0 | 359.1 | full |
| 31 | 405.7 | 361.2 | 359.3 | 368.3 | full |
| 32 | 412.1 | 359.9 | 358.7 | 378.0 | full |

At 16 hits both builds used projected reads, so the 7.5% reduction came
with the observation cache. At 32 hits the combined reduction was 12.7%.
On the production measurement build, full beat projected by 19.3 ms at
that density. At 30 hits the 1.1 ms separation is too small to distinguish
the strategies reliably. The zero- and one-hit cases showed no speedup.
These are warm-query medians on one host. [Individual timings](production-timings.csv)
are in microseconds.
