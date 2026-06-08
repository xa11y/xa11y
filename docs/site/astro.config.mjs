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
      sidebar: [
        {
          label: "Docs",
          items: [
            { label: "Quick Start", slug: "guides/quick-start" },
            { label: "Overview", slug: "guides/overview" },
            { label: "CLI", slug: "guides/cli" },
            { label: "Desktop Testing", slug: "guides/desktop-testing" },
            { label: "Testing in CI", slug: "guides/ci" },
            { label: "Input Simulation", slug: "guides/input" },
            { label: "Screenshots", slug: "guides/screenshots" },
            { label: "Platform Details", slug: "guides/platform-details" },
            {
              label: "Accessibility Quirks",
              slug: "guides/accessibility-quirks",
            },
            { label: "Architecture & Design", slug: "guides/design" },
          ],
        },
        {
          label: "API",
          items: [
            {
              label: "Rust",
              link: "https://docs.rs/xa11y/",
              attrs: { target: "_blank", rel: "noopener" },
            },
            { label: "Python", link: "/api/python/reference/api/xa11y/" },
            { label: "JavaScript", link: "/api/javascript/" },
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
