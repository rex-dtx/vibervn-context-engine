# 09 — Performance & Scaling

> Phần 9/14. Hot path on query, vector search scaling, in-memory shard cap, RocksDB memory bounds, per-channel bounds, embed concurrency, BFS complexity, rerank prompt size, Phase 2 incremental cost, frame watcher overhead, bottleneck inventory, complexity inventory.

## 9.1 Hot Path on Query — `codebase-retrieval`

Trace: `mcp.rs:387` → `run_codebase_retrieval` (`mcp.rs:630`) → `do_query` →
`query::engine::run_query_with_filters` (`engine.rs:117`).

Per-call stages:

| Stage | Location | Work |
|---|---|---|
| **0. Filter parse** | `engine.rs:135` `parse_query_filters` | O(query length), pure string work |
| **1. Embed query** | `engine.rs:144` → `voyage_client.embed_query` | 1 HTTP call tới Voyage AI. Latency = external API RTT (~200-800ms). |
| **2. Vector search** | `engine.rs:154` → `index_engine.vector_search` (`mod.rs:509`) | `engine.rs:154` asks cho `top_k * 2` (20 default). Cold-repo single-repo path blocks on `warm_repo_blocking` (line 525, `warm_wait` capped at `mcp_index_wait_secs` default 50s). Sau đó `sharded::search` (`sharded.rs:243`). |
| **3. Fetch chunk content** | `engine.rs:191-204` loop over `filtered` | **N SurrealDB queries**, sequential (`for sr in &filtered`), mỗi một query per top-K chunk. Ở `top_k=10 → 20` queries. Mỗi is a `SELECT content, symbol_ref FROM chunk WHERE file=$file AND line_start=$ls AND line_end=$le LIMIT 1`. |
| **4. Graph expand** | `engine.rs:226` → `graph_expand.rs:64` | BFS up to `MAX_DEPTH=2`, `MAX_BONUS_CHUNKS=30`. Worst case: ~2 SurrealDB queries per BFS node × ~30 nodes = ~60 queries. |
| **5. Caller/callee stats** | `engine.rs:260` `fetch_caller_stats_batch` | **2 DB queries per chunk** × `top_k` (10 default → 20 queries). Không batching — all sequential. |
| **6. Merge** | `engine.rs:246` `merge_chunks` | O(N log N) HashMap dedup + group-by-file merge. Bounded bởi `top_k`. |
| **7. Disk read** | `engine.rs:254-257` `read_lines_from_fs` | O(top_k) full file reads qua `fs::read_to_string` (engine.rs:907). Cho mỗi: read entire file, sau đó index vào a `Vec`. Files chưa yet in memory = full file I/O per chunk. |
| **8. Rerank** | `engine.rs:284` → `reranker::rerank` (`reranker.rs:48`) | 1 LLM API call. Single-shot, non-agentic. |

**Time spent:** dominated bởi `embed` (network RTT) + `rerank` (LLM RTT) +
N×DB queries (steps 3, 5) + file reads (step 7). Actual dot-product math is
negligible (<100ms at 500K×1024).

## 9.2 Vector Search Scaling

`vector/mod.rs:193-204` — `par_chunks(self.dim).enumerate().map(|(_, row)|
dot_product)`. `select_nth_unstable_by` (O(N)) + `sort_unstable_by` (O(k log k)).

**500K × 1024 dims reality check:**
- Rayon's `par_chunks` work-stealing: ≈ num_logical_cores parallel slices.
  Trên a 16-core box, mỗi slice ≈ 31K × 1024 dot products = 31M FMAs.
- `dot_product` tại `vector/mod.rs:297` is **plain scalar**
  `iter().zip().map(|x, y)| x*y).sum()` — **không SIMD**. LLVM may auto-
  vectorize, nhưng không `#[target_feature]` hoặc `std::simd`.
- Wall time trên typical M-class CPU: ~200-400ms, **không phải sub-100ms** as
  README claims. Claim is optimistic — sub-100ms holds chỉ ở much smaller
  scale (e.g. 100K × 1024) hoặc với explicit SIMD.

**Scaling cliff:** search is O(N × dim), fully parallelized over rayon. Wall
time grows linearly in `N` × `dim`. The `select_nth_unstable_by(k-1, ...)` is
O(N) per shard, nên global fan-out tại `sharded.rs:264` does `O(R × N/R ×
top_k)` = O(N × R) where R = resident shards. Concatenation + sort ở
`sharded.rs:282-287` is O(R × top_k log(R × top_k)). Cho `R × top_k ≪ N`, this
is fine.

## 9.3 In-Memory Shard Cap (`vector_resident_cap_mb`)

Default 2048 MB (`config.rs:113-120`, `config.rs:271`). Converted tới bytes ở
`indexing/mod.rs:264-266` (`saturating_mul(1024*1024)`), passed tới
`ShardedVectorIndex::new(cap_bytes)`.

**Enforcement:** `sharded.rs:159-180` `evict_to_cap`. Eviction rule: lowest
`last_touched` AtomicU64 stamp (true LRU), nhưng never evicts just-installed
`protected` repo hoặc anything trong `active`.

**Shard math:** per-shard bytes = `len × dim × 4`. Ở `dim=1024`: 1 shard = 1M
vectors = 4 GB. Vậy a default cap of 2 GB fits **zero** full 1M-vector repos —
chỉ fractional shards. A 500K-vector repo = 2 GB exactly. **A single 500K-
chunk repo hits the cap by itself; second repo forces eviction.**

**Single-repo overflow:** `sharded.rs:74-76` comment is explicit: "a single
shard larger than the cap is still kept — we never evict a repo tới below
usefulness, và never evict the repo just installed." Verified ở
`sharded.rs:173-178`: if everything is protected/active, `victim = None` →
`break` → cap exceeded without eviction. **Không có hard ceiling** — một
5GB repo có thể OOM a 4GB host.

**Eviction granularity:** coarse — drops entire repo shards. Không thể keep
a 50% slice of a mega-repo resident.

## 9.4 RocksDB Memory Bounds

`main.rs:68-84` `set_rocksdb_memory_bounds()` pins:
- `BLOCK_CACHE_SIZE = 128 MiB` (shared LRU)
- `WRITE_BUFFER_SIZE = 32 MiB × MAX_WRITE_BUFFER_NUMBER = 2` → **64 MiB / DB**
- Block cache is global, write buffers are per-DB.

`main.rs:91` comment explicitly says SurrealDB defaults tới `~31 GiB` cache +
up tới `128 MiB × 8 = 1 GiB` write buffers per DB trên a 64 GiB host. The
pinning is correct.

**Per-repo RAM:** `128 MiB (shared) + 64 MiB × repo_count`.

**16 GB host ceiling:** available cho RocksDB depends trên what else runs
(vector shards default 2 GB + OS + app ≈ 1 GB ≈ 3 GB). Conservative RocksDB
budget = ~13 GB.
- Repo count = `(13000 MiB - 128 MiB) / 64 MiB ≈ 201 repos` for write
  buffers alone.
- Block cache is shared ở 128 MiB regardless.

Vậy: **~200 repos trên 16 GB** is the upper bound. Reality is lower vì of
vector shards, surrealkv compaction buffers, page cache, và OS overhead.
Practical ceiling: ~50-100 repos.

**One DB per repo:** confirmed ở `main.rs:193`
`repo_dbs: Arc<RwLock<HashMap<String, Surreal<Db>>>>`. Không pooling, không
sharing.

## 9.5 Per-Channel Bounds (Pipeline)

`pipeline.rs:51` `PARSE_CHANNEL_CAP = 64`, `pipeline.rs:54`
`EMBED_CHANNEL_CAP = 64`. Cả hai `tokio::sync::mpsc::channel` (`pipeline.rs:745,
767`).

**Peak inflight RAM:**
- Parse channel: up to 64 `ParseOutput` items = 64 × `ParsedFile`
  (`pipeline.rs:100-112`) = 64 × (path + Vec<Symbol> + Vec<Chunk> +
  Vec<RawEdge> + Instant). Cho typical 500-line / ~20-chunk file với ~10
  symbols và ~20 raw edges: ~30-50 KB/parsed file → **~2-3 MB peak**.
- Embed channel: up to 64 `EmbeddedFile` items (`pipeline.rs:115`) — **adds
  `Vec<Vec<f32>>`** = 64 × chunks × dim × 4 bytes. Ở 20 chunks × 1024 dims
  = ~5 MB per file × 64 = **~320 MB peak** at default voyage-4-lite. Ở
  voyage-3-large (2048 dims): ~640 MB.
- Channel send ở `pipeline.rs:917-920` dùng `try_send` với backpressure
  (`send().await` — blocks if full).

**Bounded:** Yes — total = `O(channels × chunks_per_file × chunk_size_bytes)`
as claimed ở `pipeline.rs:50`.

## 9.6 Embed Concurrency

`config.rs:106-111` `default_embed_concurrency = 16`. Multiplier:
`embedding.embed_concurrency × api_keys.len()` (`config.rs:130`).

**Default `api_keys.len()`:** typically 1-3 trong real configs. Default total
in-flight = `16 × 3 = 48` concurrent embedding batches.

**Không auto-tuning:** default of 16 is hard-coded với không rationale tied tới
voyage-4-lite's documented rate limits (it handles ~300 RPM per key). Ở 16
concurrent với batch_size=128, mỗi key sends ~16 batches, mỗi up tới ~7.5 RPM
sustained — well within limits. Default is conservative.

**What scales:** linear in keys. **What doesn't:** không adaptive backoff; nếu
voyage returns 429, không visible backpressure adjustment (relies trên per-key
Tokio semaphore).

## 9.7 BFS Complexity (`graph_expand`)

`graph_expand.rs:53-54`: `MAX_DEPTH=2`, `MAX_BONUS_CHUNKS=30`,
`SCORE_FLOOR=0.15`. Score multipliers: callers ×0.6, callees ×0.5.

**Worst-case BFS:** per BFS node = 2 SurrealDB queries
(`query_callers` + `query_callees` tại `graph_expand.rs:210`/`245`). Mỗi has
`LIMIT 20`. Vậy per node ≤ 40 candidates; depth-multiplied BFS ≤ 30 nodes × 40
candidates = ~1,200 candidates max, NHƯNG limited bởi `MAX_BONUS_CHUNKS=30`
enforced ở line 115 (`break 'outer`).

**Actual query count:** bounded bởi `30 + initial overlap query` ≈ 31 + 30 × 2
= **~91 queries worst case** (mỗi chunk's overlap + mỗi expanded chunk's
caller/callee). Ở typical latency (1ms local SurrealDB), this is ~90ms. Với
1M-edge graph: still indexed (idx_calls_in_name / idx_calls_out_name per
`graph_expand.rs:211,246`) nên latency is bound bởi index scan, không graph
size.

**BFS is O(expanded_nodes × edges_per_node) not O(edges).** `global_seen`
HashSet ở `graph_expand.rs:74` dedupes FQNs, nên 2-hop fan-out từ một chunk
is bounded: `20 + 20² = 420` per chunk, capped tới 30 across all chunks.
**Không O(2^depth) blowup** vì per-node `LIMIT 20` + per-chunk
`MAX_BONUS_CHUNKS=30` enforce hard caps.

**Latency trên 1M-edge graph:** still O(30 × indexed_lookup) ≈ 30-60ms.
Index handles it.

## 9.8 Rerank Prompt Size

`reranker.rs:142` `truncate_content(raw, 100)` keeps first 50 + last 50 lines.

**Top-K=10 reranker payload:** 10 chunks × (up tới ~100 lines × ~80 chars/line)
= **~80 KB content** + system prompt (~2 KB) + JSON envelope. Realistic total:
**~100-150 KB per LLM call**.

**Latency:** Gemini 1.5 Flash / GPT-4o-mini ở 100 KB prompt → 0.5-3s response
time. **The reranker is the single largest latency contributor trong query
pipeline.**

**Không batched rerank:** một LLM call per query. Không streaming. Không
pre-filter trước LLM.

**Agentic RAG mode** (`reranker::rerank_agentic`, starts ở `reranker.rs:200`):
up tới `agentic_rag_max_turns = 9` (`config.rs:169`) iterations of `query` +
`add_chunks` tool calls. **Up tới 9 × (1 embed + 1 vector search + 1 graph
expand + 1 LLM call) ≈ 9× the latency of single-shot.**

## 9.9 Phase 2 Incremental Path

`pipeline.rs:1838` `resolve_edges_incremental`. Steps:

1. Build `resolve_set = changed_files ∪ pre_delete_callers` (line 1858).
2. Direction-2 expansion qua `raw_edge.to_name` (lines 1876-1908) — **1 query
   cho changed-files' symbol names + 1 query cho matching raw_edge from_files**.
3. DELETE scoped calls (line 1919) — `O(resolve_set)` indexed delete.
4. Keyset-paginated re-resolution qua `raw_edge WHERE from_file IN $files`
   (lines 1931+) — bounded bởi edge count trong affected files.

**1-file change cost trong 100K-file repo:**
- 1 query cho symbol names trong the 1 file (~instant).
- 1 query cho name-based fan-in: bounded bởi `raw_edge.to_name` match count
  (typically 0-10 callers).
- 1 DELETE trên `calls WHERE in_file IN $files OR out_file IN $files`
  (typically 0-50 rows).
- 1 paginated re-resolve qua the 1 file's raw_edges (typically 0-50 rows).

**Total: ~3-5 queries, sub-50ms.** Keyset pagination ở line 1927 dùng
`WRITE_BATCH_SIZE = 512` (line 34) as the page size.

**Hidden cost:** the `name_expansion` query ở line 1897
(`raw_edge WHERE to_name IN $names GROUP BY from_file`) có thể grow nếu a new
symbol has a common name like `parse` hoặc `init` — could match hundreds of
files. Mitigation: `pre_delete_callers` list is already captured trước delete
(line 675), nên cascading re-resolves don't recursively expand.

## 9.10 Frame Watcher Overhead

`watcher.rs:19` 3-second debounce window via `notify-debouncer-full`. Channel:
`tokio::sync::mpsc::channel::<IndexTrigger>(256)` (`indexing/mod.rs:260`).

**Trigger channel capacity:** 256.

**Drop policy:**
- `watcher.rs:53`: `let _ = tx_inner.try_send(trigger);` — **non-blocking,
  drops on full** với comment "will recover on next poll."
- `watcher.rs:98` polling fallback: `tx.send(trigger).await` — **blocking** on
  full.

**Burst absorption:** 256 events buffered trước drop. Typical git operations
(checkout, merge) produce 50-500 events. A massive `git checkout` hoặc
`npm install` với 10K+ file changes would overflow và silently drop.
**Dropped events mean stale index cho đến next 30-second polling tick or
filesystem event.**

**Polling fallback ở 30s** (`watcher.rs:92`): always sends a full incremental
trigger (`changes: None`), bounded chỉ bởi consumer throughput. Nếu consumer
is slow, trigger channel fills; `send().await` blocks polling task nhưng does
NOT lose data.

## 9.11 Bottleneck Inventory (Top 5)

| # | Bottleneck | Location | Why |
|---|---|---|---|
| **1** | **Sequential N+1 DB queries cho chunk content + caller stats** | `engine.rs:191-204` (step 3), `engine.rs:260` (`fetch_caller_stats_batch`, line 633 loop) | Ở `top_k=10`: 20 + 20 = 40 sequential SurrealDB round-trips. Could be 1-2 batched queries. **Wall-time cost ở 1ms local DB = 40-200ms wasted.** |
| **2** | **Rerank LLM call (single-shot, blocking)** | `engine.rs:284` → `reranker::rerank` → `client.complete().await` (`reranker.rs:175`) | 0.5-3s cho ~100 KB prompt. Single point of failure; nếu LLM is slow/down, query hangs. |
| **3** | **Cold-repo warm blocks query** | `indexing/mod.rs:525` `warm_repo_blocking` với `warm_wait` timeout | First query tới a cold repo triggers `load_from_db` (`vector/mod.rs:222`) which scans entire `chunk` table. Cho 500K-row repo: 0.4-1.1 GB deserialize spike + 200-1000ms blocking trên first request. |
| **4** | **Filesystem reads per chunk** | `engine.rs:254-257` `read_lines_from_fs` (`engine.rs:907`) | `fs::read_to_string` reads ENTIRE file sau đó indexes. Cho top-K=10 chunks từ same file: 10 reads of same file. Không cache; không mmap. |
| **5** | **Shard search dùng không SIMD + dot product is scalar** | `vector/mod.rs:297` | Ở 500K×1024: ~200-400ms vs. <100ms với packed_simd/avx2 dot product. README claim "sub-100ms cho 500K×1024" is unverified trên commodity hardware. |

## 9.12 Complexity Inventory (Linear vs Quadratic)

| Op | Complexity | Location |
|---|---|---|
| **Vector search (per shard)** | O(N × dim) parallel | `vector/mod.rs:193-204` |
| **Sharded fan-out** | O(R × top_k) — R resident shards, sorted ở `sharded.rs:282-287` | `sharded.rs:243` |
| **Merge dedup + group** | O(N) HashMap; O(N log N) sort | `merger.rs:38-54, 139-144` |
| **Merge "drop contained"** | **O(N²)** | `merger.rs:124-133` — `survivors.iter().any(...)` inside loop. Cho N survivors per file: O(N²). **Nhưng N per file ≪ top_k, nên bounded trong practice.** |
| **BFS graph expand** | O(MAX_BONUS_CHUNKS × LIMIT 20) = **O(600)** worst case | `graph_expand.rs:111-158` |
| **Caller stats batch** | O(top_k × 2 queries) sequential | `engine.rs:633-647` |
| **Filter apply** | O(N × |filters|) | `engine.rs:495-540` |
| **Rerank prompt build** | O(top_k × truncate_lines) = O(top_k × 100) | `reranker.rs:128-149` |
| **Rerank LLM call** | O(prompt_size) network latency | `reranker.rs:175` |

**Nested loops trong hot paths:**
- `merger.rs:58-114`: file-group → inner sort + merge loop → inner merge-
  condition. **Two-level nhưng linear** vì mỗi file group is bounded bởi
  `top_k`.
- `merger.rs:124-133`: `for candidate in merged { survivors.iter().any(...) }`
  — **O(N²) inside per-file group**. Per-file candidate count bounded bởi
  `top_k`, nên practical impact = `top_k²` comparisons ≈ 100 ops.

**README claim "no O(n²) paths":** **partially false.** `merger.rs:124-133` is
textbook O(N²) containment check. It's bounded bởi `top_k` nên practically
fine, nhưng README's blanket claim is misleading. Không unbounded O(N²) paths
exist trong query hot path.

**Không nested DB-query loops** mà compound: N+1 trong `engine.rs:191-204` is
linear trong result count, không quadratic.

## 9.13 Worth-Defending Decisions

1. **Per-repo sharded vector index với resident-byte cap + LRU eviction
   (`sharded.rs:65-180`).** Search runs trên `&self` với atomic recency
   stamps, nên concurrent reads don't serialize. Cap + LRU prevent unbounded
   growth without sacrificing hot-repo locality. This is the load-bearing
   memory-bounding pattern.
2. **Flat row-major `Vec<f32>` storage với `par_chunks(dim).enumerate()`**
   (`vector/mod.rs:44-50, 193-204`). Cache locality beats `Vec<Vec<f32>>`;
   enables rayon parallelism without boxing per-row. Dual-format loader tại
   `vector/mod.rs:235` (handles old array<float> + new packed bytes) avoids
   forcing a full migration trên read.
3. **Lock-order discipline: `repo_dbs` → `vector_index`, never reverse**
   (`sharded.rs:16`, `indexing/mod.rs:506-508`). Documented và enforced.
   Single-flight warm locks ở `indexing/mod.rs:559-602` prevent thundering-
   herd trên cold-repo first query.
4. **Bounded mpsc channels trong indexing pipeline** (`pipeline.rs:51,54`) với
   explicit backpressure comment (`pipeline.rs:50`). O(channel_cap × chunks_
   per_file) RAM, không O(repo). Streaming pipeline doesn't scale linearly với
   repo size.
5. **RocksDB memory pinning trước bất kỳ datastore opens** (`main.rs:68-84`).
   `set_rocksdb_memory_bounds()` runs trước `IndexEngine::start` và dùng
   `LazyLock`-safe env-var reads. Repo-count-stable: total RocksDB RAM grows
   linearly với repo count, không super-linearly. The comment ở `main.rs:54-67`
   documents the WHY (SurrealDB's RAM-derived defaults are catastrophic cho
   an always-on local server).

## 9.14 Anti-Patterns / Landmines

1. **`fetch_chunk_content` và `fetch_caller_stats_batch` issue N+1 sequential
   SurrealDB queries** (`engine.rs:191-204, 633-647`). Không
   `IN ($files, $line_ranges)` batching. Ở top_k=10 + 30 expanded + caller
   stats: ~60-80 round-trips. Nên batch vào single query per stage. Latency
   waste: 50-150ms per query.
2. **`dot_product` is scalar** (`vector/mod.rs:297`). Plain
   `iter().zip().map(|x,y)| x*y).sum()` — không `std::simd`, không
   `target_feature`. README's "sub-100ms cho 500K×1024" is unverified. Manual
   `packed_simd` hoặc `core::arch::x86_64::*` dot product would 3-5x
   throughput.
3. **Cap-1GB single-shard có thể OOM the host** (`sharded.rs:74-76` explicit
   comment). "Single shard larger than cap is still kept" rule means a single
   5GB repo trên a 4GB-cap setting sẽ not be evicted. Cap is **best-effort,
   not a hard ceiling.** A malicious hoặc accidentally-large repo có thể blow
   the budget.
4. **Watcher `try_send` drops events silently** (`watcher.rs:53`). Channel
   capacity 256. A 10K-file `git checkout` overflows và drops back of burst.
   The comment "will recover on next poll" is misleading vì the polling
   fallback is 30s. **A full burst during checkout → stale index for up tới
   30 seconds.** Either coalesce into one trigger hoặc use unbounded channel
   với coalescing.
5. **`read_lines_from_fs` reads entire file sau đó indexes**
   (`engine.rs:907-921`). Cho top_k=10 chunks trong the same file, this reads
   the same file 10 times. Không per-file read cache, không mmap, không
   chunk-content reuse giữa reranker và output. Cho 10K-line file với 10
   chunks: 100K lines read 10 times = 1M string ops khi 100K would do.
   **Ở `agentic_rag_max_turns=9` this is the same file read 90 times.**
6. **Engine starts blocking với `warm_wait` default 50s** (`config.rs:237`).
   If a 500K-file repo's first index pass exceeds 50s, query fails with
   timeout. Operator must tune `mcp_index_wait_secs` manually.
7. **No streaming từ MCP tools** — see [[02-mcp-server#2.5 Streaming|§2.5]].
   Long-running retrievals block; client sees nothing cho entire duration.

## 9.15 Open Questions

- **Verifier cho "sub-100ms cho 500K×1024":** which CPU, which compiler flags,
  which rayon thread count? Scalar dot product tại `vector/mod.rs:297` makes
  this claim unverifiable without explicit benchmark. Suggest: add `criterion`
  bench + `RUSTFLAGS="-C target-cpu=native"`.
- **`fetch_caller_stats_batch` does NOT batch the queries** (`engine.rs:633-647`):
  is the per-chunk `await` necessary, hoặc could it be
  `futures::future::join_all`? Ở 10 chunks × 2 queries each = 20 sequential
  awaits.
- **`mcp_index_wait_secs: 50` default** (`config.rs:237`) is hard-coded với
  không operator override path documented. A 500K-file repo's first index
  pass could exceed 50s — does it fall back gracefully?

Xem [[04-query-embed-store]] cho query path internals, [[07-failure-modes]] cho performance-related failure modes.
