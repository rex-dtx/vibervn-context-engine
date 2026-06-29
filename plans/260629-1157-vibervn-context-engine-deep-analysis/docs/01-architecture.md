# 01 — Architecture & Module Graph

> Phần 1/14. Module dependencies, hot data structures, threading model, lifecycle.

## 1.1 Module Dependency Graph

Project dùng **flat subdirectories** (e.g. `src/vector/mod.rs`, không phải
`src/vector.rs`). `lib.rs:2-12` declare 11 `pub mod`:

```
server ──→ config, defender, embedding, indexing, llm, mcp, query, store, path_in_repo
mcp    ──→ config, embedding, indexing, llm, store
query  ──→ embedding, indexing, llm, store, path_in_repo
         └─ engine ──→ (above) + graph_expand, merger, reranker
indexing
  mod  ──→ config, embedding, indexing::{events,pipeline,tracker,watcher}, store::{RepoDbMap, ops::set_meta}, vector::{SearchResult, ShardedSearch, ShardedVectorIndex, VectorIndex}
  pipeline ──→ embedding::{InputType, cache::EmbeddingCache}, parsing::{parse_file, relations, symbols}, indexing::{events, ProgressHandle, tracker, walker}, store::ops, vector::{ChunkId, ShardedVectorIndex}
  watcher ──→ indexing::{IndexTrigger, tracker::ChangeKind/FileChange}
  import_resolver ──→ parsing::Lang
  frameworks/* ──→ indexing::frameworks::{DetectionContext, FrameworkResolver}, parsing::{relations, symbols}
store  ──→ parsing::{symbols, relations}
parsing ──→ parsing::{chunker, relations, symbols}
vector ──→ path_in_repo
llm    ──→ config::LlmConfig (sub-modules `openai`, `google` use super::ToolDef)
embedding::voyage ──→ embedding::InputType

bin/chunk_bench ──→ context_engine_rs::{indexing::walker::walk_repo, parsing::{parse_file, symbols::Symbol}}
```

**Key observation:** `parsing` là **leaf module** — zero internal deps, mọi
module khác consume types của nó. `config` cũng là leaf. Dependency flow là
**unidirectional**: `parsing` / `config` → everything else.

## 1.2 Public Surface

**Declared modules** (`src/lib.rs:2-12`): `config`, `defender`, `embedding`,
`indexing`, `llm`, `mcp`, `parsing`, `query`, `server`, `store`, `vector`.

**`pub use` re-exports:**
- `src/vector/mod.rs:10` — `pub use sharded::{ShardedSearch, ShardedVectorIndex}`
- `src/query/mod.rs:7` — `pub use engine::{CodeResult, QueryResult, QueryTiming, RerankInfo, run_query}`

**`pub(crate)` helpers:**
- `path_in_repo` (`src/lib.rs:22`) — string-prefix containment check
- `find_db_for_file` (`src/query/mod.rs:16`)
- `warm_repo_shard` + `seed_statuses_from_db` (`src/indexing/mod.rs:163, 203`)

**Binary consumption** (`src/bin/chunk_bench/main.rs:45-47`):
`indexing::walker::walk_repo`, `parsing::parse_file`,
`parsing::symbols::Symbol` — bench binary bypasses live indexing engine, chỉ
exercise parsing chunker.

## 1.3 Hot Data Structures

| Type | File:Line | Key Fields |
|---|---|---|
| `IndexEngine` | `indexing/mod.rs:102` | `data_dir`, `embeddings_dir`, `statuses: Arc<RwLock<HashMap<String, RepoStatus>>>`, `repo_locks`/`warm_locks: Mutex<HashMap<…,Arc<Mutex<()>>>>`, `trigger_tx: mpsc::Sender<IndexTrigger>`, `vector_index: Arc<RwLock<ShardedVectorIndex>>`, `event_bus: IndexEventBus`, `cancel_tokens: Mutex<HashMap<String, CancellationToken>>`, `repo_dbs: RepoDbMap` |
| `RepoStatus` | `indexing/mod.rs:50` | `state: IndexState` (Idle/Indexing/Error), `indexed_files: u64`, `total_files: u64`, `last_indexed_at: Option<DateTime<Utc>>`, `error: Option<String>` |
| `IndexTrigger` | `indexing/mod.rs:147` | `repo: String`, `changes: Option<Vec<FileChange>>`, `rebuild: bool` |
| `IndexEvent` | `indexing/events.rs:7` | Tagged enum: Started, FileParsed, FileEmbedded, FileStored, FileIndexed, Phase2Start/Done, Completed, Failed, Cancelled — broadcast channel cap 1024 |
| `AppState` | `server.rs:77-104` | `home_dir`, `data_dir`, `embeddings_dir`, `index_engine: Arc<IndexEngine>`, `repo_dbs: Arc<RwLock<HashMap<String, Surreal<Db>>>>`, `settings: Arc<RwLock<Settings>>`, `repo_mcp_services` |
| `RepoDbMap` | `store/mod.rs:30` | `Arc<RwLock<HashMap<String, Surreal<Db>>>>` — single canonical alias for shared DB-handle cache |
| `VectorIndex` | `vector/mod.rs:43` | Row-major flat `embeddings: Vec<f32>`, parallel `chunk_ids: Vec<ChunkId>`, `dim: usize` |
| `ShardedVectorIndex` | `vector/sharded.rs:65` | `shards: HashMap<String, Shard>` (Shard wraps `VectorIndex` + `AtomicU64` recency), `clock: AtomicU64`, `cap_bytes: usize` |
| `ChunkId` / `SearchResult` | `vector/mod.rs:17, 25` | `(file, line_start, line_end)` / `(chunk_id, score: f32)` |
| `QueryResult` / `CodeResult` | `query/engine.rs:69, 23` | `results`, `pre_rerank_results`, `timing: QueryTiming`, `rerank: Option<RerankInfo>` |
| `McpHandler` / `RepoMcpHandler` | `mcp.rs:300, 474` | 90% identical dual impls (anti-pattern!) |
| `LlmClient` | `llm/mod.rs:60` | `provider`, `model`, `api_keys: Vec<String>`, `http: reqwest::Client`, `key_cursor: Arc<AtomicUsize>` for round-robin |
| `Settings` | `config.rs:251-315` | `version`, `repos`, `embedding`, `llm`, `mcp_stale_after_days`, `vector_resident_cap_mb`, `enabled_mcp_tools`, `custom_extensions`, `index_ignore_filenames` |

## 1.4 Threading & Async Model

**Có ZERO `rayon` use trong async paths.** Chỉ một `rayon` site duy nhất:
`src/vector/mod.rs:194` `self.embeddings.par_chunks(self.dim).enumerate()` cho
parallel dot-product scoring trong (sync) `VectorIndex::search`.

**Model hầu hết là `tokio::sync` với strict lock-order contract:**

- **Boundary:** `mpsc::channel::<IndexTrigger>(256)` (`indexing/mod.rs:260`) là
  single sync point giữa watchers (producers) và consumer task. KHÔNG có
  `spawn_blocking` trên hot query path — `tokio::task::spawn_blocking` chỉ
  dùng cho `std::fs::remove_dir_all` trong `remove_index_dir` (`store/mod.rs:689`).
- **2 mutex flavours, cả hai `tokio::sync::Mutex`:**
  - `repo_locks` + `warm_locks` (per-repo async serialisation)
  - `OPEN_GATES` là **only** `std::sync::Mutex` (`store/mod.rs:640`) — dùng vì
    held synchronously khi build HashMap per-repo open gates. Reasoning
    documented.
- **Async-only locks:** `Arc<RwLock<…>>` wraps `RepoDbMap`, `Settings`,
  `ShardedVectorIndex`, `RepoStatus` map.
- **Atomic counters:** `key_cursor: AtomicUsize` trong `LlmClient` (round-robin
  API keys); `clock: AtomicU64` + `Shard.last_touched: AtomicU64` cho lock-free
  LRU touch-bumping trong search (`vector/sharded.rs:69, 39`).
- **Lock order contract** (`indexing/mod.rs:162, 506`): **always
  `repo_dbs → vector_index`**, never reversed. `warm_repo_shard` calls
  `get_or_open` (repo_dbs) **trước** khi take `vector_index.write()`. Vector
  search chỉ take read guard cho search, sau đó spawn warm task sau khi drop
  guard.

**Tokio/rayon intersection** là `VectorIndex::search` sync compute: hot-path
queries pre-warm shard async, sau đó sync `search()` call đủ nhanh
(sub-100ms target cho 500K chunks qua rayon par_chunks) nên không block tokio
runtime đáng kể.

## 1.5 Lifecycle & Boot Order

```
main.rs
  ├─ set_rocksdb_memory_bounds()         (env vars BEFORE any DB open)
  ├─ ensure_dir_and_load(home_dir) → Settings
  ├─ settings_handle: Arc<RwLock<Settings>>      [single source of truth]
  ├─ repo_dbs: RepoDbMap = Arc::new(RwLock::new(HashMap::new()))  [lazy]
  └─ IndexEngine::start(data_dir, embeddings_dir, &boot_settings, repo_dbs, settings_handle)
      ├─ mpsc::channel(256)
      ├─ ShardedVectorIndex::new(cap_bytes) → wrapped in Arc<RwLock<>>
      ├─ seed status entries (Idle default per repo)
      ├─ tokio::spawn(seed_statuses_from_db)         [restores prior counts]
      ├─ tokio::spawn(start_watcher) per repo        [filesystem notify]
      └─ tokio::spawn(run_consumer(trigger_rx, settings_handle))
           └─ loop: recv IndexTrigger → clone settings snapshot →
              acquire per-repo lock → open_or_reset_index → build pipeline → run →
              update RepoStatus → emit IndexEvent
```

**Owners:**

- **RocksDB handles** owned exclusively by `repo_dbs: RepoDbMap`.
  `get_or_open` là **only** producer (với per-repo open gate để serialise
  against exclusive directory lock); `close_repo_db` là **only** remover, chỉ
  sau khi acquire per-repo index lock + cancel in-flight runs.
- **`ShardedVectorIndex`** owned by `IndexEngine` behind `Arc<RwLock<>>`.
  `vector_search` take read guard; `warm_repo_shard` / `install_shard` take
  write guard.
- **`Settings`** owned by `Arc<RwLock<Settings>>` shared giữa `main`,
  `IndexEngine`, `AppState`, cả hai `McpHandler`s. Consumer task clones
  snapshot ở đầu mỗi iteration — API-key changes take effect ở **next
  trigger**. `PUT /api/config` writes to disk first, sau đó mutate handle.
- **Watchers** (`start_watcher` per repo) push `IndexTrigger` vào
  `trigger_tx`. Spawned both at boot và on `register_repo` (auto-add của repo
  user queries lần đầu qua MCP — xem `mcp.rs:677`).
- **`IndexEngine`** wrapped in `Arc`, shared via `AppState`; `Clone`-via-Arc
  everywhere.

**Settings mutation propagation:** `AppState::settings: Arc<RwLock<Settings>>`.
`PUT /api/config` writes atomically to disk (`write_settings_atomic`), sau đó
replaces in-memory `Settings`. Consumer task takes
`settings_handle.read().await.clone()` ở đầu mỗi `rx.recv()` iteration —
drop-on-snapshot, không held across await chain. Đây là lý do boot-frozen
`data_dir`/`embeddings_dir` (resolved trong main.rs trước `IndexEngine::start`)
không bao giờ re-derive từ `Settings` mid-run.

## 1.6 Worth-Copying Architectural Decisions

1. **`open_or_reset_index` self-healing** (`store/mod.rs:779`) với
   `remove_index_dir` là safety valve: try open → retry 30s cho stale LOCK →
   nếu thật sự fail thì remove dir + reopen 1 lần. Non-destructive vì
   `remove_dir_all` fails nếu live OS handle vẫn giữ LOCK.
2. **Per-repo open gate** (`store/mod.rs:640-649`) — `LazyLock<StdMutex<HashMap
   <String, Arc<tokio::Mutex<()>>>>>` inserted lazily per repo path.
   Double-check-under-gate pattern là cleanest fix cho "two callers race the
   exclusive directory lock."
3. **Boot-frozen `data_dir`/`embeddings_dir`** captured once trong `main.rs`
   (`main.rs:142-174`), với `Settings.data_dir` không bao giờ re-read ở
   runtime. Combined với precedence chain (CLI > env > Settings > default) và
   comment-explained decision anchor `embeddings_dir` to HOME rather than
   data_dir, đây là right way để make long-running service handle "settings
   change while running" safely.
4. **`SHARDED` vector index với `AtomicU64` recency** (`vector/sharded.rs:37-49`)
   — `search` takes only `&self` và bumps per-shard atomic stamp, nên LRU
   tracking không bao giờ serialise reads behind write lock. Explicit
   lock-order contract là loại invariant nên là comment-mandated design rule
   trong mọi project có multi-resource async state.
5. **MCP output-budget assembly** (`mcp.rs:50-121`) — `assemble_with_budget` +
   `merge_overlapping_blocks`: emit full content đến 48K-char MCP client
   ceiling, rồi header + first 120 chars + elision marker. 150-char footer
   reserve. Line-merge heuristic.

## 1.7 Anti-Patterns / Landmines

1. **Dual MCP handler types** (`mcp.rs:300, 474`) — `McpHandler` và
   `RepoMcpHandler` 90% identical, cả hai `#[tool_router]` macros. Nên là
   generic `<T: WithWorkspace>` hoặc accept param trong tool, ignore qua
   server-side filter. Đừng clone 200 lines macro-decorated impls.
2. **`IndexEngine::data_dir` và `embeddings_dir` là `pub` fields mutated
   nowhere** nhưng cũng **không** behind abstraction — `server::AppState`
   re-exposes chúng as `pub` (`server.rs:88, 94`). "Boot-frozen" invariant
   enforced by convention và comments, không by type. Nên wrap trong
   `BootConfig` newtype với private constructor.
3. **Std mutex `OPEN_GATES` vs tokio mutex `repo_locks`** coexist (`store/
   mod.rs:640` vs `indexing/mod.rs:119`). Code is correct, nhưng future reader
   phải verify lock-ordering across **2 mutex flavours + 3 `Arc<RwLock<>>`
   kinds**. Centralise on one mutex flavour hoặc build small `LockGraph`
   type documents order at the type level.
4. **`pipeline::run` được gọi với 7 optional `Some(...)` parameters**
   (`indexing/mod.rs:773-784`) — `trigger.changes`, `force_rebuild`,
   `Some(&vector_index)`, `Some(progress)`, `Some(&event_bus)`, `&key_hints`,
   `Some(cancel_token)`. Method signature crosses into
   `#[allow(clippy::too_many_arguments)]` territory. Group thành single
   `RunContext` struct. `run_query` trong `query/engine.rs:93` có cùng
   disease.

Xem [[../08-test-coverage|08-test-coverage]] cho module test coverage, [[03-indexing-pipeline|03-indexing-pipeline]] cho pipeline internals.
