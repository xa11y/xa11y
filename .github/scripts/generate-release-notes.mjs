#!/usr/bin/env node
// Generate customer-facing release notes from git commits and PR descriptions
// using GitHub Models (free LLM inference authenticated via GITHUB_TOKEN).
//
// The LLM is constrained to emit structured data via an OpenAI-style
// function/tool call, which we then render into markdown.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { argv, env, exit, stderr, stdout } from "node:process";

// GitHub Models inference endpoint (OpenAI-compatible chat completions).
// Authenticates with the workflow's GITHUB_TOKEN — no separate API key needed.
// See: https://docs.github.com/en/github-models/prototyping-with-ai-models
const GITHUB_MODELS_URL = "https://models.github.ai/inference/chat/completions";
// Defaults to openai/gpt-4o-mini: long-standing on GitHub Models, Low
// rate-limit tier (150 req/day, 8K in / 4K out on free), native tool
// calling. Override via MODEL_ID env var to try alternatives without a
// code change. Note: the catalog at models.github.ai/catalog/models
// lists more models than the inference endpoint actually serves on the
// free tier — a slug being in the catalog is not a guarantee.
const MODEL_ID = env.MODEL_ID || "openai/gpt-4o-mini";

// Per-commit caps. Kept tight because the GitHub Models free tier has
// strict per-request input caps (e.g. 4000 tokens for gpt-5-mini), and
// we'd rather trim aggressively than overflow into a 413.
const PR_DESCRIPTION_MAX_CHARS = 600;
const COMMIT_BODY_MAX_CHARS = 200;
// Per-batch budget for the user message (commit blocks). Leaves room
// for the system prompt + tool definition + response. ~8K chars ≈ 2K
// tokens, well under the 4K-token cap for the smallest free models.
const BATCH_USER_CHAR_BUDGET = 8000;

const TOOL_DEFINITION = {
  type: "function",
  function: {
    name: "emit_release_notes",
    description:
      "Emit structured release note entries. Call this exactly once with ALL entries.",
    parameters: {
      type: "object",
      properties: {
        entries: {
          type: "array",
          items: {
            type: "object",
            properties: {
              category: {
                type: "string",
                enum: ["breaking", "features", "bug fixes", "deprecations"],
              },
              description: {
                type: "string",
                description:
                  "Customer-facing description of the change, written from the user's perspective.",
              },
              reference: {
                type: "string",
                description:
                  "PR link like (#1234) or commit hash in backticks like (`abc1234`).",
              },
            },
            required: ["category", "description", "reference"],
          },
        },
      },
      required: ["entries"],
    },
  },
};

const SYSTEM_PROMPT_TEMPLATE = `You are writing customer-facing release notes for {repo_name}.

{readme_section}

You will receive raw git commit messages and PR descriptions. Your job is to call the emit_release_notes tool with structured entries that a customer would find useful.

What to INCLUDE (only changes a user of this package would notice):
- New public APIs, selectors, actions, or behaviors they can rely on
- New CLI flags or options they can use
- Bug fixes that affected their workflows
- Breaking changes to public API signatures, selector syntax, or action semantics
- Deprecations of public APIs
- Performance improvements they would notice
- Platform support changes (new OS, new toolkit, dropped support)

What to EXCLUDE (never mention these even if the commit prefix suggests otherwise):
- CI/CD pipeline changes, GitHub Actions workflow updates, release process changes
- Test additions, test fixes, test infrastructure, test-app changes
- Internal refactors that don't change user-facing behavior
- Dependency bumps (dependabot, Cargo.lock updates)
- Documentation-only changes (docs site, README, doc comments)
- Build infrastructure, xtask changes, developer tooling
- Merge commits, chore commits, code quality / linting / formatting changes
- Changes to internal modules that aren't part of the public API
- Fuzz harness changes

When in doubt, ask: "Would a user of xa11y notice this change?" If no, skip it.

Writing style:
- Write from the user's perspective — what changed FOR THEM.
- For breaking changes, explain what the user needs to do differently.
- For features, explain what the user can now do.
- For bug fixes, explain what was broken and that it's now fixed.
- Keep descriptions concise — one or two sentences max.
- Use the PR number as reference when available (e.g. "(#1234)"), otherwise use the short commit hash.
- Combine related commits into a single entry when they're part of the same feature/fix.
- If there are no user-visible changes at all, call the tool with an empty entries array.
- ALWAYS call the tool. Never respond with plain text.`;

function runGit(...args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function getPreviousTag(currentTag) {
  const tags = runGit("tag", "--sort=-v:refname").split("\n").filter(Boolean);
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

function getPrDescriptions(commits) {
  const prNumbers = new Set();
  for (const c of commits) {
    const hay = `${c.subject}\n${c.body}`;
    for (const m of hay.matchAll(/#(\d+)/g)) {
      prNumbers.add(m[1]);
    }
  }

  const descriptions = {};
  for (const pr of prNumbers) {
    let result;
    try {
      result = execFileSync(
        "gh",
        ["pr", "view", pr, "--json", "title,body", "-q", '.title + "\\n" + .body'],
        { encoding: "utf8", timeout: 15_000, stdio: ["ignore", "pipe", "pipe"] },
      );
    } catch (err) {
      if (err.code === "ENOENT") {
        // gh not available — skip PR body enrichment, commit messages alone
        // are still enough for the LLM to work with.
        stderr.write("gh CLI not found; skipping PR description lookup.\n");
        return descriptions;
      }
      continue; // non-existent PR number etc. — best-effort
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

function buildInputText(commits, prDescriptions) {
  return commits.map((c) => buildCommitBlock(c, prDescriptions)).join("");
}

// Greedily pack commit blocks into batches under BATCH_USER_CHAR_BUDGET.
// A single oversize commit (rare, but possible if a PR description is
// near the cap) gets its own batch.
function chunkCommits(commits, prDescriptions) {
  const batches = [];
  let current = [];
  let currentChars = 0;
  for (const c of commits) {
    const block = buildCommitBlock(c, prDescriptions);
    if (currentChars + block.length > BATCH_USER_CHAR_BUDGET && current.length > 0) {
      batches.push(current);
      current = [];
      currentChars = 0;
    }
    current.push(c);
    currentChars += block.length;
  }
  if (current.length > 0) batches.push(current);
  return batches;
}

function buildSystemPrompt(repoName) {
  const readme = getReadme();
  const readmeSection = readme
    ? `Here is the README for context on what this project does:\n<readme>\n${readme}\n</readme>`
    : "";
  return SYSTEM_PROMPT_TEMPLATE.replace("{repo_name}", repoName).replace(
    "{readme_section}",
    readmeSection,
  );
}

// Retry 5xx and 429 with exponential backoff. Don't retry other 4xx —
// those are deterministic config errors (bad model, bad payload) and a
// retry just wastes the daily quota.
const RETRY_DELAYS_MS = [2000, 5000, 10000];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function invokeGithubModelsOnce(payload, token) {
  const resp = await fetch(GITHUB_MODELS_URL, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "Content-Type": "application/json",
      "X-GitHub-Api-Version": "2022-11-28",
    },
    body: JSON.stringify(payload),
  });

  if (resp.ok) return { ok: true, body: await resp.json() };

  const detail = await resp.text();
  // Surface status, rate-limit headers, and response body as separate
  // stderr lines so the workflow log shows them even when the thrown
  // Error message gets truncated or its newlines collapsed.
  stderr.write(`GitHub Models error: ${resp.status} ${resp.statusText}\n`);
  const interesting = [
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
    "retry-after",
    "x-request-id",
    "www-authenticate",
  ];
  for (const name of interesting) {
    const value = resp.headers.get(name);
    if (value) stderr.write(`  ${name}: ${value}\n`);
  }
  stderr.write(`Response body:\n${detail || "(empty)"}\n`);
  return {
    ok: false,
    status: resp.status,
    statusText: resp.statusText,
    detail,
    retryAfter: Number(resp.headers.get("retry-after")) || null,
  };
}

async function invokeGithubModels(inputText, repoName, token) {
  const payload = {
    model: MODEL_ID,
    temperature: 0.2,
    max_tokens: 4096,
    messages: [
      { role: "system", content: buildSystemPrompt(repoName) },
      { role: "user", content: inputText },
    ],
    tools: [TOOL_DEFINITION],
    tool_choice: {
      type: "function",
      function: { name: "emit_release_notes" },
    },
  };

  let result;
  for (let attempt = 0; attempt <= RETRY_DELAYS_MS.length; attempt++) {
    stderr.write(
      `POST ${GITHUB_MODELS_URL} (model=${MODEL_ID}, attempt ${attempt + 1}/${RETRY_DELAYS_MS.length + 1})\n`,
    );
    try {
      result = await invokeGithubModelsOnce(payload, token);
    } catch (err) {
      // Network-level failure (DNS, socket reset, fetch timeout). Retry
      // these the same as a 5xx — they're almost always transient.
      stderr.write(`Network error: ${err.message ?? err}\n`);
      result = { ok: false, status: 0, statusText: "network error", detail: String(err.message ?? err), retryAfter: null };
    }
    if (result.ok) break;

    const isTransient = result.status === 0 || result.status === 429 || result.status >= 500;
    const hasRetriesLeft = attempt < RETRY_DELAYS_MS.length;
    if (!isTransient || !hasRetriesLeft) {
      throw new Error(
        `GitHub Models request failed: ${result.status} ${result.statusText} — ${(result.detail ?? "").slice(0, 500) || "(empty body)"}`,
      );
    }
    const delay = result.retryAfter ? result.retryAfter * 1000 : RETRY_DELAYS_MS[attempt];
    stderr.write(`Transient ${result.status} — retrying in ${delay}ms.\n`);
    await sleep(delay);
  }

  const body = result.body;
  const choices = body.choices ?? [];
  if (choices.length === 0) {
    throw new Error(`No choices in response: ${JSON.stringify(body).slice(0, 1000)}`);
  }

  const message = choices[0].message ?? {};
  const toolCalls = message.tool_calls ?? [];
  for (const call of toolCalls) {
    if (call.function?.name === "emit_release_notes") {
      const args = call.function.arguments ?? "{}";
      const parsed = typeof args === "string" ? JSON.parse(args) : args;
      return parsed.entries ?? [];
    }
  }

  throw new Error(
    `Model did not emit a tool call. Response message: ${JSON.stringify(message).slice(0, 1000)}`,
  );
}

function renderMarkdown(version, entries, repo, prevTag, newTag) {
  const sections = {
    breaking: { title: "Breaking Changes", items: [] },
    deprecations: { title: "Deprecations", items: [] },
    features: { title: "Features", items: [] },
    "bug fixes": { title: "Bug Fixes", items: [] },
  };

  for (const entry of entries) {
    if (sections[entry.category]) sections[entry.category].items.push(entry);
  }

  const lines = [`## What's Changed in ${version}`, ""];
  let anySection = false;
  for (const key of ["breaking", "deprecations", "features", "bug fixes"]) {
    const { title, items } = sections[key];
    if (items.length === 0) continue;
    anySection = true;
    lines.push(`### ${title}`);
    for (const item of items) {
      const ref = item.reference ? ` ${item.reference}` : "";
      lines.push(`- ${item.description}${ref}`);
    }
    lines.push("");
  }
  if (!anySection) {
    lines.push("_No user-visible changes in this release._");
    lines.push("");
  }
  if (repo && prevTag && newTag) {
    lines.push(`**Full Changelog**: https://github.com/${repo}/compare/${prevTag}...${newTag}`);
  }
  return lines.join("\n").replace(/\s+$/, "") + "\n";
}

function parseArgs(args) {
  const opts = { version: null, since: null, repo: env.GITHUB_REPOSITORY ?? "", json: false, dryRun: false };
  const positional = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--since") opts.since = args[++i];
    else if (a === "--repo") opts.repo = args[++i];
    else if (a === "--json") opts.json = true;
    else if (a === "--dry-run") opts.dryRun = true;
    else if (a === "-h" || a === "--help") {
      stdout.write(
        "usage: generate-release-notes.mjs <version> [--since TAG] [--repo OWNER/NAME] [--json] [--dry-run]\n",
      );
      exit(0);
    } else positional.push(a);
  }
  opts.version = positional[0];
  if (!opts.version) {
    stderr.write("error: <version> positional argument is required (e.g. v0.5.0)\n");
    exit(2);
  }
  return opts;
}

async function main() {
  const args = parseArgs(argv.slice(2));
  const newTag = args.version.startsWith("v") ? args.version : `v${args.version}`;
  const prevTag = args.since ?? getPreviousTag(newTag);

  if (!prevTag) {
    stderr.write(`No previous tag found; treating ${newTag} as the initial release.\n`);
    const today = new Date().toISOString().slice(0, 10);
    stdout.write(`Initial release of xa11y ${newTag} (${today}).\n`);
    return;
  }

  let rangeSpec = newTag === "HEAD" ? `${prevTag}..HEAD` : `${prevTag}..${newTag}`;
  try {
    execFileSync("git", ["rev-parse", "--verify", `${newTag}^{commit}`], {
      stdio: ["ignore", "ignore", "ignore"],
    });
  } catch {
    // Tag doesn't exist yet (common during the release workflow before
    // `gh release create` runs). Diff against HEAD instead.
    rangeSpec = `${prevTag}..HEAD`;
  }

  const commits = getCommits(rangeSpec);
  stderr.write(`Found ${commits.length} commits in ${rangeSpec}.\n`);
  if (commits.length === 0) {
    stdout.write(`_No changes since ${prevTag}._\n`);
    return;
  }

  const prDescriptions = getPrDescriptions(commits);
  stderr.write(`Fetched ${Object.keys(prDescriptions).length} PR descriptions.\n`);

  const batches = chunkCommits(commits, prDescriptions);
  stderr.write(
    `Split into ${batches.length} batch(es) under ${BATCH_USER_CHAR_BUDGET}-char budget.\n`,
  );

  if (args.dryRun) {
    batches.forEach((batch, i) => {
      stdout.write(`\n===== batch ${i + 1}/${batches.length} (${batch.length} commits) =====\n`);
      stdout.write(buildInputText(batch, prDescriptions));
    });
    return;
  }

  const token = env.GITHUB_TOKEN;
  if (!token) {
    stderr.write("GITHUB_TOKEN is required to call the GitHub Models API.\n");
    exit(1);
  }

  const repoName = args.repo ? args.repo.split("/").pop() : "xa11y";
  const allEntries = [];
  for (let i = 0; i < batches.length; i++) {
    const batch = batches[i];
    const inputText = buildInputText(batch, prDescriptions);
    stderr.write(
      `Batch ${i + 1}/${batches.length}: ${batch.length} commits, ${inputText.length} chars.\n`,
    );
    const entries = await invokeGithubModels(inputText, repoName, token);
    stderr.write(`Batch ${i + 1}/${batches.length}: model emitted ${entries.length} entries.\n`);
    allEntries.push(...entries);
  }

  if (args.json) {
    stdout.write(JSON.stringify(allEntries, null, 2) + "\n");
  } else {
    stdout.write(renderMarkdown(newTag, allEntries, args.repo, prevTag, newTag));
  }
}

main().catch((err) => {
  stderr.write(`${err.stack ?? err.message ?? err}\n`);
  exit(1);
});
