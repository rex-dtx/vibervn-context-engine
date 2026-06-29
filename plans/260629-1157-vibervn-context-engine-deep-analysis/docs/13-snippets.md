# 13 — Snippets (Copy-Paste-Ready)

> Phần 13/14. Ready-to-use Rust snippets từ patterns in [[10-patterns-to-copy]]. Mỗi snippet là self-contained, có thể copy-paste vào project mới.

## 13.1 Atomic Config Write

```rust
use std::path::Path;
use anyhow::Result;
use tempfile::NamedTempFile;
use std::io::Write;

pub fn write_settings_atomic(path: &Path, json: &serde_json::Value) -> Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent dir"))?;
    let tmp = NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&tmp.as_file_mut(), json)?;
    tmp.flush()?;
    tmp.persist(path)?;  // atomic rename, no partial file on crash
    #[cfg(unix)]
    { std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?; }
    Ok(())
}
```

Source: `src/config.rs:426-482` (vibervn-context-engine)

## 13.2 Content-Addressed Cache (mtime-LRU)

```rust
use std::path::{Path, PathBuf};
use anyhow::Result;
use filetime::FileTime;

pub struct ContentCache { pub dir: PathBuf }

impl ContentCache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Key is md5(text + model), NOT repo-scoped.
    /// Same code chunk across repos hits same cache entry.
    pub fn key_path(&self, model: &str, text: &str) -> PathBuf {
        let hash = format!("{:x}", md5::compute(text.as_bytes()));
        let safe_model = model.replace(['/', '\\', ':', '*'], "_");
        self.dir
            .join(safe_model)
            .join(&hash[..2])  // shard by first 2 chars
            .join(format!("{hash}.bin"))
    }

    pub fn get(&self, model: &str, text: &str) -> Option<Vec<u8>> {
        let path = self.key_path(model, text);
        let data = std::fs::read(&path).ok()?;
        // mtime-touch = free LRU signal for external purge
        let _ = filetime::set_file_mtime(&path, FileTime::now());
        Some(data)
    }

    pub fn put(&self, model: &str, text: &str, data: &[u8]) -> Result<()> {
        let path = self.key_path(model, text);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        std::io::Write::write_all(&mut tmp.as_file_mut(), data)?;
        tmp.persist(&path)?;  // atomic rename
        Ok(())
    }

    /// External purge: walks tree, deletes files older than duration.
    /// Approximates LRU because mtime-touch on every get bumps recent files.
    pub fn purge_global(&self, older_than: std::time::Duration) -> Result<u64> {
        let cutoff = std::time::SystemTime::now() - older_than;
        let mut deleted = 0u64;
        for entry in walkdir::WalkDir::new(&self.dir).into_iter().flatten() {
            if entry.file_type().is_file()
                && entry.path().extension().and_then(|s| s.to_str()) == Some("bin")
            {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if mtime < cutoff {
                            if std::fs::remove_file(entry.path()).is_ok() {
                                deleted += 1;
                            }
                        }
                    }
                }
            }
        }
        Ok(deleted)
    }
}
```

Source: `src/embedding/cache.rs:17-250`

## 13.3 Boot Precedence Helper

```rust
/// Resolves a setting using CLI > env_or_settings > default precedence.
/// Captured ONCE at boot, never re-read at runtime.
pub fn resolve<T>(cli: Option<T>, env_or_settings: Option<T>, default: T) -> T {
    cli.or(env_or_settings).unwrap_or(default)
}

// Usage in main.rs:
//   let data_dir = resolve(cli.data_dir, settings.data_dir, default_data_dir(&home));
//   let port = resolve(cli.port, env::var("PORT").ok().and_then(|s| s.parse().ok()), 6699);
//
// Anti-pattern to avoid (duplicated 4× in main.rs):
//   let data_dir = cli.data_dir.clone()
//       .or_else(|| settings.data_dir.clone())
//       .unwrap_or_else(|| default_data_dir(&home_dir));
```

Source: distilled from `main.rs:142-184`, `main.rs:170-174`

## 13.4 Lock Discipline

```rust
use std::sync::Arc;
use tokio::sync::RwLock;

// ❌ BAD — holds lock across await
async fn bad(state: &AppState, repo: &str) -> Result<Db> {
    let guard = state.repo_dbs.read().await;
    let db = guard.get(repo).unwrap();
    let result = db.query("SELECT ...").await?;  // lock held during await!
    Ok(result)
}

// ✅ GOOD — clone payload, drop guard, await
async fn good(state: &AppState, repo: &str) -> Result<Db> {
    let db = {
        let guard = state.repo_dbs.read().await;
        guard.get(repo).cloned()  // clone
    };  // guard dropped here
    let result = db.unwrap().query("SELECT ...").await?;
    Ok(result)
}
```

Source: `query/engine.rs:186-189, 441-444`

## 13.5 Per-Repo Open Gate

```rust
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::collections::HashMap;
use tokio::sync::Mutex as AsyncMutex;

static OPEN_GATES: OnceLock<StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>>
    = OnceLock::new();

fn gate_for(repo: &str) -> Arc<AsyncMutex<()>> {
    let mut map = OPEN_GATES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap();
    map.entry(repo.to_string())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

async fn get_or_open(repo: &str) -> Result<Db> {
    // fast path: already open
    if let Some(db) = cache_read(repo) {
        return Ok(db);
    }
    // slow path: take gate, re-check, open
    let _g = gate_for(repo).lock().await;
    if let Some(db) = cache_read(repo) {
        return Ok(db);
    }
    let db = open_db_with_retry(repo).await?;  // see §13.6 below
    cache_write(repo, db.clone());
    Ok(db)
}
```

Source: `src/store/mod.rs:640-649` (OPEN_GATES) + `src/store/mod.rs:107-129`
(get_or_open với retry)

## 13.6 Bounded Retry cho Exclusive Lock

```rust
use std::time::Duration;

async fn open_db_with_retry(repo: &str) -> Result<Db> {
    const MAX_ATTEMPTS: u32 = 20;
    let mut backoff_ms: u64 = 200;
    let cap_ms: u64 = 2_000;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        match open_db_attempt(repo).await {
            Ok(db) => return Ok(db),
            Err(e) => {
                tracing::warn!(repo, attempt, error = %e, "open_db failed, retrying");
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(cap_ms);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("open_db failed without error")))
}
```

Source: `src/store/mod.rs:107-129` (20 attempts, 200ms→2s backoff, ~30s budget).
Comments cite Windows+Defender 7s+ LOCK-file drain as motivation.

## 13.7 rmcp Tool (schemars + Never-Err)

```rust
use rmcp::{
    ServerHandler, tool, tool_handler, tool_router,
    ErrorData,
    handler::server::tool::{Parameters, ToolRouter},
    model::*,
    schemars, serde,
};
use std::sync::Arc;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MyToolArgs {
    /// The user question (will be embedded semantically).
    pub query: String,
    /// Optional path filter (substring match).
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MyHandler {
    pub shared_state: Arc<MySharedState>,
}

#[tool_router]
impl MyHandler {
    #[tool(description = "Search the codebase semantically")]
    async fn my_tool(
        &self,
        Parameters(args): Parameters<MyToolArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Never return Err — wrap errors as text content
        match self.do_search(&args).await {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(
                format!("Error: {e}")
            )])),
        }
    }
}

#[tool_handler]
impl ServerHandler for MyHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
    }
}
```

Source: `src/mcp.rs:268-411` (CodebaseRetrievalArgs + codebase_retrieval).
Key insights:
- `#[derive(schemars::JsonSchema)]` on args struct → auto JSON Schema
- `Parameters<T>` extractor → auto deserialize
- `ErrorData` import only to satisfy macro — never constructed
- Doc comments on fields become schema descriptions
- `#[serde(default)]` on `Option<T>` makes them optional

## 13.8 Per-Session Handler Factory

```rust
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, StreamableHttpServerConfig, session::local::LocalSessionManager,
};

// In build_router():
let mcp_service = StreamableHttpService::new(
    move || {
        // Fresh handler per session — capture config at session start
        Ok(MyHandler::new(arc_state.clone()))
    },
    LocalSessionManager::default(),
    StreamableHttpServerConfig::default(),
);

let app = Router::new()
    .nest_service("/mcp", mcp_service);
```

Source: `src/server.rs:151-168` (global) + `src/server.rs:1051-1069` (per-repo)

## 13.9 Pre-Normalize + Dot Product Vector Index

```rust
use rayon::prelude::*;

pub struct VectorIndex {
    pub embeddings: Vec<f32>,  // row-major flat
    pub chunk_ids: Vec<ChunkId>,
    pub dim: usize,
}

impl VectorIndex {
    /// L2-normalize on insert. After this, cosine = dot product.
    pub fn add_chunks(&mut self, chunks: Vec<(ChunkId, Vec<f32>)>) {
        for (id, mut emb) in chunks {
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                emb.iter_mut().for_each(|x| *x /= norm);
            }
            self.dim = emb.len();
            self.embeddings.extend_from_slice(&emb);
            self.chunk_ids.push(id);
        }
    }

    /// Brute-force cosine = dot product on L2-normalized vectors.
    /// Parallel via rayon par_chunks.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
        if self.embeddings.is_empty() || query.is_empty() || top_k == 0 || self.dim == 0 {
            return vec![];
        }
        assert_eq!(query.len(), self.dim);

        let mut scores: Vec<(usize, f32)> = self
            .embeddings
            .par_chunks(self.dim)
            .enumerate()
            .map(|(i, row)| {
                let dot: f32 = row.iter().zip(query).map(|(a, b)| a * b).sum();
                (i, dot)
            })
            .collect();

        // select_nth_unstable_by: O(N) partition
        if top_k < scores.len() {
            scores.select_nth_unstable_by(top_k - 1, |a, b| b.1.partial_cmp(&a.1).unwrap());
            scores.truncate(top_k);
        }
        scores.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        scores
            .into_iter()
            .map(|(i, score)| (self.chunk_ids[i], score))
            .collect()
    }
}
```

Source: `src/vector/mod.rs:44-50, 96, 185-202, 297`. **No SIMD**; add
`packed_simd` or `std::simd` for 3-5x speedup.

## 13.10 Filter Stripping Trước Embed

```rust
#[derive(Debug, Default)]
pub struct QueryFilters {
    pub kinds: Vec<String>,
    pub languages: Vec<String>,
    pub path_filters: Vec<String>,
    pub name_filters: Vec<String>,
}

pub fn parse_query_filters(query: &str) -> (String, QueryFilters) {
    let mut filters = QueryFilters::default();
    let mut clean_tokens = Vec::new();

    for token in query.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            let value = value.trim_matches('"');
            match key.to_lowercase().as_str() {
                "kind" => filters.kinds.push(value.to_string()),
                "lang" | "language" => filters.languages.push(value.to_string()),
                "path" => filters.path_filters.push(value.to_string()),
                "name" => filters.name_filters.push(value.to_string()),
                _ => clean_tokens.push(token.to_string()),
            }
        } else {
            clean_tokens.push(token.to_string());
        }
    }

    (clean_tokens.join(" "), filters)
}

// Usage in query::run_query:
let (clean_query, filters) = parse_query_filters(&query);
// Embed only `clean_query`, not the original.
let embedding = voyage_client.embed_query(&clean_query).await?;
// Apply filters AFTER vector search, not before.
let filtered = apply_query_filters(candidates, &filters);
```

Source: `src/query/filters.rs:48` (parse), `src/query/engine.rs:135-140` (use)

## 13.11 Lock-Order Discipline (Type-Level)

```rust
// Type-level encoding of lock order: never acquire vector_index before repo_dbs.
pub struct RepoDbsLock<'a> {
    inner: tokio::sync::RwLockReadGuard<'a, HashMap<String, Surreal<Db>>>,
}

pub struct VectorIndexLock<'a> {
    inner: tokio::sync::RwLockReadGuard<'a, ShardedVectorIndex>,
}

impl<'a> RepoDbsLock<'a> {
    pub async fn then_vector(self, vector: &'a RwLock<ShardedVectorIndex>) -> (RepoDbsLock<'a>, VectorIndexLock<'a>) {
        let vector_guard = vector.read().await;
        (self, VectorIndexLock { inner: vector_guard })
    }
}

// Usage:
let (dbs_lock, vector_lock) = repo_dbs_lock.then_vector(&state.vector_index).await;
// Compile-time guarantee: cannot construct VectorIndexLock without first
// holding RepoDbsLock.
```

Note: This is **proposed** type-level enforcement. The current code enforces
this order via comment (`sharded.rs:16, indexing/mod.rs:506-508`). The
type-level version prevents future violations.

## 13.12 MIGRATIONS Array Pattern

```rust
use serde_json::Value;

pub const CURRENT_VERSION: u32 = 7;

type MigrationFn = fn(&mut Value) -> anyhow::Result<()>;

pub const MIGRATIONS: &[MigrationFn] = &[
    migrate_v0_to_v1,
    migrate_v1_to_v2,
    migrate_v2_to_v3,
    // ...
];

fn migrate_v0_to_v1(v: &mut Value) -> anyhow::Result<()> {
    // Add new field with default if missing
    if v.get("repos").is_none() {
        v["repos"] = Value::Array(vec![]);
    }
    Ok(())
}

pub fn run_migrations(v: &mut Value) -> anyhow::Result<u32> {
    let mut current = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    while current < CURRENT_VERSION {
        let idx = current as usize;
        if idx >= MIGRATIONS.len() {
            anyhow::bail!("no migration for v{} → v{}", current, current + 1);
        }
        MIGRATIONS[idx](v)?;
        current += 1;
        v["version"] = Value::Number(current.into());
    }
    Ok(current)
}

// In ensure_dir_and_load:
let file_version = raw.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
if file_version > CURRENT_VERSION {
    return Err(ConfigError::VersionTooNew {
        file_version,
        binary_version: CURRENT_VERSION,
    });
}
let mut json = raw;
run_migrations(&mut json)?;
```

Source: `src/config.rs:11-19` (MIGRATIONS array), `src/config.rs:488-581`
(ensure_dir_and_load)

## 13.13 Per-Version Test Pattern

```rust
#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn v0_to_v1_adds_repos_field() {
        let mut v = serde_json::json!({});
        migrate_v0_to_v1(&mut v).unwrap();
        assert_eq!(v["repos"], serde_json::json!([]));
    }

    #[test]
    fn v1_to_v2_adds_embedding_provider() {
        let mut v = serde_json::json!({"version": 1, "repos": []});
        migrate_v1_to_v2(&mut v).unwrap();
        assert_eq!(v["embedding"]["provider"], "voyage");
    }

    // ... one test per migration pair
}
```

Source: `src/config.rs:592-955` (16 migration tests in this style)

## 13.14 MockBackend Pattern cho Agentic LLM Tests

```rust
#[derive(Debug, Clone)]
pub enum MockTurn {
    /// LLM responds with this text
    Text(String),
    /// LLM calls this tool with these args, returns next LLM turn
    Calls { tool: String, args: Value, next: Box<MockTurn> },
    /// LLM errors with this
    Err(String),
    /// LLM returns empty (test budget exhaustion)
    Empty,
}

pub struct MockBackend {
    pub turns: VecDeque<MockTurn>,
    pub call_count: AtomicUsize,
}

impl MockBackend {
    pub fn new(script: Vec<MockTurn>) -> Self {
        Self {
            turns: script.into(),
            call_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AgenticBackend for MockBackend {
    async fn complete(&self, _prompt: &str) -> Result<String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        match self.turns.pop_front() {
            Some(MockTurn::Text(t)) => Ok(t),
            Some(MockTurn::Err(e)) => Err(anyhow::anyhow!(e)),
            Some(MockTurn::Empty) => Ok(String::new()),
            Some(other) => panic!("unexpected mock turn: {other:?}"),
            None => panic!("MockBackend exhausted"),
        }
    }

    async fn call_tool(&self, name: &str, _args: Value) -> Result<String> {
        match self.turns.pop_front() {
            Some(MockTurn::Calls { tool, next, .. }) if tool == name => {
                Ok(format!("mocked tool result for {name}"))
                    .and_then(|r| {
                        // Push `next` so the next LLM turn gets it
                        self.turns.push_front(*next);
                        Ok(r)
                    })
            }
            other => panic!("unexpected mock tool call: {other:?}"),
        }
    }
}
```

Source: `src/query/reranker.rs:1825-2340` (MockBackend với `MockTurn` variants
exhaustively cover loop termination, errors, budget exhaustion).

## 13.15 Crash-Safe Commit Marker Pattern

```rust
// Stage 1: write derived data (chunks, embeddings, edges)
for batch in chunk_batches {
    flush_chunk_batch(&db, &batch).await?;
    // DEFER file_meta write until this batch's last chunk flushes
    pending_file_metas.extend(batch.file_metas);
}

// Stage 2: write commit markers only after derived data is durable
// This is the WAL-at-row-level pattern.
for file_meta in pending_file_metas.drain(..) {
    upsert_file_meta(&db, &file_meta).await?;
    // Now this file is "durably indexed" — if we crash here,
    // the next trigger sees file_meta present and skips re-index.
}
```

Source: `src/indexing/pipeline.rs:1054-1124` (file_meta deferred write),
`src/indexing/pipeline.rs:457-480` (recovery detection)

Xem [[10-patterns-to-copy]] cho full pattern list, [[11-anti-patterns]] cho
landmines, [[12-decision-matrix]] cho when-to-use-which.
