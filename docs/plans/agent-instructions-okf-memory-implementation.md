# OKF-Native Agent Instructions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the accumulated `AGENTS.md` inventory with a concise,
OKF-native operating contract without dropping a current invariant, proof
rule, or durable project memory.

**Architecture:** `AGENTS.md` becomes the stable startup contract and router.
`knowledge/testing/test-strategy.md` owns changing verification commands,
concept pages own synthesized memory, `knowledge/index.md` provides semantic
orientation, and `knowledge/log.md` remains chronology. A dependency-free
Python validator checks the stable guide contract and local links, proves its
own checks can fail, and is reused by CI.

**Tech Stack:** Markdown, Python 3 standard library, `rg`, GitHub Actions YAML.

## Global Constraints

- Work from `release/v0.6.6`; do not push or modify `main`.
- Repository work has standing consent to create isolated worktrees and task
  branches. Each task branch starts from the integration branch named by the
  active plan, and each work-item PR targets that integration branch. Only the
  final integration PR targets `main`.
- The approved design is
  `docs/plans/v0.6.6-v0.6.7-stabilization-design.md`, section
  **OKF-native agent instructions**.
- The current guide remains authoritative until its replacement and validator
  land together.
- Migrate every durable proof rule before removing it from `AGENTS.md`.
- Preserve every current non-negotiable. The escape-at-load contract remains
  binding until its separately approved atomic replacement lands.
- `AGENTS.md` contains stable instructions and routes, not changing file
  inventories, exact counts, incident narratives, or a duplicated knowledge
  catalog.
- Keep `knowledge/index.md` as the semantic catalog and `knowledge/log.md` as
  append-only chronology. Add no generated index, database, search service,
  dependency, or schema.
- Use Python's standard library only. The validator must run from any working
  directory by deriving the repository root from `__file__`.
- Every behavioral check is observed failing for its intended check id before
  it is made green.
- Run an independent reviewer after every task and before its commit. The
  reviewer must read the approved design and the files changed by that task.
- A task is complete only after its named commands pass and its proof is
  recorded in this file.

---

## File Map

- Create `scripts/check_agent_guide.py` — validate the stable guide contract,
  repository-local Markdown links, and proof-route destination; provide
  `--selftest`.
- Modify `scripts/render_check.js` — add reusable embedded-script syntax and
  negative self-test modes without launching Chromium.
- Modify `knowledge/testing/test-strategy.md` — become the authoritative,
  accurate routing page for changing verification commands and their limits.
- Modify `AGENTS.md` — compact startup contract, invariants, work loop, memory
  operations, Ponytail, proof routing, and authority boundaries.
- Create `knowledge/decisions/okf-query-ingest-lint.md` — record why the
  repository uses the LLM-wiki Query → Ingest → Lint model and how the index
  and log differ.
- Modify `knowledge/index.md` — add the new decision to the semantic catalog.
- Modify `knowledge/log.md` — record the decision/ingest and later CI gate.
- Modify `.github/workflows/ci.yml` — call the same committed validator used
  locally.
- Modify `docs/plans/v0.6.6-presentation-layer-rationalization.md` — record
  task proof and close the pre-step only after final verification.

## Validator Interface

`scripts/check_agent_guide.py` exposes these repository-local interfaces:

```python
@dataclass(frozen=True)
class Problem:
    check: str
    detail: str


def validate(root: pathlib.Path, guide: pathlib.Path) -> list[Problem]:
    """Return every contract, proof-route, or local-link problem; empty means valid."""


def selftest() -> int:
    """Observe each supported check id fail for its intended reason."""


def main(argv: list[str] | None = None) -> int:
    """Run --selftest or validate the repository guide."""
```

Normal success prints:

```text
agent guide OK — stable contracts present; local links resolve
```

Failures are one per line and identify the responsible check:

```text
[local-link] AGENTS.md: knowledge/missing.md does not exist
[contract:memory] AGENTS.md: missing '## Memory: Query → Ingest → Lint'
```

The stable contract ids are:

```python
REQUIRED = {
    "contract:start": ("## Start here", "docs/plans/", "knowledge/index.md"),
    "contract:invariants": (
        "## Invariants",
        "Fail closed",
        "Escape once",
        "Zero upstream rate violations",
        "The wire format does not move",
        "Data is never localized",
        "Identifiers stay frozen",
    ),
    "contract:work": (
        "## Work loop",
        "Plan → Act → Verify",
        "Scope deltas",
        "red → green",
        "independent review",
    ),
    "contract:repository": (
        "## Repository operations",
        "isolated worktree",
        "integration branch named by the active plan",
        "work-item PR",
        "Only the final integration PR targets `main`",
    ),
    "contract:memory": (
        "## Memory: Query → Ingest → Lint",
        "Query",
        "Ingest",
        "Lint",
        "knowledge/index.md",
        "knowledge/log.md",
        "knowledge/decisions/okf-query-ingest-lint.md",
        "git log --",
    ),
    "contract:proof-route": (
        "## Verification and review",
        "knowledge/testing/test-strategy.md",
    ),
    "contract:ponytail": (
        "## Ponytail",
        "Does this need to exist?",
        "already in the repository",
        "standard library",
        "native platform",
        "installed dependency",
        "one line",
        "minimum",
    ),
    "contract:authority": (
        "## Authority boundaries",
        "Plans:",
        "Code/tests:",
        "Knowledge:",
        "GitHub:",
        "External systems:",
    ),
}
```

These markers intentionally describe stable responsibilities, not command
inventories. Changing one requires an explicit guide-contract decision and a
matching self-test update.

---

### Task 1: Self-testing agent-guide validator

**Files:**

- Create: `scripts/check_agent_guide.py`
- Reference: `scripts/locale_v1.py`
- Reference: `scripts/check_i18n.py`

**Interfaces:**

- Consumes: the repository root, `AGENTS.md`, and
  `knowledge/testing/test-strategy.md`.
- Produces: `validate(root, guide) -> list[Problem]`,
  `selftest() -> int`, and the two CLI modes
  `python3 scripts/check_agent_guide.py` and
  `python3 scripts/check_agent_guide.py --selftest`.

- [x] **Step 1: Write the validator self-test before its checks**

Create the script with the `Problem` type, `REQUIRED` mapping, CLI parsing, and
a temporary valid bundle. The first version deliberately leaves
`validate()` returning `[]`, while `selftest()` applies these mutations and
asserts the named check:

```python
CASES = {
    "missing-local-link": (
        "local-link", "AGENTS.md", "knowledge/index.md", "knowledge/missing.md"
    ),
    "missing-start": ("contract:start", "AGENTS.md", "## Start here", "## Begin"),
    "missing-invariant": (
        "contract:invariants", "AGENTS.md", "Escape once", "Escape differently"
    ),
    "missing-memory": (
        "contract:memory",
        "AGENTS.md",
        "## Memory: Query → Ingest → Lint",
        "## Memory",
    ),
    "missing-work": (
        "contract:work",
        "AGENTS.md",
        "independent review",
        "self review",
    ),
    "missing-repository": (
        "contract:repository",
        "AGENTS.md",
        "## Repository operations",
        "## Git operations",
    ),
    "missing-proof-route": (
        "contract:proof-route",
        "AGENTS.md",
        "knowledge/testing/test-strategy.md",
        "knowledge/testing/missing.md",
    ),
    "missing-proof-section": (
        "proof-route",
        "knowledge/testing/test-strategy.md",
        "## Proof routing",
        "## Checks",
    ),
    "missing-ponytail": (
        "contract:ponytail", "AGENTS.md", "## Ponytail", "## Minimality"
    ),
    "missing-authority": (
        "contract:authority",
        "AGENTS.md",
        "## Authority boundaries",
        "## Ownership",
    ),
}
```

Construct the valid bundle under `tempfile.TemporaryDirectory()` with:

```python
for relative in (
    "docs/plans/current.md",
    "knowledge/index.md",
    "knowledge/log.md",
    "knowledge/decisions/okf-query-ingest-lint.md",
    "knowledge/testing/test-strategy.md",
):
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("# Fixture\n", encoding="utf-8")
```

The valid guide contains every `REQUIRED` marker and Markdown links to all five
fixture paths. After creating the files, write `## Proof routing\n` to the
fixture test-strategy page. For
each case, restore every fixture from a pristine in-memory mapping, rewrite the
named file's token, call `validate()`, and compare the returned
`problem.check` values with the expected id. No mutation carries into the next
case.

- [x] **Step 2: Run the self-test and observe the intended red result**

Run:

```sh
python3 scripts/check_agent_guide.py --selftest
```

Expected: exit 1 with lines such as:

```text
missing-local-link: expected check 'local-link', got nothing
missing-memory: expected check 'contract:memory', got nothing
```

The assertion is on check ids, not fragments of diagnostic prose.

- [x] **Step 3: Implement the minimum validator**

Implement contract and local-link validation with the standard library:

```python
LINK = re.compile(r"(?<!!)\[[^]]+\]\(([^)]+)\)")


def contract_problems(text: str) -> list[Problem]:
    problems = []
    for check, markers in REQUIRED.items():
        for marker in markers:
            if marker not in text:
                problems.append(Problem(check, f"AGENTS.md: missing {marker!r}"))
                break
    return problems


def local_link_problems(root: pathlib.Path, guide: pathlib.Path, text: str) -> list[Problem]:
    problems = []
    for raw in LINK.findall(text):
        target = raw.strip().strip("<>").split("#", 1)[0]
        if not target or urllib.parse.urlparse(target).scheme:
            continue
        resolved = (guide.parent / urllib.parse.unquote(target)).resolve()
        try:
            resolved.relative_to(root.resolve())
        except ValueError:
            problems.append(Problem("local-link", f"AGENTS.md: {target} leaves repository"))
            continue
        if not resolved.exists():
            problems.append(Problem("local-link", f"AGENTS.md: {target} does not exist"))
    return problems


def proof_route_problems(root: pathlib.Path) -> list[Problem]:
    strategy = root / "knowledge/testing/test-strategy.md"
    if not strategy.exists():
        return [Problem("proof-route", f"{strategy}: does not exist")]
    text = strategy.read_text(encoding="utf-8")
    if "## Proof routing" not in text:
        return [Problem("proof-route", f"{strategy}: missing '## Proof routing'")]
    return []


def validate(root: pathlib.Path, guide: pathlib.Path) -> list[Problem]:
    text = guide.read_text(encoding="utf-8")
    return (
        contract_problems(text)
        + proof_route_problems(root)
        + local_link_problems(root, guide, text)
    )
```

`main()` derives `root = pathlib.Path(__file__).resolve().parent.parent`, runs
the requested mode, prints each `[check] detail`, and returns 1 on problems.

- [x] **Step 4: Run red/green and syntax checks**

Run:

```sh
python3 scripts/check_agent_guide.py --selftest
python3 -m py_compile scripts/check_agent_guide.py
python3 scripts/check_agent_guide.py
```

Expected:

- self-test: exit 0, with every case reporting `trips <check id>`;
- compile: exit 0;
- normal mode: exit 1 on the old guide, including
  `[contract:memory]`, proving the checker can reject the pre-rewrite state.

- [x] **Step 5: Independent review**

Give the reviewer the approved design, this task, and
`scripts/check_agent_guide.py`. Require it to check path traversal handling,
URL/anchor exclusions, self-test specificity, working-directory independence,
stdlib-only imports, and whether every `REQUIRED` marker is stable rather than
an inventory.

- [x] **Step 6: Commit the green self-test**

```sh
git add scripts/check_agent_guide.py
git commit -m "checks: guard the agent instruction contract"
```

Record the self-test and expected normal-mode failure beneath Task 1 before
marking it complete.

**Proof record (2026-07-30):**

- RED: `python3 scripts/check_agent_guide.py --selftest` exited 1 while
  `validate()` returned no problems; all ten named fixtures reported their
  expected check id as missing.
- GREEN: `python3 scripts/check_agent_guide.py --selftest` exited 0 and
  reported every one of the ten fixtures as `trips <check id>`; its assertion
  requires the exact singleton check-id set for each mutation.
- Syntax: `python3 -m py_compile scripts/check_agent_guide.py` exited 0.
- Expected pre-rewrite normal-mode failure: `python3 scripts/check_agent_guide.py`
  exited 1 and included `[contract:memory] AGENTS.md: missing
  '## Memory: Query → Ingest → Lint'` (the existing guide remains intentionally
  pre-rewrite for Task 1).
- Independent review: initial review found one Important self-test-isolation
  defect; the fix was scoped re-reviewed and approved with spec compliance PASS
  and task quality APPROVED, with no Critical or Important findings.

---

### Task 2: Reusable page-syntax proof and lossless verification migration

**Files:**

- Modify: `scripts/render_check.js`
- Modify: `knowledge/testing/test-strategy.md`
- Reference: `AGENTS.md`
- Reference: `.github/workflows/ci.yml`
- Test: `scripts/check_agent_guide.py`

**Interfaces:**

- Consumes: every row in the current `AGENTS.md` **What proves what** section,
  the actual commands in `.github/workflows/ci.yml`, and known limitations
  already recorded in the approved stabilization plan.
- Produces: one authoritative `## Proof routing` section that the slim guide
  can link without reproducing changing commands, plus
  `node scripts/render_check.js --syntax-selftest` and
  `node scripts/render_check.js --syntax-only`.

- [x] **Step 1: Capture the baseline before editing**

Run:

```sh
sed -n '/^## What proves what/,/^## Traps/p' AGENTS.md
sed -n '1,130p' .github/workflows/ci.yml
sed -n '1,220p' knowledge/testing/test-strategy.md
```

Save the command output in the task handoff to the reviewer. Do not copy it
into another repository file.

- [x] **Step 2: Write the failing embedded-script syntax self-test**

Add `node:vm` and syntax-mode argument handling to `scripts/render_check.js`.
Before implementing extraction, make `syntaxProblems()` return `[]` and add
these self-test cases:

```javascript
const SYNTAX_CASES = [
  { name: 'valid', html: '<script>const ok = 1;</script>', want: null },
  { name: 'invalid', html: '<script>const = ;</script>', want: 'syntax' },
  { name: 'missing', html: '<main></main>', want: 'script-block' },
  {
    name: 'multiple',
    html: '<script>const a = 1;</script><script>const b = 2;</script>',
    want: 'script-block',
  },
];
```

`syntaxSelftest()` asserts the exact returned check id. `--syntax-selftest`
runs it and exits before browser discovery or fixture loading.

- [x] **Step 3: Observe the syntax self-test fail**

Run:

```sh
node scripts/render_check.js --syntax-selftest
```

Expected: exit 1 with:

```text
invalid: expected check syntax, got nothing
missing: expected check script-block, got nothing
```

- [x] **Step 4: Implement the reusable syntax mode**

Implement the standard-library parser:

```javascript
const vm = require('vm');

function syntaxProblems(html, filename) {
  const blocks = [...html.matchAll(/<script>(?:\r?\n)?([\s\S]*?)<\/script>/g)];
  if (blocks.length !== 1) {
    return [{
      check: 'script-block',
      detail: `${filename}: expected one plain <script>, found ${blocks.length}`,
    }];
  }
  try {
    new vm.Script(blocks[0][1], { filename });
    return [];
  } catch (err) {
    return [{ check: 'syntax', detail: `${filename}: ${err.message}` }];
  }
}
```

`--syntax-only` reads `src/dashboard.html` and `src/setup.html`, reports each
`[check] detail`, and exits nonzero on any problem. It exits before Chrome
lookup. Success prints:

```text
embedded page syntax OK — dashboard and setup parse
```

Run:

```sh
node scripts/render_check.js --syntax-selftest
node scripts/render_check.js --syntax-only
```

Expected: both exit 0; the self-test names each observed check id.

- [x] **Step 5: Add an authoritative proof-routing section**

Add `## Proof routing` near the top of
`knowledge/testing/test-strategy.md`. It must contain these accurate routes:

- Rust logic: `cargo test` and
  `cargo clippy --all-targets -- -D warnings`.
- Handler or wire-type changes:
  `UPDATE_OPENAPI=1 cargo test --test openapi`, then verify
  `openapi.json` has only the deliberate diff.
- Embedded pages:
  `node scripts/render_check.js --syntax-selftest` proves the syntax gate can
  reject its fixtures, and `node scripts/render_check.js --syntax-only` parses
  the real dashboard and setup scripts without launching Chromium. This proves
  parsing only. Behavior uses
  `node scripts/render_check.js`,
  `node scripts/render_check.js --escape-probe`,
  `node scripts/render_check.js --page setup`, and
  `node scripts/render_check.js --page setup --escape-probe`.
- Number/date/duration formatting:
  `TZ=UTC LC_ALL=en_US.UTF-8 node scripts/formatter_fixture.js --check`.
- Catalog and UI text:
  `python3 scripts/check_i18n.py --selftest`,
  `python3 scripts/check_i18n.py`, and
  `python3 scripts/gen_pseudolocale.py --check` when English changes.
- Locale files:
  `python3 scripts/locale_v1.py --selftest` and
  `python3 scripts/locale_v1.py --all`.
- Pacing, pool, dispatch, and affinity: the enforcing mock plus load harness;
  one upstream violation is failure. Preserve the setup prerequisites already
  documented in the load section.
- Layout: mechanical overflow probes where available plus explicit human
  review under rendered data and supported widths. Behavior passing does not
  prove fit.
- Before push: `cargo fmt --check`.

State the boundaries truthfully:

- CI runs automated checks configured in `.github/workflows/ci.yml`; it does
  not perform human layout review or the strict load scenario.
- `cargo test` does not execute embedded-page JavaScript.
- syntax-only mode proves parsing, not behavior.
- `render_check.js` proves only its covered fixtures and interactions; it is
  not evidence that every page path, locale, or layout is covered.
- A relevant missing reusable proof is a work item. A scratch reproduction may
  demonstrate a problem but does not become the regression gate.

- [x] **Step 6: Prove the migration is structurally available**

Run:

```sh
rg -n '^## Proof routing|cargo test|UPDATE_OPENAPI|syntax-selftest|syntax-only|render_check|formatter_fixture|check_i18n|locale_v1|gen_pseudolocale|mock_nim|loadtest|cargo fmt' knowledge/testing/test-strategy.md
node scripts/render_check.js --syntax-selftest
node scripts/render_check.js --syntax-only
git diff --check
python3 scripts/check_agent_guide.py
```

Expected:

- `rg` prints every required proof family from the destination page;
- both embedded-script syntax modes exit 0;
- `git diff --check` exits 0;
- the validator still exits 1 because `AGENTS.md` has not been rewritten, but
  no failure may claim the proof-route destination is absent.

- [x] **Step 7: Independent losslessness review**

Give the reviewer the baseline output from Step 1, the changed test-strategy
page, current CI, `scripts/render_check.js`, and the approved design. Require a
one-to-one accounting of every displaced proof rule, confirmation that the
syntax self-test observes both failure ids, and an explicit check that the CI
claims match the workflow. Any missing or overstated rule blocks the commit.

- [x] **Step 8: Commit the reusable proof and destination**

```sh
git add scripts/render_check.js knowledge/testing/test-strategy.md
git commit -m "checks: centralize embedded-page proof routing"
```

Record the structural command and reviewer result beneath Task 2 before
marking it complete.

**Proof record (2026-07-30):**

- Baseline: captured the old guide proof matrix, CI's first 130 lines, and the
  test-strategy opening before editing; the Task 2 handoff preserves the
  commands and migration accounting.
- RED: `node scripts/render_check.js --syntax-selftest` exited 1 while
  `syntaxProblems()` returned no problems. The intended fixtures reported
  `invalid: expected check syntax, got nothing`, `missing: expected check
  script-block, got nothing`, and `multiple: expected check script-block, got
  nothing`.
- GREEN: `node scripts/render_check.js --syntax-selftest` exited 0 and
  observed exact check ids `syntax` for invalid and `script-block` for missing
  and multiple; `node scripts/render_check.js --syntax-only` exited 0 and
  printed `embedded page syntax OK — dashboard and setup parse`.
- Structural: the required `rg` command located every proof family,
  `git diff --check` exited 0, and `python3 scripts/check_agent_guide.py`
  exited 1 only for the intentionally unreplaced AGENTS contract headings;
  it did not report `[proof-route]`.
- Independent review: approved with no Critical, Important, or Minor
  findings. The reviewer confirmed one-to-one proof accounting, exact syntax
  self-test ids, early syntax-mode exits before Chrome/fixtures, and CI-claim
  accuracy against `.github/workflows/ci.yml`.

---

### Task 3: OKF-native guide and durable memory ingest

**Files:**

- Modify: `AGENTS.md`
- Create: `knowledge/decisions/okf-query-ingest-lint.md`
- Modify: `knowledge/index.md`
- Modify: `knowledge/log.md`
- Test: `scripts/check_agent_guide.py`

**Interfaces:**

- Consumes: the approved design, the Task 2 proof-routing page, the current
  non-negotiables, and the repository's existing OKF frontmatter/ADR shape.
- Produces: a validator-clean startup contract and a durable decision page
  explaining the memory model.

- [x] **Step 1: Observe the guide-contract failure before rewriting**

Run:

```sh
python3 scripts/check_agent_guide.py
```

Expected: exit 1 with the missing stable contract ids, including
`[contract:memory]`, and no `[proof-route]` after Task 2. Save the exact ids in
the Task 3 proof record.

- [x] **Step 2: Add the durable OKF decision before removing its schema**

Create `knowledge/decisions/okf-query-ingest-lint.md` with this frontmatter:

```yaml
---
type: Decision
title: Query, ingest, and lint repository memory
description: Keep AGENTS.md as a stable router while concept pages compound verified project knowledge.
tags: [knowledge, okf, agent-instructions]
timestamp: 2026-07-30T00:00:00Z
resource: https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf
---
```

Write an ADR with:

- **Context:** accumulated operational lessons turned `AGENTS.md` into a
  volatile catalog; the long log is greppable chronology but weak synthesis.
- **Options:** keep the large guide; generate a catalog/search layer; use the
  OKF/LLM-wiki router and concept graph.
- **Choice:** stable guide plus Query → Ingest → Lint, semantic index,
  append-only log, relative-link graph, and `rg`/Git history.
- **Schema contract:** one durable concept per Markdown file; filenames are
  kebab-case; the file path minus `.md` is its identity; parseable YAML
  frontmatter has a non-empty `type`; this repository uses Decision, Research
  Finding, Component, and Runbook for concept pages; optional `title`,
  `description`, `resource`, `tags`, and `timestamp` remain available;
  ordinary relative Markdown links form the graph; decisions use Context →
  Options → Choice → Consequences.
- **Consequences:** agents must search before reasoning, promote durable
  findings out of chronology, maintain cross-links, and lint drift; no
  generator or retrieval service exists.
- **References:** the OKF resource above and Karpathy's LLM Wiki gist at
  `https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f`.

Add the decision to the Decisions section of `knowledge/index.md`. Add the
following entry as the newest entry at the top of `knowledge/log.md` without
rewriting prior entries:

```markdown
## [2026-07-30] decision — make agent instructions an OKF memory router
```

Follow it with the decision, migration boundary, and validator evidence. Do not
copy the entire guide into the log. The schema now has a durable destination
before the next step removes its detailed copy from `AGENTS.md`.

- [x] **Step 3: Replace `AGENTS.md` with the stable router**

Use this exact top-level structure:

```markdown
# Agent guide for nim-proxy

## Start here
## Invariants
## Repository operations
## Work loop
## Memory: Query → Ingest → Lint
## Verification and review
## Ponytail
## Authority boundaries
```

Required content by section:

**Start here**

- One compact paragraph defining nim-proxy.
- Read the applicable `docs/plans/` file completely before non-trivial work.
- Read `knowledge/index.md`, then search concepts and synonyms with:

  `rg -n -i`, supplying concrete domain terms and known synonyms across
  `knowledge/` and `docs/plans/`.

- Read the relevant concept pages completely. Search `knowledge/log.md` with
  concrete concept names, historical vocabulary, and `gotcha` when chronology
  matters.

- Follow relative links; when evolution or provenance matters, run
  `git log --` with the concept page path. Read source files only after the
  governing plan and memory.

**Invariants**

- Preserve the six existing non-negotiables without weakening them.
- Clarify that fail-closed durable-store startup applies to the config/auth
  store; history recovery follows its own documented format policy.
- Keep **Escape once** explicitly tied to the current escape-at-load decision
  and state that only the approved atomic replacement may supersede it.
- Retain the explicit invariant **The wire format does not move**. Define
  protected machine contracts as API bodies/status/content types,
  OpenAPI, config/history formats, metric names and label values, and stable
  identifiers. Intentional breaking changes require a decision, tests, and
  release note.
- Clarify that identifier freezing applies to label-only work; deliberate
  contract rationalization is separate work.

**Repository operations**

- Treat worktree and task-branch creation as standing repository
  authorization; do not ask the owner for repeated consent.
- Resolve the current integration branch from the active plan rather than
  hard-coding a release name.
- Create every independently reviewable work item in an isolated worktree on
  a task branch based on that integration branch. Prefer native worktree
  support; otherwise use Git's worktree support and verify the project-local
  worktree directory is ignored before creation.
- Open each work-item PR against the integration branch. Keep `main`
  untouched; only the final integration PR targets `main`.
- Preserve unrelated changes. Remove a worktree only after its branch is
  safely integrated and retain the branch/PR evidence required by the plan.

**Work loop**

- Plans are live artifacts and are updated whenever work is discovered.
- Scope deltas are recorded and shown to the owner before implementation.
- Every edit uses visible Outcome, Proof, Constraint, and Ponytail Rung,
  followed by Act and the named proof.
- Behavioral work uses a committed **red → green** regression check; docs use
  structural validation and review; layout additionally requires human
  inspection.
- Results, counts, and percentages are reported only from fresh output.
- Bugs outside scope use the configured GitHub bug form fields; if GitHub is
  unavailable, record the pending issue in the plan.
- Authoring may be inline or delegated by surface area. Every substantive work
  item receives an independent review before commit. If no independent agent
  exists, record that limitation and obtain a fresh-context or owner review;
  never claim independence that did not occur.

**Memory: Query → Ingest → Lint**

- Query: plan → index → `rg` terms/synonyms → full concept pages → relevant
  log entries → links/file history.
- Ingest: promote only reproduced, measured, authoritatively referenced, or
  approved knowledge into one concept page. Use the existing types Decision,
  Research Finding, Component, and Runbook. Update affected pages with the
  behavior they explain.
- Update `knowledge/index.md` when adding, moving, or retiring a concept;
  append `knowledge/log.md` for every ingest. Never leave a durable fact only
  in the log.
- Lint: search for contradictions, stale claims, duplicate/orphan concepts,
  missing cross-links, broken local links, and log-only facts. A code/knowledge
  mismatch is investigated; code is evidence of behavior and knowledge is
  approved intent/rationale.
- `knowledge/log.md` is chronology, not the primary retrieval surface.
- The durable schema is linked from
  `knowledge/decisions/okf-query-ingest-lint.md`; do not duplicate it in the
  guide.

**Verification and review**

- Route every changing command and coverage statement to
  `knowledge/testing/test-strategy.md`.
- Name the exact proof before editing and run that exact proof afterward.
- Report skipped checks. A green first run requires a demonstrated failure
  path; self-tests assert which check fired.
- Before any push, run the relevant checks plus `cargo fmt --check`.

**Ponytail**

Retain the seven-rung ladder without examples that will decay:

1. Does this need to exist?
2. Is it already in the repository?
3. Does the standard library do it?
4. Does the native platform do it?
5. Does an installed dependency do it?
6. Is it one line?
7. Only then add the minimum new machinery.

**Authority boundaries**

- Plans: current scope, state, decisions in flight, committed proof.
- Code/tests: current executable behavior and regression evidence.
- Knowledge: approved understanding, constraints, rationale, and runbooks.
- GitHub: issue/PR/release execution state; mirror plan status without
  replacing the plan.
- External systems: read-only unless the owner authorizes a write.

- [x] **Step 4: Run the committed contract proof**

Run:

```sh
python3 scripts/check_agent_guide.py --selftest
python3 scripts/check_agent_guide.py
git diff --check
```

Expected: all commands exit 0. Normal mode prints:

```text
agent guide OK — stable contracts present; local links resolve
```

- [x] **Step 5: Run an adversarial semantic review**

Give the reviewer:

- `git show b025602:AGENTS.md` as the pre-rewrite baseline;
- the new `AGENTS.md`;
- `knowledge/testing/test-strategy.md`;
- the new OKF decision and changed index/log;
- the approved design.

Require explicit findings on:

- every old non-negotiable preserved or precisely clarified;
- every old proof rule present in test strategy;
- the prior OKF schema and ADR shape preserved in the new decision page;
- no volatile inventories, exact helper counts, CI overclaims, or incident
  narratives remain in the guide;
- Query → Ingest → Lint is actionable from a fresh session;
- index and log roles match OKF;
- code/knowledge mismatch wording cannot legitimize an accidental code bug.

- [x] **Step 6: Commit the guide and memory decision**

```sh
git add AGENTS.md knowledge/decisions/okf-query-ingest-lint.md \
  knowledge/index.md knowledge/log.md
git commit -m "docs: make agent guidance an OKF memory router"
```

Record the red contract ids, green commands, and reviewer result beneath Task 3
before marking it complete.

**Proof record (2026-07-30):**

- RED: `python3 scripts/check_agent_guide.py` exited 1 on exactly
  `contract:start`, `contract:invariants`, `contract:work`,
  `contract:repository`, `contract:memory`, `contract:proof-route`,
  `contract:ponytail`, and `contract:authority`. It did not report
  `proof-route`, so Task 2's destination was present before the guide rewrite.
- Ordering: `knowledge/decisions/okf-query-ingest-lint.md` existed and
  `knowledge/index.md` plus `knowledge/log.md` contained the ingest while
  `git diff --quiet -- AGENTS.md` still confirmed the guide was unchanged from
  the pinned baseline. The three-file migration diff passed
  `git diff --check` before the guide schema was removed.
- GREEN: `python3 scripts/check_agent_guide.py --selftest` exited 0 after all
  10 fixtures tripped their exact check ids; `python3
  scripts/check_agent_guide.py` exited 0 and printed `agent guide OK — stable
  contracts present; local links resolve`; `git diff --check` exited 0.
- Self-review: the guide has exactly the required eight `##` sections, all six
  invariant labels and binding behavior remain, the new ADR contains the
  complete former schema contract, Task 2's page contains every displaced
  proof family, and a volatile-content scan of the guide returned no matches.
- Independent review: spec compliance PASS and semantic quality APPROVED, with
  no Critical, Important, or Minor findings. The reviewer explicitly accounted
  for the six invariants, proof migration, schema/ADR preservation, stable
  guide content, fresh-session Query → Ingest → Lint flow, index/log roles,
  code/knowledge mismatch safety, repository operations, and ingest-before-cutover
  ordering.

---

### Task 4: CI reuse and pre-step closure

**Files:**

- Modify: `.github/workflows/ci.yml`
- Modify: `knowledge/testing/test-strategy.md`
- Modify: `knowledge/log.md`
- Modify: `docs/plans/v0.6.6-presentation-layer-rationalization.md`
- Modify: this implementation plan (Task 4 proof/status and task-branch
  expectation)
- Test: `scripts/check_agent_guide.py`

**Interfaces:**

- Consumes: the green local validator from Task 3.
- Produces: one CI step that runs the same self-test and normal validation;
  accurate testing memory; a closed, evidence-backed pre-step.

- [x] **Step 1: Add the CI gate**

After checkout in the `check` job, add:

```yaml
      - name: Agent guide contract
        run: |
          python3 scripts/check_agent_guide.py --selftest
          python3 scripts/check_agent_guide.py
```

Do not duplicate validator logic in workflow YAML and do not add an Action or
dependency.

Replace the existing inline **Embedded page JS syntax** extraction block with:

```yaml
      - name: Embedded page JS syntax
        run: |
          node scripts/render_check.js --syntax-selftest
          node scripts/render_check.js --syntax-only
```

CI and the proof router now call the same committed syntax check.

- [x] **Step 2: Reconcile testing memory and chronology**

Update `knowledge/testing/test-strategy.md` to state that PR CI calls both
agent-guide modes. Add the following entry as the newest entry at the top of
`knowledge/log.md` without rewriting prior entries:

```markdown
## [2026-07-30] lint — enforce the agent-guide memory contract in CI
```

Name the two commands and what they reject.

- [x] **Step 3: Run final local non-Rust verification**

Run fresh:

```sh
python3 scripts/check_agent_guide.py --selftest
python3 scripts/check_agent_guide.py
python3 -m py_compile scripts/check_agent_guide.py
node scripts/render_check.js --syntax-selftest
node scripts/render_check.js --syntax-only
git diff --check
```

Expected: every listed command exits 0. Task 4 changes only agent guidance,
documentation, validator/syntax scripts, and CI wiring; by owner ruling, Rust
build, test, and formatting commands are not Task 4 proof. Record each command
and actual result in the pre-step section of
`docs/plans/v0.6.6-presentation-layer-rationalization.md`, then change its
status from implementation pending to complete.

- [x] **Step 4: Independent workflow and completion review**

Give the reviewer the complete `b025602..HEAD`
documentation/check/workflow diff, the approved design, and the recorded
commands. Require it to verify:

- CI calls the committed script rather than inline logic;
- no new permissions, Actions, dependencies, or network calls were added;
- the testing page describes actual CI;
- the log is chronology rather than duplicated guide content;
- the plan is marked complete only after fresh proof;
- no unrelated file changed.

- [x] **Step 5: Commit the CI gate and closure**

```sh
git add .github/workflows/ci.yml knowledge/testing/test-strategy.md \
  knowledge/log.md docs/plans/v0.6.6-presentation-layer-rationalization.md
git commit -m "ci: enforce the agent guide contract"
```

- [x] **Step 6: Verify the committed state**

Run:

```sh
git show --check --stat --oneline HEAD
git status --short --branch
python3 scripts/check_agent_guide.py --selftest
python3 scripts/check_agent_guide.py
node scripts/render_check.js --syntax-selftest
node scripts/render_check.js --syntax-only
```

Expected:

- the commit check reports no whitespace errors;
- the working tree is clean on the isolated task branch based on
  `release/v0.6.6`;
- both validator modes and both embedded-page syntax modes exit 0.

Task 4's documented scope excludes Rust build, test, and formatting proof by
owner ruling; no `cargo` command is an acceptance requirement for this task.

Do not push. Report the commit ids, exact verification output, any skipped
external CI check, and the next approved v0.6.6 work item.

**Proof record (2026-07-30):**

- Steps 1–2: CI calls `scripts/check_agent_guide.py` in self-test and normal
  modes immediately after checkout. Its embedded-page syntax step now calls
  the committed `render_check.js` self-test and syntax-only modes; the workflow
  adds no Action, permission, dependency, or network operation.
- Step 3: `python3 scripts/check_agent_guide.py --selftest`, `python3
  scripts/check_agent_guide.py`, `python3 -m py_compile
  scripts/check_agent_guide.py`, `node scripts/render_check.js
  --syntax-selftest`, `node scripts/render_check.js --syntax-only`, and `git
  diff --check` each exited 0. Their complete output is recorded in the
  v0.6.6 pre-step above.
- Scope amendment: the owner ruled that Task 4 changes no Rust/Cargo source,
  so Rust build, test, and formatting commands are not acceptance proof. The
  prior host/container formatter diagnostics are historical environment facts,
  not a requirement used to close this task.
- Step 4: independent review of the prospective `b025602..HEAD` plus
  uncommitted Task 4 diff returned APPROVE with 0 Critical, 0 Important, and 0
  Minor findings. It independently re-ran the non-formatter commands and
  `git diff --check`, confirmed the read-only formatter substitute, and found
  only the approved workflow, testing-memory, chronology, proof/status, and
  task-branch-expectation scope.
- Steps 5–6: implementation commit `8296117 ci: enforce the agent guide
  contract` passed
  `git show --check --stat --oneline HEAD` with no whitespace errors. `git
  status --short --branch` printed only `## work/v0.6.6-okf-agent-guide`.
  Both agent-guide and embedded-page syntax modes exited 0 with the same green
  output recorded above. No Rust/Cargo command was required for Task 4.

**Final-review fix wave (2026-07-30):** Correct the Task 4 log placement and
make every Task 3/Task 4 chronology instruction explicit: add new entries at
the top without rewriting prior entries, preserving reverse-chronological
order. The pre-fix date parser rejected the trailing `2026-07-30` entry and
the placement assertion rejected Task 3 as the first dated heading; after the
fix, both focused assertions plus the guide self-test, normal validator, and
`git diff --check` passed. Independent review found no findings.

---

## Delegation-contract amendment — 2026-07-30

- **Outcome:** make delegated work a complete management contract so ambiguous
  context, oversized tasks, and missing exhaustion instructions are corrected
  at the delegating agent rather than converted into repeated reminders or
  worker blame.
- **Proof:** committed RED `1a4f38b` adds an eleventh independent validator
  fixture and makes normal validation fail on the unchanged guide with
  `[contract:work] AGENTS.md: missing 'Delegation is a management boundary'`.
- **Constraint:** keep `AGENTS.md` succinct and stable. Put full rationale in
  the existing OKF agent-instruction decision; add no new concept, schema,
  dependency, generated index, or agent-management machinery.
- **Ponytail Rung:** 2. Extend the existing guide contract, validator, OKF
  decision, semantic index path, and chronology.
- [x] **Act:** add one compact guide paragraph; define the complete handoff and
  exhaustion boundary in `okf-query-ingest-lint.md`; add a newest-first log
  entry; reconcile Task 9's merged execution state.
- [x] **Verify:** both agent-guide modes, Python syntax, Cargo formatting, and
  structural diff are green. Independent semantic review returned APPROVE with
  no findings after checking every changed file and rerunning both guide modes
  plus `git diff --check`.
