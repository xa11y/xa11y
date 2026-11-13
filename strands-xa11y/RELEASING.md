# Releasing strands-xa11y

strands-xa11y has its own version line, its own tag series, and its own
workflow. It is not part of the xa11y release.

## Why it is separate

`release.toml` sets `shared-version = true`, so cargo-release moves every
crate in the workspace to the same number in one commit. That is right for
`xa11y-core`, `xa11y-python` and `xa11y-js`, which are the same library
compiled for different consumers.

It is wrong for this package. It tracks two things xa11y does not: the Strands
SDK's tool contract, and the shape of the tool schema a model sees. A change
to either is a release here and nothing at all in xa11y, and the reverse holds
too — xa11y shipping a new element property does not oblige this package to
publish an identical build.

## Where the version lives

In `strands-xa11y/pyproject.toml`, as a plain `version = "X.Y.Z"` line.

It was `dynamic = ["version"]` through hatch-vcs while this package lived in
its own repository, where the only `v*` tags were its own. In this repository
those tags belong to xa11y, so hatch-vcs would read `v0.13.0` and stamp the
wheel with xa11y's version. A static line plus `strands-xa11y-v*` tags is what
`pytest-xa11y` does for the same reason.

`.github/scripts/bump_python_package.py` is what edits it; run it with
`--package strands-xa11y --show` to print the current version.

## Release gate: the xa11y bound

The declared dependency is `xa11y>=0.13.0,<0.17.0` — a bounded minor range,
not the bare floor `pytest-xa11y` declares. The difference is deliberate.

This package reads xa11y by string, not by import: `_errors.py` keys its
guidance off exception *class names* and reads diagnosis fields with
`getattr(exc, field, None)`. A renamed exception or a dropped diagnosis
attribute does not raise anything. It degrades the tool result the model
sees — the guidance stops matching, the near misses stop appearing — while
every call still succeeds. Pre-1.0 those names can move in a minor release,
so the metadata says which minors this package has actually been run against.

The publish workflow fails if that bound is ever reduced to a bare floor. Keep
that check: the workflow is `workflow_dispatch`, anyone can run it, and the
test suite swaps in a fake xa11y, so it would pass against any version at all.

**When widening the bound to a new xa11y minor**, run the surface check
against that version first (see below), then raise the ceiling.

## The surface check

`tests/check_real_surface.py` is what stands behind the bound. It asserts that
every exception name in `_errors._GUIDANCE`, every diagnosis attribute
`describe` renders, and every method `tests/fake_xa11y.py` claims to fake
still exists on the real module.

It is not a `test_*.py` on purpose: pytest would apply the conftest that
installs the fake, and the check would then verify the fake against itself.

It runs in two places, against two different xa11y builds:

- the `python` CI job, against the bindings just built from this tree, so a
  breaking change to xa11y's Python surface and this package fail in the same
  pull request;
- the publish workflow, against the xa11y resolved from PyPI, which is what a
  consumer's `pip install` will actually pair this wheel with.

## Cutting a release

1. Confirm CI is green on `main`.
2. Run the **Publish strands-xa11y** workflow from the Actions tab, choosing a
   level:
   - `patch` — bug fixes, better error guidance, nothing an agent's behaviour
     depends on.
   - `minor` — a new action, a new field on an existing action, a widened
     xa11y bound.
   - `major` — a change that can break a working agent: a removed or renamed
     action, a required field, a changed default, or a narrowed schema.
   - `release` — publish the current version without bumping. Use it only to
     recover from a failed publish of an already-tagged version.

The workflow bumps `strands-xa11y/pyproject.toml`, commits, tags
`strands-xa11y-vX.Y.Z`, checks the dependency bound, runs the tests and the
surface check, builds an sdist and a universal wheel, publishes to PyPI
through trusted publishing (the `pypi-strands-xa11y` environment), and creates
a GitHub release.

Nothing here is tag-triggered: pushing a tag by hand publishes nothing, the
same as the xa11y release.

## What versioning means for this package

The public surface is what a model can call and what an agent author can
import:

- the exported tools (`use_desktop`, `inspect_desktop`)
- the action names and their fields, as they appear in the tool schema
- the input models exported from `strands_xa11y.models`
- the environment variables that gate consent

Everything else — the ref store's eviction policy, the snapshot renderer's
internals, the module layout under `_`-prefixed names — is free to change in a
patch release.

One case deserves care: **tightening what an action accepts is breaking in
effect, even though no name moved.** An agent whose prompt has it passing a
field that is now rejected will start failing at a step that used to work.
Ship that as a major bump, and say so in the release notes.

## Release notes

GitHub's generated notes, anchored to the previous `strands-xa11y-v*` tag.

The xa11y release runs commits through an AI classification pass using
`.github/release-notes-prompt.md`, which is written about the xa11y public API
— `App`, locators, selectors, roles, actions, events. It would misfile a
change to a tool schema. If this package's changelog becomes busy enough to
want the same treatment, it needs its own prompt rather than a share of that
one.

`.github/scripts/release-notes.mjs` filters the tag list to the
`v\d+\.\d+\.\d+` series when resolving the previous tag, so a
`strands-xa11y-v*` tag can never be picked as the predecessor of an xa11y
release.

## History

This package lived at `github.com/xa11y/strands-xa11y` through v0.1.0 on PyPI.
It moved here so that a change to xa11y's Python surface and the tool that
reads it break in the same pull request. PyPI keeps the project and its
release history; only the source moved.
