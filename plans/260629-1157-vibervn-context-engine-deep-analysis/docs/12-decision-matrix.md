# 12 — Decision Matrix

> Phần 12/14. Quick lookup: situation → pattern. Đọc doc này khi bắt đầu build MCP project mới, dùng [[10-patterns-to-copy]] và [[11-anti-patterns]] làm references.

## 12.1 By Situation

### Boot & Config

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Long-running service, user-editable config | **P2 Boot-frozen resolved paths** (`main.rs:142-184`) | Re-reading Settings at runtime (split-brain) |
| Build CLI > env > Settings > default chain | **P2 + `resolve_or_default` helper** | Duplicating inline 4× trong main.rs (Anti-Pattern #27) |
| User-edited config cần migrate | **P5 JSON version + MIGRATIONS array** | Implicit schema migration; no version stamp |
| Config file mutation | **P6 Atomic config write** (NamedTempFile + 0o600) | Bare `fs::write` (no atomicity) |

### MCP Server

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Build MCP tool có REST proxy | **P1 Single shared query funnel** | Duplicating handler logic (Anti-Pattern #28) |
| rmcp tool input validation | **P7 schemars::JsonSchema + Parameters<T>** | Manual `serde_json::json!()` schemas |
| LLM-callable tool | **P8 Never-Err pattern** | Returning `Result::Err` (tears down session) |
| rmcp per-session config | **P9 Per-session handler factory** | Global Mutex<Config> |
| MCP tool returns lớn | **P10 Output budget assembly** (48K ceiling) | Silent truncation (Anti-Pattern #7) |
| Per-repo MCP service | **P11 Per-repo service caching** | Construct service per request (expensive) |
| MCP tool description | **Keep it semantic, no imperative** | Prompt injection (Anti-Pattern #6) |
| Long-running MCP tool | **P9 streaming if available, else document blocking** | Busy-poll (Anti-Pattern #8); No streaming (Anti-Pattern #9) |

### Database / Storage

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Embedded DB (RocksDB/LMDB/Sled) | **P3 Per-repo open gate** | 2 callers race exclusive lock |
| DB có thể corrupt từ OS/process crash | **P4 open_or_reset_index self-heal** | Fail-fast với no recovery |
| DB có nhiều secondary indexes | **P13 Drop-then-bulk-insert-then-rebuild** | Insert-then-update-index per row |
| Embedded DB schema cần evolve | **P21 DB_SCHEMA_VERSION + dual-format readers** | Strict reader (breaks trên migration) |
| Một Surreal<Db> per repo | **P22 Lock discipline** | Holding RwLock across await |

### Indexing Pipeline

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Write pipeline có derived data | **P12 Ordered writes + commit marker** | Write derived data first, no marker |
| Derived per-file artifact | **P14 Chunking version const** | Manual DB schema bump |
| Code graph builder | **P15 5-level candidate selection** | Single global symbol table |
| Streaming pipeline với bounded memory | **P16 Bounded mpsc channels** | Unbounded channels (memory bloat) |
| Large pipeline file (>1500 dòng) | **Split thành parse/embed/store/phase2** | God file (Anti-Pattern #1) |
| Language dispatch | **Single source of truth, trait dispatch** | Two language maps (Anti-Pattern #12) |
| Raw edges staging | **Typed enum `EdgeSink::Ram \| EdgeSink::Db`** | Path-dependent crash-safety (Anti-Pattern #13) |
| Cross-process safety | **PID file + advisory lock** | Per-repo lock only (Anti-Pattern #14) |

### Query / Search

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Per-tenant in-memory vector index | **P18 Sharded + atomic-stamp LRU** | Global lock per shard (Anti-Pattern #19) |
| Vector search | **P19 Pre-normalize + dot product** | Per-candidate division (slow) |
| Free-form query có filter prefix | **P20 Filter stripping trước embed** | Embed cả filters (waste API cost) |
| Query state object | **Struct, không 14-arg function** | `#[allow(too_many_arguments)]` (Anti-Pattern #5) |
| BFS graph expansion | **Config-driven depth, score factors** | Hardcoded magic constants (Anti-Pattern #16) |
| Cross-file symbol lookup | **Look up by primary key** | Parse FQN to derive file (Anti-Pattern #17) |
| LLM rerank optional | **`Option<&LlmClient>`** | Hard-required, no graceful degrade |
| Rerank correctness | **Regression test verifies LLM output ≠ input order** | No eval harness (Anti-Pattern #18) |

### Async / Concurrency

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Async code với RwLock | **P22 Clone guard payload, drop guard, await** | Holding lock across await |
| Long-running consumer với killable sleep | **`tokio::select!` vs cancel_token** | Uninterruptible sleep (Anti-Pattern in §7) |
| 2 mutex flavours mix | **Centralise on one flavour** | Std + Async + 3 Arc<RwLock> mix (Anti-Pattern #4) |

### Caching

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Embed/summary cache nhiều repo | **P17 Content-addressed (md5(text+model))** | Repo-scoped cache (misses dedup) |
| Atomic file write | **NamedTempFile + persist** | Bare `fs::write` (no crash safety) |
| External cache eviction | **mtime-touch on read = free LRU** | In-memory LRU (extra state) |
| Cache corruption detection | **Magic number + dim header** | Only length check (silent garbage) |

### HTTP / Web

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| MCP service caching | **P11 Per-repo service caching** | Construct per session (expensive) |
| HTTP state injection | **axum `State` + clone Arc-wrapped state** | Pass via `Context` (overkill) |
| Web UI deps | **Vendored Tailwind + SRI** | CDN without SRI (Anti-Pattern #24) |
| 239KB+ single-file SPA | **Build step + content-hashed assets** | Single file với CDN (Anti-Pattern #26) |

### Security

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| API keys storage | **Encrypted at rest + masked in response** | Plaintext in JSON response (Anti-Pattern #21) |
| Path traversal | **`path_in_repo` explicit separator check** (`lib.rs:22-39`) | Just `starts_with` (collision bug) |
| MCP tool auto-register paths | **Allowlist, not directory-exists check** | Auto-add bất kỳ dir (Anti-Pattern #23) |
| PowerShell execution | **Sanitize paths + UTF-16LE base64** | `format!()` without metachar check (Anti-Pattern #25) |
| TLS to API | **rustls-tls + default cert validation** | `.danger_accept_invalid_certs` |
| Bind 0.0.0.0 vs loopback | **No — always default loopback** | Unauthenticated LAN exposure |

### CI / Release

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Multi-platform CLI distribution | **npm optionalDependencies + cli.js shim** | Native build toolchain on user machines |
| Versioning | **Cargo.toml version + auto-bump patch** | `sed` + `awk` round-trip on Cargo.lock |
| CI | **`cargo test --workspace` on PRs** | Build only (Anti-Pattern #30) |

### Testing

| Situation | Pattern | Anti-Pattern to Avoid |
|---|---|---|
| Agentic loop tests | **MockBackend deque of MockTurn variants** | No mock → live API required |
| Schema migration tests | **Concrete version-pair tests** | Meta-framework / DSL |
| Per-language parser tests | **Co-located inline `#[cfg(test)] mod`** | External fixtures directory |
| Property-style assertions | **Named `#[test]` functions** | proptest/quickcheck (overkill cho đa số) |
| Cache tests | **MD5 + encode/decode + atomic write + purge** | No concurrent-access test |
| Repro scripts | **`examples/` hoặc `tests-regress/` target** | Root `tests/` directory (Anti-Pattern #29) |

## 12.2 By Risk Tier

| Tier | Key Patterns | Key Anti-Patterns |
|---|---|---|
| **T0 (security critical)** | path_in_repo, content-addressed cache, atomic write, mtime-touch LRU, schemars + Parameters<T>, Never-Err pattern | Auto-add paths, format! PowerShell, plaintext API keys, CDN no SRI |
| **T1 (standard MCP)** | Single shared query funnel, per-session handler factory, output budget assembly, sharded LRU | Dual handler types, busy-poll, silent truncation, god file |
| **T2 (tooling/scripts)** | Atomic config write, MIGRATIONS array, manual npm multi-platform | sed/awk Cargo.lock round-trip, no test step in CI |
| **T3 (spike)** | (any of the above as appropriate) | (avoid all anti-patterns) |

## 12.3 By Project Phase

### Phase 1 — Initial scaffold
- P2 Boot-frozen config
- P6 Atomic config write
- P22 Lock discipline
- P7 schemars + Parameters<T>
- P8 Never-Err pattern

### Phase 2 — Core MCP tools
- P1 Single shared query funnel
- P10 Output budget assembly
- P9 Per-session handler factory
- P11 Per-repo service caching

### Phase 3 — Persistence / DB
- P3 Per-repo open gate
- P4 open_or_reset_index self-heal
- P13 Drop-then-bulk-insert-then-rebuild
- P21 DB_SCHEMA_VERSION + dual-format

### Phase 4 — Indexing pipeline
- P12 Ordered writes + commit marker
- P14 Chunking version const
- P16 Bounded mpsc channels
- P15 5-level candidate selection

### Phase 5 — Query / search
- P17 Content-addressed cache
- P18 Sharded vector index
- P19 Pre-normalize + dot product
- P20 Filter stripping

### Phase 6 — Hardening
- Atomic write audit
- path_in_repo everywhere
- TLS validation enforced
- mtime-touch LRU
- Cache corruption detection
- Regression tests for LLM-driven logic

### Phase 7 — Distribution
- npm optionalDependencies pattern
- CI `cargo test --workspace`
- Per-version schema migration tests
- Repro scripts in `examples/`

## 12.4 Quick Reference: 5-Minute Audit

Khi bạn đọc code mới, 5 câu hỏi audit:

1. **Có boot-frozen config không?** Hoặc runtime re-derive từ Settings? (Risk:
   split-brain)
2. **Có single shared query funnel không?** Hoặc duplicate handler logic?
   (Risk: drift)
3. **Có per-repo open gate không?** Hoặc race on exclusive lock? (Risk:
   corruption)
4. **Có atomic file write không?** Hoặc `fs::write`? (Risk: partial files
   on crash)
5. **Có mtime-touch LRU không?** Hoặc in-memory LRU? (Risk: extra state,
   not restart-safe)

Nếu 5/5 = YES → greenfield chất lượng cao.
Nếu <3/5 = YES → re-evaluate before adopting.

Xem [[10-patterns-to-copy]] cho full pattern list, [[11-anti-patterns]] cho
20 landmines consolidated, [[13-snippets]] cho copy-paste-ready code.
