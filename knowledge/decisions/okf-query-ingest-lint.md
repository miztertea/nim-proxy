---
type: Decision
title: Query, ingest, and lint repository memory
description: Keep AGENTS.md as a stable router while concept pages compound verified project knowledge.
tags: [knowledge, okf, agent-instructions]
timestamp: 2026-07-30T00:00:00Z
resource: https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf
---

# Query, ingest, and lint repository memory

## Context

Accumulated operational lessons turned [`AGENTS.md`](../../AGENTS.md) from a
startup contract into a volatile catalog of files, checks, incident narratives,
and implementation details. The long [`knowledge/log.md`](../log.md) remains
useful as greppable chronology, but chronology is a weak synthesis and retrieval
surface: a durable fact can be stranded inside an old entry instead of living
with the concept it explains.

The repository already stores its durable memory as an Open Knowledge Format
bundle. The problem is therefore not a missing documentation system, but the
lack of an explicit operating model that keeps the guide stable, the semantic
index useful, the concept graph current, and chronology subordinate to
synthesized knowledge.

## Options

1. **Keep the large guide.** All instructions remain immediately visible, but
   every changing inventory, command, count, and lesson makes the startup
   contract longer and more volatile.
2. **Generate a catalog or search layer.** A generated index, database, or
   retrieval service could provide richer lookup, but adds machinery and a
   second maintenance path for a repository that is already searchable as
   text.
3. **Use the OKF/LLM-wiki router and concept graph.** Keep the guide as a stable
   router and operate repository memory through Query → Ingest → Lint.

## Choice

Choose option 3. [`AGENTS.md`](../../AGENTS.md) holds the stable startup
contract and routes agents into the active plan, the semantic
[`knowledge/index.md`](../index.md), and the concept graph.
[`knowledge/log.md`](../log.md) remains append-only chronology rather than the
primary retrieval surface. Ordinary relative Markdown links connect concepts;
`rg` supplies repository search, and Git file history supplies provenance and
evolution when the current page is not enough.

The operating loop is:

1. **Query** — read the applicable plan, orient through the semantic index,
   search concrete concepts and synonyms, read relevant concept pages
   completely, inspect relevant chronology, then follow links and file history.
2. **Ingest** — promote reproduced, measured, authoritatively referenced, or
   approved knowledge into the concept page that owns it. Update the semantic
   index for concept lifecycle changes and add every new chronology entry as
   the newest entry at the top. Append-only means existing log entries are
   never rewritten; because the file is reverse chronological, new entries are
   inserted at the top.
3. **Lint** — search for contradictions, stale claims, duplicates, orphans,
   missing cross-links, broken local links, and durable facts left only in the
   log. A code/knowledge mismatch is investigated rather than silently resolved
   in either direction.

### Delegation contract

Delegation is a management boundary, not an accountability transfer. The
delegating agent remains responsible for the result and supplies a complete
initial contract instead of using repeated reminders as a control plane. Every
handoff names:

- the working directory, branch/worktree, relevant live processes, and other
  operating-environment facts;
- Outcome, Proof, Constraint, and Ponytail Rung;
- the exact files, artifacts, commit range, or surface in scope and explicit
  exclusions;
- authorized read/write/external actions; and
- exhaustion behavior: what evidence to return, what mutation to stop, and how
  to report the blocker or remaining work when the delegate runs out of
  in-scope actions.

Task size is part of that contract. A broad or ambiguous assignment is split
before delegation. If a delegate diverges, the first suspected defect is the
manager's context, boundaries, or exhaustion instructions; correct or split
the handoff before adding reminders. The delegate never expands scope merely
to stay busy.

### Schema contract

- Store one durable concept per Markdown file.
- Use a kebab-case filename. The file path without the `.md` suffix is the
  concept identity.
- Begin every concept page with parseable YAML frontmatter containing a
  non-empty `type`.
- This repository uses `Decision`, `Research Finding`, `Component`, and
  `Runbook` as concept-page types.
- Optional frontmatter fields remain available: `title`, `description`,
  `resource`, `tags`, and `timestamp`.
- Use ordinary relative Markdown links to form the concept graph.
- Decision pages follow Context → Options → Choice → Consequences.

## Consequences

- Agents search before reasoning and use the active plan plus concept graph for
  progressive disclosure.
- Durable findings are promoted out of chronology; the log records that an
  ingest happened but is never the fact's only home.
- Adding, moving, or retiring a concept requires an index update, and every
  ingest requires a log entry.
- Maintainers preserve useful cross-links and lint memory drift instead of
  treating code or knowledge as automatically authoritative over the other.
- Delegated work is evaluated as a managed contract: divergence improves the
  next handoff and task boundary rather than becoming a worker blame record.
- No generator, database, or retrieval service exists. Repository text search,
  relative links, and Git history are the complete mechanism.
- Changing verification commands remain centralized in the
  [test strategy](../testing/test-strategy.md), not copied into the stable
  guide.

## References

- [Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
- [Karpathy's LLM Wiki gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
