//! Benchmarks for SQLite insert throughput — Issue #606
//!
//! Performance targets (from issue #606):
//!   - Single insert:  > 1,000 ops/sec
//!   - Batch insert:   > 10,000 ops/sec  (measured per-record inside a transaction)
//!
//! Because `SqliteStore` and `MemoryStore` in the devkit source are still stubs
//! (no real implementation), this benchmark creates a minimal inline SQLite
//! database via `rusqlite` to provide real, measurable numbers.  Once the
//! production `SqliteStore` is implemented, the setup helper below can be
//! swapped out for the real type.
//!
//! `rusqlite` is added as a dev-dependency with the `bundled` feature so that
//! the build is hermetic: libsqlite3 is compiled from source and statically
//! linked, removing any dependency on the host system's SQLite installation.
//! This keeps CI reproducible and avoids "libsqlite3.so not found" failures on
//! minimal Docker images.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A minimal, self-contained SQLite store used exclusively in this benchmark.
///
/// Opens a **temporary on-disk file** (via `tempfile`) so that I/O behaviour
/// reflects a real SQLite write path rather than the in-memory `:memory:`
/// shortcut which skips the page-cache entirely.
struct BenchStore {
    conn: Connection,
    /// Hold the tempfile handle so the file is not deleted until `BenchStore`
    /// is dropped.
    _tmp: tempfile::NamedTempFile,
}

impl BenchStore {
    fn new() -> Self {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let conn = Connection::open(tmp.path()).expect("open sqlite");

        // WAL mode — improves concurrent write throughput and is the
        // recommended mode for any write-heavy SQLite workload.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .expect("pragma");

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fee_records (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                fee_amount        INTEGER NOT NULL,
                ledger_sequence   INTEGER NOT NULL,
                timestamp_ms      INTEGER NOT NULL,
                transaction_hash  TEXT,
                is_spike          INTEGER NOT NULL DEFAULT 0,
                created_at        TEXT NOT NULL
            );",
        )
        .expect("create table");

        BenchStore { conn, _tmp: tmp }
    }

    /// Insert a single record outside of any explicit transaction.
    fn insert_single(&self, seq: u64) {
        self.conn
            .execute(
                "INSERT INTO fee_records
                    (fee_amount, ledger_sequence, timestamp_ms, transaction_hash, is_spike, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    100u64 + seq,
                    seq,
                    1_700_000_000_000i64 + seq as i64,
                    Option::<String>::None,
                    false,
                    "2024-01-01T00:00:00Z",
                ],
            )
            .expect("insert single");
    }

    /// Insert `n` records inside a single transaction (batch mode).
    fn insert_batch(&self, n: u64) {
        let mut stmt = self
            .conn
            .prepare_cached(
                "INSERT INTO fee_records
                    (fee_amount, ledger_sequence, timestamp_ms, transaction_hash, is_spike, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .expect("prepare");

        // A single BEGIN / COMMIT wrapper is the key to batch throughput.
        self.conn.execute_batch("BEGIN").expect("begin");

        for i in 0..n {
            stmt.execute(params![
                100u64 + i,
                i,
                1_700_000_000_000i64 + i as i64,
                Option::<String>::None,
                false,
                "2024-01-01T00:00:00Z",
            ])
            .expect("insert batch row");
        }

        self.conn.execute_batch("COMMIT").expect("commit");
    }
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// bench_single_insert — measures the cost of individual, auto-committed inserts.
///
/// Performance target: > 1,000 ops/sec  (Issue #606)
fn bench_single_insert(c: &mut Criterion) {
    let store = BenchStore::new();
    let mut seq = 0u64;

    let mut group = c.benchmark_group("storage_single_insert");
    group.throughput(Throughput::Elements(1));

    group.bench_function("sqlite_single_insert", |b| {
        b.iter(|| {
            store.insert_single(seq);
            seq += 1;
        });
    });

    group.finish();
}

/// bench_batch_insert — wraps N inserts in a single transaction.
///
/// Performance target: > 10,000 ops/sec per record  (Issue #606)
fn bench_batch_insert(c: &mut Criterion) {
    const BATCH_SIZE: u64 = 1_000;

    let store = BenchStore::new();

    let mut group = c.benchmark_group("storage_batch_insert");
    // Report throughput as individual record ops so Criterion shows ops/sec.
    group.throughput(Throughput::Elements(BATCH_SIZE));

    group.bench_with_input(
        BenchmarkId::new("sqlite_batch_insert", BATCH_SIZE),
        &BATCH_SIZE,
        |b, &n| {
            b.iter(|| store.insert_batch(n));
        },
    );

    group.finish();
}

criterion_group!(benches, bench_single_insert, bench_batch_insert);
criterion_main!(benches);
