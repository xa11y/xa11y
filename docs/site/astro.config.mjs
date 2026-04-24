import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  site: "https://xa11y.dev",
  integrations: [
    starlight({
      title: "xa11y",
      customCss: ["./src/styles/custom.css"],
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
          label: "Docs",
          items: [
            { label: "Quick Start", slug: "guides/quick-start" },
            { label: "Overview", slug: "guides/overview" },
            { label: "CLI", slug: "guides/cli" },
            { label: "Desktop Testing", slug: "guides/desktop-testing" },
            { label: "Input Simulation", slug: "guides/input" },
            { label: "Screenshots", slug: "guides/screenshots" },
            { label: "Platform Details", slug: "guides/platform-details" },
          ],
        },
        {
          label: "API",
          items: [
            { label: "Rust", link: "/api/rust/reference/xa11y/" },
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
});
