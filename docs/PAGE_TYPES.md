# Documentation page types

The hand-written pages of the docs site follow [Diátaxis](https://diataxis.fr).
Every page declares which of the four modes it is, and the declaration is
machine-checked. This file is the contract.

## The declaration

Each `.mdx` page under `docs/site/src/content/docs/` carries two things:

1. A `pageType` key in its frontmatter.
2. A `{/* DIATAXIS: … */}` banner comment immediately after the frontmatter,
   whose text is fixed per type.

```mdx
---
title: Testing in CI
description: …
pageType: how-to
---

{/* DIATAXIS: how-to — a goal-oriented recipe for a reader who already knows
    what they want. Steps and decisions only. Concepts go to explanation/,
    exhaustive option lists go to reference/. */}
```

The frontmatter key is what tooling reads. The banner is what a human or an
agent reads when they open the file to edit it, which is the moment the rule
actually needs to be visible. Because the banner text is fixed, the two cannot
drift apart.

## The types

| `pageType` | Directory | What belongs there |
| --- | --- | --- |
| `tutorial` | `tutorials/` | A guided lesson, followed start to finish, that the reader is guaranteed to complete successfully. |
| `how-to` | `guides/` | A recipe for a reader who already knows the goal and needs the steps. |
| `reference` | `reference/` | Factual description of the machinery, structured for lookup. |
| `explanation` | `explanation/` | Background: why the design is what it is, and how the pieces relate. |
| `evaluation` | site root | Pre-adoption material for someone deciding whether to use xa11y. |
| `landing` | site root (`index.mdx`) | The site entry point. Navigational only. |

`evaluation` and `landing` are deliberate additions to the four Diátaxis modes,
not sloppiness about them. `compare.mdx` and `testing.mdx` address someone who
has not adopted xa11y yet and is asking "should I?". Diátaxis has no mode for
that question, and filing those pages under `explanation` would bury them below
the fold for the audience they exist to serve. Keep the exception narrow: a
page that a *user* of xa11y would consult is one of the four real modes.

## The banner text for each type

The wording is fixed, and `docs/check_page_types.py` compares it after
collapsing whitespace, so wrap the lines however suits the file. Copy the one
matching your page's `pageType`. (If you forget, the check prints the right
text for you.)

### `tutorial`

```mdx
{/* DIATAXIS: tutorial — a guided lesson the reader follows start to finish and is guaranteed to finish successfully. Keep momentum: no options, no alternatives, no exhaustive lists. Those belong in guides/, reference/, explanation/. */}
```

### `how-to`

```mdx
{/* DIATAXIS: how-to — a goal-oriented recipe for a reader who already knows what they want. Steps and decisions only. Concepts go to explanation/, exhaustive option lists go to reference/. */}
```

### `reference`

```mdx
{/* DIATAXIS: reference — a factual description of the machinery, structured for lookup rather than for reading through. Neutral and complete. No task narratives, no rationale, no teaching. */}
```

### `explanation`

```mdx
{/* DIATAXIS: explanation — background and rationale: why the design is what it is, and how the pieces relate. No step-by-step instructions, no exhaustive tables. */}
```

### `evaluation`

```mdx
{/* DIATAXIS: evaluation — pre-adoption material for someone deciding whether to use xa11y. Deliberately outside the four Diátaxis modes; see docs/PAGE_TYPES.md before adding another one. */}
```

### `landing`

```mdx
{/* DIATAXIS: landing — the site entry point. Navigational only: every claim here is a summary of a page it links to. */}
```

## What the check enforces

`docs/check_page_types.py` runs in `cargo xtask docs` and in the `docs` CI job,
before the site build. It fails when a page:

- has no `pageType`, or one outside the table above;
- has no banner comment, a banner naming a different type than the frontmatter,
  or banner text that does not match the canonical wording for its type;
- lives in a directory that does not match its type.

The Astro content schema in `docs/site/src/content.config.ts` enforces the
frontmatter key a second time, so `npm run build` also fails on a page that
omits it. The Python check exists alongside it because it needs no Node
toolchain and reports every offending page at once.

Run it directly with:

```bash
python docs/check_page_types.py
```

## Adding a page

Decide the mode first, then write. If a page seems to need two modes, that is
the signal to write two pages. The failure this framework exists to prevent is
the page that starts as a tutorial, grows a table of every option, picks up a
paragraph of design rationale, and ends up serving nobody. `guides/overview.mdx`
reached 584 lines that way before the docs were reorganised.

Practical rules that fall out of the modes:

- A reference table has exactly one home. If two pages need it, one links.
- A tutorial never lists alternatives. Alternatives break the guarantee that
  following it works.
- A how-to never explains a concept from scratch. It links to `explanation/`.
- An explanation never carries step-by-step instructions. It links to `guides/`.
