# 07 — Failure Modes & Error Handling

> Phần 7/14. Error type hierarchy, process-level errors, RocksDB lock contention, embed failures, cache corruption, indexing partial failure, MCP tool failure, SSE backpressure, watcher error recovery, vector bounds, graph expansion, LLM rate limiting.

## 7.1 Error Type Hierarchy

**Custom enums** (deliberate, minimal):
- `ConfigError` — `src/config.rs:339-369`. Variants: `Io`, `Parse`,
  `VersionTooNew`, `MigrationFailed`. Hand-rolled `Display` +
  `std::error::Error`. `IntoResponse` impl tại `src/server.rs:46` (returned as
  500).
- `PipelineAbort` — `src/indexing/pipeline.rs:142-156`. Variants: `Cancelled`,
  `EmbeddingFailed(String)`. Used as a sentinel error; converted qua `.into()`
  vào `anyhow::Error`.
- `EmbedError` — `src/embedding/voyage.rs:304-307`. `RateLimited` vs
  `Other(anyhow::Error)`. Internal-only, never escapes module.

**Mọi thứ khác là `anyhow::Error` / `anyhow::Result`.** Không có `StoreError`
hoặc `McpError`. Codebase has không `thiserror` dependency. Conversion points:
- `store/mod.rs:133-134` — RocksDB open failure → `anyhow::Error::new(last_err)
  .context("open surrealdb")`.
- `pipeline.rs:548, 1180-1182` — `PipelineAbort` → `anyhow::Error` qua `.into()`.
- `server.rs:46` — `ConfigError` → HTTP response (only one HTTP error type với
  custom response).

Net effect: callers bubble `anyhow!` up; `main.rs` prints it và exits. Server
handlers do their own `.context(...)` và return 500/400 strings.

## 7.2 Process-Level Error Handling

`main.rs:115-241` — `eprintln!` + `exit(2)` cho pre-serve errors (no `Dir`, no
`Settings`, bad bind, port-bind fail). Sau đó `axum::serve(...).await
.unwrap_or_else(|e| { eprintln!; exit(1) })` tại `main.rs:238-241`.

**Không panic handler, không `catch_unwind`, không `tokio::main` `set_hook`.**
A panic trong bất kỳ request handler hoặc spawned task aborts runtime và tears
down process — user sees a dropped connection và must restart.

**`tokio::spawn` sites:**
- `indexing/mod.rs:200-210` (`IndexEngine::start`) — spawns consumer và
  watcher tasks. Không `JoinHandle` is stored và không result is awaited.
- `indexing/watcher.rs:60-84` — spawns notify watcher **hoặc** polling fallback.
- `server.rs` — không `tokio::spawn` found for request paths (all handlers are
  axum-driven futures).

A panic hoặc silent `?`-induced `Err` trong bất kỳ of these spawned tasks
kills that one task. `tokio::spawn` tại `indexing/mod.rs` cho consumer/phase2
does not have `.in_current_span()` hoặc a guard. If indexer panics, server
keeps running với a dead `IndexEngine` — reads will work, writes will silently
never happen.

## 7.3 RocksDB Lock Contention

`store/mod.rs:107-129` — 20 attempts, 200ms linearly increasing tới 2s cap →
~30s total budget. Comments tại lines 96-103 explicitly cite Windows+Defender
7s+ LOCK drain sau `remove_index_dir` as the cause.

**On exhaustion** (`store/mod.rs:130-135`): returns `anyhow::Error::new
(last_err).context("open surrealdb")`. Propagates up qua `get_or_open` →
request handler → HTTP 500 tới caller, hoặc tới indexer consumer (which logs
và retries trên next event). User sees `Error: could not open index database:
open surrealdb` trong MCP tool output (`mcp.rs:691`, `1634`) — không automatic
recovery, không actionable guidance.

## 7.4 Embed API Failure Modes

`embedding/voyage.rs:162-216` — `embed_batch`:
- First pass: tries mỗi API key once, returns immediately trên non-429.
- All-429: **unbounded exponential backoff**, 2s → 4s → 8s → ... → capped 60s,
  **never returns**. Only escape là a non-429 error hoặc a key succeeding
  (`voyage.rs:206-207`).
- A 5xx từ một key returns `Err` immediately tới caller (line 188) — không
  retry across providers cho non-429 errors.

`embed_query` (lines 117-158) — different policy: một 2s backoff, sau đó
**bails**. User-facing queries fail fast hơn hang.

**Nếu Voyage is down for hours:** `embed_batch` sẽ block indexer consumer
indefinitely. Cancel_token tại `pipeline.rs:1180-1182` chỉ checks trên
`?`-propagation trong `streaming_index`, không inside embed call itself — nên
a 60s backoff sleep sẽ not be interrupted. 120s HTTP timeout (`voyage.rs:88`)
gives single-attempt cap, nhưng loop is still infinite across keys. **Không
circuit breaker.**

`try_embed_with_key_using` (line 264) silently swallows non-success body:
`response.text().await.unwrap_or_default()` — nếu body read fails, error là
`voyage error <status>: ` (empty), và body parse failure ở line 274 is
indistinguishable từ a 500.

## 7.5 Cache Corruption

`cache.rs:66-76` — `decode_embedding` rejects `is_empty()` và
`!is_multiple_of(4)`. **Không magic number, không checksum, không length
header.** Bất kỳ 4-byte-aligned random bytes sẽ be parsed as a `Vec<f32>`
của wrong dimension và returned as a cache hit (`cache.rs:96-107`).

`cache.rs:109-115` — corrupt files (decode returns `None`) are deleted trên
read. Nhưng valid-aligned-but-wrong-content passes through — caller gets
garbage floats mà look like a real embedding. **Silent data corruption; the
downstream cosine search sẽ return nonsense results, not an error.**

Partial writes: `cache.rs:159-175` — `NamedTempFile::new_in(shard_dir)` sau
đó `write_all` sau đó `persist` (atomic rename). If process is killed trong
`write_all`, the `.tmp` file is left behind trong shard dir. `get_many` ignores
`.tmp` files (it chỉ reads `md5.bin` paths). `purge_global` (`cache.rs:233`)
chỉ matches `.bin` extension. **Orphan `.tmp` files accumulate over time,
slowly wasting disk.** Không cleanup sweep exists.

## 7.6 Indexing Partial Failure

`pipeline.rs:551-566` — `full_rebuild` deletes first, sau đó runs
`streaming_index`. `streaming_index` (line 955-1119) defers `file_meta` writes:
a `pending_file_metas: Vec<FileMeta>` accumulates và is flushed chỉ khi
**batch containing last chunk cho a file** is committed (line 1098-1108) hoặc
ở tail (line 1202-1207).

**`file_meta` is the commit marker.** Trên crash mid-`streaming_index`:
- Process restart → `run_consumer` → `get_all_file_meta` (line 338) shows chỉ
  files mà fully completed.
- A `file_meta`-present-but-edges-unresolved state is detected ở
  `pipeline.rs:457-480` và triggers a full rebuild cho RAM-path fast case
  (`resolve_edges_from_ram`, lines 1660-1681). Detection specifically checks
  `raw_edge_count=0 AND file_meta non-empty AND edges_resolved absent`
  (line 1679-1681).
- Comment tại `pipeline.rs:457-461` và `1668-1681` documents this explicitly.

**Recovery is automatic but expensive** — a mid-`streaming_index` crash forces
a full rebuild trên next boot, re-embedding all chunks. Cached embeddings trên
disk (`cache.rs`) absorb most of cost, nhưng the `db.query` DDL/schema và
symbol insert is re-done. Cho 50K-file repo this is difference giữa 2s và 5min.

**Worth noting:** không có WAL. If RocksDB has unflushed writes khi process
dies (no `fsync` is forced bởi SurrealDB's `RocksDb` engine trong this config),
partial symbol/chunk rows could exist cho files có `file_meta` was never
written. The `edges_resolved` marker prevents Phase 2 từ running trên a
half-built graph, nhưng schema-level half-state is not separately checked.

## 7.7 MCP Tool Failure Modes

**`grep '\.unwrap()' src/mcp.rs` returns ZERO matches.** Mọi error path returns
a `String` of the form `"Error: ..."` (e.g. `mcp.rs:642, 653, 682, 691, 758,
1509, 1530, 1616, 1621, 1624, 1628, 1634, 1642, 1656, 1661, 1665`). MCP layer
is panic-free trong tool handler hot path.

`mcp.rs:410, 438, 573, 601` — `CallToolResult::success(vec![Content::text(text)])`
is returned even cho error strings. MCP protocol-level error (`rmcp::ErrorData`)
is reserved cho protocol violations only. This is deliberate "never-Err" pattern
— model sees a string error và can retry.

**Tuy nhiên:** the `unwrap_or_default` / `unwrap_or` calls bên trong `server.rs`
và downstream (e.g. `server.rs:1119` — `serde_json::to_string(&event)
.unwrap_or_default()`) silently swallow serialization failures. If `IndexEvent`
ever grows a non-serializable field, all SSE events cho that repo sẽ go blank
without bất kỳ log.

## 7.8 SSE Stream Backpressure

`events.rs:80` — `broadcast::channel(1024)`. Cap = 1024 events. `emit` tại
`events.rs:84-86` dùng `let _ = self.tx.send(event)` — **send error (Lagged
hoặc closed) is silently dropped.** Không log, không counter.

Slow subscriber: `tokio::sync::broadcast` dùng ring-buffer overwrite. Khi a
subscriber is slow, they get `RecvError::Lagged(n)` — `server.rs:1125` filters
that tới `None` và silently drops event. User sees SSE events stop arriving
without a connection drop và without a reconnection signal.

The `keepalive_stream` tại `server.rs:1130-1145` sends a 15s `: keepalive`
comment, vậy TCP-level dead-connection detection is covered. **Nhưng application
never tells client "I dropped events 5-9"** — clients should re-fetch state,
nhưng nothing trong code forces that.

## 7.9 Watcher Error Recovery

`watcher.rs:66-84` — notify-recommended-watcher failure → `run_polling_fallback`.
Polling fallback (`watcher.rs:88-103`) is a 30s `tokio::time::sleep` +
`tx.send(trigger)` loop. **Triggers are mpsc `Sender<IndexTrigger>` (unbounded)**
— polling task will block trên `tx.send` nếu consumer is stuck.

**Individual file permission errors** trong `streaming_index` (the notify events
carry paths) are NOT caught ở watcher level. Permission denied surfaces bên
trong indexer consumer's `walk` call (e.g. `pipeline.rs:440`). Xem
`pipeline.rs:443` — `.context("incremental walk spawn_blocking")?` will fail
entire walk trên a single unreadable directory, dropping tất cả changes cho
that batch.

## 7.10 Vector Index Bounds

`vector/mod.rs:106-129` — `remove_file` dùng `swap_remove` trên `chunk_ids` và
a manual copy of last row vào slot i. Pattern is correct: `last` is recomputed
sau `swap_remove` (line 111), và `i` is not advanced (line 121 comment: "the
swapped element now lives at i"). **Tuy nhiên:** if `self.dim` is 0 (line
127), the `src_start = last * self.dim` evaluates tới 0, và the `copy_from_slice`
becomes a no-op — nhưng it still runs, với `i < last` being `false` once array
is empty, nên branch is skipped (line 112). Safe.

`search` at `vector/mod.rs:185-192` — guards on `is_empty()`, `query.is_empty()`,
`top_k == 0`, `self.dim == 0`. **All four guards present**, nên empty shards
return `vec![]` cleanly. Không panic risk on search.

`remove_repo` (line 134-148) is structurally identical tới `remove_file` và
cũng resets `dim = 0` trên empty (line 151). **Safe.**

Real risk: sau `remove_file` resets `dim = 0` (line 127), a subsequent
`add_chunks` call với a new dimension (line 84-86) reassigns `self.dim = raw_emb
.len()`. **This is silent** — a shard mà was supposed tới hold 1024-dim
vectors sẽ start holding 768-dim vectors nếu a file is re-added với wrong
model. Search sẽ return mixed-dimension nonsense cho đến khi shard is fully
repopulated.

## 7.11 Graph Expansion Infinite Loops

`query/graph_expand.rs:74` — `global_seen: HashSet<String>` keyed bởi FQN.
Cycle detection: mọi FQN is inserted once tại lines 127 và 147.

**Score floor + depth guard:**
- `MAX_DEPTH = 2` (line 53) — hard cap.
- `SCORE_FLOOR = 0.15` (line 52).
- Caller score = `score * 0.6` (line 50), callee score = `score * 0.5` (line 51).
- Starting từ a base score of 1.0: depth-0 caller = 0.6, callee = 0.5;
  depth-1 caller = 0.36, callee = 0.30; depth-2 = 0.216, 0.18; depth-3 = 0.129
  (below floor).
- Combined với `MAX_DEPTH = 2`, loop is bounded ở depth 2, nên nó never reaches
  score floor trong practice.

**`MAX_BONUS_CHUNKS = 30` outer break** (line 115-117) — the `'outer: for
base_chunk` label provides an exit từ BFS+queue.

**Real termination:** BFS is bounded bởi `MAX_DEPTH=2` và `MAX_BONUS_CHUNKS=30`.
Cycle detection qua `global_seen` is belt-and-suspenders. **Cannot infinite
loop** even with a real data cycle, because depth is capped. Worst case: the
`query_callers`/`query_callees` (line 122, 142) return `unwrap_or_default()` on
error, nên a DB error trong graph expansion becomes a silent no-op expansion —
expansion chỉ stops growing từ that node.

## 7.12 LLM Rate Limiting

`llm/mod.rs:140-181`:
- Round-robin key cursor (line 142).
- First pass: mỗi key once; 429s are recorded trong `rate_limited[key_idx] =
  true` (line 155).
- One 2s backoff, sau đó retry **non-rate-limited** keys (line 168 `if
  rate_limited[key_idx] { continue; }`).
- Final return: `Err(last_err.unwrap())` (line 180) — **panics if `last_err` is
  `None`**, mà can happen if all keys returned 429 và no other error. `unwrap()`
  tại line 180 is the only `unwrap`/panic trong LLM module và it is reachable.

**Per-call strategy, not global:** a long-running consumer mà hammers LLM sẽ
retry same key sau 2s regardless of whether that key just got 429'd globally.
Không jitter, không exponential growth, không shared rate-limit state across
concurrent calls.

## 7.13 Worth-Defending Decisions

1. **RocksDB open retry với structured budget** (`store/mod.rs:107-129`) —
   bounded 30s, logged at first failure và mọi ~5s, explicit comment naming
   OS-level cause (Windows+Defender). Copy this pattern cho bất kỳ
   exclusive-lock acquisition.
2. **`file_meta` as the sole commit marker** (`pipeline.rs:955-1119`) —
   deferred-write pattern means DB is never ahead of durable state. Phase 2
   marker + raw-edge count triple-check (`pipeline.rs:1668-1681`) is a clean
   way tới make RAM-fast-path crash-safe.
3. **MCP "never-Err" pattern** (`mcp.rs:410, 438, 573, 601`) — returning a
   `String` error bên trong `CallToolResult::success` is the right call cho
   an LLM-facing tool; model can read và recover, whereas a protocol-level
   `Err` terminates session.
4. **Cache atomic-rename qua `NamedTempFile::persist`** (`cache.rs:160-175`) —
   same-directory temp + rename gives filesystem-level atomicity trên cả
   Unix và Windows without fsync. Standard, correct, worth copying.
5. **Bounded retry budget trong `open_db` và `embed_query`; unbounded chỉ
   trong `embed_batch`** — asymmetry is deliberate: pipeline-level embedding
   should never give up (user can re-trigger a cancel), nhưng a user-facing
   query embed must fail fast. Document asymmetry, don't normalize it.

## 7.14 Anti-Patterns / Landmines

1. **`last_err.unwrap()` tại `llm/mod.rs:180` (và `280`)** — panic trên
   all-429-no-other-error path. Replace với `bail!` hoặc explicit
   `Err(anyhow::anyhow!("all keys rate-limited"))`.
2. **Silent cache corruption acceptance** (`cache.rs:66-76`) — không checksum,
   không length header. A 4-byte-aligned random file is a valid cache hit
   returning garbage floats. Add a magic number + dimension prefix trong `.bin`
   file hoặc store a sidecar `md5.meta` với expected `len`.
3. **`.unwrap_or_default()` trên `serde_json::to_string` trong `server.rs:1119,
   256` và `mcp.rs` paths** — a non-serializable field will blank entire event
   stream với không log. Use `.unwrap_or_else(|e| { warn!(...); "{}".to_string() })`.
4. **Unbounded `broadcast::channel(1024)` với silent `let _ = tx.send(...)`**
   (`events.rs:80, 84-86`) — slow subscribers get `RecvError::Lagged` filtered
   tới `None` (`server.rs:1125`) với zero client-side signal. Either log lag
   count, send a synthetic `Lagged(n)` event, hoặc reduce cap và accept drop
   semantics explicitly.
5. **`embed_batch` infinite retry loop với không circuit breaker**
   (`voyage.rs:192-215`) — nếu Voyage is down for hours, indexer consumer is
   wedged indefinitely; cancel_token does not interrupt the `tokio::time::sleep
   (60s)` (line 199). Wrap sleep trong `tokio::select!` chống cancel_token, và
   add a max-retry-then-fail với a clear error tới user (not a silent hang).
6. **Watcher `try_send` drops events silently** (`watcher.rs:53`) — Channel
   capacity 256. A 10K-file `git checkout` overflows và drops back of burst.
   Comment "will recover on next poll" is misleading vì polling fallback is
   30s. **A full burst during checkout → stale index for up to 30 seconds.**
   Either coalesce vào one trigger hoặc use unbounded channel với coalescing.

## 7.15 Open Questions

- Does `embed_batch`'s exponential backoff respect `cancel_token`? Currently
  the 60s sleep is not interruptible.
- Is there a maximum total runtime per indexing pass? What happens nếu a
  full rebuild mất > 1 hour?
- Does `close_repo_db` (`store/mod.rs:171-173`) có cơ chế cancel in-flight
  queries? If not, concurrent close + query could panic.

Xem [[08-test-coverage]] cho test gaps trong failure paths, [[09-performance-scaling]] cho performance-related failure modes.
