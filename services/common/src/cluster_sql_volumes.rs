// SPDX-License-Identifier: CC0-1.0

//! The ONE backing database each database of a DB cluster is held in, for every SURFACE of
//! that cluster.
//!
//! A DB cluster is ONE database behind SEVERAL doors. Its engine endpoints serve the
//! engine's own wire, and an Aurora cluster whose HTTP endpoint is turned on also answers
//! the RDS Data API, whose `ExecuteStatement` "runs a SQL statement against a database" of
//! that same cluster. A client therefore reads back through one door what it wrote through
//! another: a table created through the Data API is a table of the cluster, so a session on
//! the cluster's writer endpoint selects it, and a table a native session created is one a
//! Data API statement selects.
//!
//! Each door is served by a different service crate, each with its own front end, its own
//! dialect and its own diagnostics — and each with its own in-process SQL engine. What this
//! module gives them is the one thing they must NOT each have: the STORAGE. A database of a
//! cluster is named here once, as a backing the engine shares between connections, so every
//! surface of that cluster opens a connection onto the SAME database while keeping its own
//! connection, its own confinement and its own bounds.
//!
//! * [`ClusterVolumes::of`] vends the naming of ONE shared engine, so two emulators in one
//!   process never address the same backing and two surfaces of one emulator always do.
//! * [`ClusterVolume::backing`] names the backing of one database of one cluster, keyed by
//!   the cluster's own immutable resource id and the database's name.
//! * [`open_backing`] is the one place such a backing is opened, and the one place a
//!   PRIVATE database (a copy, a snapshot, a working set) is opened instead — so the choice
//!   between the two is a parameter and never a second way of opening a connection. It is
//!   also where the surfaces are SERIALISED: two of them write one backing, so a statement
//!   that arrives while another surface is writing waits for the pages rather than being
//!   refused them ([`BACKING_BUSY_TIMEOUT`]), exactly as a second connection to a database
//!   server does.
//! * [`ClusterVolume::locks`] is the one table of ROW LOCKS over that storage
//!   ([`crate::sql_row_locks`]): the rows a transaction holds are rows every surface of the
//!   cluster waits for, because they are rows of one database.
//!
//! Every database here lives in MEMORY and nothing is written to the host: a shared backing
//! is an in-memory database the engine keeps for as long as a connection to it is open, so
//! the data of a cluster lives exactly as long as a surface of that cluster holds it. The
//! caller keeps its connection in the record of the resource it belongs to, which is what
//! makes a delete of the resource a delete of its data.

use ::std::sync::{Arc, Weak};

use ::rusqlite::{Connection, OpenFlags};

/// The naming of the shared backings of ONE emulator.
///
/// Engine-scoped, like every other store two services on either side of a hand-off share:
/// the record lives in the SAME shared engine as the services' own state, so a cluster one
/// service opened a backing for is the backing another service resolves, and two engines in
/// one process share nothing — which is what keeps one test's cluster out of another's.
///
/// RETENTION: a record here lives exactly as long as a SURFACE holds the volume it belongs
/// to, and not a moment longer. The lock table is held through a [`Weak`] reference, so it
/// goes when the last [`ClusterVolume`] of that cluster does — the engine endpoint's record
/// when the cluster is deleted, this data plane's cluster when its own store sweeps it — and
/// with it go the rows its transactions were holding. A record whose table has gone is
/// dropped the next time a volume is resolved ([`ClusterVolumes::cluster`]), so the store
/// holds one entry per LIVE cluster volume rather than one per cluster this process has ever
/// seen.
#[derive(Debug, Default)]
pub struct SharedSqlVolumes {
    /// The token every backing of this engine is named under, drawn once from
    /// [`next_namespace`] on the first read. Zero means nothing has drawn one yet, which is
    /// why the counter below never hands that value out.
    namespace: u64,
    /// The ONE lock table of each cluster's storage, by the cluster's own resource id: the
    /// rows the transactions of every surface of that cluster hold
    /// ([`crate::sql_row_locks`]), held only for as long as a surface holds the volume.
    locks: ::std::collections::HashMap<String, Weak<crate::sql_row_locks::LockTable>>,
}

/// The namespace tokens handed out in this process, one per shared engine that asks for one.
///
/// A process-wide counter rather than a value derived from the engine: two engines must
/// never collide, and nothing about an engine's address is stable enough to name a backing
/// by.
static NAMESPACES: ::std::sync::atomic::AtomicU64 = ::std::sync::atomic::AtomicU64::new(1);

/// The next namespace token, which is never zero.
fn next_namespace() -> u64 {
    NAMESPACES.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
}

/// A handle on one engine's backing naming.
///
/// Clone-cheap (an `Arc` inside), so a service can keep one beside the resource record whose
/// storage it opened.
#[derive(Clone)]
pub struct ClusterVolumes(::engine::ServiceState<SharedSqlVolumes>);

/// Written by hand rather than derived: the state handle inside carries no `Debug`, and what
/// is diagnosable about this handle is the namespace it names backings under.
impl std::fmt::Debug for ClusterVolumes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterVolumes")
            .field("namespace", &self.namespace())
            .finish()
    }
}

impl ClusterVolumes {
    /// The backing naming of the SAME shared engine the services' own stores live in, or
    /// `None` for an engine that vends none — and then a surface opens its own private
    /// database, which is where every surface was before one was shared.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<SharedSqlVolumes>()))
    }

    /// This engine's namespace token, drawn on the first read and kept from then on.
    #[must_use]
    pub fn namespace(&self) -> u64 {
        let mut held = self.0.lock();
        if held.namespace == 0 {
            held.namespace = next_namespace();
        }
        held.namespace
    }

    /// The volume of the DB cluster whose IMMUTABLE resource id is `resource_id`
    /// (`DBCluster$DbClusterResourceId`), which is "Region-unique [and] immutable" — so a
    /// rename of the cluster does not move its data, and two clusters never share a volume.
    #[must_use]
    pub fn cluster(&self, resource_id: &str) -> ClusterVolume {
        let namespace = self.namespace();
        let mut held = self.0.lock();
        // A record of a volume no surface holds any more is dropped here, which is what keeps
        // this store at one entry per LIVE cluster volume: the table itself is already gone
        // (and the rows it held with it), so the record is only its name.
        held.locks.retain(|_, table| table.strong_count() > 0);
        let locks = match held.locks.get(resource_id).and_then(Weak::upgrade) {
            Some(locks) => locks,
            None => {
                let locks = Arc::new(crate::sql_row_locks::LockTable::default());
                held.locks
                    .insert(resource_id.to_string(), Arc::downgrade(&locks));
                locks
            }
        };
        ClusterVolume {
            namespace,
            cluster: resource_id.to_string(),
            locks,
        }
    }

    /// How many cluster volumes this engine holds a lock table for, for the cases that pin
    /// that a volume no surface holds leaves no record behind.
    #[must_use]
    pub fn volumes_held(&self) -> usize {
        let mut held = self.0.lock();
        held.locks.retain(|_, table| table.strong_count() > 0);
        held.locks.len()
    }
}

/// The volume of ONE DB cluster: the naming of each of its databases' backings, and the one
/// lock table over them.
#[derive(Clone)]
pub struct ClusterVolume {
    /// The engine this volume belongs to ([`ClusterVolumes::namespace`]).
    namespace: u64,
    /// The cluster's own immutable resource id.
    cluster: String,
    /// The rows the transactions of every surface of this cluster hold.
    locks: Arc<crate::sql_row_locks::LockTable>,
}

/// Written by hand rather than derived: what is diagnosable about a volume is which cluster's
/// it is and how many rows its transactions hold, never a caller's data.
impl std::fmt::Debug for ClusterVolume {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterVolume")
            .field("namespace", &self.namespace)
            .field("cluster", &self.cluster)
            .field("held_rows", &self.locks.held_rows())
            .finish()
    }
}

impl ClusterVolume {
    /// The ONE table of row locks over this cluster's storage.
    ///
    /// Its holders are the transactions of EVERY surface of the cluster: a row a transaction
    /// opened through one door holds is a row a transaction on another door waits for, because
    /// the two address one database ([`crate::sql_row_locks`]).
    #[must_use]
    pub fn locks(&self) -> &Arc<crate::sql_row_locks::LockTable> {
        &self.locks
    }

    /// The backing of the database `database` of this cluster: the name every surface of the
    /// cluster opens that database's connection on.
    ///
    /// NO PART OF IT IS CALLER TEXT. `database` is a name a caller chose — a request's
    /// `database` member, or the name a `CREATE DATABASE` statement gave — so it is
    /// HEX-ENCODED into the name rather than interpolated: the result can carry no path
    /// separator, no `.`/`..` component, no drive or UNC prefix, no NUL and none of the `?`,
    /// `&` or `#` a URI reads, whatever the caller wrote. The cluster's own id is encoded the
    /// same way, and what is left outside the encoding is this module's constants.
    ///
    /// What the name addresses is therefore always ONE in-memory database of the engine,
    /// shared between the connections opened on that name and reachable through nothing else:
    /// the VFS the name carries holds the whole database in memory, so no host path is opened
    /// and no file is created, and the two encoded components are what keep one cluster's
    /// database out of another's. The database name is folded to lower case first, because the
    /// engines behind these endpoints resolve a database name without regard to case.
    #[must_use]
    pub fn backing(&self, database: &str) -> String {
        format!(
            "{BACKING_PREFIX}{}-{}-{}{BACKING_SUFFIX}",
            self.namespace,
            ::hex::encode(self.cluster.as_bytes()),
            ::hex::encode(database.to_ascii_lowercase().as_bytes()),
        )
    }
}

/// What every shared backing's name begins with, so a name this module built is recognisable
/// in a diagnostic, can collide with nothing else, and is told from any other name at the one
/// place a connection is opened ([`names_a_cluster_volume`]).
const BACKING_PREFIX: &str = "file:/aws-emulator-cluster-volume-";

/// What every shared backing's name ends with: the engine's own IN-MEMORY file system, which
/// holds the database named in front of it in memory and shares it with every connection
/// opened on that name, and nothing else.
///
/// It is this reading — rather than one page cache shared between the connections — that
/// makes a statement of one surface WAIT for storage another surface is writing instead of
/// being refused it: a shared page cache locks per table and answers a second connection
/// "locked" at once, without ever asking the waiter whether it would wait
/// ([`BACKING_BUSY_TIMEOUT`]), while a database each connection has its own cache of is
/// locked the way any other database file is, which the engine's own wait covers.
const BACKING_SUFFIX: &str = "?vfs=memdb";

/// How long a statement of one surface WAITS for storage another surface is writing before the
/// engine reports it could not have it.
///
/// A DB cluster is one database behind several doors, so two surfaces of it write one backing,
/// and the engine serialises them: the one that arrives second waits for the pages rather than
/// being refused them, exactly as a second connection to a database server does. The wait is
/// bounded so a waiter cannot sit for ever, and the bound is as long as the longest ceiling a
/// surface of this emulator runs ONE statement under — so a waiter outlasts a full-length
/// statement of the other door, and a statement of its own that is cut off is cut off by its
/// own deadline rather than by this.
const BACKING_BUSY_TIMEOUT: ::std::time::Duration = ::std::time::Duration::from_secs(5);

/// Whether `backing` is a name [`ClusterVolume::backing`] composed: this module's own prefix,
/// then the namespace, the cluster and the database as hexadecimal and nothing else, then the
/// in-memory reading above.
///
/// The invariant is CHECKED here rather than trusted across the crate boundary. A data plane
/// composes a backing from a database name its CALLER chose, so this is what makes that name
/// unable to decide anything about what is opened: a name carrying a host path, a `..`
/// segment, a second URI parameter, a NUL or any other character is not of this form, and
/// [`open_backing`] opens nothing for it.
fn names_a_cluster_volume(backing: &str) -> bool {
    let Some(rest) = backing.strip_prefix(BACKING_PREFIX) else {
        return false;
    };
    let Some(name) = rest.strip_suffix(BACKING_SUFFIX) else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

/// Open a connection on `backing`, or on a PRIVATE in-memory database when there is none.
///
/// The one place a data plane of this emulator opens the engine, whichever of the two it
/// asks for:
///
/// * `Some(backing)` — a database of a cluster, which every surface of that cluster opens a
///   connection on ([`ClusterVolume::backing`]): the engine keeps ONE database for that name
///   and hands each connection its own view of it, so what one surface commits is what the
///   next statement of another surface reads, and a statement that arrives while another
///   surface is writing WAITS for the pages ([`BACKING_BUSY_TIMEOUT`]) rather than being
///   refused them;
/// * `None` — a database nothing else addresses: a copy taken for a snapshot, the working
///   set of one transaction, the scratch of one request.
///
/// NOTHING ELSE IS EVER OPENED. A backing that is not a name [`ClusterVolume::backing`]
/// composed ([`names_a_cluster_volume`]) is refused here rather than handed to the engine, so
/// the only names a connection of this emulator is ever opened on are this module's own
/// opaque in-memory ones and the engine's anonymous in-memory database — whatever a caller
/// wrote in the database name a data plane composed its backing from.
///
/// The connection is opened with the flags a bounded in-process engine needs and NOTHING
/// else: read/write, create, and the URI reading that names the in-memory file system a
/// backing is held in. No path on the host is ever opened — a backing names that file system
/// ([`BACKING_SUFFIX`]), and a private database is the engine's anonymous in-memory one — and
/// the caller installs its own confinement, limits and functions on the connection it gets
/// back, exactly as it does on a private one.
pub fn open_backing(backing: Option<&str>) -> ::rusqlite::Result<Connection> {
    match backing {
        // `SQLITE_OPEN_FULL_MUTEX`: one connection on a backing is used from more than one
        // thread (a surface runs each statement on the pool), so the engine serialises the
        // uses of it rather than leaving that to the callers' own locks. The WAIT below is
        // what serialises the surfaces' separate connections against each other.
        Some(backing) if names_a_cluster_volume(backing) => {
            let conn = Connection::open_with_flags(
                backing,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
            )?;
            conn.busy_timeout(BACKING_BUSY_TIMEOUT)?;
            Ok(conn)
        }
        Some(refused) => Err(::rusqlite::Error::InvalidPath(
            ::std::path::PathBuf::from(refused),
        )),
        None => Connection::open_in_memory(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: two connections opened on ONE cluster database's backing address the same
    /// database — what one writes is what the other reads back — while a connection opened
    /// on no backing, or on another cluster's or another database's, addresses its own.
    ///
    /// The rows differ along what the naming separates: the database of a volume, the
    /// cluster, the engine's namespace and a private database.
    #[test]
    fn the_surfaces_of_one_cluster_database_address_one_backing() {
        let volumes = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let elsewhere = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let cluster = volumes.cluster("cluster-ABCDEF");
        let written = cluster.backing("appdata");

        let one = open_backing(Some(&written)).expect("the backing opens");
        one.execute_batch("CREATE TABLE seeded (id INTEGER PRIMARY KEY); INSERT INTO seeded VALUES (1)")
            .expect("the first surface writes");

        // (case, the backing the second connection opens, whether it must see the table)
        let rows: Vec<(&str, Option<String>, bool)> = vec![
            (
                "the same database of the same cluster",
                Some(cluster.backing("appdata")),
                true,
            ),
            (
                "the same database named in another case",
                Some(cluster.backing("APPDATA")),
                true,
            ),
            (
                "another database of the same cluster",
                Some(cluster.backing("other")),
                false,
            ),
            (
                "the same database name of another cluster",
                Some(volumes.cluster("cluster-FEDCBA").backing("appdata")),
                false,
            ),
            (
                "the same cluster and database on another engine",
                Some(elsewhere.cluster("cluster-ABCDEF").backing("appdata")),
                false,
            ),
            ("a private database", None, false),
        ];
        for (case, backing, shared) in rows {
            let other = open_backing(backing.as_deref()).expect("the database opens");
            let seen = other
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'seeded'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("the catalog is readable");
            assert_eq!(
                seen == 1,
                shared,
                "case {case:?}: expected the table to be visible={shared}, got {seen} \
                 matching catalog rows"
            );
        }

        // And the sharing is LIVE in both directions: a row the second connection on the
        // same backing writes is a row the first one reads.
        let two = open_backing(Some(&cluster.backing("appdata"))).expect("the backing opens");
        two.execute("INSERT INTO seeded VALUES (2)", [])
            .expect("the second surface writes");
        let rows: i64 = one
            .query_row("SELECT count(*) FROM seeded", [], |row| row.get(0))
            .expect("the first surface reads");
        assert_eq!(
            rows, 2,
            "case {:?}: expected both rows through either connection, got {rows}",
            "a row written through the other surface"
        );
    }

    /// RULE: two surfaces of one cluster database may use it AT THE SAME TIME — a statement
    /// that arrives while another surface is writing waits for the storage and is served,
    /// rather than being refused it, and every write of both surfaces is in the database
    /// afterwards.
    ///
    /// The rows differ along what the SECOND surface is doing while the first writes: reading
    /// the very table being written, writing that table, writing another table of the same
    /// database, and running a transaction of several statements.
    #[test]
    fn a_statement_waits_for_storage_another_surface_is_writing() {
        /// How many statements each surface runs in one case: enough rounds that the two
        /// surfaces' statements overlap, few enough that the case costs milliseconds.
        const ROUNDS: usize = 200;
        // (case, what the second surface runs each round)
        let rows: &[(&str, &str)] = &[
            (
                "a read of the table the other surface is writing",
                "SELECT count(*) FROM seats",
            ),
            (
                "a write of that same table",
                "INSERT INTO seats (holder) VALUES ('the second surface')",
            ),
            (
                "a write of another table of the same database",
                "INSERT INTO seats_elsewhere (holder) VALUES ('the second surface')",
            ),
            (
                "a transaction of several statements",
                "BEGIN; \
                 INSERT INTO seats_elsewhere (holder) VALUES ('in a transaction'); \
                 UPDATE seats_elsewhere SET holder = 'committed' \
                   WHERE id = (SELECT max(id) FROM seats_elsewhere); \
                 COMMIT;",
            ),
        ];
        for (case, second) in rows {
            let volumes =
                ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
            let backing = volumes.cluster("cluster-ABCDEF").backing("appdata");
            // Held for the whole case: a backing lives exactly as long as a connection to it
            // is open, so this is the surface that keeps the cluster's database there.
            let held = open_backing(Some(&backing)).expect("the backing opens");
            held.execute_batch(
                "CREATE TABLE seats (id INTEGER PRIMARY KEY, holder TEXT NOT NULL);
                 CREATE TABLE seats_elsewhere (id INTEGER PRIMARY KEY, holder TEXT NOT NULL)",
            )
            .expect("the first surface writes");

            // Each surface runs on a thread of its own, with a connection of its own on the
            // one backing — which is how the two doors of a cluster reach it.
            let surfaces: Vec<std::thread::JoinHandle<Vec<String>>> = [
                "INSERT INTO seats (holder) VALUES ('the first surface')",
                second,
            ]
            .into_iter()
            .map(|statement| {
                let backing = backing.clone();
                let statement = statement.to_string();
                std::thread::spawn(move || {
                    let conn = open_backing(Some(&backing)).expect("the backing opens");
                    let mut refused = Vec::new();
                    for round in 0..ROUNDS {
                        if let Err(error) = conn.execute_batch(&statement) {
                            refused.push(format!("round {round}: {error}"));
                        }
                    }
                    refused
                })
            })
            .collect();
            let refused: Vec<String> = surfaces
                .into_iter()
                .flat_map(|surface| surface.join().expect("the surface finishes"))
                .collect();
            assert!(
                refused.is_empty(),
                "case {case:?}: every statement of two surfaces at once must be served, got \
                 {} refusal(s), the first {:?}",
                refused.len(),
                refused.first()
            );

            // And the writes are IN the database: the first surface's rows are all there,
            // whatever the second surface was doing beside them.
            let seats: i64 = held
                .query_row(
                    "SELECT count(*) FROM seats WHERE holder = 'the first surface'",
                    [],
                    |row| row.get(0),
                )
                .expect("the database is readable");
            assert_eq!(
                seats,
                ROUNDS as i64,
                "case {case:?}: expected the {ROUNDS} rows the first surface wrote, got {seats}"
            );
        }
    }

    /// RULE: one engine names its backings under ONE namespace, drawn once, and two engines
    /// never draw the same one.
    #[test]
    fn each_engine_names_its_backings_under_its_own_namespace() {
        let engine = ::engine::InMemoryEngine::new();
        let volumes = ClusterVolumes(engine.state::<SharedSqlVolumes>());
        let again = ClusterVolumes(engine.state::<SharedSqlVolumes>());
        assert_eq!(
            volumes.namespace(),
            again.namespace(),
            "case {:?}: one engine names its backings under one namespace",
            "two handles on one engine"
        );
        let other = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        assert_ne!(
            volumes.namespace(),
            other.namespace(),
            "case {:?}: two engines must not name one backing",
            "a handle on another engine"
        );
    }

    /// RULE: the row locks over one cluster's storage are ONE table, which every surface of
    /// that cluster reaches — and another cluster's are another table.
    #[test]
    fn one_clusters_storage_has_one_lock_table() {
        let volumes = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let one_surface = volumes.cluster("cluster-ABCDEF");
        let another_surface = volumes.cluster("cluster-ABCDEF");
        let another_cluster = volumes.cluster("cluster-FEDCBA");
        let row = ("appdata".to_string(), "seats".to_string(), 1i64);
        let held = one_surface.locks().acquire_without_waiting(
            7,
            ::std::slice::from_ref(&row),
            crate::sql_row_locks::LockMode::Exclusive,
        );
        assert!(
            matches!(held, crate::sql_row_locks::LockOutcome::Granted(_)),
            "case {:?}: the first surface takes the row, got {held:?}",
            "one surface of the cluster"
        );
        // (case, the volume the second holder asks through, whether the row is held there)
        for (case, volume, taken) in [
            (
                "another surface of the same cluster",
                &another_surface,
                true,
            ),
            ("another cluster", &another_cluster, false),
        ] {
            let answered = volume.locks().acquire_without_waiting(
                9,
                ::std::slice::from_ref(&row),
                crate::sql_row_locks::LockMode::Exclusive,
            );
            assert_eq!(
                answered == crate::sql_row_locks::LockOutcome::Unavailable,
                taken,
                "case {case:?}: expected the row to be held={taken}, got {answered:?}"
            );
        }
    }

    /// RULE: a cluster's volume is held for exactly as long as a SURFACE holds it: the store
    /// keeps one record per live volume, and a volume every surface has let go of leaves none
    /// behind — nor the rows its transactions were holding.
    #[test]
    fn a_volume_no_surface_holds_leaves_no_record_behind() {
        let volumes = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let row = ("appdata".to_string(), "seats".to_string(), 1i64);
        // (case, how many clusters have been named by now, how many records are held)
        let mut named = Vec::new();
        for (case, clusters, held) in [
            ("one cluster", 1usize, 1usize),
            ("a second cluster", 2, 2),
            ("the first cluster again", 2, 2),
        ] {
            named.push(volumes.cluster(&format!("cluster-{clusters}")));
            let records = volumes.volumes_held();
            assert_eq!(
                records, held,
                "case {case:?}: expected {held} record(s) for {clusters} cluster(s), got \
                 {records}"
            );
        }
        // Every surface of every one of them lets go — a delete, a sweep — and the store is
        // back where it started, with no table left holding a row.
        let still_held = named
            .first()
            .expect("a volume was named")
            .locks()
            .acquire_without_waiting(
                7,
                ::std::slice::from_ref(&row),
                crate::sql_row_locks::LockMode::Exclusive,
            );
        assert!(
            matches!(still_held, crate::sql_row_locks::LockOutcome::Granted(_)),
            "case {:?}: a live volume's table takes a row, got {still_held:?}",
            "a row held when the volumes go"
        );
        drop(named);
        let records = volumes.volumes_held();
        assert_eq!(
            records, 0,
            "case {:?}: expected no record of a volume no surface holds, got {records}",
            "every surface let go"
        );
    }

    /// RULE: no part of a backing's name is caller text — a database name is encoded into it,
    /// so a name carrying a path, a URI parameter or a NUL addresses its own in-memory
    /// database of this cluster and reaches nothing else.
    ///
    /// The rows are the forms a caller could use to try to reach something else: a relative
    /// and an absolute path, a parent-directory component, a URI parameter, a NUL and a name
    /// that is only a dot.
    #[test]
    fn a_callers_database_name_cannot_reach_the_structure_of_a_backing() {
        let volumes = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let cluster = volumes.cluster("cluster-ABCDEF");
        let ordinary = cluster.backing("appdata");
        for case in [
            "../../etc/passwd",
            "/etc/passwd",
            "..",
            ".",
            "appdata?vfs=unix&mode=rwc",
            "appdata#fragment",
            "app\0data",
            "C:\\windows\\system32",
            "\\\\host\\share",
        ] {
            let backing = cluster.backing(case);
            assert!(
                names_a_cluster_volume(&backing),
                "case {case:?}: expected a name of this module's own form, got {backing}"
            );
            let name = backing
                .strip_prefix(BACKING_PREFIX)
                .and_then(|rest| rest.strip_suffix(BACKING_SUFFIX))
                .unwrap_or_else(|| panic!("case {case:?}: expected a named backing, got {backing}"));
            assert!(
                !name.contains(['/', '\\', '?', '&', '#', '.', ':', '\0']),
                "case {case:?}: expected a name carrying none of the caller's own, got {name:?}"
            );
            assert_ne!(
                backing, ordinary,
                "case {case:?}: expected a database of its own, got the one named {:?}",
                "appdata"
            );
            // And it opens: an in-memory database of this cluster, holding nothing of
            // anybody else's.
            let opened = open_backing(Some(&backing)).expect("the backing opens");
            let objects: i64 = opened
                .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
                .expect("the catalog is readable");
            assert_eq!(
                objects, 0,
                "case {case:?}: expected a database of its own, holding nothing, got {objects} \
                 objects"
            );
        }
    }

    /// RULE: the only names a connection is ever opened on are the ones
    /// [`ClusterVolume::backing`] composed — this module's own opaque in-memory names — and
    /// the engine's anonymous in-memory database. Any other name is refused rather than
    /// handed to the engine, so nothing a caller's text could carry decides what is opened.
    ///
    /// The rows are the forms a name that is NOT one of this module's own can take: a host
    /// path, a `file:` URI naming one, a backing whose reading has been changed to a stored
    /// file, one carrying a second URI parameter, one carrying a path inside its own name,
    /// and the empty name the engine reads as a temporary file.
    #[test]
    fn nothing_but_a_composed_backing_is_ever_opened() {
        let volumes = ClusterVolumes(::engine::InMemoryEngine::new().state::<SharedSqlVolumes>());
        let composed = volumes.cluster("cluster-ABCDEF").backing("appdata");
        assert!(
            open_backing(Some(&composed)).is_ok(),
            "case {:?}: a composed backing opens, got a refusal",
            "the name this module composed"
        );
        assert!(
            open_backing(None).is_ok(),
            "case {:?}: the engine's own anonymous database opens, got a refusal",
            "no backing at all"
        );
        for case in [
            "/etc/passwd",
            "file:/etc/passwd",
            "file:aws-emulator-cluster-volume-1-61-62?mode=rwc",
            "file:aws-emulator-cluster-volume-1-61-62?mode=memory&cache=shared&vfs=unix",
            "file:aws-emulator-cluster-volume-1-../../etc/passwd?mode=memory&cache=shared",
            "file:aws-emulator-cluster-volume-?mode=memory&cache=shared",
            // The forms a backing of the composed shape takes once the IN-MEMORY file system
            // it names is dropped or replaced: without that reading the same name is a path on
            // the HOST, which is the one thing a connection here is never opened on.
            "file:/aws-emulator-cluster-volume-1-61-62",
            "file:/aws-emulator-cluster-volume-1-61-62?vfs=unix",
            "file:/aws-emulator-cluster-volume-1-61-62?vfs=memdb&mode=rwc",
            "",
            "appdata",
        ] {
            assert!(
                !names_a_cluster_volume(case),
                "case {case:?}: expected a name this module did not compose"
            );
            let refused = open_backing(Some(case));
            assert!(
                refused.is_err(),
                "case {case:?}: expected the engine never to be asked for it, got a connection"
            );
        }
    }
}
