#!/usr/bin/env python3
"""Generate customer-facing release notes from git commits and PR descriptions
using GitHub Models (free LLM inference authenticated via GITHUB_TOKEN).

The LLM is constrained to emit structured data via an OpenAI-style
function/tool call, which we then render into markdown.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import date

# GitHub Models inference endpoint (OpenAI-compatible chat completions).
# Authenticates with the workflow's GITHUB_TOKEN — no separate API key needed.
# See: https://docs.github.com/en/github-models/prototyping-with-ai-models
GITHUB_MODELS_URL = "https://models.github.ai/inference/chat/completions"
MODEL_ID = "openai/gpt-4o"

TOOL_DEFINITION = {
    "type": "function",
    "function": {
        "name": "emit_release_notes",
        "description": "Emit structured release note entries. Call this exactly once with ALL entries.",
        "parameters": {
            "type": "object",
            "properties": {
                "entries": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "category": {
                                "type": "string",
                                "enum": ["breaking", "features", "bug fixes", "deprecations"],
                            },
                            "description": {
                                "type": "string",
                                "description": "Customer-facing description of the change, written from the user's perspective.",
                            },
                            "reference": {
                                "type": "string",
                                "description": "PR link like (#1234) or commit hash in backticks like (`abc1234`).",
                            },
                        },
                        "required": ["category", "description", "reference"],
                    },
                }
            },
            "required": ["entries"],
        },
    },
}

SYSTEM_PROMPT = """\
You are writing customer-facing release notes for {repo_name}.

{readme_section}

You will receive raw git commit messages and PR descriptions. Your job is to call the emit_release_notes \
tool with structured entries that a customer would find useful.

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
- ALWAYS call the tool. Never respond with plain text."""


def run_git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args], capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


def get_previous_tag(current_tag: str) -> str | None:
    """Return the tag immediately preceding current_tag in version order."""
    tags = run_git("tag", "--sort=-v:refname").splitlines()
    try:
        idx = tags.index(current_tag)
    except ValueError:
        return tags[0] if tags else None
    if idx + 1 < len(tags):
        return tags[idx + 1]
    return None


def get_readme() -> str:
    for name in ("README.md", "README.rst", "README.txt", "README"):
        if os.path.isfile(name):
            with open(name) as f:
                return f.read(1500)
    return ""


def get_commits(range_spec: str) -> list[dict]:
    log = run_git(
        "log", range_spec,
        "--no-merges",
        "--pretty=format:%H%x00%s%x00%b%x1e",
    )
    if not log:
        return []

    commits = []
    for entry in log.split("\x1e"):
        entry = entry.strip()
        if not entry:
            continue
        parts = entry.split("\x00", 2)
        if len(parts) < 2:
            continue
        commits.append({
            "hash": parts[0][:7],
            "subject": parts[1],
            "body": parts[2] if len(parts) > 2 else "",
        })
    return commits


def get_pr_descriptions(commits: list[dict]) -> dict[str, str]:
    """Fetch PR titles + descriptions via gh CLI for commits that reference PRs."""
    pr_numbers: set[str] = set()
    for c in commits:
        for m in re.findall(r"#(\d+)", f"{c['subject']}\n{c['body']}"):
            pr_numbers.add(m)

    descriptions: dict[str, str] = {}
    for pr in pr_numbers:
        try:
            result = subprocess.run(
                ["gh", "pr", "view", pr, "--json", "title,body", "-q", '.title + "\\n" + .body'],
                capture_output=True, text=True, timeout=15,
            )
        except FileNotFoundError:
            # gh not available — skip PR body enrichment, commit messages alone
            # are still enough for the LLM to work with.
            print("gh CLI not found; skipping PR description lookup.", file=sys.stderr)
            return descriptions
        if result.returncode == 0 and result.stdout.strip():
            descriptions[pr] = result.stdout.strip()[:2000]
    return descriptions


def build_input_text(commits: list[dict], pr_descriptions: dict[str, str]) -> str:
    lines = []
    for c in commits:
        pr_match = re.search(r"#(\d+)", c["subject"])
        pr_num = pr_match.group(1) if pr_match else None

        lines.append(f"COMMIT {c['hash']}: {c['subject']}")
        if c["body"]:
            lines.append(f"  Body: {c['body'][:500]}")
        if pr_num and pr_num in pr_descriptions:
            lines.append(f"  PR #{pr_num} description: {pr_descriptions[pr_num][:1500]}")
        lines.append("")

    return "\n".join(lines)


def build_system_prompt(repo_name: str) -> str:
    readme = get_readme()
    readme_section = (
        f"Here is the README for context on what this project does:\n<readme>\n{readme}\n</readme>"
        if readme else ""
    )
    return SYSTEM_PROMPT.format(repo_name=repo_name, readme_section=readme_section)


def invoke_github_models(input_text: str, repo_name: str, token: str) -> list[dict]:
    payload = {
        "model": MODEL_ID,
        "temperature": 0.2,
        "max_tokens": 4096,
        "messages": [
            {"role": "system", "content": build_system_prompt(repo_name)},
            {"role": "user", "content": input_text},
        ],
        "tools": [TOOL_DEFINITION],
        "tool_choice": {
            "type": "function",
            "function": {"name": "emit_release_notes"},
        },
    }

    req = urllib.request.Request(
        GITHUB_MODELS_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
        method="POST",
    )

    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            body = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"GitHub Models request failed: {e.code} {e.reason}\n{detail}") from e

    choices = body.get("choices") or []
    if not choices:
        raise RuntimeError(f"No choices in response: {json.dumps(body)[:1000]}")

    message = choices[0].get("message") or {}
    tool_calls = message.get("tool_calls") or []
    for call in tool_calls:
        fn = call.get("function") or {}
        if fn.get("name") == "emit_release_notes":
            args = fn.get("arguments") or "{}"
            parsed = json.loads(args) if isinstance(args, str) else args
            return parsed.get("entries", [])

    raise RuntimeError(
        f"Model did not emit a tool call. Response message: {json.dumps(message)[:1000]}"
    )


def render_markdown(version: str, entries: list[dict], repo: str, prev_tag: str, new_tag: str) -> str:
    sections = {
        "breaking": ("Breaking Changes", []),
        "deprecations": ("Deprecations", []),
        "features": ("Features", []),
        "bug fixes": ("Bug Fixes", []),
    }

    for entry in entries:
        cat = entry.get("category")
        if cat in sections:
            sections[cat][1].append(entry)

    lines = [f"## What's Changed in {version}", ""]

    any_section = False
    for key in ("breaking", "deprecations", "features", "bug fixes"):
        title, items = sections[key]
        if not items:
            continue
        any_section = True
        lines.append(f"### {title}")
        for item in items:
            ref = f" {item['reference']}" if item.get("reference") else ""
            lines.append(f"- {item['description']}{ref}")
        lines.append("")

    if not any_section:
        lines.append("_No user-visible changes in this release._")
        lines.append("")

    if repo and prev_tag and new_tag:
        lines.append(f"**Full Changelog**: https://github.com/{repo}/compare/{prev_tag}...{new_tag}")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate release notes via GitHub Models")
    parser.add_argument("version", help="Version being released, e.g. v0.5.0")
    parser.add_argument("--since", help="Git tag to diff from (default: tag preceding <version>)")
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""),
                        help="owner/name for changelog links (default: $GITHUB_REPOSITORY)")
    parser.add_argument("--json", action="store_true", help="Emit the raw tool-call JSON")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show the input that would be sent to the LLM without calling it")
    args = parser.parse_args()

    new_tag = args.version if args.version.startswith("v") else f"v{args.version}"
    prev_tag = args.since or get_previous_tag(new_tag)

    if not prev_tag:
        print(f"No previous tag found; treating {new_tag} as the initial release.", file=sys.stderr)
        print(f"Initial release of xa11y {new_tag} ({date.today().isoformat()}).")
        return 0

    range_spec = f"{prev_tag}..HEAD" if new_tag == "HEAD" else f"{prev_tag}..{new_tag}"
    # Fall back to HEAD if the new tag doesn't exist yet (common during release workflow).
    try:
        run_git("rev-parse", "--verify", f"{new_tag}^{{commit}}")
    except subprocess.CalledProcessError:
        range_spec = f"{prev_tag}..HEAD"

    commits = get_commits(range_spec)
    print(f"Found {len(commits)} commits in {range_spec}.", file=sys.stderr)
    if not commits:
        print(f"_No changes since {prev_tag}._")
        return 0

    pr_descriptions = get_pr_descriptions(commits)
    print(f"Fetched {len(pr_descriptions)} PR descriptions.", file=sys.stderr)

    input_text = build_input_text(commits, pr_descriptions)

    if args.dry_run:
        print(input_text)
        return 0

    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        print("GITHUB_TOKEN is required to call the GitHub Models API.", file=sys.stderr)
        return 1

    repo_name = args.repo.split("/")[-1] if args.repo else "xa11y"
    entries = invoke_github_models(input_text, repo_name, token)

    if args.json:
        print(json.dumps(entries, indent=2))
    else:
        print(render_markdown(new_tag, entries, args.repo, prev_tag, new_tag))
    return 0


if __name__ == "__main__":
    sys.exit(main())
