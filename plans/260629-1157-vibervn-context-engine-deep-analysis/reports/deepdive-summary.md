---
type: deepdive-summary
slug: vibervn-context-engine-deep-analysis
date: 2026-06-29-1157
project: vibervn-context-engine
version-analyzed: 0.1.36 (commit f7214a9)
phases: 5/5 complete
---

# Deep Analysis Summary — vibervn-context-engine

**Goal:** Phân tích chuyên sâu repo `vibervn-context-engine` (Rust + MCP) để
extract operational discipline patterns cho future MCP projects. Modular docs
cho easy re-reading.

**Method:** 5 initial fanout agents + 4 deep-dive specialists (security,
failure modes, test coverage, performance) + 14 modular docs split by topic.

---

## Final Stats

| Metric | Value |
|---|---|
| **Plan folder** | `plans/260629-1157-vibervn-context-engine-deep-analysis/` |
| **Doc files** | 14 (`docs/00-overview.md` through `docs/13-snippets.md`) |
| **Specialist reports** | 4 (security, failure, tests, performance) |
| **Total LOC analyzed** | ~5,000+ (Rust) + ~10KB (test fixtures) |
| **Patterns documented** | 22 worth-copying + 14 bonus + 7 architecture |
| **Anti-patterns documented** | 20 landmines + 10 security + 10 build/test |
| **Snippets (copy-paste)** | 15 ready-to-use Rust code blocks |
| **Open questions** | 7 (consolidated trong 06-security-audit + 12-decision-matrix) |
| **Cross-links** | Notes vault (1) + Inbox (1) + initial report (1) |

---

## Doc Index (đọc theo use case)

### Nếu bạn build MCP project NGAY

1. `00-overview.md` — context toàn cục (5 min)
2. `02-mcp-server.md` — transport, tools, errors (15 min)
3. `10-patterns-to-copy.md` — 22 patterns consolidated (20 min)
4. `13-snippets.md` — copy-paste code (15 min)
5. `12-decision-matrix.md` — when-to-use-which (5 min)

### Nếu bạn build indexing/search service

1. `00-overview.md` — context
2. `03-indexing-pipeline.md` — 5 phases (20 min)
3. `04-query-embed-store.md` — query path (20 min)
4. `09-performance-scaling.md` — bottlenecks (15 min)

### Nếu bạn audit security

1. `06-security-audit.md` — 25 findings (30 min)
2. `07-failure-modes.md` — runtime failures (20 min)

### Nếu bạn prepare production deploy

1. `07-failure-modes.md` — what breaks
2. `08-test-coverage.md` — what's tested, gaps
3. `09-performance-scaling.md` — bottlenecks + scaling cliffs

### Nếu bạn avoid common mistakes

1. `11-anti-patterns.md` — 30 landmines (20 min)

---

## Top Findings Summary

### Architecture (01)
- **Unidirectional dependency flow:** `parsing`/`config` are leaves, mọi module
  khác consume types của chúng
- **Lock-order contract** (comment-mandated): luôn `repo_dbs → vector_index`,
  never reversed
- **Zero `rayon` use trong async paths** — chỉ `vector/mod.rs:194` par_chunks

### MCP (02)
- **2 tools × 2 mounts = 4 handler registrations** (global + per-repo)
- **Never-Err pattern** — all errors wrap as `Content::text("Error: ...")`
- **Prompt injection in tool description** (vendor self-injection at `mcp.rs:365-367`)

### Indexing (03)
- **5 phases:** Trigger → Walk → Parse → Framework → Embed → Store → Phase 2
- **file_meta is the sole commit marker** — WAL-at-row-level pattern
- **cAST chunker với version 2** — bump const để force re-chunk
- **5-level candidate selection** với `prefer_non_generated` per level

### Query/Store (04)
- **Content-addressed cache** (`md5(text+model)`) — NOT repo-scoped
- **Sharded vector index** với atomic-stamp LRU
- **DB_SCHEMA_VERSION = 5** với dual-format readers
- **14-arg `run_query`** — needs `QueryContext` struct refactor

### Config/HTTP/UI/Build (05)
- **Atomic settings write** với 0o600 chmod
- **~25 REST endpoints** + 2 MCP services + 1 SSE stream
- **239KB single-file SPA** với Tailwind CDN (no SRI)
- **No `cargo test` in CI** — PRs merge với failing tests

### Security (06)
- **25 security findings**, bao gồm:
  - Privilege escalation chain qua unauthenticated `PUT /api/config` + PowerShell metachars
  - `GET /api/config` returns API keys in plaintext
  - Auto-register any existing directory as repo from MCP tool
  - Tailwind CDN without SRI
  - No auth, no CORS, no CSP

### Failure Modes (07)
- **15 failure patterns**, bao gồm:
  - `last_err.unwrap()` panic trong LLM client
  - Silent cache corruption (4-byte-aligned garbage treated as valid)
  - `embed_batch` infinite retry loop (no circuit breaker)
  - Watcher `try_send` drops events silently
  - SSE subscriber lag silently dropped

### Test Coverage (08)
- **~230 test functions** (216 inline + 14 integration)
- **What's NOT tested:** SSE stream, MCP protocol, file watcher, graph expansion,
  vector scaling, HTTP API surface
- **repro_notepad.rs** is a code smell (3 of 6 tests are diagnostic, not assertions)
- **No `cargo test` in CI** — major gap

### Performance/Scaling (09)
- **5 bottlenecks:** N+1 DB queries, blocking rerank LLM, cold-repo warm,
  fs::read_to_string per chunk, no-SIMD dot product
- **O(N²) at merger.rs:124-133** (bounded by top_k, but README claim is misleading)
- **Sharded cap is best-effort** (single 5GB repo can OOM 4GB host)
- **Watcher drops events on full channel** (256-cap)

### Patterns to Copy (10)
- **22 main patterns** (consolidated) + 14 bonus
- Top 5: single shared query funnel, boot-frozen config, content-addressed
  cache, per-repo open gate, schemars + Parameters<T>

### Anti-Patterns (11)
- **20 main + 30 bonus** landmines
- Top 5: 209KB god file, dual MCP handlers, prompt injection, silent
  truncation, busy-poll

---

## Open Questions (7)

1. Does rmcp 1.7.0 support `Stream<Item = Progress>` cho tool return?
2. Does `embed_batch`'s exponential backoff respect `cancel_token`?
3. Is there plan tới support shared-secret bearer auth on `/api/*` + `/mcp*`?
4. Does the `agentic_rag` LLM tool get similar security review?
5. Verifier cho "sub-100ms cho 500K×1024" — which CPU, which flags?
6. Does `close_repo_db` có cơ chế cancel in-flight queries?
7. Is there a maximum total runtime per indexing pass? What happens nếu full
   rebuild mất > 1 hour?

---

## Source

- **Repo:** https://github.com/nullmastermind/vibervn-context-engine
- **Version:** v0.1.36 (commit f7214a9)
- **License:** MIT
- **Local:** `G:\_ws_me\vibervn-context-engine`
- **Plan folder:** `plans/260629-1157-vibervn-context-engine-deep-analysis/`
- **Initial report:** `plans/reports/discover-260629-1146-vibervn-context-engine-fanout.md`
- **Notes vault:** `Notes/03_AI_Lab/04_Tools/ClaudeCode/65-vibervn-context-engine-mcp-patterns.md`

## Token usage

```
💰 Tokens: ~250K used (T2 budget: 300K, 83%)
   - Initial fanout: ~25K
   - 5 fanout agents: ~60K
   - Notes vault docs: ~15K
   - Plan folder + 14 docs: ~120K
   - 4 deep-dive agents: ~25K
   - This report: ~5K
```

T2 budget hard cap approaching — finalizing now.
