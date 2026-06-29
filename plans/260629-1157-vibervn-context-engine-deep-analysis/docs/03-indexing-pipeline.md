# 03 — Indexing Pipeline

> Phần 3/14. 5 phases end-to-end: trigger → walk → parse → framework → embed → store → phase 2.

## 3.1 Pipeline Phases

| Phase | Entry function | File:Line | Input | Output |
|---|---|---|---|---|
| **Trigger / dispatch** | `run_consumer` | `indexing/mod.rs:608` | `IndexTrigger` (manual or watcher-driven) | calls `IndexPipeline::run` |
| **Walk** | `walk_repo_with` | `indexing/walker.rs:204` | repo path + ext/ignore lists | `Vec<String>` of indexable paths |
| **Parse** | `parse_one_file_with_frameworks` → `parse_one_file` | `indexing/pipeline.rs:2057, 2101` | file path | `ParseOutput::Parsed { symbols, chunks, raw_edges, mtime, size }` |
| **Framework extract** | `FrameworkRegistry::extract_edges` (5 active resolvers) | `indexing/frameworks/mod.rs:89` | file path, source, symbols | extra `Vec<RawEdge>` appended to `parsed.raw_edges` |
| **Embed** | `embed_parsed_file` | `indexing/pipeline.rs:2221` | parsed file + `VoyageClient` + `EmbeddingCache` | `EmbedFileResult { embeddings, hit_chunks, miss_chunks }` |
| **Store** | `flush_chunk_batch` / `flush_symbol_batch_native` / `flush_raw_edge_batch_native` / `upsert_file_meta` | `indexing/pipeline.rs:2507, 2557, 2617` + `store::ops` | accumulated batches | SurrealDB rows + `file_meta` (mtime/size/chunker_version) as crash-safe commit marker |
| **Phase 2 — resolve edges** | `resolve_edges_phase2` (full) / `resolve_edges_incremental` / `resolve_edges_from_ram` | `indexing/pipeline.rs:1441, 1838, 1682` | `raw_edge` table + symbol map | `calls` edges in DB; writes `edges_resolved` marker only on success |

Orchestrator: `IndexPipeline::run` (`indexing/pipeline.rs:326`). Streaming core:
`streaming_index` (`indexing/pipeline.rs:701`). `parse_file` ở
`parsing/mod.rs:86` là parse dispatcher; `Lang` enum ở `parsing/mod.rs:29`.

## 3.2 File Watching Model

- **Engine:** `notify` v8 + `notify-debouncer-full` v0.5 (`watcher.rs:4-5`).
- **Debounce window:** fixed 3 s (`watcher.rs:19`, `Duration::from_secs(3)`).
  Không user-configurable.
- **Cache:** `RecommendedCache` = `FileIdMap` trên macOS/Windows, `NoCache`
  trên Linux (`watcher.rs:23-26`).
- **Channel:** tokio mpsc, **`try_send` (drops on full)** (`watcher.rs:53`); 256-cap
  trigger channel created in `indexing/mod.rs:260`.
- **Spurious-event filtering:** KHÔNG làm trong watcher. `convert_events`
  (`watcher.rs:105`) là straight 1:1 translation của `DebouncedEvent` →
  `FileChange`. Không write-then-rename coalescing, không IDE-save heuristic.
- **Filtering xảy ra trong `run`:** `IndexPipeline::run` re-applies
  `walk_repo_with` + `ChangeFilter` + `filter_hidden_changes_with`
  (`pipeline.rs:434-449`) nên `cargo build` rewriting `target/*.exe` không
  pollute index. Debouncer's 3 s window là **only** barrier against event bursts.
- **Polling fallback:** 30 s polling loop nếu `notify`/`watch()` fails
  (`watcher.rs:88-103`).
- **Recursive:** `RecursiveMode::Recursive` (`watcher.rs:62`).

## 3.3 Incremental Indexing — Change Tracker

- **Marker location:** per-file row trong `file_meta` table (`store::ops::FileMeta`,
  fields: `path, mtime, size, chunker_version, chunk_count, repo`).
- **KHÔNG có commit marker file** — "commit marker" là `file_meta` row itself,
  written **sau khi** file's chunks durable (`pipeline.rs:1110-1124`).
- **Dedup logic:** `tracker::detect_changes` (`indexing/tracker.rs:38`). 3-way check:
  `mtime != indexed_mtime || size != indexed_size || chunker_version !=
  current_chunker_version`. Any mismatch → `Modified`.
- **Stale chunker recovery:** bumping `CHUNKER_VERSION` trong
  `parsing/chunker.rs:37` (currently `2`, was `1`) làm mọi file với old version
  re-chunk trên next trigger — không cần DB schema bump.
- **Crash safety anchors:**
  - `file_meta` (per-file, full rebuild)
  - `edges_resolved` key trong `index_meta` (Phase 2 sentinel)
  - RAM-path fallback để detect RAM-only `raw_edge` loss
    (`pipeline.rs:472-492`)

## 3.4 Parsing Strategy

- **Dispatch:** `parse_file` trong `parsing/mod.rs:86` làm single `match` trên
  `Lang` (20+ language arms), mỗi arm binding right `tree_sitter_*::LANGUAGE`
  và extractor fn. Không registry, không `dyn Trait` — straight match.
- **Parser instance:** một `tree_sitter::Parser::new()` per file bên trong
  `parse_with_tree_sitter` (`parsing/mod.rs:278`). Không pooled. Tree-sitter
  `Parser` cheap để construct; per-file OK.
- **Tree consumed in-place** vì `tree_sitter::Tree`/`Node` là `!Send` —
  `chunk_file_ast` được gọi **inside** cùng closure (`parsing/mod.rs:290`).
- **Extractors:** 22 cái (`extract_python`, `extract_javascript`, ...,
  `extract_liquid`) — all `fn(&str, &str, &tree) -> (Vec<Symbol>, Vec<RawEdge>)`.
  Cùng signature; dispatched qua generic `parse_with_tree_sitter` helper
  (`parsing/mod.rs:269`).
- **Per-language shape:** mọi extractor walks AST manually cho symbols
  (function/method/class/etc. với scope path) và calls. Generic helper làm
  parse + chunk wiring, extractors chỉ làm AST→Symbol/RawEdge. C/C++ có
  special `parse_with_tree_sitter_c_cpp` cũng returns imports `HashMap`
  (`parsing/mod.rs:301`).
- **Failure mode:** parser-init failure hoặc `parse()` returns `None` → fall
  back về `chunk_file` (source-only line-window chunking, no symbols,
  `parsing/chunker.rs:265`).

## 3.5 Chunking (cAST Algorithm)

`parsing/chunker.rs` (24KB):

- **2 strategies:**
  - `chunk_file_ast` (`chunker.rs:243`) — recursive split-then-merge, AST-aware,
    sized by non-whitespace char count (`MAX_CHUNK_NONWS = 1500`, `chunker.rs:25`).
    cAST algorithm (arXiv 2506.15655).
  - `chunk_file` (`chunker.rs:265`) — source-only line-window fallback (no tree
    available).
- **Internal mechanism:**
  - `split_node` (recursive greedy child packing với `nonws` O(1) prefix-sum,
    `chunker.rs:104`)
  - `merge_spans` (mandatory — split-only degrades nDCG 85→66, `chunker.rs:175`)
  - `symbol_ref_for` — deepest-enclosing rule với `None` cho true straddles
    (`chunker.rs:220`)
- **Version storage:** `pub const CHUNKER_VERSION: i64 = 2;` (`chunker.rs:37`).
  Stored trong `file_meta.chunker_version`; compared trong
  `tracker::detect_changes` để force lazy re-chunking.
- **Blank-chunk filter** ở `spans_to_chunks` (`chunker.rs:318`) để preserve
  1:1 alignment giữa `Chunk` list và `Vec<f32>` embeddings (filter ở embed
  layer sẽ desync the zip).

## 3.6 Framework Extraction

Tất cả ở `src/indexing/frameworks/`, registered trong `frameworks/mod.rs:58-64`.
Trait: `FrameworkResolver { name, detect, extract_edges }`. Detection runs
**once per run**, cached cho session.

| Resolver | File | Detection signal | Edge type produced |
|---|---|---|---|
| `ReactResolver` | `react.rs:14` | `package.json` contains `"react"` | `Calls` từ containing fn to capitalized JSX tag (`<UserProfile />` etc.) |
| `ExpressResolver` | `express.rs:14` | `package.json` contains `"express"` | `Calls` tới handler trong `router.get/post/.../use(path, handler)` |
| `DjangoResolver` | `django.rs:13` | `.py` imports `django` hoặc `manage.py`/`settings.py` mentions django | `Calls` từ `urls.py` tới view trong `path('route', view)` (function + CBV `as_view()`) |
| `SpringResolver` | `spring.rs:14` | `.java` imports `org.springframework` hoặc `pom.xml`/`build.gradle` mentions spring-boot | `Uses` cho `@Autowired FieldType`; `Calls` cho `@XxxMapping` → method |
| `GoGinResolver` | `go_gin.rs:14` | `go.mod` contains `github.com/gin-gonic/gin` | `Calls` từ `r.GET/POST/...` tới handler; splits qualified refs (`handlers.ListUsers`) thành `name` + `import_path` hint |

Tất cả dùng static `LazyLock<Regex>` cho patterns. Tất cả produce `RawEdge` với
`EdgeTarget::Unresolved { name, import_path?, qualifier? }`. Resolver emission
appended tới `parsed.raw_edges` trong `pipeline.rs:2080-2092`.

## 3.7 Phase 2 — Edge Resolution

Algorithm ở `IndexPipeline::resolve_edges_phase2` (`pipeline.rs:1441`):

1. **Pass 1 — symbol map load:** `SELECT meta::id(id) AS fqn, file, name,
   line_start, line_end FROM symbol` → `HashMap<name, Vec<SymbolWithPos>>`
   (~3.3 MB ở 27K symbols). Pre-sorted buckets cho deterministic tie-breaking
   (`pipeline.rs:1473-1484`).
2. **Drop 4 `calls` indexes** trước bulk insert (`pipeline.rs:1490-1496`).
3. **Pass 2 — compound keyset scan over `raw_edge`:** outer loop là
   `GROUP BY from_file ORDER BY from_file LIMIT 256` (dùng
   `idx_raw_edge_from_file` cho O(log N) seek); inner loop fetches tất cả rows
   cho file trong một query (`pipeline.rs:1522-1614`).
4. **Per-page in-memory resolve** qua `resolve_raw_edge_page_from_map`
   (`pipeline.rs:2018`): lookup name trong pre-built map, run
   `select_best_candidate` (5-level priority, `pipeline.rs:1280`).
5. **Flush** tại `EDGE_RELATE_BATCH_SIZE = 8192` (`pipeline.rs:40`).
6. **Rebuild indexes synchronous** sau bulk insert (`pipeline.rs:1639-1645`).
7. **Stamp `edges_resolved = "1"`** trong `index_meta` chỉ sau tất cả above
   (`pipeline.rs:1650`).

**`select_best_candidate` 5-level priority** (`pipeline.rs:1280-1380`):
- L0: full `resolve_import_path` (TS/JS alias, Python module path, Go
  `go.mod` strip, Rust `crate::`/`self::`/`super::`).
- L1: `candidate.file.ends_with(import_path)` (subdirectory imports).
- L2: bare import → same parent dir như `from_file`.
- L3: same-file match.
- L4: first in sorted bucket (lexicographic file path).
- Tất cả levels prefer non-generated files (per `parsing/generated.rs`).

**Re-export / barrel chasing:** `chase_reexports` (`import_resolver.rs:410`) —
khi L0 resolves tới `index.ts`/`__init__.py`, looks cho sibling file matching
target symbol name. Depth-capped tại 8.

**RAM fast path** cho full rebuilds: `resolve_edges_from_ram`
(`pipeline.rs:1682`) — buffers tất cả `raw_edges` trong RAM (cap
`MAX_RAM_EDGES = 200_000`, `pipeline.rs:971`), skip DB write+scan round-trip
(~27s saved trên benchmark repo). Crash-safety: nếu process dies sau Stage 3
nhưng trước Phase 2, `run()` detects `edges_resolved absent + raw_edge empty +
file_meta present` và forces full rebuild (`pipeline.rs:472-492`).

**Incremental path:** `resolve_edges_incremental` (`pipeline.rs:1838`) —
captures pre-delete callers **trước** `delete_files_data_bulk` (nên "removal
direction" preserved), builds `resolve_set = changed ∪ pre_delete_callers ∪
new-name-targeters`, deletes chỉ các calls rows touching set, re-resolves
chỉ những raw_edges.

**Data structure:** `HashMap<String, Vec<SymbolWithPos>>` cho symbol lookup;
không general-purpose graph (graph materialized như `calls` table rows trong
SurrealDB).

## 3.8 Concurrency

- **Backend split:** tokio (async I/O) + rayon (CPU), bridged qua
  `tokio::task::spawn_blocking`.
- **Rayon usage:**
  - **Parse stage** (`pipeline.rs:749-758`): `files_owned.par_iter().for_each(…)` —
    mỗi file parsed song song, results sent vào bounded `mpsc::channel(
    PARSE_CHANNEL_CAP=64)`. `blocking_send` cung cấp backpressure.
- **Async usage:**
  - **Embed stage** (`pipeline.rs:788-927`): `futures::stream::buffer_unordered
    (embed_concurrency)` — `embed_concurrency = settings.embed_concurrency *
    api_keys.len()` (`mod.rs:752-754`). Concurrent Voyage API calls.
  - **Writer stage** (`pipeline.rs:981+`): drains `embed_rx`, batches symbols
    (2048), chunks (512), raw_edges, file_metas.
  - **Phase 2:** async SurrealDB queries, nhưng resolution work in-memory CPU.
- **Sync↔async boundary:** `parse_with_tree_sitter` closure là hard wall —
  `tree_sitter::Tree`/`Node` là `!Send`, nên chunking (only consumer của tree)
  MUST chạy bên trong rayon `spawn_blocking` worker (`parsing/mod.rs:288-291`).
  Documented explicit ở cả `parsing/mod.rs` và `chunker.rs:233-238`.
- **Bounded channels:** `PARSE_CHANNEL_CAP=64`, `EMBED_CHANNEL_CAP=64`
  (`pipeline.rs:51, 54`). `PARSE_CHANNEL_CAP × chunks_per_file` là peak
  inflight bound, **independent** của repo size (`pipeline.rs:50-51` comment).
- **Per-repo serialisation:** `IndexEngine::get_repo_lock` (`mod.rs:477`) —
  `tokio::sync::Mutex` per repo nên chỉ một run mutates repo's DB tại một
  thời điểm. `run_consumer` holds this cho whole iteration.
- **Embed error fan-in:** shared `Arc<std::sync::Mutex<Option<String>>>`
  (`pipeline.rs:774-775`); khi Stage 2's Voyage call fails, write error và
  cancel token, sau đó post-loop re-check distinguishes `Cancelled` vs
  `EmbeddingFailed` (`pipeline.rs:1175-1183`).

## 3.9 Worth-Copying Decisions

1. **Chunker version là stored build constant, không phải DB schema bump**
   (`chunker.rs:37` + `tracker.rs:67`). Bumping `CHUNKER_VERSION` là "bake a
   freshness flag vào commit marker" trick cho phép evolve chunking algorithms
   without migrations. Emulate cho *any* derived/computed per-file artifact
   (embeddings, summaries).
2. **Crash-safety qua ordered writes: chunks trước `file_meta`**
   (`pipeline.rs:1054-1124`). `file_meta` (với mtime + chunker_version) là
   commit marker; deferred cho đến khi chunk batch chứa file's last chunk
   flushes. `pipeline.rs:1110-1124` comment explains. Đây là exact
   "write-ahead-log at the row level" pattern — steal it.
3. **Streaming pipeline với bounded channels + `buffer_unordered` cho IO-bound
   concurrency + `par_iter` cho CPU-bound work** (`pipeline.rs:744-928`).
   3 stages, 2 channels, peak inflight = `O(channel_cap × chunks_per_file)`
   regardless of repo size. Async-stream / spawn_blocking / rayon split
   correctly placed ở language's `Send` boundary.
4. **Drop-then-bulk-insert-then-rebuild indexes cho graph edges**
   (`pipeline.rs:1490, 1639`). Classic bulk-load trick (bears repeating):
   turning N×O(log N) per-insert thành một O(N) write + một O(N log N) build.
   Same pattern cũng applied tới chunks (`idx_chunk_file` drop+rebuild path,
   `pipeline.rs:159-188` perf counters).
5. **5-level candidate selection với `prefer_non_generated` per level**
   (`pipeline.rs:1280-1380`). Cascade là language-agnostic và non-generated
   fallback cheap. Structure composes well — future project có thể swap
   language resolvers trong `import_resolver.rs` without touching cascade.
   `chase_reexports` là clean bolt-on.

## 3.10 Anti-Patterns / Landmines

1. **`pipeline.rs` là 209 KB / ~5000 dòng trong một file** với `IndexPipeline`
   struct, free fns, helper structs, stage flushers all mixed. New contributor
   không navigate được; `// ───` ASCII banners (`pipeline.rs:530, 688, 1265,
   1975`) đang làm việc của modules. **Split thành `parse.rs` / `embed.rs` /
   `store.rs` / `phase2.rs`** trước khi grows further.
2. **Embed là hard fork trong pipeline:** `voyage` không optional, không
   `None`-embed mode cho offline/cached-only operation, và cả pipeline abort
   trên first embed error (`pipeline.rs:859-872`). Test harnesses và local
   dev cần `--no-embed` hoặc fake-client path; `PipelineAbort::EmbeddingFailed`
   enum tồn tại, nên design clearly anticipated nhưng chưa finished.
3. **Churn trong `chunker` strategy versioned bởi global `const
   CHUNKER_VERSION`**, không bởi chunk shape itself. Bất kỳ change trong
   cAST merge rule, budget, hoặc symbol linkage rule đều yêu cầu manual bump
   *và* invalidates mọi file trong mọi index. cAST history note tại
   `chunker.rs:32-36` là only documentation; content-hash of chunker params sẽ
   self-documenting.
4. **Hai language dispatch sites:** `parse_file` ở `parsing/mod.rs:86` là
   real entry, nhưng `detect_language` ở `pipeline.rs:1312` (bên trong
   `select_best_candidate`) là *second* copy of language extension table
   (`Lang` enum trong `parsing/mod.rs:29` là canonical). Drift risk: thêm
   `.vue`/`.astro` etc. yêu cầu updating `Lang` enum *và* nothing else here
   — nhưng nếu new arm added tới `parse_file`'s match, `select_best_candidate`
   won't notice cho đến khi new `Lang::*` variant handled. Không có
   exhaustiveness check tại call site.
5. **Raw edges buffered trong RAM chỉ trong full-rebuild path**
   (`pipeline.rs:964-978, 1682`). Incremental runs *always* tới DB. Hai paths
   có *different* crash-safety contracts: full-rebuild có thể mất edges trên
   crash (recovered bởi force-rebuild on next run, `pipeline.rs:472-492`);
   incremental is crash-safe anchor. Reader của `streaming_index` phải track
   which path code is on bằng reading surrounding comments. Nên typed enum
   (`EdgeSink::Ram | EdgeSink::Db`) trên streaming call.
6. **Không global lock quanh `resolve_edges_phase2` cho cùng repo:** per-repo
   `get_repo_lock` (`mod.rs:477`) prevents concurrent runs của *cùng* repo,
   nhưng 2 indexers cùng `data_dir` (e.g. stale process + fresh one) sẽ cả
   hai gọi `DROP INDEX` / `REBUILD` / `RELATE` và corrupt `calls` table.
   Pipeline trusts trigger channel và cancel token cho single-instance
   safety, nhưng không có file-lock hoặc process-ID check.

Xem [[04-query-embed-store]] cho query side, [[09-performance-scaling]] cho hot path costs.
