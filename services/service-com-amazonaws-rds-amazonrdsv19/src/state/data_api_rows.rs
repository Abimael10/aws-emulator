// SPDX-License-Identifier: CC0-1.0

//! The tables a captured copy of a cluster's DATA API databases contributes to a copy of that
//! cluster's DATA — which is what an export of the cluster, or of a snapshot of it, writes
//! out.
//!
//! A DB cluster publishes more than one endpoint onto ONE database. Its writer and reader
//! endpoints serve the engine's own wire ([`super::data_plane`]); an Aurora cluster whose
//! `HttpEndpointEnabled` is on also answers the RDS Data API, whose `ExecuteStatement` "runs a
//! SQL statement against a database" of that same cluster. The service behind that endpoint
//! OWNS the databases it serves, so a copy of them is taken by that service and handed over
//! whole ([`super::capture_data_api_cluster_data`]): that is the copy a cluster snapshot
//! carries and a restore installs again.
//!
//! What an EXPORT needs is those tables beside the ones the wire endpoint wrote, in the single
//! database it reads ("the data is exported in Apache Parquet format", one file per table):
//! a client that seeds a table through the Data API and then exports the cluster, or a
//! snapshot of it, is looking for exactly those rows. So this module carries the tables of
//! that copy into the copy of the cluster's wire storage the export reads
//! ([`super::capture_cluster_data`]) — each from the declaration the owning service's engine
//! holds for it and with its rows as that engine's own typed values
//! ([`InstanceDatabase::carry_tables_from`]), so no dialect, quoting or type mapping of this
//! module's own can get a carried table wrong, and a table the copy already holds is left
//! where it is, because a carry adds the other endpoint's tables and replaces none of the
//! cluster's own.
//!
//! A READ of that copy throughout: it is handed over by its owner, read here, and left
//! exactly as it was, so the same snapshot still restores the databases it captured.

use ::service_com_amazonaws_rdsdata_rdsdataservice::state::CapturedClusterData;

use super::data_plane::sql_engine::InstanceDatabase;

/// Carry the tables of every database `data_api` holds into `copy`, and answer how many
/// tables were carried.
///
/// `default_database` is the cluster's own `DatabaseName` — "the name of the database that you
/// want to create" when the cluster was created — which is the database a connection that
/// selects none of its own is opened on. The Data API holds the tables of a statement that
/// named no database under no name, so they are carried into that database; a cluster created
/// without one has no default database to carry them into, and they stay where they are rather
/// than being given a name this emulator invented.
///
/// A database the engine would not carry is REPORTED, naming it and what the engine said: the
/// caller is reading the source of an export, and an export that could not read part of its
/// source ends as the failure it is rather than writing out less data as a success.
///
/// Blocking, CPU-bound work — the engine's own carry, per database — so a caller runs it on a
/// blocking task, exactly as it runs the capture this carries into.
pub fn carry_data_api_tables(
    data_api: &CapturedClusterData,
    default_database: Option<&str>,
    copy: &InstanceDatabase,
) -> Result<usize, String> {
    let mut carried = 0;
    let mut refused: Option<String> = None;
    data_api.read_each(|name, database| {
        if refused.is_some() {
            return;
        }
        let Some(target) = target_database(name, default_database) else {
            return;
        };
        match copy.carry_tables_from(target, database) {
            Ok(tables) => carried += tables,
            Err(error) => {
                refused = Some(format!(
                    "the data the source holds in the database {target} could not be read: {}",
                    error.message
                ));
            }
        }
    });
    match refused {
        Some(refused) => Err(refused),
        None => Ok(carried),
    }
}

/// Which database of the cluster the tables of the Data API's database `name` belong to.
///
/// A named one is that name. The Data API's unnamed database is "the connection's default
/// database", which on the cluster is the database it was created with; a cluster created with
/// no database has none, which is what `None` says.
fn target_database<'a>(name: &'a str, default_database: Option<&'a str>) -> Option<&'a str> {
    if !name.is_empty() {
        return Some(name);
    }
    default_database.filter(|default| !default.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the tables of a NAMED database of the Data API belong to the cluster's database
    /// of that name, and those of its unnamed one to the database the cluster was created
    /// with — which a cluster created with no database does not have.
    #[test]
    fn the_unnamed_data_api_database_is_the_clusters_own_default() {
        for (case, name, default_database, expected) in [
            (
                "a named database",
                "appdb",
                Some("created_with"),
                Some("appdb"),
            ),
            (
                "a named database of a cluster created with none",
                "appdb",
                None,
                Some("appdb"),
            ),
            (
                "the unnamed database of a cluster with a default",
                "",
                Some("created_with"),
                Some("created_with"),
            ),
            (
                "the unnamed database of a cluster created with none",
                "",
                None,
                None,
            ),
            (
                "the unnamed database of a cluster whose default is empty",
                "",
                Some(""),
                None,
            ),
        ] {
            let resolved = target_database(name, default_database);
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }
}
