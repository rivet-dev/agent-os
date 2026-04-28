---
name: ralph-review
description: Start a self-paced loop that monitors Ralph autonomous agent progress on the current branch, reviews each new commit with an Explore subagent, and adds real findings as new stories to scripts/ralph/todo/prd.json. Use when the user asks to monitor ralph, watch the prd, review ralph progress, or start the review loop.
---

# Ralph Review Loop

## Usage
- `/ralph-review` — start self-paced loop
- `/ralph-review 5m` — start with 5-minute interval

## What it does

1. On each tick, check `git log` on the current branch for new commits since the last reviewed commit.
2. For each new commit, spawn an Explore subagent to review it.
3. If the subagent finds real issues, add them as new stories to `scripts/ralph/todo/prd.json`.
4. Track progress in `.agent/notes/loop-prd-baseline.md` (append one line per tick).

## How to start

Invoke `/loop` with the interval (if provided) and the body below.

## Loop body

Monitor `scripts/ralph/todo/prd.json` and the git log on the current branch. On each tick:

1. Run `git log --oneline -10` and compare against the last reviewed commit in `.agent/notes/loop-prd-baseline.md`.
2. For each new commit, spawn an Explore subagent with:
   - The commit hash, story ID, parent hash, and one-sentence summary
   - Instructions to `git show` the commit and review changed files
   - The review rules below

3. If the subagent returns findings, add them as new stories to `scripts/ralph/todo/prd.json`. Place each story based on severity:
   - **CRITICAL or HIGH** → Insert immediately after the last passing story so Ralph picks it up next. Set priority to slot right after the current passing block (use a priority number that doesn't collide with existing stories).
   - **MEDIUM** → Append to the end of the backlog.
   Keep the `userStories` array sorted by ascending priority.
4. If the subagent returns NOTHING NOTABLE, skip — do not pad.
5. Append a one-line summary to `.agent/notes/loop-prd-baseline.md`.
6. **Auto-archive**: If the number of passing stories exceeds 20, archive them to `scripts/ralph/archive/passing-stories-<date>.json` and remove them from `todo/prd.json`. This keeps the active PRD lean. The archive file should include an `archivedAt` timestamp and the full story objects.

## Subagent review rules

Include these rules verbatim in every subagent prompt:

### Every finding MUST pass all four gates. Drop findings that fail any gate.

**Gate A — Exploit path:** Write one concrete sentence: "Guest calls X with Y, causing Z." If you cannot write that sentence, the finding is not real — drop it.

**Gate B — Not-sanctioned check:** Before flagging any isolation violation or kernel bypass, grep the relevant CLAUDE.md for the symbol or code path. If CLAUDE.md explicitly permits or carves out that path, DROP the finding. Cite the CLAUDE.md file and line you checked.

**Gate C — Fix-not-covered check:** If your finding is "add more tests" or "expand coverage", read the existing test file first. If existing tests already exercise the fix's intent, DROP it. Test-padding is not a bug.

**Gate D — Severity bar:**
- CRITICAL: Guest can escape isolation, read/write outside VM, or crash sidecar. Concrete exploit required.
- HIGH: Correctness bug a real npm package hits on its golden path. Name the package.
- MEDIUM: Correctness bug with a demonstrated trigger but narrow blast radius.
- Anything else: do not file. No "document the invariant", "consider tightening", "rare in practice", "add defensive validation", or dead-code-cleanup stories.

### Output format per finding:
```
SEVERITY | file:line | one-sentence bug
EXPLOIT: <concrete trigger from Gate A>
CLAUDE-CHECK: <file:line checked, or N/A>
```

### Null reports
Most commits are clean. If zero findings survive the gates, report "NOTHING NOTABLE" and stop. Do NOT pad.
