# 10 — Patterns To Copy (22 consolidated)

> Phần 10/14. Tất cả worth-copying decisions từ 9 analyses, consolidated với `file:line` references. Reference implementation, đừng phát minh lại.

Mỗi pattern có:
- **What:** one-sentence summary
- **Why:** key insight
- **Where:** file:line reference
- **How:** concrete implementation hint

## 10.1 Boot & Config

### Pattern 1 — Single shared query funnel
- **What:** Core function `-> String`, MCP + REST + tests cùng call → byte-
  identical wire output
- **Why:** Một source of truth cho "tool output". Khi thay đổi format, không
  cần sync 2-3 chỗ.
- **Where:** `mcp.rs:629` (comment), `mcp.rs:run_codebase_retrieval` (633),
  `mcp.rs:run_file_retrieval` (1605)
- **How:** Bất kỳ MCP tool nào có REST proxy — viết hàm core `-> String`,
  mọi caller (tool, REST, test) đều wrap.

### Pattern 2 — Boot-frozen resolved paths
- **What:** `CLI > env > Settings > builtin default`, resolve once ở boot,
  never re-read ở runtime
- **Why:** Closing RocksDB mid-run sẽ split-brain reads. Pin resolved value,
  document loudly, warn-on-PUT.
- **Where:** `main.rs:142-146` (data_dir), `main.rs:170-174` (embeddings_dir),
  `server.rs:83-88` (doc), `server.rs:359-371` (warn-on-PUT)
- **How:** Bất kỳ long-running service nào mà config có thể thay đổi giữa
  chừng. See [[13-snippets#13.3-boot-precedence-helper|snippet]].

### Pattern 3 — Per-repo open gate
- **What:** `LazyLock<StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>>` inserted
  lazily per repo, double-check-under-gate
- **Why:** Cleanest fix cho "hai caller race exclusive directory lock" of
  RocksDB. 3 dòng implementation, massive correctness payoff.
- **Where:** `store/mod.rs:640-649` (OPEN_GATES), `store/mod.rs:107-129`
  (get_or_open)
- **How:** See [[13-snippets#13.5-per-repo-open-gate|snippet]].

### Pattern 4 — open_or_reset_index self-healing
- **What:** try open → retry 30s for stale LOCK → nếu vẫn fail thật, remove
  dir + reopen 1 lần
- **Why:** Non-destructive vì `remove_dir_all` fails nếu live OS handle vẫn
  giữ LOCK. Canonical self-healing pattern.
- **Where:** `store/mod.rs:779` (open_or_reset_index)
- **How:** Bất kỳ RocksDB/LMDB wrapper cần recover từ corrupted state.

### Pattern 5 — JSON settings với explicit version + MIGRATIONS array
- **What:** `version: u32` + `MIGRATIONS: &[MigrationFn]` chaining v1→vN
- **Why:** Older binaries refuse newer files (forward-compat tripwire) without
  dropping fields. Cheaper hơn schema library.
- **Where:** `config.rs:11-19` (MIGRATIONS), `config.rs:488-581`
  (ensure_dir_and_load), `config.rs:592-955` (per-version tests)
- **How:** See [[05-config-http-ui-build#5.10|Anti-pattern section]].

### Pattern 6 — Atomic config write
- **What:** `NamedTempFile` + `persist` (atomic rename) + 0o600 on Unix
- **Why:** Cross-platform atomic write, no `flock` cần cho single-process
  mutation
- **Where:** `config.rs:426-482` (write_settings_atomic)
- **How:** See [[13-snippets#13.1-atomic-config-write|snippet]].

## 10.2 MCP Server

### Pattern 7 — schemars::JsonSchema + Parameters<T>
- **What:** `#[derive(schemars::JsonSchema)]` trên input struct + rmcp's
  `Parameters<T>` extractor
- **Why:** Cleanest input-validation path trong rmcp. Doc comments trên fields
  tự trở thành schema descriptions → LLM biết param nghĩa là gì.
- **Where:** `mcp.rs:268-283` (CodebaseRetrievalArgs), `mcp.rs:285-295`
  (FileRetrievalArgs)
- **How:** See [[13-snippets#13.6-rmcp-tool|snippet]].

### Pattern 8 — "Never Err" pattern cho tool results
- **What:** Errors wrap as `Content::text("Error: ...")`, LLM self-corrects
- **Why:** Better than tearing down call. LLM thấy failure trong context.
- **Where:** `mcp.rs:619-621` (comment), `mcp.rs:410, 438, 573, 601` (Ok
  pattern)
- **How:** See [[13-snippets#13.6-rmcp-tool|snippet]].

### Pattern 9 — Per-session handler factory
- **What:** `StreamableHttpService::new(closure, ...)` tạo fresh handler per
  `initialize`
- **Why:** Capture config tại session start without global mutable state
- **Where:** `server.rs:151-168` (global), `server.rs:1051-1069` (per-repo)
- **How:** Move mọi per-session config capture vào factory closure.

### Pattern 10 — MCP output budget assembly
- **What:** `assemble_with_budget` emit full content đến 48K-char ceiling,
  sau đó header + first 120 chars + elision marker
- **Why:** Every MCP server sẽ hit client ceiling (Cursor, Cline, etc).
- **Where:** `mcp.rs:50-121` (assemble_with_budget)
- **How:** 150-char footer reserve + line-merge heuristic.

### Pattern 11 — Per-repo MCP service caching
- **What:** `Arc<RwLock<HashMap<String, RepoMcpService>>>` caches
  `StreamableHttpService`
- **Why:** Service expensive tới construct, factory closure pattern lets mỗi
  session get fresh handler
- **Where:** `server.rs:103, 1038-1072`
- **How:** Cache services by repo path, factory closure per session.

## 10.3 Indexing Pipeline

### Pattern 12 — Crash-safety qua ordered writes
- **What:** Chunks written **trước**, `file_meta` (mtime + chunker_version) là
  **commit marker**, deferred write
- **Why:** WAL-at-row-level pattern. If crash mid-stream → next trigger re-
  index half-written files.
- **Where:** `indexing/pipeline.rs:1054-1124` (file_meta commit), `pipeline.rs:
  457-480` (recovery detection)
- **How:** Mọi write pipeline có "derived data" (embeddings, summaries,
  hashes). Commit marker phải là row cuối.

### Pattern 13 — Drop-then-bulk-insert-then-rebuild indexes
- **What:** Drop secondary indexes, bulk insert, rebuild indexes synchronous
- **Why:** Biến N×O(log N) per-insert thành 1×O(N) write + 1×O(N log N) build
- **Where:** `indexing/pipeline.rs:1490, 1639` (edges), `pipeline.rs:159-188`
  (chunks)
- **How:** Bất kỳ DB có nhiều secondary indexes.

### Pattern 14 — Chunking version là stored build constant
- **What:** `pub const CHUNKER_VERSION: i64 = 2;` stored trong `file_meta`
- **Why:** Bump constant → mọi file cũ tự re-chunk lần trigger kế tiếp.
  Không cần DB schema migration.
- **Where:** `parsing/chunker.rs:37` (const), `indexing/tracker.rs:67`
  (3-way check)
- **How:** Mọi derived per-file artifact (embeddings, summaries, AST hashes).
  Self-documenting freshness flag.

### Pattern 15 — 5-level candidate selection
- **What:** L0 import path → L1 subdirectory → L2 same parent → L3 same file →
  L4 first sorted bucket. Mỗi level prefer non-generated files.
- **Why:** Language-agnostic cascade composes với language-specific resolvers
- **Where:** `indexing/pipeline.rs:1280-1380` (select_best_candidate)
- **How:** Code graph builder cần resolve cross-file refs.

### Pattern 16 — Streaming pipeline với bounded channels
- **What:** 3 stages, 2 channels, peak inflight = `O(channel_cap × chunks_per_
  file)`, **independent** of repo size
- **Why:** Don't scale linearly với repo size. Bounded memory regardless.
- **Where:** `indexing/pipeline.rs:744-928` (streaming_index), `pipeline.rs:50`
  (comment)
- **How:** Use `mpsc::channel` with explicit cap, `buffer_unordered` for I/O,
  `par_iter` for CPU.

## 10.4 Query, Embed & Store

### Pattern 17 — Content-addressed embedding cache
- **What:** `md5(text+model)` key, atomic NamedTempFile+persist, mtime-touch
  LRU
- **Why:** Same code chunk across repos hits same cache entry. Zero deps, no
  locking.
- **Where:** `embedding/cache.rs:17` (struct), `cache.rs:48` (key),
  `cache.rs:160-170` (atomic write)
- **How:** See [[13-snippets#13.2-content-addressed-cache|snippet]].

### Pattern 18 — Per-repo sharded vector index với atomic-stamp LRU
- **What:** `search` takes `&self`, bumps recency qua `AtomicU64`
- **Why:** Read guards không bao giờ block trên write guard. Lock-free LRU
  touch-bumping.
- **Where:** `vector/sharded.rs:65` (struct), `sharded.rs:130` (touch)
- **How:** Bất kỳ per-tenant/per-repo in-memory index cần bounded RAM.

### Pattern 19 — Pre-normalize tại insert, dot product tại query
- **What:** L2-normalize mọi vector khi insert. Cosine = dot product khi cả
  hai unit length.
- **Why:** Eliminates per-candidate division. Parallel over
  `par_chunks(self.dim)` + `select_nth_unstable_by`.
- **Where:** `vector/mod.rs:96` (normalize), `mod.rs:297` (dot_product)
- **How:** 500K × 1024 dims, sub-100ms với rayon.

### Pattern 20 — Filter stripping trước embedding
- **What:** Parse `kind:`, `lang:`, `path:`, `name:` prefixes, strip chúng,
  embed chỉ semantic content
- **Why:** Tiny parser, big save (API cost + improved relevance)
- **Where:** `query/filters.rs:48` (parse_query_filters), `engine.rs:135-140`
  (use)
- **How:** Bất kỳ MCP tool accept free-form query có structured filter
  prefixes.

### Pattern 21 — Schema evolution qua DB_SCHEMA_VERSION + dual-format readers
- **What:** `DB_SCHEMA_VERSION = 5` với migrations chained, custom Visitor
  accepts cả old `array<float>` và new packed `bytes`
- **Why:** Half-migrated DB is correct-but-unoptimized, không broken
- **Where:** `store/mod.rs:24` (const), `ops.rs:51` (de_embedding_dual)
- **How:** Bất kỳ embedded DB cần evolve. Reader linh hoạt hơn writer strict.

### Pattern 22 — Lock discipline: clone guard, drop guard, await
- **What:** `let x = { let g = lock.read().await; g.clone() }; x.do_thing().await`
- **Why:** Không bao giờ hold `RwLock` qua await point. Massive correctness
  pattern.
- **Where:** `query/engine.rs:186-189` (canonical), `engine.rs:441-444`
- **How:** See [[13-snippets#13.4-lock-discipline|snippet]].

## 10.25 Bonus — Worth-Defending Bonus Patterns

| # | Pattern | Where | Why |
|---|---|---|---|
| 23 | Atomic settings write với pre+post `0o600` chmod | `config.rs:426-482` | Closes rename-onto-existing race |
| 24 | Boot-frozen data_dir/embeddings_dir | `server.rs:359-371, 384-390` | Prevents split-brain reads |
| 25 | path_in_repo với explicit separator-after-prefix check | `lib.rs:22-39` | Correctly rejects `/foo` vs `/foobar` |
| 26 | UTF-16LE base64 -EncodedCommand cho PowerShell | `defender.rs:231-241` | Eliminates PowerShell quoting landmines |
| 27 | Single-write-lock critical section cho newly-added repos | `server.rs:330-340` | Closes concurrent-PUT race |
| 28 | Manual npm multi-platform với optionalDependencies | `npm/vibervn-context-engine/bin/cli.js` | Predictable, không native build toolchain trên user machines |
| 29 | MockBackend script-driven LLM tests | `query/reranker.rs:1825-2340` | Cleanest agentic-loop test pattern |
| 30 | Per-language parsing tests | `parsing/mod.rs` | Keeps fixtures co-located |
| 31 | Bit-exact pack/unpack test | `store/mod.rs:1594` | Catches endian flips |
| 32 | Cross-shard top-k = merged top-k | `vector/sharded.rs:357` | Contract assertion cho partitioned search |
| 33 | Flat row-major `Vec<f32>` storage | `vector/mod.rs:44-50` | Cache locality beats Vec<Vec<f32>> |
| 34 | Lock-order discipline (repo_dbs → vector_index) | `sharded.rs:16, indexing/mod.rs:506-508` | Documented và enforced |
| 35 | Bounded mpsc channels trong indexing pipeline | `pipeline.rs:51, 54` | O(channel_cap × chunks_per_file) RAM |
| 36 | RocksDB memory pinning trước any datastore opens | `main.rs:68-84` | Repo-count-stable total RAM |

## Decision Table

| Situation | Pattern |
|---|---|
| Build MCP tool có REST proxy | Pattern 1 (shared query funnel) |
| Long-running service, user-editable config | Pattern 2 (boot-frozen) |
| Embedded DB (RocksDB/LMDB/Sled) | Pattern 3 (per-repo open gate) |
| DB có thể corrupt từ OS/process crash | Pattern 4 (open_or_reset_index) |
| User-edited config cần migrate | Pattern 5 (version + MIGRATIONS) |
| Config file mutation | Pattern 6 (atomic write) |
| rmcp tool input | Pattern 7 (schemars + Parameters<T>) |
| LLM gọi tool hay fail | Pattern 8 (never-Err) |
| rmcp per-session config | Pattern 9 (handler factory) |
| MCP tool return lớn | Pattern 10 (output budget) |
| Per-repo MCP service | Pattern 11 (service caching) |
| Write pipeline có derived data | Pattern 12 (ordered writes) |
| DB có nhiều secondary indexes | Pattern 13 (drop-then-bulk-insert) |
| Derived per-file artifact | Pattern 14 (CHUNKER_VERSION) |
| Code graph builder | Pattern 15 (5-level candidate) |
| Streaming pipeline với bounded memory | Pattern 16 (bounded channels) |
| Embed/summary cache nhiều repo | Pattern 17 (content-addressed) |
| Per-tenant in-memory vector index | Pattern 18 (sharded + atomic LRU) |
| Vector search | Pattern 19 (pre-normalize + dot) |
| Free-form query có filter prefix | Pattern 20 (filter stripping) |
| Embedded DB schema cần evolve | Pattern 21 (DB_SCHEMA_VERSION) |
| Async code với RwLock | Pattern 22 (clone guard, drop, await) |

Xem [[13-snippets]] cho copy-paste-ready implementations.
