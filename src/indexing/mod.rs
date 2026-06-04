pub mod pipeline;
pub mod tracker;
pub mod walker;
pub mod watcher;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

use crate::config::Settings;
use crate::embedding::voyage::VoyageClient;
use crate::indexing::pipeline::IndexPipeline;
use crate::indexing::tracker::FileChange;
use crate::indexing::watcher::start_watcher;
use crate::store;
use crate::vector::{SearchResult, VectorIndex};

// ─── Repo indexing status ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    Idle,
    Indexing,
    Error,
}

/// Per-repo status snapshot returned by `GET /api/index-status`.
///
/// Dual-meaning fields (state-gated contract):
/// - `state == Indexing`: `total_files` = workset size for this run (denominator);
///   `indexed_files` = files whose chunks have been embedded so far (numerator).
///   Progress % = indexed_files / total_files (guard against total_files == 0).
/// - `state == Idle` / `Error`: `indexed_files` = files in the index;
///   `total_files` = total files in the repo on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoStatus {
    pub state: IndexState,
    pub indexed_files: u64,
    pub total_files: u64,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

impl Default for RepoStatus {
    fn default() -> Self {
        Self {
            state: IndexState::Idle,
            indexed_files: 0,
            total_files: 0,
            last_indexed_at: None,
            error: None,
        }
    }
}

// ─── ProgressHandle ───────────────────────────────────────────────────────

/// Lightweight handle passed into `IndexPipeline::run` so the pipeline can
/// write live progress into the shared status map without knowing the full engine.
#[derive(Clone)]
pub struct ProgressHandle {
    statuses: Arc<RwLock<HashMap<String, RepoStatus>>>,
    repo: String,
}

impl ProgressHandle {
    /// Set the denominator once the parsed file set is known.
    pub async fn set_run_total(&self, total: u64) {
        let mut map = self.statuses.write().await;
        let s = map.entry(self.repo.clone()).or_default();
        s.total_files = total;
    }

    /// Advance the numerator. Monotonic — never decreases.
    pub async fn set_processed(&self, processed: u64) {
        let mut map = self.statuses.write().await;
        let s = map.entry(self.repo.clone()).or_default();
        if processed > s.indexed_files {
            s.indexed_files = processed;
        }
    }
}

// ─── IndexEngine ──────────────────────────────────────────────────────────

/// Central orchestrator for all indexing operations.
/// Stored in `AppState` and shared via `Arc`.
pub struct IndexEngine {
    pub home_dir: PathBuf,
    /// Per-repo status map, keyed by repo path string.
    /// Wrapped in Arc so `ProgressHandle` can hold a reference without borrowing self.
    pub statuses: Arc<RwLock<HashMap<String, RepoStatus>>>,
    /// Serialises concurrent pipeline runs per repo.
    repo_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Channel sender for triggering index runs (manual or watcher-driven).
    trigger_tx: tokio::sync::mpsc::Sender<IndexTrigger>,
    /// In-process vector index for fast cosine similarity search.
    pub vector_index: Arc<RwLock<VectorIndex>>,
}

#[derive(Debug)]
pub struct IndexTrigger {
    pub repo: String,
    pub changes: Option<Vec<FileChange>>, // None = full incremental scan
}

impl IndexEngine {
    /// Create the engine and spawn the watcher background task.
    pub async fn start(home_dir: PathBuf, settings: &Settings) -> Arc<Self> {
        let (trigger_tx, trigger_rx) = tokio::sync::mpsc::channel::<IndexTrigger>(256);

        // Load the vector index from the first available repo DB, or start empty.
        let vector_index = if let Some(first_repo) = settings.repos.first() {
            match store::open_db(&home_dir, first_repo).await {
                Ok(db) => match VectorIndex::load_from_db(&db).await {
                    Ok(vi) => {
                        info!(count = vi.len(), "VectorIndex loaded from DB");
                        Arc::new(RwLock::new(vi))
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to load VectorIndex from DB; starting empty");
                        Arc::new(RwLock::new(VectorIndex::new()))
                    }
                },
                Err(e) => {
                    warn!(error = %e, "failed to open DB for VectorIndex load; starting empty");
                    Arc::new(RwLock::new(VectorIndex::new()))
                }
            }
        } else {
            Arc::new(RwLock::new(VectorIndex::new()))
        };

        let engine = Arc::new(IndexEngine {
            home_dir: home_dir.clone(),
            statuses: Arc::new(RwLock::new(HashMap::new())),
            repo_locks: Mutex::new(HashMap::new()),
            trigger_tx: trigger_tx.clone(),
            vector_index,
        });

        // Initialise status entries.
        {
            let mut statuses = engine.statuses.write().await;
            for repo in &settings.repos {
                statuses.insert(repo.clone(), RepoStatus::default());
            }
        }

        // Start watcher for each repo.
        for repo in settings.repos.clone() {
            let tx = trigger_tx.clone();
            let repo_path = repo.clone();
            tokio::spawn(async move {
                start_watcher(repo_path, tx).await;
            });
        }

        // Spawn the single consumer task.
        let engine_clone = engine.clone();
        let settings_clone = settings.clone();
        tokio::spawn(async move {
            run_consumer(engine_clone, trigger_rx, settings_clone).await;
        });

        engine
    }

    /// Send a manual trigger to index a single repo.
    pub async fn trigger_index(&self, repo: &str) -> Result<()> {
        self.trigger_tx
            .send(IndexTrigger {
                repo: repo.to_string(),
                changes: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("trigger channel closed: {e}"))?;
        Ok(())
    }

    /// Send a manual trigger to index all repos.
    pub async fn trigger_index_all(&self, repos: &[String]) -> Result<()> {
        for repo in repos {
            self.trigger_index(repo).await?;
        }
        Ok(())
    }

    /// Return per-repo status snapshot.
    pub async fn all_statuses(&self) -> HashMap<String, RepoStatus> {
        self.statuses.read().await.clone()
    }

    /// Return status for a single repo.
    pub async fn repo_status(&self, repo: &str) -> Option<RepoStatus> {
        self.statuses.read().await.get(repo).cloned()
    }

    async fn get_repo_lock(&self, repo: &str) -> Arc<Mutex<()>> {
        let mut locks = self.repo_locks.lock().await;
        locks
            .entry(repo.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Search the in-memory vector index for the top-k most similar chunks.
    ///
    /// This is a read-only, lock-free (read lock) operation. No DB call is
    /// made — all work happens in-process.
    pub async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<SearchResult> {
        let index = self.vector_index.read().await;
        index.search(query_embedding, top_k)
    }
}

// ─── Consumer task ────────────────────────────────────────────────────────

async fn run_consumer(
    engine: Arc<IndexEngine>,
    mut rx: tokio::sync::mpsc::Receiver<IndexTrigger>,
    settings: Settings,
) {
    while let Some(trigger) = rx.recv().await {
        let repo = trigger.repo.clone();
        let engine_ref = engine.clone();
        let settings_ref = settings.clone();

        // Acquire per-repo serialisation lock.
        let lock = engine_ref.get_repo_lock(&repo).await;
        let _guard = lock.lock().await;

        // Mark indexing. Reset progress counters so the UI shows the
        // indeterminate pulse (total_files == 0) until the pipeline reports a total.
        {
            let mut statuses = engine_ref.statuses.write().await;
            let status = statuses.entry(repo.clone()).or_default();
            status.state = IndexState::Indexing;
            status.error = None;
            status.indexed_files = 0;
            status.total_files = 0;
        }

        // Build embedding client — skip if no keys configured.
        let voyage_client = if settings_ref.embedding.api_keys.is_empty() {
            info!(repo = %repo, "no embedding API keys configured, skipping embed");
            None
        } else {
            match VoyageClient::new(
                settings_ref.embedding.model.clone(),
                settings_ref.embedding.api_keys.clone(),
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    error!(repo = %repo, error = %e, "failed to create voyage client");
                    let mut statuses = engine_ref.statuses.write().await;
                    let s = statuses.entry(repo.clone()).or_default();
                    s.state = IndexState::Error;
                    s.error = Some(e.to_string());
                    continue;
                }
            }
        };

        // Build a progress handle that lets the pipeline write live counts.
        let progress = ProgressHandle {
            statuses: Arc::clone(&engine_ref.statuses),
            repo: repo.clone(),
        };

        let pipeline = IndexPipeline::new(
            engine_ref.home_dir.clone(),
            repo.clone(),
            voyage_client,
        );

        match pipeline.run(trigger.changes, Some(&engine_ref.vector_index), Some(progress)).await {
            Ok(stats) => {
                info!(repo = %repo, indexed = stats.indexed_files, "indexing complete");
                let mut statuses = engine_ref.statuses.write().await;
                let s = statuses.entry(repo.clone()).or_default();
                s.state = IndexState::Idle;
                s.indexed_files = stats.indexed_files;
                s.total_files = stats.total_files;
                s.last_indexed_at = Some(Utc::now());
                s.error = None;
            }
            Err(e) => {
                error!(repo = %repo, error = %e, "indexing failed");
                let mut statuses = engine_ref.statuses.write().await;
                let s = statuses.entry(repo.clone()).or_default();
                s.state = IndexState::Error;
                s.error = Some(e.to_string());
            }
        }
    }
}
