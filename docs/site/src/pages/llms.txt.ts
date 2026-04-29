import type { APIRoute } from "astro";
import { getCollection } from "astro:content";

const SITE = "https://xa11y.dev";

// Controls sort order for known pages; new pages added to the collection
// will appear at the end automatically.
const SIDEBAR_ORDER = [
  "guides/quick-start",
  "guides/overview",
  "guides/cli",
  "guides/desktop-testing",
  "guides/input",
  "guides/screenshots",
  "guides/platform-details",
  "api/rust",
];

export const GET: APIRoute = async () => {
  const entries = await getCollection("docs");

  const known = SIDEBAR_ORDER.flatMap((id) => {
    const entry = entries.find((e) => e.id === id);
    return entry ? [entry] : [];
  });
  const rest = entries.filter(
    (e) => e.id !== "index" && !SIDEBAR_ORDER.includes(e.id)
  );
  const sorted = [...known, ...rest];

  const docLines = sorted.map((entry) => {
    const url = `${SITE}/${entry.id}/`;
    const desc = entry.data.description ? `: ${entry.data.description}` : "";
    return `- [${entry.data.title}](${url})${desc}`;
  });

  const body = [
    "# xa11y",
    "",
    "> Cross-platform accessibility library for reading and interacting with accessibility trees on macOS, Windows, and Linux.",
    "",
    "## Docs",
    "",
    ...docLines,
    "",
    "## API Reference",
    "",
    `- [Rust API](${SITE}/api/rust/reference/xa11y/)`,
    `- [Python API](${SITE}/api/python/reference/api/xa11y/)`,
    `- [JavaScript API](${SITE}/api/javascript/)`,
    "",
  ].join("\n");

  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
};
