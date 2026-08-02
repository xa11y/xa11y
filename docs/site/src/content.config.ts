import { docsLoader } from "@astrojs/starlight/loaders";
import { docsSchema } from "@astrojs/starlight/schema";
import { defineCollection, z } from "astro:content";

// Every hand-written page declares which Diátaxis mode it is (see
// `docs/PAGE_TYPES.md`). Declaring it in the schema means `npm run build`
// fails on a page that omits it, so a new page cannot be added without its
// author deciding what kind of page it is. `docs/check_page_types.py` runs
// the same rule without a Node toolchain and additionally checks the
// in-file banner comment and the directory the page lives in.
//
// Generated API pages under `src/content/docs/api/` are .mdx too and do reach
// this schema, so `docs/generate_python_api.py` and `docs/generate_js_api.py`
// emit `pageType: reference` in the frontmatter they write. They are
// gitignored, so a local build that skips generation would not catch a
// regression here; the docs CI job generates before building and would.
const PAGE_TYPES = [
  "tutorial",
  "how-to",
  "reference",
  "explanation",
  "evaluation",
  "landing",
] as const;

export const collections = {
  docs: defineCollection({
    loader: docsLoader(),
    schema: docsSchema({
      extend: z.object({
        pageType: z.enum(PAGE_TYPES),
      }),
    }),
  }),
};
