# 02 — MCP Server Surface

> Phần 2/14. Transport, tools exposed, schema pattern, error handling, streaming, state injection.

## 2.1 Transport & Wiring

MCP server là **`rmcp::StreamableHttpService`** nested như sub-router tại `/mcp`
trên axum router. Per-repo variant mount tại `/mcp-repo/:repo_id`.

| What | File:Line |
|---|---|
| `StreamableHttpService` import | `server.rs:24-25` |
| Global `/mcp` mount | `server.rs:199` `.nest_service("/mcp", mcp_service)` |
| Global service build | `server.rs:151-168` factory closure returns fresh `McpHandler` per session |
| Per-repo service build | `server.rs:1051-1069` factory closure returns fresh `RepoMcpHandler` per session, cached in `AppState.repo_mcp_services` |
| Per-repo mount | `server.rs:198` `route("/mcp-repo/:repo_id", any(handle_repo_mcp))` |
| `ServerHandler` impl (global) | `mcp.rs:442-451` `#[tool_handler(router = self.tool_router)]` |
| `ServerHandler` impl (per-repo) | `mcp.rs:605-614` same pattern, different `ServerInfo` |
| DNS-rebinding guard | `server.rs:137-147` adds `bind_host` to `allowed_hosts` if non-loopback |

**Cả hai handlers expose cùng 2 tool names với different argument structs**
(per-repo variants drop `workspace_full_path` vì pre-bound by endpoint).

## 2.2 Tools Exposed

2 tools × 2 mounts = **4 handler registrations, 2 unique names**:

### `codebase-retrieval` — `mcp.rs:343-411` (global) / `:514-574` (per-repo)

- **Input schema** (`CodebaseRetrievalArgs`, `mcp.rs:268-283`):
  - `information_request: String` (required)
  - `workspace_full_path: String` (required, dropped in repo variant)
  - `filter_kind: Option<Vec<String>>`
  - `filter_lang: Option<Vec<String>>`
  - `filter_path: Option<String>`
- **Output:** `CallToolResult::success(vec![Content::text(text)])` — single text
  content block, budget-capped at 48K chars (`MAX_TOOL_OUTPUT_CHARS`, `mcp.rs:27`).
- **Description:** ~1.5KB embedded prompt engineered để redirect LLMs away from
  Bash/Grep. Chứa `<RULES>` block claim "appends to system prompt" — vendor
  self-injection (xem [[06-security-audit#2.5-prompt-injection|§6.2.5]]).

### `file-retrieval` — `mcp.rs:413-439` (global) / `:576-602` (per-repo)

- **Input schema** (`FileRetrievalArgs`, `mcp.rs:285-295`):
  - `workspace_full_path: String` (required, dropped in repo variant)
  - `file_path: String` (required)
  - `information_request: String` (required)
  - `top_k: Option<usize>` (default 5)
- **Output:** Same `Content::text` pattern.

**Tool enablement** filtered at startup qua `enabled_mcp_tools` setting:
`Self::tool_router()` → `disable_route(name)` (`mcp.rs:328-331`, `:497-501`).

## 2.3 Schema Definition Pattern

Dùng `schemars::JsonSchema` derive trên plain structs, consumed via rmcp's
`Parameters<T>` extractor:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CodebaseRetrievalArgs { ... }

// Tool signature:
async fn codebase_retrieval(
    &self,
    Parameters(args): Parameters<CodebaseRetrievalArgs>,
) -> Result<CallToolResult, ErrorData>
```

- **Không có manual JSON Schema strings**, không `serde_json::json!()` schemas.
- **Doc comments trên fields** tự trở thành schema `description` (rendered to LLM).
- **`#[serde(default)]` trên `Option<T>`** fields làm chúng optional.
- **`#[tool]` macro** (`name = ..., description = "..."`) cung cấp tool-level
  metadata; argument schemas tự động từ `Parameters<T>` type.

## 2.4 Error Handling on the Wire

**Errors KHÔNG routed qua `McpError`.** Tất cả tools return
`Result<CallToolResult, ErrorData>` nhưng chỉ `Ok` arm được dùng. Mọi failure
bên trong `run_codebase_retrieval` / `run_file_retrieval` returns a `String`
prefixed với `"Error: ..."` rồi wrap trong `Content::text(...)`:

- `mcp.rs:642-644`: `"Error: workspace_full_path is required..."`
- `mcp.rs:691`: `"Error: could not open index database: {e}"`
- `mcp.rs:758`: `"Error: indexing failed ({})..."`
- `mcp.rs:1616, 1621, 1624, 1628, 1634, 1642, 1656, 1661`: all `"Error: ..."`

Doc comment tại `mcp.rs:619-621` explicit: *"Returns plain-text results or an
error/guidance string. Never panics, never returns `Err`."* `ErrorData` được
imported (`mcp.rs:14`) nhưng **không bao giờ constructed** — chỉ tồn tại để
satisfy `#[tool]` macro return-type signature.

**Trade-off:** Clients luôn nhận HTTP 200 với error bên trong text content.
KHÔNG có `isError: true` flag set trên `CallToolResult` vì standard
`CallToolResult::success` constructor không accept nó ở đây. Model thấy
`"Error:"` prefix trong context.

## 2.5 Streaming / Progress

**KHÔNG có streaming từ MCP tools.** README "SSE progress stream" là **separate
REST endpoint**, không phải MCP transport feature:

- `GET /api/repos/:repo_id/index-events` (`server.rs:1088-1147`) — `Sse<...>`
  response backed by `tokio::sync::broadcast` qua `IndexEngine::event_bus`
  (`server.rs:1097-1098`).
- 15-second keepalive qua `async_stream::stream!` (`server.rs:1130`).
- Cả 4 `#[tool]` functions là `async fn ... -> Result<CallToolResult, ErrorData>`
  với không `Sender`, không `Stream`, không progress callbacks. Long-running
  retrievals block đến khi complete, rồi return full result.

Tools **trigger indexing internally** (`index_engine.trigger_index(repo).await` tại
`mcp.rs:713, 724`) và **busy-poll** cho completion trong 500ms loop đến
`mcp_index_wait_secs` (`mcp.rs:730-732`), nhưng client thấy nothing cho đến khi
function returns.

## 2.6 State Sharing

`McpHandler` / `RepoMcpHandler` là **plain `Clone` structs** holding `Arc`-wrapped
shared state. **Không có axum `State`, không `Data`, không rmcp `Context`** dùng
cho dependency injection ở tool level:

```rust
pub struct McpHandler {
    home_dir: PathBuf,
    data_dir: PathBuf,
    index_engine: Arc<IndexEngine>,
    repo_dbs: Arc<RwLock<HashMap<String, Surreal<Db>>>>,
    settings: Arc<RwLock<Settings>>,
    tool_router: ToolRouter<McpHandler>,
}
```
(`mcp.rs:299-314`)

`StreamableHttpService::new` factory closure (`server.rs:151-168`) clones `Arc`s
và captures chúng — **fresh handler constructed per session** (per MCP
`initialize` handshake). `AppState` là axum-side (`server.rs:77-104`); MCP
tools không bao giờ touch nó. `Settings` handle cloned at request time với
`self.settings.read().await.clone()` (`mcp.rs:392, 427, 562, 590`) để drop read
guard trước bất kỳ `.await` nào.

## 2.7 DNS-Rebinding Guard (chỉ bind non-loopback)

`server.rs:135-148`:
- Loopback bind → `StreamableHttpServerConfig::default()` — rmcp's default
  behavior. Cần check xem rmcp 1.7.0 default có enforce allowlist không.
- Non-loopback bind → adds `bind_host`, `localhost`, `127.0.0.1`, `::1` to
  allowlist. **Bind host added nhưng DNS resolution không pinned**.

**What this misses:**
- `Origin`/`Referer` headers không checked
- IPv6 loopback `::1` vs IPv4 `127.0.0.1` — attacker rebinds hostname; attacker
  controls resolved IP. `Host` header có thể match allowlist (vì attacker
  picked hostname).
- Không có `Sec-Fetch-Site: same-origin` enforcement.

Xem [[06-security-audit#2.13-dns-rebinding|§6.2.13]] cho full security analysis.

## 2.8 Per-Session Handler Factory Pattern

```rust
// server.rs:151-168
let mcp_service = StreamableHttpService::new(
    move || Ok(McpHandler::new(
        home_dir.clone(),
        data_dir.clone(),
        embeddings_dir.clone(),
        index_engine.clone(),
        repo_dbs.clone(),
        settings_handle.clone(),
    )),
    ...,
);
```

Fresh `McpHandler` per `initialize` cho phép capture config at session start
(e.g. `enabled_mcp_tools`) without global mutable state. Pattern rẻ hơn nhiều
so với global Mutex<Config>.

## 2.9 Worth-Copying Decisions (MCP-specific)

1. **Single shared query funnel** — `run_codebase_retrieval` /
   `run_file_retrieval` là `pub async fn returning String`; MCP tool handler,
   REST `/api/mcp-tool` handler, và tests all call chúng. Đảm bảo wire output
   của MCP và REST là **byte-identical** (`mcp.rs:629` comment explicit).
2. **Per-session handler factory** — xem [[#2.8 Per-Session Handler Factory|§2.8]].
3. **"Never `Err`" pattern for tool results** — returning errors as
   `Content::text("Error: ...")` lets LLM thấy failure trong context và
   self-correct, thay vì client tear down call. `ErrorData` return type chỉ
   tồn tại để satisfy macro.
4. **Per-repo MCP endpoint với arg dropping** — `/mcp-repo/:repo_id` variant
   strips `workspace_full_path` từ schema, eliminating a class of client
   errors (wrong repo path) ở transport level.
5. **`schemars::JsonSchema` + `Parameters<T>`** — cleanest input-validation
   path trong rmcp: derive trên struct, rmcp generates JSON Schema và
   extractor handles deserialization. Không manual schema strings.

## 2.10 Anti-Patterns / Landmines (MCP-specific)

1. **Prompt injection trong tool description** — `codebase-retrieval` description
   chứa `<RULES>...</RULES>` claim "appends to rules trong system prompt" và
   instructs model để never dùng Bash/Grep. Một số MCP clients strip hoặc
   surface verbatim. **Vendor self-injection** — see [[06-security-audit#2.5-prompt-injection|§6.2.5]].
2. **48K-char silent truncation** — `assemble_with_budget` (`mcp.rs:50-121`)
   emits footer nói `"N of M results truncated"` nhưng returns không
   `isError`, không `is_partial`, không extension field. Clients không thể
   tell result bị cut off without reading trailing text.
3. **Busy-poll cho indexing** — `tokio::time::sleep(500ms)` trong loop đến
   `mcp_index_wait_secs` (`mcp.rs:732`) blocks tokio task và holds lock-free
   `Arc<IndexEngine>` reference. Với nhiều concurrent MCP sessions trên cold
   repos: N×500ms polling pressure trên cùng `IndexEngine` status map.
4. **Description duplication** — 1.5KB `codebase-retrieval` description
   copy-pasted verbatim vào cả `McpHandler` và `RepoMcpHandler`
   (`mcp.rs:345-385` vs `:516-556`). Mọi edit phải làm ở 2 chỗ; shared
   `const &str` sẽ safer.
5. **Dual MCP handler types** — `McpHandler` và `RepoMcpHandler` 90% identical
   (xem [[01-architecture#1.7|Anti-patterns ở §1.7]]). Nên là generic
   `<T: WithWorkspace>`.

## 2.11 Open Questions

- rmcp 1.7.0 có support `Stream<Item = Progress>` cho tool return? README
  claim SSE nhưng là REST. Worth investigating for future-proofing.
- Does `StreamableHttpService` support request-scoped cancellation? Current
  `busy-poll` loop không có `tokio::select!` against client-disconnect signal —
  client cancel mid-wait = tool vẫn runs to deadline.

Xem [[03-indexing-pipeline]] cho cách indexing triggered, [[06-security-audit]] cho security review của MCP surface.
