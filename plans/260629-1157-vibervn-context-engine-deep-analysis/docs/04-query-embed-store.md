# 04 — Query, Embedding & Storage

> Phần 4/14. Query entry, field filter parser, vector search, BFS expansion, result merger, LLM rerank, Voyage client, embedding cache, SurrealDB schema, store ops.

## 4.1 Query Entry

`src/query/engine.rs:93` — `pub async fn run_query(query, top_k, repo_filter,
voyage_client, index_engine, repo_dbs, min_prune_lines, llm_client, warm_wait,
agentic_rag, ...) -> Result<QueryResult>`.

Delegates tới `run_query_with_filters` (engine.rs:117) takes 14 args returns
`QueryResult { results, pre_rerank_results, timing, rerank }`. Pipeline:

1. `filters::parse_query_filters` → strip `kind:`, `lang:`, `path:`, `name:`
   prefixes (engine.rs:135)
2. `voyage_client.embed_query(&clean_query)` (engine.rs:144)
3. `index_engine.vector_search(&embedding, top_k*2, repo_filter, warm_wait)`
   (engine.rs:154)
4. `fetch_chunk_content` per candidate (engine.rs:828) — pulls stored content
   + symbol FQN
5. `apply_query_filters` (engine.rs:495) — path/lang/kind/name narrowing +
   fuzzy fallback
6. `graph_expand` (engine.rs:226) — BFS over call graph (2 hops, xem [[#4.4 BFS Graph Expansion|§4.4]])
7. `merger::merge_chunks` (engine.rs:246) — dedup + adjacent-range merge
8. `read_lines_from_fs` (engine.rs:906) — numbered text từ disk, bounded bởi `top_k`
9. `fetch_caller_stats_batch` (engine.rs:260) — caller/callee counts/names per chunk
10. `reranker::rerank` (single-shot) hoặc `reranker::rerank_agentic` (engine.rs:274)
11. Format `CodeResult` rows từ reranked indices + line selections (engine.rs:303)

A `run_sub_query` variant (engine.rs:416) là agentic tool's internal call —
embed → search → graph expand → merge, **no rerank**, không `llm_client`.

## 4.2 Field Filter Parser

`src/query/filters.rs:48` — `pub fn parse_query_filters(query: &str) -> (String, QueryFilters)`.

Returns **structured** `QueryFilters { kinds, languages, path_filters, name_filters }`
struct (filters.rs:13). Function tokenises trên whitespace, tries
`match_filter_prefix` (filters.rs:157) ở mỗi token, extracts value (quoted hoặc
unquoted), và collects non-filter tokens vào `clean_query`. Recognised prefixes:
`kind:`, `lang:`, `language:`, `path:`, `name:` (case-insensitive).

**Evaluation là inline, không pre-compiled query plan.** `apply_query_filters`
(engine.rs:495) runs sau vector search và filters candidate Vec: path substring
match trên normalised file path, language detected từ extension qua
`detect_language` + alias table, kind substring match trên `chunk.symbol_kind`,
name exact-then-fuzzy qua `bounded_edit_distance(..., 2)`. Fuzzy fallback fires
chỉ nếu exact name match yields zero results (engine.rs:567). The `merge` method
(filters.rs:34) allows union từ MCP-supplied `external_filters` (engine.rs:137).

## 4.3 Vector Search

**Why sharded:** memory cap + LRU eviction. `src/vector/sharded.rs:65` —
`ShardedVectorIndex` holds một `VectorIndex` per repo, bounded bởi `cap_bytes`.
Eviction (sharded.rs:159) drops whole shards (no O(n) scan). Search runs trên
`&self` — recency bumped qua `AtomicU64` per shard (sharded.rs:130), nên read
guards không bao giờ block trên write guard.

**Sharding key: per-repo.** Shards keyed bởi repo path string (sharded.rs:67).
A cold repo (chưa resident) returns partial results + `cold_repos: Vec<String>`
cho background warm (sharded.rs:249, 440).

**Cosine computation: brute-force dot product.** `src/vector/mod.rs:185` — tất
cả vectors L2-normalised tại insert (mod.rs:96), nên cosine = dot product
(mod.rs:297). `par_chunks(self.dim)` qua rayon cho parallelism (mod.rs:195).
**Không có ANN.** Cross-shard global top-k is exact (sharded.rs:282) vì
normalised cosine is comparable across shards (verified bởi
`cross_shard_topk_equals_merged_topk` test tại sharded.rs:357).

`remove_file` / `remove_repo` dùng swap-remove với explicit row swaps
(mod.rs:106, 134). `byte_size()` counts chỉ flat f32 storage (mod.rs:283) cho
resident-cap accounting.

## 4.4 BFS Graph Expansion

`src/query/graph_expand.rs:64` — `pub async fn graph_expand(base_chunks, db_map, schema_version) -> Vec<ExpandedChunk>`.

- **Max depth: 2** (graph_expand.rs:53 `MAX_DEPTH = 2`).
- **Cycle handling: `global_seen: HashSet<String>`** keyed bởi FQN
  (graph_expand.rs:74). Inserted trước enqueueing (graph_expand.rs:127, 147) —
  already-seen FQNs are skipped.
- **BFS seed:** overlapping symbols (range overlap test trên
  `symbol.line_start <= chunk_end AND symbol.line_end >= chunk_start`,
  graph_expand.rs:183).
- **Score decay:** callers ×0.6 (`CALLER_SCORE_FACTOR`, graph_expand.rs:50),
  callees ×0.5 (`CALLEE_SCORE_FACTOR`, graph_expand.rs:51). Score floor 0.15
  (`SCORE_FLOOR`, graph_expand.rs:52) stops recursion.
- **Cap: 30 bonus chunks** (`MAX_BONUS_CHUNKS`, graph_expand.rs:54), enforced
  với `break 'outer` across tất cả base chunks (graph_expand.rs:116).
- **Indexed queries:** schema v2+ dùng `idx_calls_in_name` / `idx_calls_out_name`
  (graph_expand.rs:208, 243). v1 fallback reconstructs FQN từ short name + file
  (graph_expand.rs:215, 252).
- **Key landmine fix:** `fetch_chunk_for_fqn` (graph_expand.rs:269) dùng
  `surrealdb::sql::Thing::from(("symbol", Id::String(fqn)))` — tránh old
  `rfind("::")` split mis-derived `file_prefix` cho namespaced symbols (e.g.
  `x.cpp::Foo::bar` → file `x.cpp::Foo`).

## 4.5 Result Merger

`src/query/merger.rs:31` — `pub fn merge_chunks(chunks: Vec<MergeChunk>, top_k: usize) -> Vec<MergeChunk>`.

6-step pipeline (merger.rs:36-145):

1. **Dedup** bởi `(file, line_start, line_end)` → keep max score.
2. **Group by file.**
3. **Sort within group** bởi `line_start` ASC.
4. **Merge adjacent ranges** khi `next.line_start <= current.line_end + 2`
   (gap ≤ 1 line). Merged range capped tại **60 lines**; nếu merge vượt 60,
   next chunk pushed như separate entry (merger.rs:71). Content tails appended
   với overlap skipped.
5. **Drop contained** — sort by width DESC, remove any chunk fully inside wider
   one (merger.rs:118-133).
6. **Global sort by score DESC, truncate tới `top_k`** (merger.rs:139-144).

`MergeChunk` carries `file, line_start, line_end, score, content, symbol,
symbol_fqn, symbol_kind` (merger.rs:5). Gap-1 threshold + 60-line cap + width-desc
containment filter là "dedup adjacent ranges" behaviour từ README.

## 4.6 LLM Reranker

`src/query/reranker.rs:48` — `pub async fn rerank(query, chunks, numbered, caller_stats, min_prune_lines, llm_client) -> RerankOutput`.

**Providers:** `openai` và `google` (`src/llm/mod.rs:128`). `LlmClient::new`
returns `None` nếu không có API keys; engine passes `Option<&LlmClient>` —
**rerank is optional and silently skipped** khi keys absent (reranker.rs:60,
returns `skip_reason: "no LLM API key configured"`).

**Configurable:** `use_structured_output` config flag (llm/mod.rs:104) gates
native JSON mode (OpenAI `response_format`, Gemini `responseSchema`).
Structured-mode providers: `google | openai` (llm/mod.rs:78). XML-tag fallback
cho non-structured providers (reranker.rs:113).

**Prompt construction** (reranker.rs:86-121): common `common_intro` string +
per-mode `element_spec`. System message instructs LLM return
`{"ranked_indices": [...]}` (structured) hoặc `<ranked_indices>JSON</ranked_indices>`
(XML). User prompt concatenates chunk entries (reranker.rs:128-149) với format:
`[i] score=X.XX callers=N files=M | file:start-end (symbol)\n<content
chunk-index="i">\n...`.

**Per-chunk metadata** trong prompt: `score`, `callers`, `files` (reranker.rs:131-134).
Chosen truncation: `truncate_content(raw, 100)` — max 100 lines per chunk
(reranker.rs:142).

**Score normalization:** **không có** — reranker returns **ranked indices**,
không scores. LLM's `chunk_index` ordering IS the ranking. Không numeric score
merging. Fallback trên parse failure: original order với `fallback_used: true`
(reranker.rs:118-120).

**Line selection:** mỗi LLM entry có thể specify
`{"chunk_index": i, "lines": [[s,e],...]}` để narrow chunk, hoặc
`{"chunk_index": i, "keep": "full"}` cho whole-chunk (reranker.rs:96-102).
`sanitize_ranges` (reranker.rs:1203) clamps + merges + pads bởi `RANGE_PAD=2`
lines (reranker.rs:46). Chunks narrower hơn `min_prune_lines` không bao giờ
line-pruned (reranker.rs:1200).

**Agentic variant:** `reranker::rerank_agentic` (reranker.rs:427) — cho LLM
một `query` tool để pull additional chunks; returns `ExtendedPool { chunks,
numbered }` nên engine có thể resolve final indices against augmented pool
(engine.rs:295-298).

**Key rotation:** `LlmClient::complete` (llm/mod.rs:140) round-robins keys,
excludes 429-quota-exhausted keys từ second pass, single 2s backoff, không
exponential escalation.

## 4.7 Voyage Client

`src/embedding/voyage.rs:65` — `pub struct VoyageClient` wrapping
`Arc<VoyageInner { http, query_http, model, api_keys, endpoint, key_cursor:
AtomicUsize }>`.

- **Auth:** `bearer_auth(key)` trên mỗi POST (voyage.rs:251).
- **Endpoint:** `https://api.voyageai.com/v1/embeddings` (voyage.rs:12).
  `voyage_url()` (voyage.rs:26) normalises optional `base_url` (strip trailing
  `/`, append `/embeddings` if missing) — mirrors `llm::openai::chat_url`.
- **Hai HTTP clients:** `http` (120s timeout) cho batch indexing, `query_http`
  (30s timeout) cho user-facing query embedding (voyage.rs:88, 92).
- **Constants:**
  - `MAX_BATCH_SIZE = 128` (voyage.rs:13) — count cap.
  - `MAX_BATCH_BYTES = 1_500_000` (voyage.rs:17) — byte-size cap từ commit
    `a6a0af0` (1.5 MB / 2 bytes-per-token ≈ 750K tokens, 25% headroom dưới
    VoyageAI's 1M token per-batch limit).
- **Batching:** `byte_aware_batches` (voyage.rs:285) splits trên cả count AND
  byte-size. Một single text vượt byte cap được sent alone (voyage.rs:293,
  won't poison batch).
- **Retry:** round-robin key rotation (voyage.rs:120, 175); cho `embed_query`
  (voyage.rs:117) một 2s backoff rồi `bail`; cho `embed` (voyage.rs:162)
  exponential backoff capped tại 60s, retries indefinitely.
- **Input type:** `InputType::Query` cho `embed_query`; `InputType::Document`
  cho index-time batches (voyage.rs:126, 182).

## 4.8 Embedding Cache

`src/embedding/cache.rs:17` — `pub struct EmbeddingCache { cache_dir: PathBuf }`.

- **Key derivation:** `md5(text.as_bytes())` → hex string (cache.rs:48).
  **Content-addressed, NOT repo-scoped** — same chunk text across repos hits
  same cache entry. Model là part of path, không phải key.
- **On-disk layout:** `{embeddings_dir}/{sanitized_model}/{md5[..2]}/{md5}.bin`
  (cache.rs:52, 36). 2-character shard prefix prevents any single directory
  growing unbounded. Binary format: raw little-endian f32 values, 4 bytes each,
  no header (cache.rs:58-64).
- **No LRU/eviction trong cache itself.** Eviction done externally qua
  `purge_global(embeddings_dir, older_than)` (cache.rs:191) walks tree và
  deletes `.bin` files có mtime older hơn `now - duration`. LRU-like behaviour
  **approximated bởi mtime-touch on read** (cache.rs:101-106) — mọi cache hit
  calls `filetime::set_file_mtime` để bump file's mtime, nên recent reads
  survive purges.
- **Atomic writes:** `NamedTempFile::new_in(shard_dir)` + `tmp.persist(&final_path)`
  (cache.rs:160-170) — rename is atomic trên POSIX/Windows, nên crash mid-write
  leaves no partial file.
- **Corrupt-entry recovery:** trên `get_many`, non-multiple-of-4 byte lengths
  return `None` và file deleted (cache.rs:110-115).
- **No lock model.** Tất cả access is synchronous filesystem I/O trong caller's
  task. Cache designed để optional — `EmbeddingCache::new` returns `None` nếu
  directory không thể created (cache.rs:39), và callers degrade gracefully.
- **No per-text locking** — concurrent writes tới cùng key có thể race, nhưng
  atomic rename pattern ensures final state luôn complete file (whichever
  rename won).

## 4.9 SurrealDB Schema

`src/store/schema.rs:17` — `pub const SCHEMA_DDL: &str` (114 lines). Run trên
mọi `open_db` (store/mod.rs:143).

**Tables:**

| Table | Type | Purpose | File:Line |
|---|---|---|---|
| `symbol` | SCHEMALESS (flipped từ SCHEMAFULL ở v4) | Code symbols keyed bởi FQN as record ID | schema.rs:31 |
| `chunk` | SCHEMALESS (flipped ở v3 cho ~8.9× write speedup) | Code chunks với `embedding` field | schema.rs:48 |
| `calls` | RELATION IN symbol OUT symbol | Function/method call edges | schema.rs:51 |
| `uses` | RELATION IN symbol OUT symbol | Symbol-use edges | schema.rs:62 |
| `imports` | RELATION IN symbol OUT symbol | Import edges | schema.rs:68 |
| `contains` | RELATION IN symbol OUT symbol | Containment (file→module, module→symbol) | schema.rs:74 |
| `implements` | RELATION IN symbol OUT symbol | Trait/interface implementation | schema.rs:80 |
| `file_meta` | SCHEMAFULL | File-level metadata (path, mtime, size, repo, chunk_count, chunker_version) | schema.rs:86 |
| `index_meta` | SCHEMAFULL | Key-value config (`db_schema_version`, `ignored_paths`) | schema.rs:100 |
| `raw_edge` | SCHEMAFULL | Staging table cho edge relations | schema.rs:105 |

**Schema evolution:** `DB_SCHEMA_VERSION = 5` (store/mod.rs:24). v1→v5
migrations run chained trong background task (store/mod.rs:180-192), bao gồm
v4→v5 packs `chunk.embedding` từ `array<float>` tới little-endian `bytes`
(ops.rs:51 `de_embedding_dual` reads cả hai formats).

**Indexes:**

| Index | Table | Fields | File:Line |
|---|---|---|---|
| `idx_symbol_file` | symbol | file | schema.rs:39 |
| `idx_symbol_name` | symbol | name | schema.rs:40 |
| `idx_chunk_file` | chunk | file | schema.rs:49 |
| `idx_calls_in_file` | calls | in_file | schema.rs:57 |
| `idx_calls_out_file` | calls | out_file | schema.rs:58 |
| `idx_calls_in_name` | calls | in_name | schema.rs:59 |
| `idx_calls_out_name` | calls | out_name | schema.rs:60 |
| `idx_uses_in_file` / `idx_uses_out_file` | uses | in_file / out_file | schema.rs:65-66 |
| `idx_imports_in_file` / `idx_imports_out_file` | imports | in_file / out_file | schema.rs:71-72 |
| `idx_contains_in_file` / `idx_contains_out_file` | contains | in_file / out_file | schema.rs:77-78 |
| `idx_implements_in_file` / `idx_implements_out_file` | implements | in_file / out_file | schema.rs:83-84 |
| `idx_filemeta_path` | file_meta | path UNIQUE | schema.rs:98 |
| `idx_meta_key` | index_meta | key UNIQUE | schema.rs:103 |
| `idx_raw_edge_from_file` | raw_edge | from_file | schema.rs:113 |

**Landmines documented inline** (schema.rs:22-30, 44-48):
- `chunk.embedding` MUST stay without typed `array<float>` definition — SurrealDB
  v2 silently coerces empty arrays tới `[]` cho f32, costing ~530ms/95-chunk
  insert under SCHEMAFULL. 8.9× write speedup là từ SCHEMALESS flip.
- `symbol` table flipped SCHEMALESS ở v4 vì native `sql::Array` INSERT path
  writes `parent` as plain string, mà older `option<record<symbol>>` definition
  rejects và silently rolls back cả batch.

## 4.10 Store Ops

`src/store/ops.rs:58KB`. Main entry points:

| Function | Purpose | File:Line |
|---|---|---|
| `delete_file_data` | Delete all data cho one file: 5 relation tables + symbol + chunk + file_meta (separate queries) | ops.rs:195 |
| `delete_files_data_bulk` | Bulk variant dùng `WHERE field IN $paths` để reduce O(files) round-trips tới O(tables) | ops.rs:249 |
| `delete_all_data` | Wipe everything | ops.rs:308 |
| `upsert_symbol` | Single-symbol upsert (pre-bulk path) | ops.rs:327 |
| `insert_edge` | `RELATE` với `SET` cho 5 relation kinds (calls, uses, imports, contains, implements) — single-statement per edge | ops.rs:355 |
| `upsert_file_meta` | UPSERT với tất cả fields bao gồm `chunker_version` | ops.rs:442 |
| `get_all_file_meta` / `get_meta` / `set_meta` | Query path | ops.rs:462-491 |
| `find_symbols_by_names` / `find_symbols_by_names_with_pos` | Graph query | ops.rs:543-623 |

**Patterns:**

- **Không explicit `BEGIN TRANSACTION` / `COMMIT`.** SurrealDB auto-commits mỗi
  `.query()` call. 5 edge deletions trong `delete_file_data` are sequential
  `.query()` calls, không atomic. Bulk delete dùng single statements với
  `IN $paths` cho amortisation.
- **Batched writes cho indexer are external** tới `ops.rs` — `ops.rs` cung cấp
  per-row primitives (`insert_edge`, `upsert_file_meta`) và bulk delete, nhưng
  batching logic cho index-time inserts sống trong `indexing/pipeline.rs` (not
  analysed here). Comment tại ops.rs:434 notes rằng former `insert_chunk`
  single-row helper was removed trong v5, replaced bởi pipeline's bulk flush
  path.
- **Không connection pooling** — một `Surreal<Db>` per repo, held behind
  `tokio::sync::RwLock<HashMap>` (store/mod.rs:30). Engine clones HashMap và
  drops read guard trước bất kỳ await point nào (engine.rs:186-189) — lock
  không bao giờ spans DB query.
- **Dual-format embedding deserialization** (ops.rs:51) — keystone decoupling
  query correctness từ migration completion. Half-migrated DB (v4 + v5 rows
  mixed) loads correctly vì custom `Visitor` accepts cả `array<float>` và
  packed `bytes` representations.
- **Retry trên `Surreal::new`** (store/mod.rs:107-129) — 20 attempts, 200ms-2s
  backoff, ~30s total budget. Comment explicitly identifies Windows + Defender
  file scanning causing 7s+ LOCK-file drain trên recently-closed RocksDB
  directory.

## 4.11 Worth-Copying Decisions

1. **Content-addressed embedding cache keyed bởi `md5(text + model)`, không
   bởi repo.** (cache.rs:48, 36) — same code chunk across multiple repos hits
   same cache entry. `tempfile::NamedTempFile` + atomic `persist` cho crash
   safety (cache.rs:160). Mtime-touch trên read (cache.rs:101) turns
   filesystem mtime thành free LRU signal cho external purges. Model name là
   directory component, nên model swap invalidates naturally. Simple,
   zero-dependency, no locking.
2. **Per-repo sharded vector index với atomic-stamp LRU và read-locked
   search.** (sharded.rs:65, 130) — `search` takes `&self`, bumps recency qua
   `AtomicU64`, never blocks trên write guard. `cap_bytes` + `evict_to_cap` +
   `active` protected-set design (sharded.rs:159) nghĩa là có thể bound RAM
   precisely without losing repo vừa search. Contract test
   `cross_shard_topk_equals_merged_topk` (sharded.rs:357) chứng minh
   partitioning is mathematically transparent khi vectors L2-normalised —
   contract worth asserting trong bất kỳ project nào fans out across shards.
3. **Pre-normalise tại insert, do dot product tại query.** (mod.rs:96, 297) —
   `cosine(a,b) = a·b` khi cả hai unit length. Eliminates per-candidate
   division. Parallel qua `par_chunks(self.dim)` với `select_nth_unstable_by`
   cho top-k (mod.rs:195, 202). Ở 500K × 1024 dims vẫn sub-100ms với rayon.
4. **Schema evolution qua `DB_SCHEMA_VERSION` + dual-format readers.**
   (ops.rs:51 `de_embedding_dual`, mod.rs:24 `DB_SCHEMA_VERSION`) — half-
   migrated DB is correct-but-unoptimized, không broken. Custom `Visitor`
   accepts cả old `array<float>` và new packed `bytes` cho `chunk.embedding`,
   decoupling query correctness từ migration completion. Inline landmine
   comments (schema.rs:22-30, 44-48) worth stealing — explain WHY a typed
   definition was removed và warn future contributors không re-add it.
5. **Lock discipline: clone guard's payload, drop guard, then await.**
   (engine.rs:186-189, 441-444) — `let db_map = { let guard = repo_dbs.read
   .await; guard.clone() };`. Comment (engine.rs:185) explicitly notes:
   "This prevents holding the lock across graph expansion await points."
   `RwLock<HashMap<String, Surreal<Db>>>` không bao giờ spans DB query trong
   read path. Lock-order rule (sharded.rs:17: always `repo_dbs` → `vector_index`,
   never nested) cũng worth lifting.
6. **Filter stripping trước embedding.** (engine.rs:135-140, filters.rs:48) —
   parse `kind:`, `lang:`, `path:`, `name:` prefixes, strip chúng từ query
   string, embed chỉ semantic content. Tiny parser (một function, ~200 lines)
   runs trước expensive embed call. MCP layer cũng có thể pass
   `external_filters` as structured type (engine.rs:137), mà `merge()`s với
   in-query filters (filters.rs:34). Apply ở narrow-time (sau vector search,
   trước merge), không ở embed-time.
7. **`Option<&LlmClient>` cho optional rerank.** (reranker.rs:60) — khi không
   LLM keys configured, rerank silently degrades tới original-order với
   `skip_reason: "no LLM API key configured"`. Không `Result::Err`, không
   panic. Pipeline vẫn works (just less precise).

## 4.12 Anti-Patterns / Landmines

1. **`run_query` / `run_query_with_filters` take 14 arguments.** (engine.rs:93,
   117) — `#[allow(clippy::too_many_arguments)]` used tại cả hai sites.
   Argument list is smell rằng query state nên là struct (`QueryContext {
   voyage_client, index_engine, repo_dbs, llm_client, ... }`). New argument
   (e.g. third index, different cache) yêu cầu editing signature tại call
   site, test site, và MCP adapter. Future project nên dùng `QueryEngine`
   struct built once và cloned.
2. **BFS hardcodes `MAX_BONUS_CHUNKS = 30` và `MAX_DEPTH = 2`.**
   (graph_expand.rs:53-54) — neither configurable. Cho small repo với deep
   call chains, 2 hops có thể miss right answer; cho large monorepo, 30 bonus
   chunks là tiny fraction of useful graph. `CALLER_SCORE_FACTOR = 0.6` và
   `CALLEE_SCORE_FACTOR = 0.5` (graph_expand.rs:50-51) cũng magic constants
   với không có comments explain asymmetry. Make these config hoặc ít nhất
   named constants ở một place.
3. **Dual-format embedding deserializer sống cạnh schema DDL trong hai
   different files.** (ops.rs:51, schema.rs:48) — reader là keystone cho
   migration safety, nhưng only findable nếu bạn biết embedding format
   changed. New contributor reading schema không biết để look tại
   `de_embedding_dual` và might break migration. Landmines như "NEVER re-add
   a typed embedding field" (schema.rs:47) là code smell — constraint nên
   enforced bởi type system (a `PackedEmbedding` newtype) hoặc CI check, không
   phải comment.
4. **`ShardedVectorIndex` là `!Sync`-externally-synchronized.** (sharded.rs:62-65)
   — struct itself has không có `Send`/`Sync` bounds, và module-level contract
   là "the engine wraps whole struct trong một `tokio::sync::RwLock`." Means
   forgetting outer lock is a data race mà type system không catch.
   `parking_lot::Mutex<ShardedVectorIndex>` hoặc newtype wrapper giữ own lock
   would enforce invariant ở compile time. `touch` method (sharded.rs:130) là
   `&self` specifically để work around lock — papers over design issue hơn
   là fixing it.
5. **Rerank là black-box LLM call với không evaluation harness.**
   (reranker.rs:48-197) — LLM returns `chunk_index` + optional `lines` ranges,
   parsed bởi `parse_rerank_response` (reranker.rs:1085) với extensive
   fallback paths. Mọi parse failure → `fallback_used: true` → original order.
   Means a model mà silently degrades tới "return chunks trong input order"
   produces results trông correct but contribute nothing. Không assertion rằng
   LLM actually reorders, không comparison tới pre-rerank order, không test
   catches "model always returns `keep: full`". Future project nên add
   regression test verifies LLM's output differs từ input order trên known
   query set.
6. **`re_extract` is wrong cho methods và namespaced symbols.**
   (graph_expand.rs:270-285) — comment tại graph_expand.rs:276-278 documents
   bug existed before: `rfind("::")` trên `"x.cpp::Foo::bar"` produced file
   `"x.cpp::Foo"`, matching no file, silently dropping mọi method-target
   expansion. Fix dùng `surrealdb::sql::Thing::from(("symbol", Id::String(fqn)))`
   — symbol record id IS the FQN. Landmine cho bất kỳ project nào tries
   reconstruct file path từ symbol name; always look up record bởi primary
   key, never parse FQN để derive file.
7. **Không connection pool, một `Surreal<Db>` per repo.** (store/mod.rs:30) —
   `RwLock<HashMap<String, Surreal<Db>>>` serialises tất cả DB access cho
   given repo behind one handle. High-concurrency workloads sẽ contend trên
   read lock. Fact rằng engine clones map và drops guard (engine.rs:186) là
   workaround, not fix. Comment tại store/mod.rs:171-173 (`close_repo_db`
   causes migration tới abort gracefully) là another smell — coupling giữa
   DB map và migration task is implicit.

Xem [[09-performance-scaling]] cho hot path latency, [[06-security-audit#2.1-secrets-handling|§6.2.1]] cho secrets storage ở Settings.
