# 11 — Anti-Patterns to Avoid (20 consolidated)

> Phần 11/14. Tất cả landmines từ 9 analyses, consolidated với `file:line` references. Học từ failure modes đã thấy trong code (một số đã được comment document).

## 11.1 Architecture Anti-Patterns

### Anti-Pattern 1 — 209KB pipeline.rs god file
- **What:** `IndexPipeline` struct + free fns + helper structs + stage flushers
  trộn trong 1 file với ASCII banners pseudo-modules
- **Why bad:** New contributor không navigate được
- **Where:** `indexing/pipeline.rs` (~5000 lines)
- **Fix:** Split thành `parse.rs` / `embed.rs` / `store.rs` / `phase2.rs` /
  `pipeline.rs` chỉ orchestrate. Ngay từ khi > 1500 dòng.

### Anti-Pattern 2 — Dual MCP handler types
- **What:** `McpHandler` (global) + `RepoMcpHandler` (per-repo) 90% identical,
  cả hai `#[tool_router]` macros
- **Why bad:** Duplication 200 lines macro-decorated impls
- **Where:** `mcp.rs:300, 474`
- **Fix:** Generic `<T: WithWorkspace>` handler parameterised over workspace
  source, hoặc accept param trong tool, ignore qua server-side filter.

### Anti-Pattern 3 — Boot-frozen invariant enforced by convention
- **What:** `data_dir`/`embeddings_dir` là `pub` fields, no abstraction
- **Why bad:** Convention + comments, not compile-time guarantee
- **Where:** `indexing/mod.rs:102` (IndexEngine fields), `server.rs:88, 94`
  (AppState fields)
- **Fix:** Wrap trong `BootConfig` newtype với private constructor.

### Anti-Pattern 4 — Std mutex + tokio mutex + 3 Arc<RwLock> mix
- **What:** `OPEN_GATES` (Std) + `repo_locks`/`warm_locks` (Async) + `repo_dbs`
  /`settings`/`vector_index` (Arc<RwLock>)
- **Why bad:** Future reader phải verify lock-ordering across 2 mutex flavours
  + 3 lock kinds
- **Where:** `store/mod.rs:640` (Std), `indexing/mod.rs:119` (Async),
  `indexing/mod.rs:260-270` (3 Arc<RwLock>)
- **Fix:** Centralise on one mutex flavour, hoặc build small `LockGraph` type
  documents order at the type level.

### Anti-Pattern 5 — `run_query` takes 14 arguments
- **What:** `#[allow(clippy::too_many_arguments)]` tại 2 sites
- **Why bad:** Mỗi argument mới = edit signature, call site, test site, MCP
  adapter
- **Where:** `query/engine.rs:93, 117` (run_query, run_query_with_filters)
- **Fix:** `QueryContext { voyage_client, index_engine, repo_dbs, llm_client,
  ... }` struct built once, cloned cho mỗi query.

## 11.2 MCP Anti-Patterns

### Anti-Pattern 6 — Prompt injection trong tool description
- **What:** `codebase-retrieval` description chứa `<RULES>...append tới system
  prompt</RULES>` vendor self-injection
- **Why bad:** Một số MCP clients strip hoặc surface verbatim. Nếu model
  providers ever start honoring tool-description content as system-prompt-
  equivalent, this is regression waiting tới happen.
- **Where:** `mcp.rs:343-386, 514-557` (verbatim ở `mcp.rs:365-367, 536-538`)
- **Fix:** Tool description phải **chỉ** chứa semantic info, không imperative.
  Nếu cần inject rules, dùng `instructions` field riêng (nếu transport
  support) hoặc tạo tool con.

### Anti-Pattern 7 — 48K-char silent truncation
- **What:** `assemble_with_budget` returns không `isError`, không `is_partial`,
  không extension field
- **Why bad:** Client không biết kết quả bị cắt trừ khi parse trailing text
- **Where:** `mcp.rs:50-121`
- **Fix:** Set `isError: true` hoặc trả về metadata `{ partial: true,
  total_chars: N, returned_chars: M }`. Hoặc MCP extension `meta: { trunc: {
  from, to } }`.

### Anti-Pattern 8 — Busy-poll cho indexing
- **What:** `tokio::time::sleep(500ms)` trong loop đến `mcp_index_wait_secs`
  (50s default)
- **Why bad:** N concurrent MCP sessions trên cold repos = N×100 lần polling
  cùng status map
- **Where:** `mcp.rs:730-732`
- **Fix:** True streaming qua `Sender<T>` + `Stream<Item = Progress>>` (rmcp
  support), hoặc index eagerly trong background, return "ready" hoặc "timeout,
  retry".

### Anti-Pattern 9 — Không streaming từ MCP tools
- **What:** README claims "SSE progress stream" nhưng it's separate REST
  endpoint, không phải MCP transport
- **Why bad:** Long-running retrievals block đến khi xong. Client sees
  nothing.
- **Where:** `/api/repos/:repo_id/index-events` (`server.rs:1088-1147`)
- **Fix:** rmcp 1.x có `StreamableHttpServer` — explore `Sender`/`Receiver`
  progress pattern. Hoặc document rõ "tool returns khi done".

### Anti-Pattern 10 — Description duplication
- **What:** 1.5KB `codebase-retrieval` description copy-pasted verbatim vào
  cả McpHandler và RepoMcpHandler
- **Why bad:** Mọi edit phải ở 2 chỗ
- **Where:** `mcp.rs:345-385` vs `mcp.rs:516-556`
- **Fix:** Shared `const &str`.

## 11.3 Indexing Anti-Patterns

### Anti-Pattern 11 — Embed là hard fork
- **What:** Voyage không optional, không `--no-embed` mode cho offline/cached-
  only
- **Why bad:** First embed error abort cả pipeline
- **Where:** `pipeline.rs:859-872` (abort), `voyage.rs:192-215` (retry)
- **Fix:** Pluggable embedder trait + fake client cho test/dev. Graceful
  degradation: indexed từ cache, không embed thì skip.

### Anti-Pattern 12 — Two language dispatch sites
- **What:** `parse_file` ở `parsing/mod.rs:86` (real entry) vs `detect_language`
  ở `pipeline.rs:1312` (second copy of ext table)
- **Why bad:** Drift risk: thêm `.vue` cần update cả hai. Không có
  exhaustiveness check.
- **Where:** `parsing/mod.rs:86` (parse_file), `pipeline.rs:1312`
  (detect_language)
- **Fix:** Single source of truth cho language map. Trait dispatch hoặc
  macro-generated match.

### Anti-Pattern 13 — Raw edges RAM vs DB có different crash-safety
- **What:** Full rebuild path: edges in RAM (có thể mất trên crash). Incremental:
  write DB (crash-safe)
- **Why bad:** Reader phải track which path code is on bằng comments
- **Where:** `pipeline.rs:964-978, 1682`
- **Fix:** Typed enum `EdgeSink::Ram | EdgeSink::Db` ở streaming call site.

### Anti-Pattern 14 — Không global lock cho resolve_edges_phase2
- **What:** Per-repo `get_repo_lock` ngăn concurrent runs của cùng repo. Nhưng
  2 indexer instances trên cùng `data_dir` sẽ corrupt `calls` table
- **Why bad:** Không có file lock, không process-ID check
- **Where:** `indexing/mod.rs:477` (per-repo lock)
- **Fix:** PID file + advisory lock ở boot. `flock` data_dir lock.

### Anti-Pattern 15 — Chunker version global const instead of content-hash
- **What:** Manual bump `CHUNKER_VERSION` invalidates mọi file
- **Why bad:** Manual + chỉ docs ở comment
- **Where:** `parsing/chunker.rs:37` (const), `chunker.rs:32-36` (history note)
- **Fix:** Content-hash of chunker params would self-document.

## 11.4 Query Anti-Patterns

### Anti-Pattern 16 — BFS hardcodes magic constants
- **What:** `MAX_BONUS_CHUNKS = 30`, `MAX_DEPTH = 2`, `CALLER_SCORE_FACTOR =
  0.6`, `CALLEE_SCORE_FACTOR = 0.5`
- **Why bad:** Neither configurable. Magic constants với không có comments
  explain asymmetry.
- **Where:** `query/graph_expand.rs:50-54`
- **Fix:** Config-driven hoặc named constants ở một file duy nhất với
  rationale comments.

### Anti-Pattern 17 — `re_extract` bug cho namespaced symbols
- **What:** Old code: `rfind("::")` trên `"x.cpp::Foo::bar"` → file
  `"x.cpp::Foo"`, match no file
- **Why bad:** Silently drops mọi method-target expansion
- **Where:** `graph_expand.rs:276-278` (bug doc), `graph_expand.rs:269-285`
  (fix)
- **Lesson:** Always look up record by primary key, **never** parse FQN to
  derive file.

### Anti-Pattern 18 — Rerank là black-box LLM call với không eval harness
- **What:** Model silently degrades tới "return chunks in input order" → look
  correct but contribute nothing
- **Why bad:** Không có assertion rằng LLM actually reorders
- **Where:** `query/reranker.rs:48-197`
- **Fix:** Regression test verify LLM output differs từ input order trên known
  query set.

### Anti-Pattern 19 — ShardedVectorIndex `!Sync`-externally-synchronized
- **What:** Struct has không `Send`/`Sync` bounds, contract "engine wraps whole
  struct trong một `tokio::sync::RwLock`" is comment-only
- **Why bad:** Quên outer lock = data race mà type system không catch
- **Where:** `vector/sharded.rs:62-65`
- **Fix:** `parking_lot::Mutex<ShardedVectorIndex>` newtype wrapper enforce
  invariant ở compile time.

### Anti-Pattern 20 — No connection pool, một Surreal<Db> per repo
- **What:** `RwLock<HashMap<String, Surreal<Db>>>` serialises tất cả DB access
- **Why bad:** High-concurrency workloads sẽ contend
- **Where:** `store/mod.rs:30`
- **Fix:** Connection pool per repo, hoặc surrealdb transaction pool. Hoặc
  document rõ đây là single-writer design.

## 11.5 Security Anti-Patterns (cross-ref §6)

### Anti-Pattern 21 — `GET /api/config` returns API keys plaintext
- **Where:** `server.rs:253-264`
- **Risk:** Combined với no auth, bất kỳ LAN caller có thể read all keys
- **Fix:** Mask hoặc omit `api_keys` từ response.

### Anti-Pattern 22 — `PUT /api/config` unauthenticated accepts arbitrary `data_dir`
- **Where:** `server.rs:266-394`, `defender.rs:198-225`
- **Risk:** Poisoned `data_dir` becomes UAC code-execution payload trên next
  restart
- **Fix:** Validate `data_dir` không chứa backticks, `$(...)`, quotes. Existing
  directory với sane absolute path.

### Anti-Pattern 23 — Auto-registering bất kỳ existing directory as repo
- **Where:** `mcp.rs:649-678`
- **Risk:** Turns MCP tool thành filesystem indexer cho bất kỳ path caller
  names
- **Fix:** Gate behind configured-repo allowlist check.

### Anti-Pattern 24 — No auth, no CORS, no CSP, no SRI trên CDN
- **Where:** `server.rs:170-201`; `assets/index.html:7`
- **Risk:** Bind tới `0.0.0.0` exposes UI/API tới LAN
- **Fix:** Add shared-secret bearer check trên `/api/*` và `/mcp*`. Pin
  Tailwind tới vendored copy với SRI.

### Anti-Pattern 25 — Defender PowerShell script constructed via `format!()` without metachar sanitization
- **Where:** `defender.rs:198-225`
- **Risk:** Full privilege escalation chain (LAN attacker → UAC → admin)
- **Fix:** Validate `data_dir` doesn't contain backticks, `$(...)`, hoặc
  quotes trước PowerShell construction.

## 11.6 Build/Test Anti-Patterns

### Anti-Pattern 26 — 239KB single-file SPA với Tailwind CDN
- **Where:** `src/assets/index.html`
- **Risk:** Full utility-class vocabulary ships tới mọi client
- **Fix:** Tailwind JIT compiled vào binary. Hoặc document trade-off.

### Anti-Pattern 27 — Settings precedence chain duplicated inline 4× trong main.rs
- **Where:** `main.rs:109, 112, 142-146, 170-174`
- **Fix:** `fn resolve<T>(cli: Option<T>, env_or_settings: Option<T>, default: T)
  -> T` helper.

### Anti-Pattern 28 — MCP-tool REST proxies duplicate handler logic
- **Where:** `server.rs:958-1009`
- **Risk:** Response là flat string, không structured
- **Fix:** Web-friendly JSON envelope với citations/results array.

### Anti-Pattern 29 — `repro_notepad.rs` shipped as test target
- **Where:** `tests/repro_notepad.rs` (456 lines)
- **Risk:** Drag 100MB+ fixture, sẽ rot nhanh hơn rest of suite
- **Fix:** Move tới `examples/` hoặc CI-only `tests-regress/` target.

### Anti-Pattern 30 — No `cargo test` trong CI
- **Where:** `.github/workflows/release.yml` (no test step)
- **Risk:** PRs có thể merge với failing tests
- **Fix:** Add `cargo test --workspace` to release.yml's PR build job.

## 11.7 Per-handler `IntoResponse` boilerplate
- **What:** Per-handler `let body = json!({...}); (StatusCode::*, Json(body))
  .into_response()` repeated 40+ lần
- **Where:** `server.rs` (multiple sites)
- **Fix:** `ApiError` enum với `IntoResponse` impl collapse noise. The one
  example that exists (`ConfigError` → HTTP, `server.rs:46-73`) shows the
  shape.

## 11.8 When NOT in MCP Domain

Nếu you're building something khác (web app, daemon, CLI), các anti-patterns
này vẫn apply, nhưng đọc [[06-security-audit]], [[07-failure-modes]],
[[08-test-coverage]] trước — security/failure/test anti-patterns phổ biến hơn.

Xem [[10-patterns-to-copy]] cho positive patterns, [[12-decision-matrix]] cho
when-to-use-which.
