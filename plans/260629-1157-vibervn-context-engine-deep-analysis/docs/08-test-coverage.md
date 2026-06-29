# 08 — Test Coverage & Test Quality

> Phần 8/14. Test inventory, what's NOT tested, fixture strategy, repro_notepad.rs analysis, CI test gate, mocking, property tests, coverage tooling, contract tests, naming convention, decisions & landmines.

## 8.1 Test Inventory

### Integration tests (`tests/`)

| File | Function | Line | Marker |
|---|---|---|---|
| `tests/integration.rs` | `test_get_creates_default` | 56 | `#[tokio::test]` |
| `tests/integration.rs` | `test_put_round_trips` | 94 | `#[tokio::test]` |
| `tests/integration.rs` | `test_unix_file_permissions` | 161 | `#[tokio::test]` |
| `tests/integration.rs` | `test_put_repo_then_query_passes_preflight` | 207 | `#[tokio::test]` |
| `tests/integration.rs` | `test_put_repo_registers_status` | 291 | `#[tokio::test]` |
| `tests/integration.rs` | `test_cancel_index_and_reindex` | 354 | `#[tokio::test]` |
| `tests/integration.rs` | `test_delete_repo_index_removes_directory` | 524 | `#[tokio::test]` |
| `tests/integration.rs` | `test_put_data_dir_persists_but_does_not_relocate` | 606 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `schema_ddl_flips_stale_symbol_so_native_insert_persists` | 15 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `insert_with_duplicate_id_merges_instead_of_failing` | 76 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `count_real_db_rows` | 129 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `repro_full_rebuild_notepad_ade_fresh_db` | 160 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `inspect_real_calls_indexes` | 229 | `#[tokio::test]` |
| `tests/repro_notepad.rs` | `repro_full_rebuild_notepad_ade_warm_cache` | 378 | `#[tokio::test]` |

### Inline unit tests (by file)

| File | `#[cfg(test)] mod` | Tests | Approx count |
|---|---|---|---|
| `src/lib.rs:41-86` | `path_in_repo_tests` | `rejects_prefix_collision`, `accepts_child_with_backslash`, `accepts_child_after_trailing_sep`, `accepts_exact_match`, `forward_slash_paths`, `mixed_separators`, `no_match_different_root` | 7 |
| `src/config.rs:585-955` | inline (16 `#[test]` at L592-L951) | version migrations v0→v7, default dirs, ignore-list migration, LLM config round-trips, Voyage base URL | 16 |
| `src/parsing/mod.rs:2880-3530` | 10 `#[cfg(test)] mod` blocks | tree-sitter extraction: TS, C#, PHP, Ruby, Kotlin, Swift, Dart, Lua, Luau, Pascal, Svelte, Liquid | ~30 |
| `src/vector/sharded.rs:308-504` | inline | `cross_shard_topk_equals_merged_topk` (L357), cross-shard ordering, shard math | 5 |
| `src/vector/mod.rs:311-509` | inline | round-trip, scoring, top-k | 8 |
| `src/indexing/walker.rs:275-620` | inline | ignore-filter, path normalization, traversal | 11 |
| `src/indexing/pipeline.rs:2709-4500` | 12 `#[cfg(test)] mod` blocks | incremental reindex, watcher-path isolation, embedding concurrency, embed errors, schema-mismatch guards | ~25 |
| `src/indexing/import_resolver.rs:484-761` | inline | import → file resolution | ~22 |
| `src/indexing/mod.rs:842-975` | inline | engine startup, repo registration | 4 |
| `src/embedding/voyage.rs:309-381` | inline | request shape, header handling, model-name paths | 9 |
| `src/embedding/cache.rs:276-402` | inline | MD5, encode/decode, lookup, atomic write, purge | 7 |
| `src/store/mod.rs:837-1786` | 9 `#[cfg(test)] mod` blocks | schema DDL, queries, transactions, pack/unpack round-trip | ~22 |
| `src/store/ops.rs:1055-1424` | 5 `#[cfg(test)] mod` blocks | ops atomicity, duplicate handling | ~12 |
| `src/mcp.rs:813-1307` | inline | tool-routing dispatch, schema validation, parameter shapes | ~31 |
| `src/defender.rs:307-338` | inline | process/data-dir exclusion probe | 2 |
| `src/query/reranker.rs:1825-2340` | inline | `MockBackend` for agentic rerank loop | ~16 |

**Approximate total: ~230 test functions** (split roughly 230 inline / 14 integration).

## 8.2 What's NOT Tested

| Gap | Status | Evidence |
|---|---|---|
| **SSE stream test** | Absent | `sse`/`EventStream`/`ServerEvent` matches: zero trong `src/`. `server.rs` wires `streamable_http_server` nhưng không client-side stream consumer test. |
| **MCP protocol round-trip** | Absent | `rmcp` appears chỉ trong `src/mcp.rs` (server-side) và `src/server.rs:24` (transport). Không `rmcp` client harness, không `cargo test` driving a `streamable_http_server` chống real client. `src/mcp.rs:813-1307` tests chỉ parameter-shape validation trong isolation. |
| **File watcher** | Absent | `src/indexing/watcher.rs` (whole module) has zero `#[test]`. Watcher-path is exercised qua `pipeline.rs:4225` (`watcher_path_processes_only_explicit_changes_no_full_walk`) which bypasses `notify` entirely. |
| **Embedding cache** | Present but narrow | `src/embedding/cache.rs:276-402` covers encode/MD5/lookup/atomic-write/purge — nhưng **không concurrent-access test**, không corrupt-file recovery test, không model-isolation test. |
| **Graph expansion** | Absent | `src/query/graph_expand.rs:64` has zero tests despite being called từ `engine.rs:226,471`. |
| **Rerank LLM (live)** | Absent by design | `src/query/reranker.rs:1825-2340` dùng scripted `MockBackend` chỉ. Không live API test. |
| **Vector index scaling** | Absent | `vector/sharded.rs` tests 3 repos với ≤17 items total. Không test với realistic shard counts, large dims, hoặc O(n log n) top-k vs O(n²) sanity. |
| **Unix perm bit** | Gated by CI | `tests/integration.rs:161` (`test_unix_file_permissions`) — runs unconditionally nhưng path check `#[cfg(unix)]` matters; trên Windows CI matrix it likely no-ops. |
| **HTTP API surface** | Absent | `src/server.rs` has không `#[test]`. Không `reqwest::Client` calling bất kỳ route. |
| **Server startup race** | Absent | `src/main.rs` boots `IndexEngine`, watchers, MCP — không integration test cho boot path. |

## 8.3 Fixture Strategy

- **Không committed fixture files.** Grep cho `fixtures` / `testdata` /
  `tests/fixtures` / `tests/data` → không files matched.
- **Không committed fixture directory.** Tất cả test data is synthesized inline
  (string literals cho tree-sitter, `TempDir` cho storage).
- **`tempfile = "3"`** appears as both `[dependencies]` (L21) và
  `[dev-dependencies]` (L69) — duplication is harmless nhưng redundant.
- **Không `insta` snapshot tests**, không `wiremock`/`httpmock`, không
  `assert_cmd` cho binary smoke.

## 8.4 `repro_notepad.rs` — 456 dòng code smell

File này is a regression-investigation scratchpad promoted tới `tests/`. 6
`#[tokio::test]` functions, all of which are debug artifacts:

| Line | Name | What it does |
|---|---|---|
| 15 | `schema_ddl_flips_stale_symbol_so_native_insert_persists` | Proves a bug-fix works. Real regression test — defensible. |
| 76 | `insert_with_duplicate_id_merges_instead_of_failing` | Real regression test. |
| 129 | `count_real_db_rows` | **Diagnostic / smoke** — prints row counts, not assertions. |
| 160 | `repro_full_rebuild_notepad_ade_fresh_db` | **Repro harness** — built tới reproduce an issue, not tới assert correctness. |
| 229 | `inspect_real_calls_indexes` | **Diagnostic** — prints internal state. Không assertions. |
| 378 | `repro_full_rebuild_notepad_ade_warm_cache` | **Repro harness** — warm-cache variant. |

**Smell confirmed.** 3 of 6 are diagnostic/repro tests mà pin library internals
và may fail trên unrelated schema changes. Chúng occupy the `tests/` namespace
và run trên mọi `cargo test`, inflating CI time và emitting noisy failures.
Chúng nên live in a `tests/repro/` subdirectory với a doc-comment banner hoặc
behind a feature flag, không in the root integration test set.

## 8.5 CI Test Gate

`release.yml` (only workflow):
- **Triggered trên PRs tới `master`** (L7-9) ✓
- **Không `cargo test` step.** Zero matches cho `cargo test` hoặc
  `cargo nextest` anywhere trong `.github/workflows/`.
- Line 75: `cargo build --release --locked --target ${{ matrix.target }}` —
  build only.
- Line 122, 126: shell `test -f` checks binary existence (không Rust tests).

**PRs có thể merge với failing tests.** This is major gap. The only correctness
gate là `cargo build` succeeding, mà catches type errors và missing imports —
nothing else.

## 8.6 Mocking Strategy

| Kind | Where | Quality |
|---|---|---|
| **`MockBackend` (LLM)** | `src/query/reranker.rs:1825-2340` — scripted turn deque, `AgenticBackend` impl. Used bởi ~16 tests. | Excellent — drives agentic loop deterministically với error/timeout/empty variants. |
| **Voyage client** | **Không mock.** `src/embedding/voyage.rs:309-381` tests request *shapes* (URL, headers, model param) nhưng never a mocked HTTP response. Live API required cho end-to-end. |
| **HTTP / MCP transport** | **Không mock.** Server-side only. |
| **Tree-sitter fixtures** | Hand-built source-string fixtures inline trong `parsing/mod.rs`. Không fixtures trên disk. Fine — tree-sitter tests are inherently fixture-free. |
| **`MockTurn`** | Enum pattern trong `reranker.rs:1834` — `Calls`/`Text`/`Err` variants exhaustively cover loop exit conditions. | Worth copying. |

Không `wiremock`, không `httpmock`, không trait-based abstraction over `reqwest::Client`.

## 8.7 Property Tests / Fuzz

Zero. Grep cho `proptest`, `quickcheck`, `cargo-fuzz` → không matches. Không in
`[dev-dependencies]` either.

## 8.8 Coverage Tooling

Zero. Grep cho `tarpaulin`, `grcov`, `llvm-cov` → không matches. Không
coverage config, không CI upload step.

## 8.9 Contract Tests

Named property-style assertions are scattered:

| File:Line | Contract |
|---|---|
| `src/vector/sharded.rs:357` `cross_shard_topk_equals_merged_topk` | Partitioned top-k must equal merged top-k. |
| `src/store/mod.rs:1594` `pack_unpack_roundtrip_exact` | `decode(pack(v)) == v` bit-exact cho f32. |
| `src/store/mod.rs:1389` `schemaless_roundtrip_integrity` | Schema flip + data round-trip. |
| `src/lib.rs:46-84` `path_in_repo_tests` | 7 cases of path-prefix containment (collision, mixed separators, trailing sep). |
| `src/store/mod.rs:1636` `empty_embedding_roundtrips_empty` | Empty embedding survives pack/unpack. |
| `src/embedding/cache.rs:297-301` | Reject malformed/short/empty bytes. |
| `src/parsing/mod.rs:2895+` | Per-language tree-sitter extraction contracts (một test per language). |

Đây are **explicitly named property-style** nhưng written as `#[test]` not
`proptest`. Quality is high; format is conventional.

## 8.10 Test Naming Convention

**Mixed nhưng biased `test_` (Rust-idiomatic):**
- `test_*` — all of `tests/integration.rs` (8), `tests/repro_notepad.rs` (6),
  `src/lib.rs` (none — uses descriptive names), `src/store/mod.rs:1532+` (uses
  descriptive names like `schemaless_roundtrip_integrity`).
- Descriptive-only — `src/store/mod.rs:1594` `pack_unpack_roundtrip_exact`,
  `src/embedding/cache.rs:284` `encode_decode_roundtrip`,
  `src/vector/sharded.rs:357` `cross_shard_topk_equals_merged_topk`.
- Integration tests are **inconsistent** với inline tests:
  `test_get_creates_default` vs `pack_unpack_roundtrip_exact`. Không documented
  convention.

Inline tests trong `parsing/mod.rs` follow the strictest convention
(`test_basic_function_extraction`, `test_nested_namespace_scope_path` — all
`test_` prefix).

## 8.11 Worth-Defending Decisions

1. **`MockBackend` script-driven LLM tests** (`src/query/reranker.rs:1825-2340`)
   — deque of `MockTurn` variants exhaustively covers loop termination, errors,
   và budget exhaustion. Cleanest agentic-loop test pattern I've seen.
2. **Schema migration tests as concrete version-to-version pairs**
   (`src/config.rs:592-955`) — mỗi migration gets a `test_vN_to_vM_migration_
   stamps_X` mà asserts exact output shape. Không meta-framework, không
   migration DSL, just direct assertions.
3. **Per-language parsing tests** (`src/parsing/mod.rs`) — mỗi language gets
   its own `#[cfg(test)] mod` block với a handful of hand-crafted source
   strings. Keeps fixtures co-located với parser they exercise.
4. **Bit-exact pack/unpack test** (`src/store/mod.rs:1594`) — names the
   property (`roundtrip_exact`), exercises negative/zero/small-fraction values.
   Catches endian flips immediately.
5. **Cross-shard top-k = merged top-k** (`src/vector/sharded.rs:357`) — names
   the invariant being defended. Easy tới understand, easy tới extend.

## 8.12 Anti-Patterns / Landmines

1. **`tests/repro_notepad.rs` as a regression-investigation scratchpad** — 3
   of 6 tests are diagnostic/repro harnesses, not assertions. Chúng pin
   internal row counts và index layouts; bất kỳ unrelated schema change breaks
   them. Belongs behind a feature flag hoặc trong `examples/`, không `tests/`.
2. **Không `cargo test` trong CI** — `.github/workflows/release.yml` runs trên
   PRs nhưng chỉ invokes `cargo build`. Tests are advisory; chúng cannot block
   a merge. The 230+ test functions are documentation, not a safety net.
3. **Không MCP protocol round-trip test** — `src/mcp.rs:813-1307` validates
   parameter shapes trong isolation, nhưng nothing actually wires an `rmcp`
   client tới `streamable_http_server` và asserts JSON-RPC compliance. Public
   MCP contract is untested end-to-end.
4. **Không HTTP API test cho `src/server.rs`** — entire Axum surface
   (`src/server.rs` với `axum = "0.7"`) has zero `#[test]`. Mọi route is
   integration-tested bởi hand hoặc not at all. A refactor of route handlers
   cannot break CI.
5. **`tempfile` duplicated trong `[dependencies]` và `[dev-dependencies]`**
   (Cargo.toml L21, L69) — minor, nhưng indicates dev-deps weren't fully thought
   through. Quan trọng hơn, `tempfile` is the only test-helper crate. Không
   `insta`, không `wiremock`, không `assert_cmd`, không `pretty_assertions` —
   mọi test reinvents wheel cho fixture setup.
6. **No assertion cho LLM rerank behavior change** — nếu LLM silently degrades
   tới "return input order", tests don't catch it. Xem [[04-query-embed-store#4.12|Anti-pattern §4.12]].
7. **No embedded-DB integration test in CI** — `tests/integration.rs` tests
   HTTP but each integration test spawns its own server. Shared DB tests across
   parallel CI jobs would race; current setup is single-process, single-DB.

## 8.13 Open Questions

- Does project intend `cargo test` tới run chỉ locally, hoặc is CI integration
  on a roadmap?
- Is the `MockBackend` pattern trong `reranker.rs` considered house style, hoặc
  did it grow organically?
- Is `repro_notepad.rs` a known cleanup target, hoặc is it "tribal knowledge"
  intentionally left cho future debugging?

Xem [[07-failure-modes]] cho runtime failures untested, [[10-patterns-to-copy]] cho test patterns worth copying.
