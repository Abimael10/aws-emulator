// SPDX-License-Identifier: CC0-1.0

//! The two engine facts this crate's own cases need the PROCESS or a second surface for:
//! SQLite's heap ceiling is process-wide, and what a backing another surface writes does to
//! this one's view of it is the engine's own report — so both live beside the code rather
//! than in `tests/all/`.

use super::*;

/// The lock a case of this module holds while it needs the ENGINE's process-wide
/// configuration to stand still.
///
/// SQLite's heap ceiling is ONE setting for every database in this process, so the case that
/// lowers it and a case that OPENS a database (which refuses a ceiling it cannot install)
/// cannot run at the same time. Taking the lock is what orders them; nothing here waits on a
/// clock.
static THE_ENGINES_OWN_CONFIGURATION: Mutex<()> = Mutex::new(());

/// A heap ceiling low enough that a statement writing a few megabytes of pages
/// runs into it, and high enough that the engine can still prepare statements
/// (so the ceiling can be put back afterwards).
const PROBE_HEAP_BYTES: i64 = 16 * 1024 * 1024;

/// Half a megabyte per row, under [`MAX_VALUE_BYTES`], so what the statements
/// below run into is the HEAP ceiling and not the per-value limit.
const PROBE_VALUE_BYTES: i64 = 512 * 1024;

/// Enough rows to need four times [`PROBE_HEAP_BYTES`] of pages.
const PROBE_ROWS: usize = 128;

/// Set SQLite's process-wide heap ceiling, through a connection of this test's
/// own: a confined [`Database`] refuses `PRAGMA` to anything, including this
/// crate, once its authorizer is installed.
fn set_process_heap_ceiling(bytes: i64) {
    let probe = Connection::open_in_memory().expect("a connection for the pragma");
    probe
        .pragma_update(None, "hard_heap_limit", bytes)
        .expect("the engine takes a heap ceiling");
}

/// `PRAGMA hard_heap_limit` bounds the ENGINE's allocator, so a statement whose
/// pages would take the engine past the ceiling is REFUSED by the engine
/// instead of growing this process — the memory axis the page, step, time and
/// output bounds do not cover.
///
/// SQLite's heap limit is process-wide, which is why this lives beside the code
/// rather than in `tests/all/`: lowering it is visible to every database in the
/// process, so the window is held open for one statement and closed again,
/// where the wire suite's databases (which deliberately fill tens of megabytes)
/// cannot see it.
#[test]
fn the_engine_heap_ceiling_refuses_an_allocation_heavy_statement() {
    let _in_turn = THE_ENGINES_OWN_CONFIGURATION
        .lock()
        .unwrap_or_else(::std::sync::PoisonError::into_inner);
    let mut database = Database::open("probe", EngineFamily::default(), None)
        .expect("a confined database opens");
    database
        .run_within(
            &mut RequestBudget::new(),
            "CREATE TABLE blobs (payload BLOB)",
            &[],
        )
        .expect("the table is created under the configured ceiling");

    // The ceiling is the only thing that changes for the length of this loop.
    set_process_heap_ceiling(PROBE_HEAP_BYTES);
    let mut refused = None;
    let mut rows_written = 0_usize;
    for _ in 0..PROBE_ROWS {
        match database.run_within(
            &mut RequestBudget::new(),
            &format!("INSERT INTO blobs (payload) VALUES (zeroblob({PROBE_VALUE_BYTES}))"),
            &[],
        ) {
            Ok(_) => rows_written += 1,
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
    }
    set_process_heap_ceiling(MAX_ENGINE_HEAP_BYTES);

    let refused = refused.unwrap_or_else(|| {
        panic!(
            "case \"rows past the heap ceiling\": the engine accepted {rows_written} rows of \
                 {PROBE_VALUE_BYTES} bytes under a {PROBE_HEAP_BYTES}-byte ceiling, so memory is \
                 not bounded"
        )
    });
    let message = refused.message();
    assert!(
        matches!(refused, SqlError::Capacity(_)) && message.contains("memory ceiling"),
        "case \"rows past the heap ceiling\": expected the engine's out-of-memory refusal, \
             reported as this emulator's capacity, after {rows_written} rows, got \
             {refused:?} ({message})"
    );

    // The ceiling is back, so the engine serves statements again: the bound
    // refuses work, it does not break the database it was applied to.
    database
        .run_within(&mut RequestBudget::new(), "SELECT count(*) FROM blobs", &[])
        .expect("the database still answers once the ceiling is restored");
}

/// What one row of the case table below does to the database: a write through this surface,
/// through another surface of the same database, or through another database of the cluster.
type Write<'a> = Box<dyn Fn(&mut Database) + 'a>;

/// RULE: a database held in a cluster's volume has MOVED whenever any surface of that cluster
/// has committed to it, so the version a transaction is built from differs from the current
/// one after a write through ANOTHER surface exactly as it does after one through this one —
/// which is what makes a transaction read, and install over, what every surface committed. A
/// database nothing else addresses moves only on its own writes.
///
/// The rows differ along WHO wrote: this surface, another surface of the same cluster
/// database, a surface of another database of the same cluster, and nobody.
#[test]
fn a_backed_databases_version_moves_when_any_surface_commits_to_it() {
    let _in_turn = THE_ENGINES_OWN_CONFIGURATION
        .lock()
        .unwrap_or_else(::std::sync::PoisonError::into_inner);
    let engine = ::engine::InMemoryEngine::new();
    let volume = ::service_common::cluster_sql_volumes::ClusterVolumes::of(&engine)
        .expect("the engine vends the volume naming")
        .cluster("cluster-PROBE");
    let backing = volume.backing("appdata");
    let mut database = Database::open("appdata", EngineFamily::default(), Some(&backing))
        .expect("a confined database on the cluster's backing opens");
    database
        .run_within(
            &mut RequestBudget::new(),
            "CREATE TABLE seats (id INT PRIMARY KEY, holder VARCHAR(20))",
            &[],
        )
        .expect("the table is created");
    // ANOTHER surface of the same cluster: its own connection on the same backing, opened
    // exactly as this service opens one.
    let elsewhere = ::service_common::cluster_sql_volumes::open_backing(Some(&backing))
        .expect("another surface's connection on the backing opens");
    let other_database = ::service_common::cluster_sql_volumes::open_backing(Some(
        &volume.backing("another"),
    ))
    .expect("another database of the cluster opens");

    // (case, what writes next, whether the version must have moved)
    let rows: Vec<(&str, Write<'_>, bool)> = vec![
        (
            "a statement of this surface",
            Box::new(|database: &mut Database| {
                database
                    .run_within(
                        &mut RequestBudget::new(),
                        "INSERT INTO seats (id, holder) VALUES (1, 'first')",
                        &[],
                    )
                    .expect("this surface writes");
            }),
            true,
        ),
        (
            "a commit of another surface of the same database",
            Box::new(move |_: &mut Database| {
                elsewhere
                    .execute("INSERT INTO seats (id, holder) VALUES (2, 'second')", [])
                    .expect("the other surface writes");
            }),
            true,
        ),
        (
            "a commit on another database of the same cluster",
            Box::new(move |_: &mut Database| {
                other_database
                    .execute_batch("CREATE TABLE elsewhere (id INTEGER PRIMARY KEY)")
                    .expect("the other database is written");
            }),
            false,
        ),
        (
            "a read of this surface",
            Box::new(|database: &mut Database| {
                database
                    .run_within(&mut RequestBudget::new(), "SELECT count(*) FROM seats", &[])
                    .expect("this surface reads");
            }),
            false,
        ),
    ];
    for (case, write, moves) in rows {
        let before = database.version();
        write(&mut database);
        let after = database.version();
        assert_eq!(
            after != before,
            moves,
            "case {case:?}: expected the version to have moved={moves}, got {before} then \
             {after}"
        );
    }

    // And a database nothing else addresses answers for its own writes only, which is what
    // the private copy a transaction writes into is.
    let mut private = Database::open("appdata", EngineFamily::default(), None)
        .expect("a private database opens");
    let before = private.version();
    private
        .run_within(
            &mut RequestBudget::new(),
            "CREATE TABLE seats (id INT PRIMARY KEY)",
            &[],
        )
        .expect("the private database is written");
    assert_ne!(
        private.version(),
        before,
        "case {:?}: expected its own write to move the version",
        "a private database"
    );
}
