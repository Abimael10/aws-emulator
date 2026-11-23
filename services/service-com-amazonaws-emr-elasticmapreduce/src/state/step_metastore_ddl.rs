// SPDX-License-Identifier: CC0-1.0

//! The METASTORE DDL a step's statement submits: the partitions of a table it adds or
//! removes, and what performing it against the cluster's Hive metastore answers.
//!
//! One concern, split out of [`super::step_runner`], which reads the work off a step's
//! command and resolves what that work answered. That module's own class of work is a
//! statement whose only effect is READING; this one is the class whose effect this emulator
//! REALLY APPLIES, because the state it changes is state the emulator holds: a table's
//! PARTITIONS in the AWS Glue Data Catalog, which is the Hive metastore of a cluster whose
//! configuration selects it ([`super::uses_glue_data_catalog`]).
//!
//! A partition is "a slice of table data" (`Partition`, Glue model) and a catalog record of
//! its table, so the records this module writes are the same ones `AWSGlue.CreatePartition`
//! writes and `AWSGlue.GetPartition`/`GetPartitions` read: a partition a step's
//! `ALTER TABLE … ADD PARTITION` adds is one a client then reads through the catalog's own
//! API, and one its `ALTER TABLE … DROP PARTITION` removes is gone for every later reader.
//!
//! What the metastore ANSWERS is what the step ends in: a statement the metastore applied
//! completed, and one it refused — a table it holds nothing under, a spec that is not the
//! table's partitioning, a partition that is already there, one that is not there — failed
//! with the metastore client's own error, which is the step's [`StepFailure`].
//!
//! Only a statement whose WHOLE effect is such a change is read here. Any other statement
//! that writes, defines or drops leaves state behind this emulator does not apply, so it is
//! no work of this class and its step keeps the runner's own not-executed failure rather
//! than reporting a discarded write as a stored one.

use ::std::collections::BTreeSet;
use ::std::time::SystemTime;

use ::service_com_amazonaws_glue_awsglue::ops::types::PartitionInput;
use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use sqlparser::ast::{
    AlterTableOperation, BinaryOperator, Expr, Partition, Statement, Value,
};

use super::*;

use super::step_runner::table_reference;

/// How many partitions ONE statement may name. CHOSEN: a bound on the metastore writes one
/// step's statement can cost, in the same units [`super::MAX_TABLES_PER_STEP`] bounds its
/// lookups.
const MAX_PARTITIONS_PER_STATEMENT: usize = 64;

/// How many partition KEYS one spec may assign a value to — a different unit from the
/// partitions a statement names. CHOSEN: a bound on the assignments one spec is read out of,
/// far above the keys any table is partitioned by.
const MAX_KEYS_PER_SPEC: usize = 32;

/// One partition as a statement's spec names it: the partition key and the value, in the
/// order the spec spelled them.
type PartitionSpec = Vec<(String, String)>;

/// A change to the cluster's Hive metastore a step's statement asks for.
///
/// The table is resolved the way every table a statement names is
/// ([`super::TableReference`]): in the database the name qualifies it with, else in the
/// session's current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetastoreChange {
    /// `ALTER TABLE … ADD [IF NOT EXISTS] PARTITION (…) [LOCATION '…']`: the partitions to
    /// register on the table, each with the location its data lives under when the statement
    /// named one.
    AddPartitions {
        table: TableReference,
        /// Every partition the statement names, in the order it names them.
        specs: Vec<PartitionSpec>,
        /// "LOCATION '…'" as the statement gave it, or `None` for a statement that gave
        /// none — such a partition takes the directory Hive lays it out under
        /// ([`hive_partition_location`]).
        location: Option<String>,
        /// Whether the statement tolerates a partition that is already there.
        if_not_exists: bool,
    },
    /// `ALTER TABLE … DROP [IF EXISTS] PARTITION (…)`: the partition to remove from the
    /// table.
    DropPartition {
        table: TableReference,
        spec: PartitionSpec,
        /// Whether the statement tolerates a partition that is not there.
        if_exists: bool,
    },
}

/// `FailureDetails$Reason` for a step whose statement named a partition the table already
/// holds, beside the metastore client's own message for it
/// ([`partition_already_exists_message`]).
pub const PARTITION_ALREADY_EXISTS_REASON: &str = "The metastore already holds that partition.";

/// `FailureDetails$Reason` for a step whose statement named a partition the table does not
/// hold, beside the compiler's own message for it ([`partition_not_found_message`]).
pub const PARTITION_NOT_FOUND_REASON: &str = "The metastore holds no such partition.";

/// `FailureDetails$Reason` for a step whose statement named a partition spec that is not the
/// table's partitioning at all.
pub const PARTITION_SPEC_REASON: &str = "The partition spec is not the table's partitioning.";

/// `FailureDetails$Reason` for a step whose statement the metastore took no partition from
/// at all — the catalog is at a bound of its own.
pub const PARTITION_NOT_STORED_REASON: &str = "The metastore stored no such partition.";

/// What a Hive metastore client raises when the partition a statement adds is already
/// registered: the class of the failure and the partition it was asked for.
#[must_use]
pub fn partition_already_exists_message(table: &TableReference, spec: &[(String, String)]) -> String {
    format!(
        "Partition already exists: {}.{} ({})",
        table.database,
        table.name,
        render_spec(spec)
    )
}

/// What the Hive compiler raises when the partition a statement drops is not registered.
#[must_use]
pub fn partition_not_found_message(spec: &[(String, String)]) -> String {
    format!("Partition not found ({})", render_spec(spec))
}

/// What the metastore client raises when a spec names a column the table is not partitioned
/// by — including every spec given for a table that is not partitioned at all.
#[must_use]
pub fn non_partition_columns_message(spec: &[(String, String)]) -> String {
    format!(
        "Partition spec {{{}}} contains non-partition columns",
        render_spec_braced(spec)
    )
}

/// What the metastore client raises when a spec leaves one of the table's partition
/// columns unset.
#[must_use]
pub fn incomplete_partition_spec_message(spec: &[(String, String)], missing: &str) -> String {
    let mut rendered: Vec<String> = spec
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    rendered.push(format!("{missing}=null"));
    format!("Partition spec is incorrect. {{{}}}", rendered.join(", "))
}

/// What the metastore client raises when it would not store the partition a statement adds.
#[must_use]
pub fn partition_not_stored_message(table: &TableReference, spec: &[(String, String)]) -> String {
    format!(
        "Unable to add partition ({}) to {}.{}",
        render_spec(spec),
        table.database,
        table.name
    )
}

/// A spec as a statement spells it: `dt = '2026-01-02'`, the form the Hive compiler quotes a
/// partition back in.
fn render_spec(spec: &[(String, String)]) -> String {
    spec.iter()
        .map(|(key, value)| format!("{key} = '{value}'"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// A spec as the metastore client's own messages carry it: `dt=2026-01-02`.
fn render_spec_braced(spec: &[(String, String)]) -> String {
    spec.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The metastore change `statement` asks for, or `None` when it asks for none this module
/// applies.
///
/// `database` is the session's current database, which an unqualified table name resolves
/// in exactly as it does for a statement that reads
/// ([`super::DEFAULT_DATABASE`], a `USE` moving it).
pub fn metastore_change(statement: &Statement, database: &str) -> Option<MetastoreChange> {
    let Statement::AlterTable {
        name,
        if_exists,
        operations,
        location,
        ..
    } = statement
    else {
        return None;
    };
    // `ALTER TABLE IF EXISTS` is no Hive form, and a statement carrying more than one
    // operation is no partition DDL: neither is read as one rather than guessed at.
    if *if_exists || operations.len() != 1 {
        return None;
    }
    let table = table_reference(name, database, &[])?;
    match &operations[0] {
        AlterTableOperation::AddPartitions {
            if_not_exists,
            new_partitions,
        } => {
            let location = location.as_ref().map(|set| set.location.value.clone());
            // Hive gives each `PARTITION (…)` clause its own `LOCATION`, and the parse
            // carries ONE location for the whole statement — so a location cannot be
            // attributed to one of several partitions, and such a statement is not read as
            // this class of work at all.
            if location.is_some() && new_partitions.len() != 1 {
                return None;
            }
            let specs = partition_specs(new_partitions)?;
            Some(MetastoreChange::AddPartitions {
                table,
                specs,
                location,
                if_not_exists: *if_not_exists,
            })
        }
        AlterTableOperation::DropPartitions {
            partitions,
            if_exists,
        } => Some(MetastoreChange::DropPartition {
            table,
            spec: partition_spec(partitions)?,
            if_exists: *if_exists,
        }),
        _ => None,
    }
}

/// Every partition spec an `ADD PARTITION` names, or `None` when one of them is no spec of
/// `<column> = <literal>` assignments.
fn partition_specs(partitions: &[Partition]) -> Option<Vec<PartitionSpec>> {
    if partitions.is_empty() || partitions.len() > MAX_PARTITIONS_PER_STATEMENT {
        return None;
    }
    partitions
        .iter()
        .map(|partition| match partition {
            Partition::Partitions(assignments) => partition_spec(assignments),
            _ => None,
        })
        .collect()
}

/// One partition spec: the `<column> = <literal>` assignments a `PARTITION (…)` clause
/// carries, or `None` when it carries anything else — an expression, a repeated column, or
/// nothing at all.
fn partition_spec(assignments: &[Expr]) -> Option<PartitionSpec> {
    if assignments.is_empty() || assignments.len() > MAX_KEYS_PER_SPEC {
        return None;
    }
    let mut spec: PartitionSpec = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        let Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } = assignment
        else {
            return None;
        };
        let Expr::Identifier(column) = left.as_ref() else {
            return None;
        };
        let Expr::Value(value) = right.as_ref() else {
            return None;
        };
        spec.push((column.value.clone(), partition_literal(&value.value)?));
    }
    // A column named twice is no spec of one partition: two clauses of a single `DROP
    // PARTITION` parse as one assignment list, and merging them would address a partition
    // neither clause named.
    let columns: BTreeSet<String> = spec
        .iter()
        .map(|(column, _)| glue_state::fold_name(column))
        .collect();
    (columns.len() == spec.len()).then_some(spec)
}

/// The partition VALUE a literal is, as the catalog stores one: "a list of values that
/// define the partition" (`PartitionInput$Values`), which is table data and so is stored
/// exactly as the statement spelled it.
fn partition_literal(value: &Value) -> Option<String> {
    match value {
        Value::SingleQuotedString(literal)
        | Value::DoubleQuotedString(literal)
        | Value::Number(literal, _) => Some(literal.clone()),
        Value::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Perform `change` against the Glue Data Catalog, answering the metastore client's own
/// failure when the catalog refused it.
///
/// The catalog is the ONE the emulator holds, reached by the caller
/// ([`super::run_due_steps`]); `now` is the instant the change is performed, which is the
/// `CreationTime` of a partition it registers.
pub(super) fn apply_metastore_change(
    catalog: &mut glue_state::ComAmazonawsGlueAwsglueState,
    catalog_id: &str,
    change: &MetastoreChange,
    now: SystemTime,
) -> Result<(), StepFailure> {
    match change {
        MetastoreChange::AddPartitions {
            table,
            specs,
            location,
            if_not_exists,
        } => {
            for spec in specs {
                add_partition(
                    catalog,
                    catalog_id,
                    table,
                    spec,
                    location.as_deref(),
                    *if_not_exists,
                    now,
                )?;
            }
            Ok(())
        }
        MetastoreChange::DropPartition {
            table,
            spec,
            if_exists,
        } => drop_partition(catalog, catalog_id, table, spec, *if_exists),
    }
}

/// Register ONE partition of the table `table` names, or answer why the metastore would not.
fn add_partition(
    catalog: &mut glue_state::ComAmazonawsGlueAwsglueState,
    catalog_id: &str,
    table: &TableReference,
    spec: &PartitionSpec,
    location: Option<&str>,
    if_not_exists: bool,
    now: SystemTime,
) -> Result<(), StepFailure> {
    // Whether the catalog as a whole can hold another partition, read before the table is
    // borrowed from it, exactly as the catalog's own create reads it.
    let store_has_room = catalog.can_store_partition();
    let entry = table_entry(catalog, catalog_id, table)?;
    // The spec in the table's own partition-key order, which is the order a partition is
    // addressed by and the order Hive nests its directories in.
    let ordered = partition_spec_in_key_order(entry, spec)?;
    let values: Vec<String> = ordered.iter().map(|(_, value)| value.clone()).collect();
    if entry.partition(&values).is_some() {
        // "IF NOT EXISTS" over a partition that is already there changes NOTHING and
        // succeeds; without it, the metastore raises that the partition exists and the
        // stored record is left exactly as it was.
        if if_not_exists {
            return Ok(());
        }
        return Err(StepFailure {
            reason: PARTITION_ALREADY_EXISTS_REASON.to_string(),
            message: partition_already_exists_message(table, spec),
        });
    }
    // A partition's storage is its table's with the partition's own location: a reader
    // decodes the partition's objects exactly as it decodes the table's, under the directory
    // the partition's data lives in.
    let storage = entry.storage_descriptor.clone();
    let partition_location = location.map(normalised_location).unwrap_or_else(|| {
        hive_partition_location(
            storage
                .as_ref()
                .and_then(|descriptor| descriptor.location.as_deref()),
            &ordered,
        )
    });
    let input = PartitionInput {
        values: Some(values),
        storage_descriptor: storage.map(|mut descriptor| {
            // A partition the statement gave no location for, under a table that states none
            // either, has NO location: there is no directory to nest one under, so the record
            // holds none rather than an empty path — which is what a reader then finds, and is
            // why a query over such a relation is answered by nothing at all.
            descriptor.location = (!partition_location.is_empty()).then_some(partition_location);
            descriptor
        }),
        ..PartitionInput::default()
    };
    // The write is encrypted under no key of the caller's. The catalog's own encryption
    // settings are applied by the operation that holds a request context to seal with
    // (`AWSGlue.CreatePartition` does); a statement the CLUSTER performs holds none, so the
    // record is stored with no key material — which a later read answers unchanged, because a
    // catalog object written under no key is served without a KMS call at all (the Glue
    // crate's `state::catalog_encryption::unseal`: "the object was written under no key … a
    // read that may be answered"). It is the position Athena's own DDL writes a partition
    // from.
    entry
        .create_partition(&input, now, store_has_room, None)
        .map(|_| ())
        .map_err(|_| StepFailure {
            reason: PARTITION_NOT_STORED_REASON.to_string(),
            message: partition_not_stored_message(table, spec),
        })
}

/// Remove ONE partition of the table `table` names, or answer why the metastore would not.
fn drop_partition(
    catalog: &mut glue_state::ComAmazonawsGlueAwsglueState,
    catalog_id: &str,
    table: &TableReference,
    spec: &PartitionSpec,
    if_exists: bool,
) -> Result<(), StepFailure> {
    let entry = table_entry(catalog, catalog_id, table)?;
    let values: Vec<String> = partition_spec_in_key_order(entry, spec)?
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    if entry.delete_partition(&values).is_ok() {
        return Ok(());
    }
    // "IF EXISTS" tolerates a partition that is not there; without it, the compiler raises
    // that it found no such partition.
    if if_exists {
        return Ok(());
    }
    Err(StepFailure {
        reason: PARTITION_NOT_FOUND_REASON.to_string(),
        message: partition_not_found_message(spec),
    })
}

/// The stored table `table` names, or the driver's own table-not-found failure — the same one
/// a statement that READS a table the metastore holds nothing under fails with
/// ([`super::table_not_found_message`]).
fn table_entry<'c>(
    catalog: &'c mut glue_state::ComAmazonawsGlueAwsglueState,
    catalog_id: &str,
    table: &TableReference,
) -> Result<&'c mut glue_state::TableEntry, StepFailure> {
    let not_found = || StepFailure {
        reason: TABLE_NOT_FOUND_REASON.to_string(),
        message: table_not_found_message(table),
    };
    catalog
        .database_mut(catalog_id, &table.database)
        .ok_or_else(not_found)?
        .tables
        .get_mut(&glue_state::fold_name(&table.name))
        .ok_or_else(not_found)
}

/// The spec a statement gave, in the PARTITION-KEY order of the table it belongs to: "a
/// list of values that define the partition", "ordered in the same order as the partition
/// keys" (`PartitionInput$Values`) — which is the order every partition of that table is
/// addressed by and nested under, whatever order the statement spelled the spec in. Each
/// pair carries the key as the TABLE declares it, so the directory a partition is laid out
/// under is the table's own spelling.
///
/// A spec that names a column the table is not partitioned by, and one that leaves one of
/// those columns unset, addresses no partition of the table at all: both are the metastore
/// client's own spec failures.
fn partition_spec_in_key_order(
    entry: &glue_state::TableEntry,
    spec: &PartitionSpec,
) -> Result<PartitionSpec, StepFailure> {
    let keys: Vec<String> = entry
        .partition_keys
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|column| column.name.clone())
        .collect();
    // A table that is not partitioned has no column any spec could name, which is the same
    // spec failure as a spec naming a column of a partitioned table's data.
    let names_a_data_column = spec.iter().any(|(column, _)| {
        !keys
            .iter()
            .any(|key| glue_state::fold_name(key) == glue_state::fold_name(column))
    });
    if names_a_data_column {
        return Err(StepFailure {
            reason: PARTITION_SPEC_REASON.to_string(),
            message: non_partition_columns_message(spec),
        });
    }
    let mut ordered: PartitionSpec = Vec::with_capacity(keys.len());
    for key in &keys {
        let Some((_, value)) = spec
            .iter()
            .find(|(column, _)| glue_state::fold_name(column) == glue_state::fold_name(key))
        else {
            return Err(StepFailure {
                reason: PARTITION_SPEC_REASON.to_string(),
                message: incomplete_partition_spec_message(spec, key),
            });
        };
        ordered.push((key.clone(), value.clone()));
    }
    Ok(ordered)
}

/// The location a Hive metastore client RECORDS for a location a statement gave: the
/// filesystem path it denotes, which carries no trailing separator — the DDL engine resolves
/// a declared location into a path of the cluster's filesystem and stores that path, so a
/// location spelled with a trailing separator and one spelled without it name one directory
/// and are recorded identically. The root of a filesystem is the one path that IS its
/// separator, so a location that is nothing but its scheme and authority keeps what it has.
#[must_use]
pub fn normalised_location(location: &str) -> String {
    let trimmed = location.trim_end_matches('/');
    // `s3://bucket/` trims to `s3://bucket`, which still denotes the authority's root; a
    // location that trims to nothing but a scheme is left as it was given.
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return location.to_string();
    }
    trimmed.to_string()
}

/// The directory a partition the statement gave no `LOCATION` for lives under: Hive lays a
/// partition out beneath its table's own location, one `<column>=<value>` directory per
/// partition key, in key order — which is the location its data is read from.
///
/// A table whose storage names no location of its own gives a partition none either: there
/// is no directory to nest one under, so the partition is registered with none rather than
/// with a path rooted nowhere.
#[must_use]
pub fn hive_partition_location(table_location: Option<&str>, spec: &[(String, String)]) -> String {
    let root = table_location.unwrap_or_default().trim_end_matches('/');
    if root.is_empty() {
        return String::new();
    }
    let directories: Vec<String> = spec
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("{root}/{}", directories.join("/"))
}
