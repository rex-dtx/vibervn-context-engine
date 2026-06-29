# 05 — Config, HTTP API, Web UI & Build

> Phần 5/14. Settings model, HTTP API surface, web UI, defender, CI/release, npm distribution, integration tests.

## 5.1 Config Model

**File:** `src/config.rs` (44KB)

**Format:** JSON (pretty-printed). On-disk location fixed và **NOT** dưới `data_dir`:
- `config_path(home_dir)` → `home_dir/.vibervn/context-engine/settings.json` (`config.rs:379-384`)

**Public `Settings` struct** (`config.rs:251-315`) fields:
- `version: u32` (schema version, stamped by server trên write)
- `repos: Vec<String>`
- `embedding: EmbeddingConfig` (provider, model, api_keys, embed_concurrency, voyage_base_url)
- `llm: LlmConfig` (provider, rerank_model, api_keys, rerank_min_prune_lines, use_structured_output, agentic_rag, agentic_rag_max_turns, agentic_rag_max_chunk_chars, openai_base_url, openai_force_tool_use)
- `mcp_index_wait_secs: u64` (default 50)
- `mcp_stale_after_days: u64` (default 7)
- `vector_resident_cap_mb: usize` (default 2048)
- `data_dir: Option<PathBuf>` (None = use builtin default)
- `embeddings_dir: Option<PathBuf>` (None = use builtin default, anchored tới home)
- `enabled_mcp_tools: Vec<String>` (default `["codebase-retrieval","file-retrieval"]`)
- `custom_extensions: Vec<String>`
- `index_ignore_filenames: Vec<String>` (default `["CLAUDE.md","AGENTS.md"]`)

**Concurrent access:** in-memory shared qua `Arc<RwLock<Settings>>` ở `AppState`
(`server.rs:101`). On-disk writes serialized qua `write_settings_atomic`
(`config.rs:426-482`) dùng `tempfile::NamedTempFile` + `persist` (atomic
rename) + 0o600 perms trên Unix (Windows relies trên inherited NTFS ACLs).

**Load function:** `ensure_dir_and_load(home_dir: &Path) -> Result<Settings, ConfigError>`
tại `config.rs:488-581`. Bootstraps default file nếu absent, runs `MIGRATIONS`
(`config.rs:19`) nếu `file_version < CURRENT_VERSION` (currently v7), và
normalizes `repos` qua `store::normalize_repo_path` (case-fold trên Windows, dedup).

**`ConfigError`** (`config.rs:339-348`): `Io`, `Parse`, `VersionTooNew`,
`MigrationFailed` — tất cả map tới HTTP responses qua `IntoResponse` impl tại
`server.rs:46-73`.

## 5.2 Boot Precedence

Pattern là **inline in `main.rs`**, không shared helper. Mỗi setting repeats
`cli.X.clone().or_else(|| settings.X.clone()).unwrap_or_else(default)` chain:

- `port` (`main.rs:109`): `cli.port.unwrap_or(6699)` — clap already collapses
  CLI > env qua `env = "CONTEXT_ENGINE_PORT"`.
- `bind` (`main.rs:112`): `cli.bind.as_deref().unwrap_or("127.0.0.1")`.
- `data_dir` (`main.rs:142-146`): `cli.data_dir > settings.data_dir >
  default_data_dir(home)`. Resolved **once** ở boot; never re-read từ `Settings`
  ở runtime (boot-frozen — xem `server.rs:83-88` doc).
- `embeddings_dir` (`main.rs:170-174`): same shape; `default_embeddings_dir(home)`
  anchored tới home, **không** tới resolved `data_dir`, nên multiple
  `--data-dir` instances share một cache by default.

Only shared helpers là default path builders `default_data_dir` (`config.rs:391`)
và `default_embeddings_dir` (`config.rs:406`). Mỗi setting duplicates
precedence chain inline — **không generic `resolve_or_env_or_default()` helper**.

## 5.3 HTTP API Endpoints

`build_router` tại **`server.rs:108-201`**, single `Router::new()` chain. Full route list:

| Method | Route | Handler | Summary |
|---|---|---|---|
| GET | `/` | `serve_index` | Returns `include_str!("assets/index.html")` (`server.rs:243`) |
| GET | `/api/config` | `get_config` | Reloads từ disk qua `ensure_dir_and_load` (`server.rs:253`) |
| PUT | `/api/config` | `put_config` | Persists atomically, diffs `repos`, triggers indexing của new dirs, warns trên `data_dir`/`embeddings_dir` change (`server.rs:266-394`) |
| POST | `/api/repos/:repo_id/index` | `post_index_repo` | `trigger_index` |
| DELETE | `/api/repos/:repo_id/index` | `delete_repo_index` | Close DB handle → `remove_index_dir` qua per-repo open gate (`server.rs:449-490`) |
| POST | `/api/repos/:repo_id/rebuild` | `post_rebuild_repo` | `trigger_rebuild` |
| POST | `/api/repos/:repo_id/cancel-index` | `post_cancel_index` | |
| GET | `/api/repos/:repo_id/status` | `get_repo_status` | |
| GET | `/api/repos/:repo_id/index-stats` | `get_index_stats` | Counts files/chunks/symbols; returns `"not_indexed"` state thay vì error (`server.rs:527`) |
| GET | `/api/repos/:repo_id/files` | `get_repo_files` | Capped tại 2000, optional `?filter=` |
| POST | `/api/repos/:repo_id/ignore-file` | `post_ignore_file` | strip_prefix guard, per-repo lock (`server.rs:636`) |
| POST | `/api/repos/:repo_id/unignore-file` | `post_unignore_file` | |
| GET | `/api/repos/:repo_id/ignored-files` | `get_ignored_files` | |
| GET | `/api/repos/:repo_id/graph` | `get_repo_graph` | 600 edges / 250 nodes caps |
| GET | `/api/repos/:repo_id/chunks?file=…` | `get_repo_chunks` | Path-traversal guard qua `path_in_repo` (`server.rs:786`) |
| GET | `/api/repos/:repo_id/index-events` | `get_index_events` | **SSE** (xem below) |
| POST | `/api/index-all` | `post_index_all` | |
| GET | `/api/index-status` | `get_index_status` | |
| POST | `/api/query` | `post_query` | Reads owned `Settings` snapshot, builds Voyage + LLM clients, calls `query::run_query` (`server.rs:865`) |
| POST | `/api/mcp-tool` | `post_mcp_tool` | REST proxy của `run_codebase_retrieval` |
| POST | `/api/mcp-tool/file-retrieval` | `post_file_retrieval` | REST proxy của `run_file_retrieval` |
| DELETE | `/api/embedding-cache?older_than=…` | `delete_embedding_cache` | `all` hoặc `30d` |
| GET | `/api/defender-status` | `get_defender_status` | |
| POST | `/api/defender-exclude` | `post_defender_exclude` | Triggers UAC PowerShell add (Windows only) |
| GET | `/api/plan/packages` | `plan_get_packages` | Proxy tới `CONTEXT_ENGINE_ADMIN_URL` |
| POST | `/api/plan/checkout` | `plan_post_checkout` | Proxy, injects `base_url` trên success |
| GET | `/api/plan/orders/:invoice/status` | `plan_get_order_status` | |
| GET | `/api/plan/usage` | `plan_get_usage` | Forwards `Authorization` header |
| GET | `/mcp-repo/:repo_name` | `handle_repo_mcp` | Per-repo `StreamableHttpService` (`server.rs:1015`) |
| nest | `/mcp` | `mcp_service` | Shared `StreamableHttpService` từ `rmcp` (`server.rs:199`) |

**SSE endpoint** là `/api/repos/:repo_id/index-events` (`server.rs:1088-1148`).
Subscribes tới `IndexEngine.event_bus` (`tokio::sync::broadcast`), filters events
by repo, yields chúng as `Sse` events với 15-second keepalive comment. Không có
WebSockets.

## 5.4 State Injection trong axum

`AppState` defined tại **`server.rs:77-104`** với `#[derive(Clone)]` fields:
- `home_dir`, `data_dir`, `embeddings_dir: PathBuf`
- `index_engine: Arc<IndexEngine>`
- `repo_dbs: Arc<RwLock<HashMap<String, Surreal<Db>>>>`
- `settings: Arc<RwLock<Settings>>`
- `repo_mcp_services: Arc<RwLock<HashMap<String, RepoMcpService>>>`

Built trong `build_router` (`server.rs:108`), attached với `.with_state(state)`
(`server.rs:200`). Handlers dùng `State(state): State<AppState>` extractor
(e.g. `server.rs:253, 266, 396`). Guards **dropped trước bất kỳ subsequent
`.await`** — e.g. `post_query` snapshots `state.settings.read().await.clone()`
ở đầu, rồi drops guard (`server.rs:872`).

## 5.5 Web UI

`src/assets/index.html` là 239KB, **single file, vanilla JS** — không có
framework. Loads Tailwind qua CDN (`<script src="https://cdn.tailwindcss.com">`,
line 7). Hand-written I18N system (3 langs: en/vi/zh, line 1089-1490). SPA-style
tab-based navigation (line 1670, `TABS` array).

**`repo_id` encoding** là `URL_SAFE_NO_PAD base64(repo_path)` (`server.rs:206-216`).
**`sanitize_repo_name`** tại `server.rs:1025` maps e.g.
`D:\projects\Python\foo` → `D__projects_Python_foo`.

**API contract** (all `fetch()`, not XHR):
- GET `/api/config`, PUT `/api/config`
- GET `/api/index-status`
- GET `/api/repos/:repoId/index-stats`, `/files`, `/ignored-files`, `/chunks?file=…`, `/graph`, `/status`
- POST `/api/repos/:repoId/index`, `/rebuild`, `/cancel-index`, `/ignore-file`, `/unignore-file`
- DELETE `/api/repos/:repoId/index`, `/api/embedding-cache?older_than=…`
- POST `/api/index-all`, `/api/query`, `/api/mcp-tool`, `/api/mcp-tool/file-retrieval`
- GET `/api/plan/packages`, `/api/plan/usage`, `/api/plan/orders/:invoice/status`; POST `/api/plan/checkout`
- GET `/api/defender-status`, POST `/api/defender-exclude`
- **SSE:** `new EventSource('/api/repos/:repoId/index-events')` (line 4218)
- **MCP:** `POST {origin}/mcp` (line 2795); per-repo `POST /mcp-repo/:repoName` (line 2993)

## 5.6 Defender

`src/defender.rs` (13.7KB) — Windows Defender real-time protection exclusion
management.

**Threat model:** indexing I/O is severely throttled bởi Windows Defender
real-time scanning. `data_dir` phải excluded để giữ indexing performance
viable. **KHÔNG** a network/external-attacker threat model — it's a
platform-OS-friction issue.

**Main function:** `add_exclusions` (`defender.rs:188-284`) spawns **elevated**
PowerShell process qua `Start-Process -Verb RunAs` (UAC prompt). Inner script:
adds `ExclusionPath` + `ExclusionProcess`, sau đó reads back qua `Get-MpPreference`
để **verify**, exits 0/2/3. Script is base64-encoded as UTF-16LE cho
`-EncodedCommand` để tránh quoting hell (`defender.rs:227-235`).

**Critical correctness detail:** a non-admin process cannot read the real
exclusion list (PowerShell returns the sentinel string `"N/A: Must be an
administrator..."`). Fix là **durable marker file** `.defender-excluded`
written bởi elevated child (`defender.rs:289-296`) — `check_status` short-
circuits tới "excluded" khi marker exists (`defender.rs:80-89`), avoiding
a "Failed — retry" loop (`defender.rs:8-22` documents the incident). DNS-
rebinding và OS-platform plumbing cho `Get-MpPreference` cũng handled.

**Non-Windows:** cả `check_status` và `add_exclusions` are stubbed qua
`#[cfg(not(windows))]` (`defender.rs:51-69`).

**SECURITY NOTE:** Xem [[06-security-audit#2.6-defender-uac-escalation|§6.2.6]] — PowerShell metacharacters trong `data_dir` (settable qua unauthenticated `PUT /api/config`) **không sanitized trước `format!()`** — privilege escalation chain.

## 5.7 CI / Release Pipeline

**Chỉ một workflow:** `.github/workflows/release.yml` (không có CI workflow —
`release.yml` cũng runs trên PRs tới master cho build verification only).

**Triggers** (`release.yml:3-9`): `push` và `pull_request` tới `master`.
**Không tag-based release** — version is driven bởi `Cargo.toml` (`release.yml:139-152`).

**Hand-rolled, không phải cargo-dist hoặc release-plz.** Strategy:
- **Build job** (`release.yml:17-93`): matrix qua 4 targets (`x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`).
  Mỗi runner builds, copies binary vào `npm/<platform-pkg>/bin/`, uploads as
  artifact. `Swatinem/rust-cache@v2` cho dep caching. `libclang-dev` (apt) và
  `LLVM` (choco) are required cho `librocksdb-sys` bindgen.
- **Publish job** (`release.yml:94-223`): runs chỉ trên `push` tới master,
  never trên PRs. Reads version từ `Cargo.toml`, auto-bumps the patch nếu
  version already on npm (`while npm view ...`), stamps version vào mỗi
  `package.json` (main + 4 platform), publishes platform packages **first**
  (nên main pkg's `optionalDependencies` resolve), sau đó publishes main
  package. Sau publish, commits `Cargo.toml` + `Cargo.lock` back tới master
  với `[skip ci]` (`release.yml:192-223`) — dùng `sed` cho `Cargo.toml` và
  `awk` cho `Cargo.lock`.

**CRITICAL TEST GAP:** workflow chạy trên PRs nhưng **không có `cargo test`
step**. Chỉ `cargo build`. PRs có thể merge với failing tests. Xem
[[08-test-coverage#8.5-ci-test-gate|§8.5]].

## 5.8 npm Distribution

**Layout** dưới `npm/`: main pkg `vibervn-context-engine/` + 4 platform
packages (`-linux-x64`, `-linux-arm64`, `-darwin-arm64`, `-win32-x64`). Mỗi
platform `package.json` (e.g. `linux-x64/package.json`) là trivial:
`{name, version:"0.0.0", os:["linux"], cpu:["x64"], files:["bin"], engines:{node:">=18"}}`.

**`bin/cli.js`** (`npm/vibervn-context-engine/bin/cli.js`, 63 lines):
1. Maps `process.platform-process.arch` tới platform package name qua
   `PLATFORMS` table (line 7-12).
2. Resolves binary path qua `require.resolve(\`${packageName}/package.json\`)`
   + `path.join(.., 'bin', 'context-engine-rs[.exe]')` (line 25-40).
3. Exits với clear error nếu platform package is missing (likely
   `--no-optional` install) hoặc binary absent.
4. Spawns qua `execFileSync(binPath, process.argv.slice(2), { stdio: 'inherit',
   env: process.env })` (line 52) — passes CLI args through, inherits stdio,
   propagates exit code.

**Không có postinstall script.** Platform is selected entirely qua npm's
`optionalDependencies` resolution.

**Main `package.json` `optionalDependencies`** (`vibervn-context-engine/package.json:17-22`):
```
"vibervn-context-engine-linux-x64": "0.0.0",
"vibervn-context-engine-linux-arm64": "0.0.0",
"vibervn-context-engine-darwin-arm64": "0.0.0",
"vibervn-context-engine-win32-x64": "0.0.0"
```
(Version `0.0.0` là placeholder; publish workflow stamps real version vào tất
cả four + main pkg trước `npm publish` — `release.yml:155-166`.)

## 5.9 Integration Tests

**`tests/integration.rs` (713 lines)** — HTTP API roundtrips chống lại real
spawned server (`start_server` helper tại line 18):
- `test_get_creates_default` (line 56) — GET `/api/config` bootstraps default
- `test_put_round_trips` (line 94) — PUT `Settings` survives reload
- `test_unix_file_permissions` (line 161) — Unix-only: settings.json is 0o600
- `test_put_repo_then_query_passes_preflight` (line 207)
- `test_put_repo_registers_status` (line 291)
- `test_cancel_index_and_reindex` (line 354) — dùng `encode_repo_id` helper (line 347)
- `test_delete_repo_index_removes_directory` (line 524)
- `test_put_data_dir_persists_but_does_not_relocate` (line 606) — pin the
  boot-frozen contract

**`tests/repro_notepad.rs` (456 lines)** — SurrealDB/RocksDB reproducer scripts
(not pure unit tests; real-DB investigations):
- `schema_ddl_flips_stale_symbol_so_native_insert_persists` (line 15)
- `insert_with_duplicate_id_merges_instead_of_failing` (line 76)
- `count_real_db_rows` (line 129) — **diagnostic, no assertions**
- `repro_full_rebuild_notepad_ade_fresh_db` (line 160) — **repro harness**
- `inspect_real_calls_indexes` (line 229) — **diagnostic**
- `repro_full_rebuild_notepad_ade_warm_cache` (line 378) — **repro harness**

**Coverage gaps:** no SSE-stream test, no MCP-protocol test (no `rmcp` client
harness), no `mcp.rs` roundtrip test, no embedding-cache test, no file-watcher
test, no graph-expansion test. `repro_notepad.rs` là investigation scratch
promoted tới `#[test]` — không long-term asset. Xem [[08-test-coverage]] cho
full analysis.

## 5.10 Worth-Copying Decisions

1. **JSON settings với explicit `version: u32` + `MIGRATIONS: &[MigrationFn]`**
   (`config.rs:11-19, 488-581`). `serde_json::Value` round-trip giữa migrations
   is simple, testable, và no-op "stamp explicit null + bump version" pattern
   (`config.rs:21-98`) makes older binaries refuse newer files (forward-compat
   tripwire) without dropping fields. Cheaper hơn schema library.
2. **Atomic write qua `NamedTempFile` trong same directory + `persist` +
   0o600 trên Unix** (`config.rs:426-482`). Correct, cross-platform, không
   `flock` cần cho single-process mutation.
3. **Boot-frozen `data_dir`/`embeddings_dir`** (`server.rs:83-94`,
   `main.rs:135-184`; xem cả `put_config` warn-and-persist, `server.rs:353-390`).
   Closing RocksDB handles mid-run sẽ split-brain reads; pinning resolved
   path ở boot và refusing re-read từ `Settings` is simplest sound contract.
   Document loudly, warn on PUT.
4. **Manual npm multi-platform packaging với `optionalDependencies`**
   (`release.yml:116-190` + `bin/cli.js` 63 lines). Không `optional` field
   trên individual deps, không `node-pre-gyp` — chỉ một platform pkg per arch
   với `os`+`cpu` constraints, tiny `cli.js` shim, và `require.resolve` để
   find binary. Predictable, không native build toolchain trên user machines.
5. **Per-repo MCP service caching với `Arc<RwLock<HashMap<String, RepoMcpService>>>`**
   (`server.rs:103, 1038-1072`). `StreamableHttpService` expensive để construct,
   và factory closure pattern (`move || Ok(RepoMcpHandler::new(...))`) lets
   mỗi session get fresh handler trong khi service itself is shared.

## 5.11 Anti-Patterns / Landmines

1. **Per-handler `IntoResponse` boilerplate repeated 40+ lần** (e.g. `let body
   = json!({...}); (StatusCode::*, Json(body)).into_response()`). Không central
   `api_error(status, msg)` helper, không `thiserror`-to-HTTP mapping.
   `ApiError` enum với `IntoResponse` would collapse noise — one example exists
   (`ConfigError` → HTTP, `server.rs:46-73`) shows the shape.
2. **Settings precedence chain duplicated inline 4× trong `main.rs`** (port,
   bind, data_dir, embeddings_dir) thay vì `resolve<T>(cli, env, settings,
   default)` helper. Adding 5th precedence-aware setting có nghĩa là
   copy-paste ladder again.
3. **Hand-rolled npm version-stamping + Cargo.toml/Cargo.lock round-trip back
   tới master** (`release.yml:128-223`). Works, nhưng `sed -i -E` trên
   `Cargo.toml` và `awk` trên `Cargo.lock` are fragile nếu either file's
   format changes. `cargo-release` hoặc `release-plz` does this với proper
   Cargo.lock regeneration.
4. **MCP-tool REST proxies duplicate handler logic** (`/api/mcp-tool` →
   `run_codebase_retrieval`, `/api/mcp-tool/file-retrieval` → `run_file_retrieval`,
   `server.rs:958-1009`). Tồn tại nên web UI có thể exercise same code path,
   nhưng response là `{result: "<plain text>"}` — a flat string, không
   structured. Web-friendly JSON envelope would let UI render citations/results
   without parsing text MCP tool returns.
5. **239KB single-file SPA với không build step** (`index.html`). Tailwind
   qua CDN (không build-purged CSS) means entire utility-class vocabulary ships
   tới mọi client; vendoring Tailwind JIT vào binary sẽ cut weight và let
   build produce content-hashed asset. I18N string table alone is 260 lines
   of inline JS — không có obvious way để split it without build step. Fine
   cho tool với small user base, painful past ~300KB.
6. **`repro_notepad.rs` shipped as test target** (456 lines). Đây are
   investigation scripts chống real Notepad++ checkout, pinned tới
   `#[tokio::test]`. They drag in 100MB+ fixture và sẽ rot faster hơn rest of
   suite; move tới `examples/` hoặc CI-only `tests-regress/` target.
7. **Defender PowerShell script constructed via `format!()` without metachar
   sanitization** (`defender.rs:198-225`) — see [[06-security-audit#2.6-defender-uac-escalation|§6.2.6]]
   cho full privilege escalation chain.

Xem [[06-security-audit]] cho security review của PUT /api/config & auto-add repos, [[08-test-coverage]] cho test gaps.
