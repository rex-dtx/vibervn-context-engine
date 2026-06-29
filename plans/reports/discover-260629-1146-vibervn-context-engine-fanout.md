---
type: discover
slug: vibervn-context-engine-fanout
date: 2026-06-29-1146
project: vibervn-context-engine
version-analyzed: 0.1.36 (commit f7214a9)
---

# /discover fanout — vibervn-context-engine

**Goal:** Khảo sát toàn diện repo `vibervn-context-engine` (local semantic
code search engine, Rust + MCP), rút patterns đáng copy & nên tránh cho
MCP projects tương lai. Output cuối: docs trong Notes vault.

**Method:** Brownfield discovery (5 steps) + 5 parallel Explore agents
(architecture, MCP surface, indexing pipeline, query/embed/store, config/
HTTP/UI/build) + synthesis.

---

## 1. Discovery Card

```
🗺️ Project: vibervn-context-engine | Type: Rust 2024 (Cargo) | Stack: axum 0.7,
   rmcp 1.7.0 (streamable HTTP), tokio, surrealdb 2 (RocksDB), tree-sitter
   22 langs, Voyage AI, OpenAI/Google rerank, rayon

Branch: master | Worktree: no
Status: 0 uncommitted | Recent: f7214a9 chore(release): v0.1.36

Active plan: none (research-only, không tạo plan folder)
Codegraph: missing (no .codegraph/ in repo)

Test: cargo test (tests/integration.rs + tests/repro_notepad.rs) | Build:
   cargo build -r | Dev: cargo run -r (4 platform targets: linux x64/arm64,
   darwin arm64, win32 x64)

Distribution: npm multi-platform (1 main shim + 4 platform packages) ·
   npx vibervn-context-engine@latest

Conventions:
- Không có CLAUDE.md project-local
- Không có docs/ folder
- 12 modules, file lớn nhất 209KB (pipeline.rs)
- 22 tree-sitter language extractors, cùng signature
- Lock order contract: repo_dbs → vector_index (comment-mandated)
- Mọi public modules re-exported qua lib.rs (path_in_repo là pub(crate))

Recommended next: docs đã viết ở Notes vault, xem
   Notes/03_AI_Lab/04_Tools/ClaudeCode/65-vibervn-context-engine-mcp-patterns.md
```

---

## 2. Fanout Scope

5 agents song song, mỗi agent focus 1 khía cạnh:

| Agent | Scope | Output |
|---|---|---|
| 1 | Architecture & module graph | Module deps, hot structs, threading model, lifecycle, worth-copy, anti-patterns |
| 2 | MCP server surface | Transport, tools, schema pattern, error handling, streaming, state injection, worth-copy, anti-patterns |
| 3 | Indexing pipeline | Phases, file watching, incremental tracker, parsing, chunking, framework extraction, phase 2 edge resolution, concurrency, worth-copy, anti-patterns |
| 4 | Query/embed/store | Query entry, field filter parser, vector search, BFS, merger, LLM rerank, Voyage client, embedding cache, SurrealDB schema, store ops, worth-copy, anti-patterns |
| 5 | Config/HTTP/UI/build | Config model, boot precedence, HTTP endpoints, state injection, web UI, defender, CI/release, npm distribution, integration tests, worth-copy, anti-patterns |

Mỗi agent trả về ≤5K tokens, tập trung vào `file:line` cụ thể + concrete
design decisions, không pad generic advice.

---

## 3. Top-Level Findings

### Stack snapshot

- **Language:** Rust 2024, single Cargo workspace
- **HTTP:** axum 0.7 (REST + MCP transport integration)
- **MCP:** rmcp 1.7.0 với `transport-streamable-http-server` feature
- **DB:** surrealdb 2 (RocksDB backend) — embedded, one datastore per repo
- **Embeddings:** Voyage AI (HTTP, with on-disk content-addressed cache)
- **LLM rerank:** OpenAI + Google (optional, structured output mode)
- **Parsing:** tree-sitter 22 languages, cAST chunker algorithm
- **Concurrency:** tokio (async I/O) + rayon (CPU parallelism)
- **Distribution:** npm multi-platform (4 targets), hand-rolled GitHub Actions release
- **Web UI:** single-file 239KB index.html (vanilla JS, Tailwind CDN, 3-lang i18n)

### Module architecture

```
server ──→ config, defender, embedding, indexing, llm, mcp, query, store, path_in_repo
mcp    ──→ config, embedding, indexing, llm, store
query  ──→ embedding, indexing, llm, store   (+ engine, graph_expand, merger, reranker)
indexing.pipeline ──→ embedding, parsing, store, vector
parsing, config, vector.path_in_repo  ←  leaves (zero internal deps)
```

Unidirectional dependency flow, `parsing` và `config` là leaves.

### Hot data structures (bảng tóm tắt)

| Type | File:Line | Role |
|---|---|---|
| `IndexEngine` | `indexing/mod.rs:102` | Watcher spawner, mpsc consumer, per-repo lock, status map, vector_index, event_bus, cancel_tokens, repo_dbs |
| `AppState` | `server.rs:77-104` | axum state: home_dir, data_dir, embeddings_dir, index_engine, repo_dbs, settings, repo_mcp_services |
| `RepoDbMap` | `store/mod.rs:30` | `Arc<RwLock<HashMap<String, Surreal<Db>>>>` — canonical alias |
| `ShardedVectorIndex` | `vector/sharded.rs:65` | per-repo shard map với atomic-stamp LRU |
| `Settings` | `config.rs:251-315` | version, repos, embedding, llm, mcp_*, vector_resident_cap_mb, data_dir, embeddings_dir, enabled_mcp_tools, custom_extensions, index_ignore_filenames |
| `McpHandler` / `RepoMcpHandler` | `mcp.rs:300, 474` | 90% identical dual impls (anti-pattern #19) |
| `LlmClient` | `llm/mod.rs:60` | provider, model, api_keys (round-robin via AtomicUsize), http client, openai_base_url |

### MCP surface (per agent 2)

- **Transport:** `StreamableHttpService` từ rmcp, nest ở `/mcp` (global) +
  per-repo mount `/mcp-repo/:repo_id` (factory closure)
- **Tools exposed:** 2 tools × 2 mounts = 4 registrations:
  - `codebase-retrieval` (`mcp.rs:343-411, 514-574`) — input:
    information_request + workspace_full_path + filter_kind/lang/path,
    output: 48K-char text
  - `file-retrieval` (`mcp.rs:413-439, 576-602`) — input:
    workspace_full_path + file_path + information_request + top_k
- **Schema:** `schemars::JsonSchema` derive + `Parameters<T>` extractor
  (cleanest rmcp pattern)
- **Error handling:** "Never-Err" pattern — errors wrap as
  `Content::text("Error: ...")`, LLM self-corrects
- **Streaming:** **KHÔNG** — SSE progress là separate REST endpoint
  `/api/repos/:repo_id/index-events`, MCP tools block đến khi xong
- **State sharing:** `Arc<IndexEngine>`, `Arc<RwLock<Settings>>` cloned
  into handler struct, no axum State/Context for MCP tools

### Indexing pipeline (per agent 3)

- **5 phases:** Trigger → Walk → Parse → Framework extract → Embed →
  Store → Phase 2 (resolve edges)
- **File watching:** notify v8 + notify-debouncer-full, 3s debounce,
  filtered in `IndexPipeline::run` (not watcher itself)
- **Incremental:** `file_meta` row = commit marker (mtime + size +
  chunker_version), 3-way check in `tracker::detect_changes`
- **Parsing:** 22 extractors với cùng signature `fn(&str, &str, &tree)
  -> (Vec<Symbol>, Vec<RawEdge>)`, dispatched qua match on `Lang` enum
- **Chunking:** cAST algorithm (`parsing/chunker.rs:243` chunk_file_ast,
  fallback `chunk_file` line-window), versioned qua
  `CHUNKER_VERSION = 2` constant
- **Framework extraction:** 5 resolvers (React, Express, Django,
  Spring, Gin) qua `FrameworkResolver` trait, detect cached per session
- **Phase 2:** 5-level candidate selection với `prefer_non_generated`
  fallback mỗi level, `chase_reexports` for barrel files, RAM fast path
  cho full rebuilds
- **Concurrency:** rayon `par_iter` cho parse stage, `buffer_unordered`
  cho embed stage, sync↔async boundary ở `tree_sitter::Tree` (vì
  `!Send`)

### Query path (per agent 4)

- **Entry:** `query::run_query` (14 args, `#[allow(too_many_arguments)]`)
- **Filter stripping:** parse `kind:/lang:/path:/name:` prefixes, embed
  chỉ semantic content
- **Vector search:** brute-force dot product trên L2-normalized vectors
  (cosine = dot), rayon par_chunks, sub-100ms ở 500K × 1024 dims
- **BFS expansion:** depth 2, max 30 bonus chunks, score decay 0.6/0.5
  (caller/callee), score floor 0.15
- **Merger:** dedup by (file, line_start, line_end), adjacent-range
  merge với gap ≤ 1 line, cap 60 lines, width-desc containment filter
- **LLM rerank:** OpenAI/Google, structured output mode, ranked indices
  (không scores), optional (Option<&LlmClient>)
- **Voyage client:** round-robin keys, byte-size + count batch limits
  (1.5MB / 128), 2 separate HTTP clients (120s batch, 30s query)
- **Embedding cache:** `md5(text+model)` key, mtime-touch LRU, atomic
  NamedTempFile+persist
- **SurrealDB schema:** 5 edge tables (calls, uses, imports, contains,
  implements) + symbol + chunk + file_meta + index_meta + raw_edge,
  dual-format embedding reader cho migration safety

### Config/HTTP/UI/Build (per agent 5)

- **Config:** JSON ở `~/.vibervn/context-engine/settings.json`, version +
  MIGRATIONS array, atomic write, 0o600 on Unix
- **Boot precedence:** CLI > env > Settings > default, inline 4× trong
  main.rs (anti-pattern #16)
- **HTTP API:** ~25 REST endpoints + 2 MCP services + 1 SSE stream
- **State injection:** `AppState` Clone với `Arc<...>` fields,
  `.with_state(state)` pattern
- **Web UI:** 239KB single-file vanilla JS, Tailwind CDN, 3-lang i18n,
  EventSource cho SSE, fetch() cho REST
- **Defender:** Windows Defender exclusion management (UAC + elevated
  PowerShell + durable marker file)
- **CI/release:** 1 workflow `release.yml`, hand-rolled, matrix 4 targets,
  auto-bump patch version, no cargo-dist/release-plz
- **npm:** 5 packages (1 main + 4 platform), 63-line `bin/cli.js` shim,
  no postinstall, no native build toolchain on user machines
- **Tests:** 27KB integration.rs (HTTP roundtrips) + 20KB
  repro_notepad.rs (investigation scripts shipped as tests)

---

## 4. Patterns Synthesis

### Worth-copying (22 patterns, full detail ở Notes doc §2)

Top 5 cho MCP project mới:
1. **Single shared query funnel** (`mcp.rs:629`) — core function `-> String`,
   MCP + REST + tests cùng call, byte-identical output
2. **Boot-frozen resolved paths** (`main.rs:142-184`) — CLI>env>Settings>
   default, resolved once, never re-read at runtime
3. **Content-addressed embedding cache** (`embedding/cache.rs:17`) —
   `md5(text+model)` key, atomic NamedTempFile+persist, mtime-touch LRU
4. **Per-repo open gate** (`store/mod.rs:640`) — LazyLock<StdMutex<HashMap
   <String, Arc<AsyncMutex<()>>>>>, double-check-under-gate pattern
5. **schemars::JsonSchema + Parameters<T>** (`mcp.rs:268-295`) — cleanest
   rmcp input validation, doc comments → schema descriptions

### Anti-patterns to avoid (20, full detail ở Notes doc §3)

Top 5 tránh:
1. **Prompt injection trong tool description** (`mcp.rs:345-385`) — chứa
   `<RULES>` claim "appends to system prompt", một số client strip
2. **48K-char silent truncation** (`mcp.rs:50-121`) — không `isError`, không
   `is_partial`, client phải read trailing text để biết truncated
3. **209KB pipeline.rs god file** (`indexing/pipeline.rs`) — IndexPipeline +
   free fns + helpers + flushers mixed, ASCII banners pseudo-modules
4. **Dual MCP handler types** (`mcp.rs:300, 474`) — McpHandler +
   RepoMcpHandler 90% identical, nên là generic `<T: WithWorkspace>`
5. **run_query takes 14 arguments** (`query/engine.rs:93, 117`) — nên
   là QueryContext struct

### Decision matrix

22 patterns × typical situations table ở Notes doc §4 — quick lookup
khi build MCP project mới.

### Copy-paste snippets

5 ready-to-use snippets ở Notes doc §5:
- Atomic config write (NamedTempFile + 0o600)
- Content-addressed cache (mtime-LRU)
- Boot precedence helper
- Lock discipline (clone guard, drop guard, await)
- Per-repo open gate
- rmcp tool with schemars + never-Err

---

## 5. Files Written

| Path | Purpose |
|---|---|
| `G:\_devtools\cfg-n-scripts\Notes\03_AI_Lab\04_Tools\ClaudeCode\65-vibervn-context-engine-mcp-patterns.md` | Detailed analysis (20KB) — 22 patterns + 20 anti-patterns + 6 snippets + decision matrix + 7 open questions |
| `G:\_devtools\cfg-n-scripts\Notes\00_Inbox\2026-06-29-vibervn-context-engine.md` | Index entry (2KB) — TL;DR + top 3 copy + top 3 avoid + cross-link to detail doc |
| `G:\_ws_me\vibervn-context-engine\plans\reports\discover-260629-1146-vibervn-context-engine-fanout.md` | This report (summary) |

---

## 6. Token usage

```
💰 Tokens: ~85K used (T2 budget: 300K, 28%)
   - Discovery basics: ~5K
   - 5 parallel Explore agents: ~60K
   - Notes docs write: ~15K
   - This report: ~5K
```

Within T2 budget (internal tooling/docs).

---

## 7. Unresolved Questions

7 open questions ở Notes doc §6:
1. Streaming MCP tools — rmcp 1.7.0 support `Stream<Item = Progress>`?
2. `re_extract` bug fix có test cho edge cases khác?
3. `PipelineAbort::EmbeddingFailed` — plan nào cho fake embedder trait?
4. BFS constants có telemetry không?
5. Cross-instance safety — 2 indexer cùng data_dir, mitigation plan?
6. `repo_mcp_services` HashMap — memory bound cho 100+ repos?
7. Schema DDL landmines — CI check enforce, hay chỉ rely on comment?

---

## 8. Next Steps for User

Đọc `Notes/03_AI_Lab/04_Tools/ClaudeCode/65-vibervn-context-engine-mcp-patterns.md` để
phân tích chi tiết. Key files để grep khi cần reference:
- `src/main.rs:142-184` (boot precedence)
- `src/mcp.rs:268-411` (tool definitions + never-Err pattern)
- `src/embedding/cache.rs:17` (content-addressed cache)
- `src/store/mod.rs:640-649, 779` (per-repo open gate + self-healing)
- `src/vector/sharded.rs:65, 130` (sharded LRU)
- `src/indexing/pipeline.rs:1054-1124, 1280-1380` (crash-safe writes,
  5-level candidate selection)
- `src/config.rs:11-19, 488-581` (MIGRATIONS array)
- `src/query/filters.rs:48` (filter stripping)
- `src/query/graph_expand.rs:53-54` (BFS constants — note magic numbers)
- `npm/vibervn-context-engine/bin/cli.js` (63-line shim)
