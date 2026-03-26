import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  site: "https://xa11y.dev",
  integrations: [
    starlight({
      title: "xa11y",
      description:
        "Cross-platform accessibility library for reading and interacting with accessibility trees.",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/xa11y/xa11y",
        },
      ],
      sidebar: [
        {
          label: "Getting Started",
          items: [
            { label: "Overview", slug: "guides/overview" },
            { label: "Quick Start", slug: "guides/quick-start" },
          ],
        },
        {
          label: "API Reference",
          items: [
            { label: "Rust API", link: "/api/rust/reference/xa11y/" },
            { label: "Python API", link: "/api/python/reference/api/xa11y/" },
          ],
        },
      ],
      editLink: {
        baseUrl: "https://github.com/xa11y/xa11y/edit/main/docs/site/",
      },
    }),
  ],
});
