# 00 — Overview

> Phần 0/14. Đọc đầu tiên để có context toàn cục.

## Project Snapshot

| | |
|---|---|
| **Tên** | `vibervn-context-engine` (Cargo package: `context-engine-rs`) |
| **Version** | v0.1.36 (commit `f7214a9`) |
| **License** | MIT |
| **Repo** | https://github.com/nullmastermind/vibervn-context-engine |
| **Local** | `G:\_ws_me\vibervn-context-engine` |
| **Author** | `nullmastermind` |

## Mục đích

Local semantic code search engine cho large repos. Index code qua tree-sitter (22
languages) → embed chunks qua Voyage AI → store vào SurrealDB (RocksDB) → serve
qua HTTP API + Web UI + MCP tools.

## Stack

| Layer | Tech |
|---|---|
| **Language** | Rust 2024, single Cargo workspace |
| **HTTP** | axum 0.7 |
| **MCP** | rmcp 1.7.0 (streamable HTTP transport) |
| **DB** | surrealdb 2 (RocksDB backend) — embedded, 1 datastore/repo |
| **Embeddings** | Voyage AI (HTTP, on-disk content-addressed cache) |
| **LLM rerank** | OpenAI + Google (optional, structured output mode) |
| **Parsing** | tree-sitter 22 languages, cAST chunker algorithm |
| **Concurrency** | tokio (async) + rayon (CPU) |
| **Distribution** | npm multi-platform wrapper (1 main + 4 platform pkgs) |
| **CI** | Hand-rolled GitHub Actions (no cargo-dist) |
| **Web UI** | Single-file 239KB `src/assets/index.html` (vanilla JS + Tailwind CDN) |

## Tại sao analyze repo này?

User muốn **học operational discipline** trước khi build MCP project mới. Repo
này là production-grade reference implementation cho pattern "local service +
axum + rmcp + surrealdb + tree-sitter + Voyage AI". Cái hay không nằm ở
thuật toán (cũng là cosine + BFS) mà ở:

1. **Boot-frozen config** — resolve paths một lần, không re-read runtime
2. **Lock-order contract** — comment-mandated invariant `repo_dbs → vector_index`
3. **Crash-safe write ordering** — `file_meta` là commit marker, deferred write
4. **Content-addressed cache** — `md5(text+model)` key, NOT repo-scoped
5. **Schema-versioned migrations** — `DB_SCHEMA_VERSION = 5` + dual-format readers
6. **Atomic config write** — `NamedTempFile + persist` + 0o600
7. **Never-Err MCP pattern** — errors wrap as text content, LLM self-corrects

## Cấu trúc 14 docs

| # | File | Topic | LOC |
|---|---|---|---|
| 00 | `00-overview.md` | This file — context toàn cục | ~150 |
| 01 | `01-architecture.md` | Module graph, hot structs, threading, lifecycle | ~400 |
| 02 | `02-mcp-server.md` | Transport, tools, schema, errors, prompt injection | ~500 |
| 03 | `03-indexing-pipeline.md` | 5 phases, watching, chunking, edge resolution | ~500 |
| 04 | `04-query-embed-store.md` | Query, vector, BFS, cache, schema, ops | ~500 |
| 05 | `05-config-http-ui-build.md` | Config, HTTP, UI, npm, CI | ~400 |
| 06 | `06-security-audit.md` | Secrets, auth, injection, privilege escalation | ~500 |
| 07 | `07-failure-modes.md` | Error handling, race conditions, recovery | ~500 |
| 08 | `08-test-coverage.md` | What's tested, gaps, anti-patterns in tests | ~400 |
| 09 | `09-performance-scaling.md` | Bottlenecks, hot paths, scaling limits | ~500 |
| 10 | `10-patterns-to-copy.md` | 22 worth-copying decisions consolidated | ~500 |
| 11 | `11-anti-patterns.md` | 20 landmines consolidated | ~500 |
| 12 | `12-decision-matrix.md` | When to use which pattern | ~200 |
| 13 | `13-snippets.md` | Copy-paste-ready Rust code | ~400 |

## How to read

- **Đọc tuần tự (00 → 13):** full deep-dive
- **Đọc theo use case:**
  - Build MCP project ngay? → 02, 10, 12, 13
  - Build indexing/search service? → 03, 04, 09
  - Audit security? → 06
  - Prepare production deploy? → 07, 08, 09
  - Avoid common mistakes? → 11

## Source materials

- Initial fanout: 5 parallel Explore agents (architecture, MCP, indexing, query/store, config/UI/build)
- Deep-dive: 4 parallel Explore agents (security, failure modes, test coverage, performance)
- Direct file reads cho verification (main.rs, mcp.rs surface, Cargo.toml)
- Reports: `plans/260629-1157-.../reports/`
- Notes vault cross-link: `Notes/03_AI_Lab/04_Tools/ClaudeCode/65-vibervn-context-engine-mcp-patterns.md`
