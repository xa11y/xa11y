# Linux selector pushdown with AT-SPI Collection

At xa11y revision `41fb1fc`, the Linux planner evaluates flat selectors
during a live tree walk. A local D-Bus experiment compared that path with
`Collection.GetMatches` and a batched walk on the same live applications.

## How much of a query fits

| Query | Shortest candidate plan | Work left in xa11y |
| --- | --- | --- |
| `button, check_box` | One rule with a union of native role bits | Account for normalized roles, logical scope and completeness |
| `button[enabled="true"]` | Role bits AND either enabled or sensitive | Preserve xa11y's state interpretation |
| `button[name="OK"]` | One button query, then pipelined Name reads on candidates | Compare names and enforce ancestry |
| `[name*="report"]` | Enumerate all references in one call, then pipeline Name reads | Apply the existing string matcher |
| `group[name="Buttons"] > button` | Find candidate groups, check their names, then query direct children of the matching groups | Resolve logical children, order and overlapping scopes |
| `button:nth(3)` | In an exact scope, request three matches in canonical order | Select the third after all residual checks |
| Full elements and actions | Find candidate references first | Read the selected elements and register their handles |

MatchRule supports native roles, state bits, toolkit object attributes and
accessibility interfaces. Its attribute map is the map from
`Accessible.GetAttributes`. Passing `{"name": "OK"}` missed an actual OK
button. Names, descriptions and values are separate data. Category-specific
match modes can express some unions directly. Combining role and state sets
for `(button AND enabled) OR (checkbox AND checked)` also admits the crossed
pairs. A broader candidate rule can still feed the original clause matcher.

Of 128 literal positional `.locator(...)` call sites in `tests/suites/python`,
100 name a role somewhere, including 82 that also filter on an attribute.
The other 28 include name-only queries and wildcards. Another 158 call sites
pass dynamic arguments. These counts measure the test source at this revision.
They give neither a production traffic distribution nor a fraction that can
safely finish in one call.

## Measurements

Median wall-clock milliseconds for a warm stationary fixture. **Candidate**
includes all property reads and residual filters for that selector. **With
depth** additionally checks returned references against the root by reading
parent links in batches. **Child-link walk** uses an unfiltered native packet
to prefetch records, follows real child links, fills in missing records, and
evaluates the fixture's role, name and state predicates locally.

| Query | Current | Candidate | With depth | Child-link walk |
| --- | ---: | ---: | ---: | ---: |
| Qt stock, count 16 buttons | 24.25 | 0.48 | 2.22 | 9.88 |
| Qt stock, count `button[name="OK"]` | 31.53 | 1.09 | 2.30 | 12.32 |
| Qt 512, count all buttons | 74.62 | 2.47 | 16.86 | 30.40 |
| Qt 512, count the last button by role and name | 108.99 | 15.19 | 16.17 | 43.12 |
| Qt 512, count a missing button by name | 107.39 | 14.11 | 14.18 | 40.45 |
| Qt 512, count the last button by name alone | 110.67 | 15.01 | 15.31 | 39.02 |
| Qt 512, any button exists | 4.00 | 0.35 | 0.93 | 30.92 |
| Chromium, count all 46 buttons | 52.55 | 0.53 | 2.57 | 17.69 |
| Chromium, count `button[name="OK"]` | 62.82 | 1.38 | 2.60 | 19.32 |

The release-built xa11y Python binding supplied the baseline. The prototype
used dbus-fast 5.2.0 and batches of 64 references per property wave on Linux
x86-64. Each case compared ordered (bus, path) pairs with `.elements()`
before timing. Timed count/exists operations excluded fixture setup, initial
root discovery and parity checks. After one warm-up per strategy, nine
interleaved runs rotated the execution order. The GTK socket used three runs.
These are local end-to-end query measurements. The numbers include differences
in client transport, and the candidate strategies assume a complete native
packet. The experiment measures counts and existence. It does not time full
element materialization, a release implementation, or CI wall time.

## What breaks a direct replacement

* **Missing descendants.** A GTK 3 host exposes a button inside another
  process through a GtkSocket/GtkPlug. From the host, the native role query
  returned only the host button. The public locator returned both buttons.
  The socket's endpoint appeared as an ordinary filler with no object
  attributes. Reading its real children revealed the foreign bus name.
  Checking parents of returned references cannot discover an omitted button.
  The child-link walk found both in order. It also found the embedded button
  by name.
* **Extra descendants.** On a Qt form with 58 nested groups, the native
  role query returned seven buttons. The public locator returned six.
  Native traversal exposes a boolean recursion choice. xa11y has a depth
  budget and flattens nested application nodes. The child-link walk recovered
  the expected count and excluded the deep button even in a name-only query.
* **Early limits.** The first native button on the stock Qt form was not OK.
  Applying a count limit of one before checking its name produced a false
  negative. Limits and per-clause ordinals depend on verified membership
  and document order.
* **Availability and roles.** Qt 6.12, the GTK 3 ATK bridge and Chromium 156
  answered the call. GTK 4.18.6 and the AccessKit test app exposed valid
  button trees and lacked Collection. Qt also logged that it ignored role bit
  129, which xa11y maps to button. Matching ordinary buttons on that build
  does not validate the full role mapping. Some other xa11y roles depend on
  state, actions or a fallback role name.
* **Changes and errors.** Fresh native queries on two Qt groups tracked a
  reparent, a deletion and an insertion. That test observed each settled
  tree. A multi-call query can still race with mutation. A production executor
  must preserve stale-object handling, deadlines and Chromium's empty-tree
  diagnostic.

The strongest general use on these fixtures was **speculative discovery**.
A wildcard GetMatches packet lets the client issue the structural and
selector reads together, including reads on grandchildren before the parent
reply arrives. Walking the returned child links from the requested scope
restores order, observes omitted branches and enforces logical depth. On GTK 4
and AccessKit the same experiment discovered nodes one level at a time
without a native packet. The narrow case for using native predicates directly
is a scope with known complete traversal and an equivalent role/state
mapping. A verified positive witness can suffice for boolean existence.
Negative answers, counts and ordered results need stronger evidence.

A cost policy should keep the current early-exit strategy available. The
child-link walk made the broad existence query on the wide form about
eight times slower. Production adoption would also need bounded speculative
work for virtualized trees, the real selector matcher, and diagnostics on the
reachable path. The fixture walker deliberately exercises only the role codes
and exact nonempty names in its benchmark cases.

## Reproduce

Use an AT-SPI-enabled Linux session with the release binding built from the
revision above. Install dbus-fast in the probe interpreter. For the synthetic
Qt fixtures, install PySide6. For the embedded fixture, use an X11 session
and a Python interpreter with GTK 3 PyGObject. Start the fixture and run the
probe from another terminal on the same accessibility bus.

```sh
python qt_fixture.py
python probe.py xa11y-512-button-probe --name "Button 511" > wide.csv
```

```sh
python qt_fixture.py --deep
python probe.py xa11y-deep-probe --name Deep --walk-only > deep.csv
```

```sh
# Start the host, then the plug after the host has written the socket ID.
python3 gtk_embed.py host /tmp/xa11y-collection.xid
python3 gtk_embed.py plug /tmp/xa11y-collection.xid
python probe.py collection-host --name "Embedded button" --walk-only > embed.csv
```

Running the deep or embedded test without `--walk-only` makes the probe
report the raw strategy's identity mismatch. The packaged medians also
cover the repository's Qt, GTK 3 rich, GTK 4 and AccessKit fixtures, plus
Chromium with renderer accessibility enabled on the Electron fixture HTML
served over localhost.

[AT-SPI Collection wire specification](https://github.com/GNOME/at-spi2-core/blob/main/xml/Collection.xml) · [xa11y Linux executor at the measured revision](https://github.com/xa11y/xa11y/blob/41fb1fc688e6053aeb631eb596bc53ad5271295a/xa11y-linux/src/atspi.rs#L3029) · [Per-fixture medians](medians.csv)
