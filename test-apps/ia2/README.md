# Private IA2 conformance fixture

This is stage one of [#483](https://github.com/xa11y/xa11y/issues/483), not a
public accessibility provider. UIA remains the default. The probe never falls
back to UIA or sends keyboard/pointer input. The owned fixture orchestration
uses a fresh browser profile; it never selects an existing user's browser.
The native executable also accepts an explicit HWND for manual diagnostics.
No rich-document support or Firefox compatibility is claimed.

The Windows CI job builds `xa11y-windows/examples/ia2_probe.rs` and runs its
ignored `owned_browser_ia2_identity` test. Python requires `pywin32` for the
owned browser Job Object. To run it locally after building the example:

```powershell
$env:XA11Y_IA2_PROBE = "$PWD/target/debug/examples/ia2_probe.exe"
cargo test -p xa11y-windows --example ia2_probe -- --ignored --test-threads=1 --nocapture
```

`XA11Y_IA2_BROWSER` can name an explicit Chromium executable; otherwise the
harness chooses the installed Chrome or Edge fixture executable. A missing
browser fails the test. It creates a unique temporary profile and two local
documents with distinct markers. Browser creation is suspended until assignment
to an owned kill-on-close Job Object. Cleanup terminates only that job, waits
for it to empty, then removes only the original temporary directory.

The owned browser starts with `--force-renderer-accessibility=complete` and
`--disable-backgrounding-occluded-windows`. The former pins complete native
accessibility for the fixture's lifetime; the latter keeps the selected tab
running when a test desktop is disconnected or occluded. It changes only this
owned test browser, without disabling UIA or the sandbox. On local Chrome
155.0.8059.80, the otherwise identical baseline exposed an IA2 document with
`busy:true`, no name and no children after five seconds. A controlled run with
only the occlusion flag added passed both tab checks in 2.38 seconds. This
supports the fixture configuration; it does not establish a general cause or
fix for production browser/provider timeouts. The harness records the browser
version and exact configuration, plus a bounded owned-browser log on failure.
The final static configuration also passed the ignored native test in 2.28
seconds on that version: both tabs produced distinct negative IDs, each walk
examined 248 nodes, and every identity assertion passed. Windows CI must repeat
this against its own recorded browser version; local Chromium evidence does not
prove other engines or versions.

For each tab, the separate native probe:

1. Checks the owned window and process, then bootstraps MSAA with
   `AccessibleObjectFromWindow(OBJID_CLIENT, IID_IAccessible)`.
2. Selects exactly one named fixture tab through `accDoDefaultAction`, once.
3. Waits for exactly one visible active document and no visible inactive
   fixture document. Only that declared publication condition is polled;
   COM errors propagate.
4. Uses `IServiceProvider::QueryService(IID_IAccessible, IID_IAccessible2)`.
   It preserves signed negative Chromium IDs, validates the event HWND's native
   root, and compares canonical IUnknown plus window/ID on repeat capture.
5. Resolves the negative ID through Chromium's signed `get_accChild` mechanism
   and checks that it denotes the same object, rather than a positive ordinal.

The walk fails beyond 2048 nodes, depth 32, or a name of 8192 UTF-16 units.
It reports only bounded fixture identity, not a reusable document tree.
The Python owner kills each probe subprocess after 30 seconds, including a
hung COM call. This is process containment for this test: it does **not** prove
that an in-process adapter can cancel arbitrary COM calls. No array-returning
IA2 methods are invoked, and their server allocations are not claimed bounded.

Hermetic tests exercise the actual SDK COM trampoline, QueryService absence
versus access failure, borrowed BSTR input, owned dispatch variants, canonical
IUnknown lifetime, invalid child counts and signed child-ID separation.
The private ABI follows the complete MSAA/IDispatch/IUnknown prefix and pinned
[official IDL](https://github.com/LinuxA11y/IAccessible2/tree/c9ae003d9c85eb707716928de97e055f5b77189c/api).

Still required by #483: shared semantics/relations, text ranges and offset
units, formatting and embedded objects, tables, actionable capture lifetimes,
explicit backend selection across bindings/CLI/MCP, provider timeout policy,
removed/hung/inaccessible fixtures, and tested browser/version capability
documentation. This fixture is evidence for discovery and identity only.
