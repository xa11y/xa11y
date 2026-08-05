#!/usr/bin/env node
// Release-note generation, split into two deterministic halves with one LLM
// step in between.
//
//   release-notes.mjs context <tag> [--since TAG] [--repo OWNER/NAME]
//       Walk the commit range since the previous tag, enrich each commit with
//       its PR description, and print a context document on stdout.
//
//   release-notes.mjs render <tag> --entries FILE [--since TAG] [--repo O/N]
//       Validate the structured entries the model produced and render them
//       into release-note markdown on stdout.
//
// Everything mechanical — resolving the previous tag, walking commits, looking
// up PRs, ordering sections, appending the changelog link — stays in code. The
// model is left with exactly one job: deciding which changes a user of xa11y
// would notice, and describing them. Anything it gets wrong is therefore a
// classification or wording problem, which is reviewable in the workflow log,
// rather than a formatting problem that silently mangles the release.
//
// The model is driven by `anthropics/claude-code-action` in the workflows, so
// no API client lives here.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { argv, env, exit, stderr, stdout } from "node:process";

// Per-commit caps. These are deliberately generous: the substance of a fix is
// routinely thousands of characters into a PR body ("root cause", "the fix"),
// and truncating before it is how a release ends up describing a provider bug
// fix as a testing change. Claude's context window is not the binding
// constraint here, so cap only to bound pathological inputs.
//
// The commit-body cap matters more than it looks. Squash-merged commits bundle
// every section of a multi-commit PR, and the section describing the
// user-visible change is frequently the *last* one — #325 in v0.12.0 opens with
// "add a WinForms test app" and only reaches "read MSAA role and selection where
// UIA publishes neither", the two actual provider fixes, at character 1928 of
// 4055. Every substantive commit in that release ran past 2000 characters, one
// to 6001. Size this well above the longest body you expect, not near it: a cap
// that lands mid-section is worse than one that truncates cleanly, because the
// model then sees a section heading with no content under it and has to guess
// what it said.
const PR_DESCRIPTION_MAX_CHARS = 12_000;
const COMMIT_BODY_MAX_CHARS = 16_000;

// Commits whose PR body is never worth fetching. Dependabot bumps and release
// chores are excluded from the notes by definition, and their PR bodies are
// enormous (upstream changelogs verbatim) — enriching them would bury the
// handful of commits that matter in noise.
//
// This skips the *enrichment*, not the commit: the subject line is still in
// the context document, so classification remains the model's call.
const NO_ENRICHMENT_SUBJECT = /^(?:build\(deps[^)]*\)|chore(?:\([^)]*\))?):/i;

const CATEGORIES = ["breaking", "deprecations", "features", "bug fixes"];

const SECTION_TITLES = {
  breaking: "Breaking Changes",
  deprecations: "Deprecations",
  features: "Features",
  "bug fixes": "Bug Fixes",
};

function runGit(...args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

// The xa11y release series. Sibling packages in this repo tag their own
// series (pytest-xa11y-v*), and those tags must never appear in an xa11y
// commit range — an unfiltered tag list would make the previous tag of
// v0.13.0 whatever sorted next, producing notes for the wrong range.
const RELEASE_TAG_PATTERN = /^v\d+\.\d+\.\d+/;

function getPreviousTag(currentTag) {
  const tags = runGit("tag", "--sort=-v:refname")
    .split("\n")
    .filter(Boolean)
    .filter((tag) => RELEASE_TAG_PATTERN.test(tag));
  const idx = tags.indexOf(currentTag);
  if (idx === -1) return tags[0] ?? null;
  return tags[idx + 1] ?? null;
}

function getReadme() {
  for (const name of ["README.md", "README.rst", "README.txt", "README"]) {
    if (existsSync(name)) {
      return readFileSync(name, "utf8").slice(0, 1500);
    }
  }
  return "";
}

function getCommits(rangeSpec) {
  const log = runGit(
    "log",
    rangeSpec,
    "--no-merges",
    "--pretty=format:%H%x00%s%x00%b%x1e",
  );
  if (!log) return [];

  const commits = [];
  for (const raw of log.split("\x1e")) {
    const entry = raw.trim();
    if (!entry) continue;
    const parts = entry.split("\x00");
    if (parts.length < 2) continue;
    commits.push({
      hash: parts[0].slice(0, 7),
      subject: parts[1],
      body: parts[2] ?? "",
    });
  }
  return commits;
}

// Fetch the PR title + body for every PR referenced by a commit worth
// enriching. A missing `gh` is fatal rather than a warning: commit subjects
// alone are not enough to tell a user-visible fix from an internal one, and
// silently generating notes from subjects is the exact failure this split is
// meant to prevent.
function getPrDescriptions(commits) {
  const prNumbers = new Set();
  let skipped = 0;
  for (const c of commits) {
    if (NO_ENRICHMENT_SUBJECT.test(c.subject)) {
      skipped += 1;
      continue;
    }
    const hay = `${c.subject}\n${c.body}`;
    for (const m of hay.matchAll(/#(\d+)/g)) {
      prNumbers.add(m[1]);
    }
  }
  if (skipped > 0) {
    stderr.write(
      `Skipped PR lookup for ${skipped} dependency/chore commit(s); their subjects are still in the context.\n`,
    );
  }

  const descriptions = {};
  for (const pr of prNumbers) {
    let result;
    try {
      result = execFileSync(
        "gh",
        [
          "pr",
          "view",
          pr,
          "--json",
          "title,body",
          "-q",
          '.title + "\\n" + .body',
        ],
        { encoding: "utf8", timeout: 30_000, stdio: ["ignore", "pipe", "pipe"] },
      );
    } catch (err) {
      if (err.code === "ENOENT") {
        throw new Error(
          "gh CLI not found. PR descriptions are required to classify changes " +
            "accurately — install gh or run this in GitHub Actions.",
        );
      }
      // A referenced number that isn't a PR (an issue, or a bare "#123" in
      // prose) is expected and not an error.
      stderr.write(`No PR found for #${pr}; continuing.\n`);
      continue;
    }
    const trimmed = result.trim();
    if (trimmed) descriptions[pr] = trimmed.slice(0, PR_DESCRIPTION_MAX_CHARS);
  }
  return descriptions;
}

function buildCommitBlock(commit, prDescriptions) {
  const lines = [`COMMIT ${commit.hash}: ${commit.subject}`];
  if (commit.body) {
    lines.push(`  Body: ${commit.body.slice(0, COMMIT_BODY_MAX_CHARS)}`);
  }
  const prMatch = commit.subject.match(/#(\d+)/);
  const prNum = prMatch?.[1];
  if (prNum && prDescriptions[prNum]) {
    lines.push(`  PR #${prNum} description: ${prDescriptions[prNum]}`);
  }
  lines.push("");
  return lines.join("\n");
}

function buildContext({ repoName, prevTag, newTag, commits, prDescriptions }) {
  const readme = getReadme();
  const sections = [
    `# Release context for ${repoName} ${newTag}`,
    "",
    `Commit range: ${prevTag}..${newTag} (${commits.length} non-merge commits)`,
    "",
  ];
  if (readme) {
    sections.push(
      "## What this project is",
      "",
      "<readme>",
      readme,
      "</readme>",
      "",
    );
  }
  sections.push(
    "## Commits in this release",
    "",
    commits.map((c) => buildCommitBlock(c, prDescriptions)).join(""),
  );
  return sections.join("\n");
}

// Validate the model's entries loudly. The previous generator silently dropped
// any entry whose category it didn't recognise, so a mis-labelled breaking
// change simply vanished from the release. Fail instead.
function parseEntries(raw, sourceLabel) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`${sourceLabel} is not valid JSON: ${err.message}`);
  }

  const entries = Array.isArray(parsed) ? parsed : parsed?.entries;
  if (!Array.isArray(entries)) {
    throw new Error(
      `${sourceLabel} must be a JSON array of entries, or an object with an "entries" array. Got: ${JSON.stringify(parsed).slice(0, 300)}`,
    );
  }

  return entries.map((entry, i) => {
    const where = `${sourceLabel} entry ${i}`;
    if (typeof entry !== "object" || entry === null) {
      throw new Error(`${where} is not an object: ${JSON.stringify(entry)}`);
    }
    if (!CATEGORIES.includes(entry.category)) {
      throw new Error(
        `${where} has category ${JSON.stringify(entry.category)}; expected one of ${CATEGORIES.map((c) => JSON.stringify(c)).join(", ")}.`,
      );
    }
    const description = String(entry.description ?? "").trim();
    if (!description) {
      throw new Error(`${where} has an empty description.`);
    }
    return {
      category: entry.category,
      description,
      reference: String(entry.reference ?? "").trim(),
    };
  });
}

function renderMarkdown(version, entries, repo, prevTag, newTag) {
  const sections = {};
  for (const key of CATEGORIES) sections[key] = [];
  for (const entry of entries) sections[entry.category].push(entry);

  const lines = [`## What's Changed in ${version}`, ""];
  let anySection = false;
  // Fixed order, independent of the order the model emitted entries in.
  for (const key of ["breaking", "deprecations", "features", "bug fixes"]) {
    const items = sections[key];
    if (items.length === 0) continue;
    anySection = true;
    lines.push(`### ${SECTION_TITLES[key]}`);
    for (const item of items) {
      // Belt and braces: the prompt tells the model to keep PR refs out of the
      // description, but models occasionally inline them anyway and the
      // renderer would then double them up. Strip a trailing parenthesised PR
      // ref or commit hash before appending the reference field.
      const description = item.description
        .replace(/\s*\((?:#\d+|`?[0-9a-f]{7,40}`?)\)\s*\.?\s*$/i, "")
        .trim();
      const ref = item.reference ? ` ${item.reference}` : "";
      lines.push(`- ${description}${ref}`);
    }
    lines.push("");
  }
  if (!anySection) {
    lines.push("_No user-visible changes in this release._");
    lines.push("");
  }
  if (repo && prevTag && newTag) {
    lines.push(
      `**Full Changelog**: https://github.com/${repo}/compare/${prevTag}...${newTag}`,
    );
  }
  return lines.join("\n").replace(/\s+$/, "") + "\n";
}

function usage() {
  stdout.write(
    [
      "usage:",
      "  release-notes.mjs context <tag> [--since TAG] [--repo OWNER/NAME]",
      "  release-notes.mjs render  <tag> --entries FILE [--since TAG] [--repo OWNER/NAME]",
      "",
    ].join("\n"),
  );
}

function parseArgs(args) {
  const opts = {
    command: null,
    version: null,
    since: null,
    entries: null,
    repo: env.GITHUB_REPOSITORY ?? "",
  };
  const positional = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--since") opts.since = args[++i];
    else if (a === "--repo") opts.repo = args[++i];
    else if (a === "--entries") opts.entries = args[++i];
    else if (a === "-h" || a === "--help") {
      usage();
      exit(0);
    } else positional.push(a);
  }

  opts.command = positional[0];
  opts.version = positional[1];

  if (opts.command !== "context" && opts.command !== "render") {
    stderr.write(
      `error: expected subcommand "context" or "render", got ${JSON.stringify(opts.command ?? null)}\n`,
    );
    usage();
    exit(2);
  }
  if (!opts.version) {
    stderr.write("error: <tag> argument is required (e.g. v0.12.0)\n");
    exit(2);
  }
  if (opts.command === "render" && !opts.entries) {
    stderr.write("error: render requires --entries FILE\n");
    exit(2);
  }
  return opts;
}

// Resolve the commit range once, so `context` and `render` always agree on
// which previous tag the release is being diffed against.
function resolveRange(args) {
  const newTag = args.version.startsWith("v")
    ? args.version
    : `v${args.version}`;
  const prevTag = args.since ?? getPreviousTag(newTag);

  let rangeSpec = null;
  if (prevTag) {
    rangeSpec = newTag === "HEAD" ? `${prevTag}..HEAD` : `${prevTag}..${newTag}`;
    try {
      execFileSync("git", ["rev-parse", "--verify", `${newTag}^{commit}`], {
        stdio: ["ignore", "ignore", "ignore"],
      });
    } catch {
      // Tag doesn't exist yet — normal during the publish workflow, which
      // generates notes before `gh release create` runs.
      rangeSpec = `${prevTag}..HEAD`;
    }
  }
  return { newTag, prevTag, rangeSpec };
}

function main() {
  const args = parseArgs(argv.slice(2));
  const { newTag, prevTag, rangeSpec } = resolveRange(args);
  const repoName = args.repo ? args.repo.split("/").pop() : "xa11y";

  if (args.command === "render") {
    const raw = readFileSync(args.entries, "utf8");
    const entries = parseEntries(raw, args.entries);
    stderr.write(`Rendering ${entries.length} entrie(s).\n`);
    stdout.write(renderMarkdown(newTag, entries, args.repo, prevTag, newTag));
    return;
  }

  if (!prevTag) {
    stderr.write(
      `No previous tag found; treating ${newTag} as the initial release.\n`,
    );
    stdout.write(
      `# Release context for ${repoName} ${newTag}\n\nThis is the initial release; there is no previous tag to diff against.\n`,
    );
    return;
  }

  const commits = getCommits(rangeSpec);
  stderr.write(`Found ${commits.length} commits in ${rangeSpec}.\n`);
  if (commits.length === 0) {
    stdout.write(
      `# Release context for ${repoName} ${newTag}\n\nNo commits since ${prevTag}.\n`,
    );
    return;
  }

  const prDescriptions = getPrDescriptions(commits);
  stderr.write(`Fetched ${Object.keys(prDescriptions).length} PR descriptions.\n`);

  const context = buildContext({
    repoName,
    prevTag,
    newTag,
    commits,
    prDescriptions,
  });
  stderr.write(`Context document is ${context.length} characters.\n`);
  stdout.write(context);
}

try {
  main();
} catch (err) {
  stderr.write(`${err.stack ?? err.message ?? err}\n`);
  exit(1);
}
