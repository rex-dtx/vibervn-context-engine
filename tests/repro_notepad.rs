use context_engine_rs::indexing::pipeline::IndexPipeline;
use context_engine_rs::store::open_db;
use std::time::Instant;
use tempfile::TempDir;

#[tokio::test]
async fn repro_full_rebuild_notepad_ade_fresh_db() {
    // Surface info-level logs (incl. the `PERF SUMMARY` line) on stderr.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("context_engine_rs=info")),
        )
        .with_test_writer()
        .try_init();

    let home = TempDir::new().unwrap();
    let repo = "D:/projects/Cpp/notepad-ade".to_string();
    if !std::path::Path::new(&repo).exists() {
        eprintln!("SKIP: source repo not present");
        return;
    }
    let db = open_db(home.path(), &repo).await.expect("open fresh db");
    // voyage = None — exercises parse + all DB writes + Phase 2 with zero embedding
    // work (the all-cached / no-network floor used for every prior measurement).
    let pipeline = IndexPipeline::new(repo.clone(), None);

    let start = Instant::now();
    let res = pipeline.run(&db, None, true, None, None, None, &[]).await;
    let wall = start.elapsed();

    match res {
        Ok(s) => eprintln!(
            "REPRO OK: indexed={} total={} wall={:.1}s",
            s.indexed_files,
            s.total_files,
            wall.as_secs_f64()
        ),
        Err(e) => panic!("REPRO FAILED at: {e:#}"),
    }
}

/// Diagnostic test: inspect the real ~/.vibervn calls indexes and repair any
/// incomplete/missing ones synchronously.
///
/// WRITES TO THE REAL ~/.vibervn INDEX — do NOT run as part of normal CI.
/// Run explicitly with:
///   cargo test --release --test repro_notepad inspect_real_calls_indexes -- --ignored --nocapture
///
/// This test:
///   1. Opens the real SurrealDB for D:/projects/Cpp/notepad-ade
///   2. Runs INFO FOR TABLE calls to inspect index definitions/states
///   3. If any of the 4 idx_calls_* indexes is missing or stuck "building",
///      repairs it synchronously: REMOVE + DEFINE without CONCURRENTLY
///   4. Verifies via a second INFO FOR TABLE calls that all 4 are complete
#[ignore]
#[tokio::test]
async fn inspect_real_calls_indexes() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("context_engine_rs=info")),
        )
        .with_test_writer()
        .try_init();

    let repo = "D:/projects/Cpp/notepad-ade".to_string();
    if !std::path::Path::new(&repo).exists() {
        eprintln!("SKIP: source repo not present at {repo}");
        return;
    }

    let home_dir = dirs::home_dir().expect("dirs::home_dir() must return a value on this platform");

    let surreal_dir = home_dir
        .join(".vibervn")
        .join("context-engine")
        .join("surreal")
        .join("D__projects_Cpp_notepad_ade");
    if !surreal_dir.exists() {
        eprintln!("SKIP: real surreal DB not found at {}", surreal_dir.display());
        return;
    }

    let db = open_db(&home_dir, &repo)
        .await
        .expect("open real surreal db");

    // ── BEFORE: inspect calls indexes ───────────────────────────────────────
    let before: Option<serde_json::Value> = db
        .query("INFO FOR TABLE calls")
        .await
        .expect("INFO FOR TABLE calls")
        .take(0)
        .ok()
        .flatten();
    let before = before.unwrap_or(serde_json::Value::Null);
    eprintln!("=== BEFORE: INFO FOR TABLE calls ===");
    eprintln!("{}", serde_json::to_string_pretty(&before).unwrap_or_else(|_| format!("{before:?}")));

    // Extract index names from the before state.
    // SurrealDB returns { "indexes": { "idx_calls_in_file": "DEFINE INDEX ...", ... } }
    let expected_indexes = [
        "idx_calls_in_file",
        "idx_calls_out_file",
        "idx_calls_in_name",
        "idx_calls_out_name",
    ];

    let indexes_obj = before
        .get("indexes")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // Check which indexes are missing or appear to be in a CONCURRENTLY/building state.
    let mut needs_repair = false;
    for name in &expected_indexes {
        match indexes_obj.get(*name) {
            None => {
                eprintln!("REPAIR NEEDED: index '{name}' is MISSING");
                needs_repair = true;
            }
            Some(def) => {
                let def_str = def.to_string();
                if def_str.contains("CONCURRENTLY") || def_str.contains("building") {
                    eprintln!("REPAIR NEEDED: index '{name}' appears incomplete/building: {def_str}");
                    needs_repair = true;
                } else {
                    eprintln!("OK: index '{name}' present: {def_str}");
                }
            }
        }
    }

    if needs_repair {
        eprintln!("Repairing calls indexes synchronously (REMOVE + DEFINE without CONCURRENTLY)...");
        db.query(
            "REMOVE INDEX IF EXISTS idx_calls_in_file  ON calls; \
             REMOVE INDEX IF EXISTS idx_calls_out_file ON calls; \
             REMOVE INDEX IF EXISTS idx_calls_in_name  ON calls; \
             REMOVE INDEX IF EXISTS idx_calls_out_name ON calls;"
        ).await.expect("remove calls indexes for repair");

        db.query(
            "DEFINE INDEX IF NOT EXISTS idx_calls_in_file  ON calls FIELDS in_file; \
             DEFINE INDEX IF NOT EXISTS idx_calls_out_file ON calls FIELDS out_file; \
             DEFINE INDEX IF NOT EXISTS idx_calls_in_name  ON calls FIELDS in_name; \
             DEFINE INDEX IF NOT EXISTS idx_calls_out_name ON calls FIELDS out_name;"
        ).await.expect("rebuild calls indexes synchronously");

        eprintln!("Repair complete.");
    } else {
        eprintln!("All 4 calls indexes look complete — no repair needed.");
    }

    // ── AFTER: re-inspect to confirm ────────────────────────────────────────
    let after: Option<serde_json::Value> = db
        .query("INFO FOR TABLE calls")
        .await
        .expect("INFO FOR TABLE calls after repair")
        .take(0)
        .ok()
        .flatten();
    let after = after.unwrap_or(serde_json::Value::Null);
    eprintln!("=== AFTER: INFO FOR TABLE calls ===");
    eprintln!("{}", serde_json::to_string_pretty(&after).unwrap_or_else(|_| format!("{after:?}")));

    // Verify all 4 indexes are present and not CONCURRENTLY after repair.
    let after_indexes = after
        .get("indexes")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    for name in &expected_indexes {
        match after_indexes.get(*name) {
            None => panic!("FAIL: index '{name}' still missing after repair"),
            Some(def) => {
                let def_str = def.to_string();
                assert!(
                    !def_str.contains("CONCURRENTLY"),
                    "index '{name}' still has CONCURRENTLY after repair: {def_str}"
                );
                eprintln!("VERIFIED: index '{name}' present and synchronous");
            }
        }
    }
}

/// Warm-cache full-rebuild benchmark.
///
/// WRITES TO THE REAL ~/.vibervn INDEX — do NOT run as part of normal CI.
/// Run explicitly with:
///   cargo test --release --test repro_notepad repro_full_rebuild_notepad_ade_warm_cache -- --ignored --nocapture
///
/// Prerequisites:
///   1. The source repo D:/projects/Cpp/notepad-ade must be present.
///   2. The embedding cache dir ~/.vibervn/context-engine/embeddings/voyage-4-lite must exist
///      (delete the surreal DB first to force a real full rebuild; the cache survives).
///
/// What it measures: the complete production path including cache-READ time for ~53K .bin files.
#[ignore]
#[tokio::test]
async fn repro_full_rebuild_notepad_ade_warm_cache() {
    // Surface info-level logs including the PERF SUMMARY line.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("context_engine_rs=info")),
        )
        .with_test_writer()
        .try_init();

    let repo = "D:/projects/Cpp/notepad-ade".to_string();
    if !std::path::Path::new(&repo).exists() {
        eprintln!("SKIP: source repo not present at {repo}");
        return;
    }

    let home_dir = dirs::home_dir().expect("dirs::home_dir() must return a value on this platform");

    // Guard: confirm the embedding cache exists (otherwise the test would just benchmark
    // empty-embedding writes, not the warm-cache-read path the user cares about).
    let cache_dir = home_dir
        .join(".vibervn")
        .join("context-engine")
        .join("embeddings")
        .join("voyage-4-lite");
    if !cache_dir.exists() {
        eprintln!(
            "SKIP: embedding cache dir not found at {}",
            cache_dir.display()
        );
        return;
    }

    // Build EmbeddingCache pointed at the real on-disk cache.
    use context_engine_rs::embedding::cache::EmbeddingCache;
    let cache = match EmbeddingCache::new(&home_dir, "voyage-4-lite") {
        Some(c) => c,
        None => {
            eprintln!("SKIP: could not open EmbeddingCache (new returned None)");
            return;
        }
    };

    // Open (or create) the real SurrealDB at ~/.vibervn/context-engine/surreal/…
    // Note: the surreal dir should be deleted before running this test so the
    // rebuild is genuinely forced from scratch, but open_db handles both cases.
    let db = open_db(&home_dir, &repo)
        .await
        .expect("open real surreal db");

    // voyage = None: on a ~100% warm cache the API is never needed.
    // The concurrency of 4 matches the default in IndexPipeline::new().
    let pipeline = IndexPipeline::new_with_concurrency(repo.clone(), None, 4, Some(cache));

    let start = Instant::now();
    let res = pipeline.run(&db, None, true, None, None, None, &[]).await;
    let wall = start.elapsed();

    match res {
        Ok(s) => {
            eprintln!(
                "REPRO WARM-CACHE OK: indexed={} wall={:.1}s \
                 cache_hit_chunks={} cache_miss_chunks={} embed_total_ms={}",
                s.indexed_files,
                wall.as_secs_f64(),
                s.cache_hit_chunks,
                s.cache_miss_chunks,
                s.embed_total_ms,
            );
        }
        Err(e) => panic!("REPRO WARM-CACHE FAILED: {e:#}"),
    }
}

