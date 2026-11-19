// SPDX-License-Identifier: CC0-1.0

//! Resolving the Data Catalog target of an APACHE ICEBERG destination, before that
//! destination is written onto a stream.
//!
//! An Iceberg destination delivers to a TABLE of a Data Catalog rather than to an Amazon S3
//! prefix: "Provides a list of `DestinationTableConfigurations` which Firehose uses to deliver
//! data to Apache Iceberg Tables"
//! (`IcebergDestinationConfiguration$DestinationTableConfigurationList`), each entry naming its
//! `DestinationDatabaseName` and `DestinationTableName`. Firehose reads that table with the
//! configuration's own `RoleARN` — "the IAM role to be assumed by Firehose for calling Apache
//! Iceberg Tables" — at the moment the configuration is submitted, exactly as it resolves the
//! other resources a destination names ([`crate::state::unassumable_role`] for the roles,
//! [`crate::state::absent_destination_bucket`] for the buckets): a table the catalog cannot
//! serve is an ARGUMENT fault of the call that named it, so nothing is written and the client
//! is told at that call rather than handed a stream that reports healthy and can never commit.
//!
//! The one Data Catalog in this emulator is the Glue service's own store, which is where the
//! delivery path reads a destination table's columns and metadata pointer from
//! ([`crate::state::delivery::iceberg`]), so the resolution is grounded in the same store the
//! commit later reads: a table reported as resolvable here is a table a commit can find.
//!
//! A destination that asks Firehose to CREATE its tables itself
//! (`TableCreationConfiguration`, "the configuration to enable automatic table creation") does
//! not need them to exist yet, so a table absent under that configuration is not a fault.
//!
//! ## Lock order
//!
//! The Data Catalog is read with ITS own lock and nothing else is held while it is: every
//! caller resolves the target BEFORE it takes this service's own store.

use std::collections::BTreeSet;

use crate::ops::types::{DestinationTableConfiguration, IcebergDestinationConfiguration};
use crate::state::delivery::iceberg::{catalog_account, MAX_DESTINATION_TABLES};
use crate::state::DestinationConfigKind;

/// Why the Data Catalog cannot serve the destination tables the Iceberg configuration `config`
/// names, worded as the refusal states it; `None` when every table it lists is one Firehose can
/// read, and for a destination of any other kind.
///
/// The configuration is read as the delivery path reads it — the catalog the
/// `CatalogConfiguration` names and the tables the list names — so one reading answers for the
/// create that writes a destination and for the update that replaces one.
pub fn unresolvable_destination_table<Eng: Send + Sync + 'static>(
    engine: &Eng,
    config: &DestinationConfigKind,
) -> Option<String> {
    let DestinationConfigKind::Iceberg(config) = config else {
        return None;
    };
    unresolvable_tables(
        engine,
        config.destination_table_configuration_list.as_deref(),
        config.catalog_configuration.catalog_arn.as_deref(),
        &config.role_arn,
        creates_its_tables(config),
    )
}

/// Whether the destination creates its destination tables itself.
fn creates_its_tables(config: &IcebergDestinationConfiguration) -> bool {
    config
        .table_creation_configuration
        .as_ref()
        .is_some_and(|configured| configured.enabled)
}

/// The same resolution over the parts a caller holds separately: the tables the call names, the
/// Data Catalog they live in, the role the destination reads them with, and whether that
/// destination creates missing tables itself.
///
/// Answers `None` for a call that lists no table — there is nothing to resolve, and "Firehose
/// will write data with insert if table specific configuration is not provided here" — and for
/// one whose tables Firehose creates itself.
///
/// WHAT THIS WALKS is bounded twice over, because the list is the CALLER's: a configuration
/// naming more targets than one delivery of this emulator walks
/// ([`MAX_DESTINATION_TABLES`]) is resolved not at all rather than walked, and the targets
/// inside that bound are reduced to the DISTINCT ones — so the names are read, folded and
/// collected once each, before the Data Catalog's lock is taken. The critical section is then
/// one bounded series of index lookups holding no allocation, so a configuration cannot decide
/// how long another caller of that catalog waits.
pub fn unresolvable_tables<Eng: Send + Sync + 'static>(
    engine: &Eng,
    tables: Option<&[DestinationTableConfiguration]>,
    catalog_arn: Option<&str>,
    role_arn: &str,
    creates_its_tables: bool,
) -> Option<String> {
    let tables = tables.unwrap_or_default();
    if tables.is_empty() || creates_its_tables {
        return None;
    }
    if tables.len() > MAX_DESTINATION_TABLES {
        // A configuration listing more destination tables than one commit walks is one this
        // emulator delivers nothing through anyway, so nothing of it is resolved and nothing is
        // refused from it: this gate answers for the configurations a stream can really deliver
        // through, and never decides a request's cost by the length of a list in it.
        ::tracing::warn!(
            tables = tables.len(),
            "the destination lists more Apache Iceberg tables than one resolution walks here, \
             so its tables were not resolved against the Data Catalog"
        );
        return None;
    }
    // Nothing is refused that could not be grounded in the Data Catalog's own store: with no
    // Glue mounted to read, nothing is known about the table and the call proceeds (worth a
    // warning, because that is a deployment where this gate does not run).
    let Some(state) = ::service_com_amazonaws_glue_awsglue::state::service_state(engine) else {
        ::tracing::warn!(
            "the destination's Apache Iceberg tables could not be resolved against the Data \
             Catalog"
        );
        return None;
    };
    let catalog_id = ::service_com_amazonaws_glue_awsglue::state::resolve_catalog_id(
        catalog_account(catalog_arn).as_ref(),
    );
    // The DISTINCT targets the configuration names, each folded the way the catalog stores and
    // looks a name up, built before the catalog is touched: a list naming one table several
    // times is one question for it.
    let targets: BTreeSet<(String, String)> = tables
        .iter()
        .map(|table| {
            (
                ::service_com_amazonaws_glue_awsglue::state::fold_name(
                    &table.destination_database_name,
                ),
                ::service_com_amazonaws_glue_awsglue::state::fold_name(
                    &table.destination_table_name,
                ),
            )
        })
        .collect();
    let store = state.lock();
    for (database, table) in &targets {
        let held = store
            .database(&catalog_id, database)
            .is_some_and(|database| database.tables.contains_key(table));
        if !held {
            return Some(crate::service::unreadable_glue_table_message(role_arn));
        }
    }
    None
}
