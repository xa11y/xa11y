# Releasing pytest-xa11y

pytest-xa11y has its own version line, its own tag series, and its own
workflow. It is not part of the xa11y release.

## Why it is separate

`release.toml` sets `shared-version = true`, so cargo-release moves every
crate in the workspace to the same number in one commit. That is right for
`xa11y-core`, `xa11y-python` and `xa11y-js`, which are the same library
compiled for different consumers — a fix in the selector engine is the same
fix in all of them.

It is wrong for this package. A pytest plugin changes when pytest changes,
when its own fixtures grow, or when a diagnostic turns out to be missing
something. None of that is an xa11y release, and the reverse holds too:
`xa11y` shipping a new element property does not oblige this package to
publish an identical build. Tying them would mean either a stream of no-op
pytest-xa11y releases or a version number that says nothing.

The dependency is a lower bound, `xa11y>=0.12`, not a pin. The plugin uses
long-stable API — `App.find`, locator waits, structured error attributes — so
a consumer can upgrade either package on its own schedule.

## Cutting a release

1. Confirm CI is green on `main`.
2. Run the **Publish pytest-xa11y** workflow from the Actions tab, choosing a
   level:
   - `patch` — bug fixes, diagnostics improvements, nothing a suite must
     react to.
   - `minor` — new fixtures, markers, options, or `AppLauncher` fields.
   - `major` — a change that can break an existing suite: a removed or
     renamed fixture, a marker that now skips where it used to run, a default
     that changes what a test does.
   - `release` — publish the current version without bumping. Use it only to
     recover from a failed publish of an already-tagged version.

The workflow bumps `pytest-xa11y/pyproject.toml`, commits, tags
`pytest-xa11y-vX.Y.Z`, runs the plugin's test suite, builds an sdist and a
universal wheel, publishes to PyPI through trusted publishing (the
`pypi-pytest-xa11y` environment), and creates a GitHub release.

Nothing here is tag-triggered: pushing a tag by hand publishes nothing, the
same as the xa11y release.

## What versioning means for this package

The public surface is what a consumer's `conftest.py` and tests can name:

- the fixtures (`xa11y_app`, `xa11y_launcher`, `xa11y_fresh_app`,
  `xa11y_app_factory`, `xa11y_events`, `xa11y_capabilities`,
  `xa11y_artifacts`)
- the markers (`xa11y_requires`, `xa11y_frontmost`)
- the command-line options
- the exported names in `pytest_xa11y.__all__`, and `AppLauncher`'s fields

Everything else — `AppSession`'s internals, the diagnostics module layout,
the probe implementations — is free to change in a patch release.

One case deserves care: **making a capability probe stricter is a breaking
change in effect, even though no name moved.** A probe that starts detecting
a missing grant will skip tests that previously ran, and a suite counting on
those tests to run will go quietly green. Ship that as a minor bump at least,
and say so in the release notes.

## Version single-source

The version lives in one place, `pyproject.toml`. `pytest_xa11y.__version__`
reads it back through `importlib.metadata`, so there is no second copy to
drift. `.github/scripts/bump_pytest_xa11y.py` is what edits it; run it with
`--show` to print the current version.

## Release notes

GitHub's generated notes, anchored to the previous `pytest-xa11y-v*` tag.

The xa11y release runs commits through an AI classification pass using
`.github/release-notes-prompt.md`, which is written about the xa11y public
API — `App`, locators, selectors, roles, actions, events. It would file a
fixture rename under the wrong heading or drop it entirely. If this package's
changelog becomes busy enough to want the same treatment, it needs its own
prompt rather than a share of that one.

Note that `.github/scripts/release-notes.mjs` filters the tag list to the
`v\d+\.\d+\.\d+` series when resolving the previous tag. Without that filter,
a `pytest-xa11y-v*` tag could be picked as the predecessor of an xa11y
release and produce notes for the wrong commit range.
