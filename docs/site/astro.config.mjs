import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  site: "https://xa11y.dev",
  // Astro 6.4 made `markdown.gfm` an opt-in flag whose default is supplied
  // internally by the markdown processor, but `@astrojs/mdx` still gates
  // `remark-gfm` on this value being truthy. Without it, GFM tables in our
  // `.mdx` docs render as raw paragraphs (see issue #247). Set it explicitly
  // so tables, strikethrough, and task lists render in the MDX pipeline.
  markdown: {
    gfm: true,
  },
  // The Diátaxis reorganisation changed every slug outside `guides/ci`,
  // `guides/desktop-testing`, `guides/input`, and `guides/screenshots`. These
  // keep inbound links from the README, crates.io, PyPI, and search results
  // working. `guides/quick-start` lands on the tutorial that replaced it; its
  // installation half now lives at `guides/install`.
  redirects: {
    "/guides/quick-start": "/tutorials/first-script/",
    "/guides/overview": "/explanation/how-it-works/",
    "/guides/errors": "/explanation/errors-and-diagnosis/",
    "/guides/design": "/explanation/design/",
    "/guides/accessibility-quirks": "/explanation/accessibility-quirks/",
    "/guides/cli": "/reference/cli/",
    "/guides/platform-details": "/reference/platform-details/",
    "/guides/testing": "/testing/",
  },
  integrations: [
    starlight({
      title: "xa11y",
      customCss: ["./src/styles/custom.css"],
      description:
        "A Playwright-style library for desktop apps. Cross-platform UI automation, end-to-end testing, and accessibility tooling for native apps on macOS, Windows, and Linux.",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/xa11y/xa11y",
        },
      ],
      // The sidebar groups follow Diátaxis (https://diataxis.fr): learning,
      // task, information, and understanding are four different needs, and a
      // reader arrives with exactly one of them. Every page declares which
      // group it belongs to in its `pageType` frontmatter, checked by
      // docs/check_page_types.py. See docs/PAGE_TYPES.md before adding a page.
      //
      // These labels are reader-facing, and two of them deliberately do not
      // spell the Diátaxis mode name: "Get started" for `tutorial` and
      // "Concepts" for `explanation`, which reads as a term of art when used
      // as a heading. The contract is the `pageType` key and the directory,
      // neither of which moves with the label. The landing page nav in
      // src/pages/index.astro uses the same five words.
      sidebar: [
        {
          label: "Get started",
          items: [{ label: "Your first script", slug: "tutorials/first-script" }],
        },
        {
          label: "How-to guides",
          items: [
            { label: "Install xa11y", slug: "guides/install" },
            { label: "Write desktop tests", slug: "guides/desktop-testing" },
            { label: "Find the right selector", slug: "guides/debug-selectors" },
            { label: "Test in CI", slug: "guides/ci" },
            { label: "Simulate input", slug: "guides/input" },
            { label: "Capture screenshots", slug: "guides/screenshots" },
          ],
        },
        {
          label: "Concepts",
          items: [
            { label: "How xa11y works", slug: "explanation/how-it-works" },
            {
              label: "Errors & Diagnosis",
              slug: "explanation/errors-and-diagnosis",
            },
            {
              label: "Accessibility Quirks",
              slug: "explanation/accessibility-quirks",
            },
            { label: "Architecture & Design", slug: "explanation/design" },
          ],
        },
        // Top-level rather than inside a Diátaxis group: these two address
        // someone deciding whether to adopt xa11y at all, which is a question
        // none of the four modes is for. Filing them under Explanation would
        // bury them below the fold for the audience they exist to serve.
        {
          label: "Evaluating xa11y",
          items: [
            { label: "Compare to other tools", slug: "compare" },
            { label: "How xa11y is tested", slug: "testing" },
          ],
        },
        // Reference sits last: it is lookup material for someone already
        // using xa11y, so it is the one group nobody reads their way into.
        // The groups above run in the order a newcomer needs them.
        {
          label: "Reference",
          items: [
            { label: "Selectors", slug: "reference/selectors" },
            { label: "Locator & Element", slug: "reference/locator" },
            { label: "Events", slug: "reference/events" },
            { label: "Errors", slug: "reference/errors" },
            { label: "CLI", slug: "reference/cli" },
            { label: "pytest plugin", slug: "reference/pytest" },
            { label: "Platform Details", slug: "reference/platform-details" },
            {
              label: "Rust API",
              link: "https://docs.rs/xa11y/",
              attrs: { target: "_blank", rel: "noopener" },
            },
            { label: "Python API", link: "/api/python/reference/api/xa11y/" },
            { label: "JavaScript API", link: "/api/javascript/" },
          ],
        },
      ],
      editLink: {
        baseUrl: "https://github.com/xa11y/xa11y/edit/main/docs/site/",
      },
    }),
  ],
  // Allow `?raw` imports from the repo-root `examples/` directory so the
  // Desktop Testing page can embed the canonical runnable example sources
  // directly. The CI `examples` job exercises those same files, so the
  // versions shown in the docs cannot drift.
  vite: {
    server: {
      fs: {
        allow: ["../.."],
      },
    },
  },
});
