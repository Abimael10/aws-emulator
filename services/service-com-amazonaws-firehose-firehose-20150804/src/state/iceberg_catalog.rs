// SPDX-License-Identifier: CC0-1.0

//! Resolving the Data Catalog target of an APACHE ICEBERG destination, before that
//! destination is written onto a stream.
//!
//! An Iceberg destination delivers to a TABLE of a Data Catalog rather than to an Amazon S3
//! prefix: "Provides a list of `DestinationTableConfigurations` which Firehose uses to deliver
//! data to Apache Iceberg Tables"
//! (`IcebergDestinationConfiguration$DestinationTableConfigurationList`), each entry naming its
//! `DestinationDatabaseName` and `DestinationTableName`. Firehose READS that table with the
//! configuration's own `RoleARN` — "the IAM role to be assumed by Firehose for calling Apache
//! Iceberg Tables" — at the moment the configuration is submitted, exactly as it resolves the
//! other resources a destination names ([`crate::state::unassumable_role`] for the roles,
//! [`crate::state::absent_destination_bucket`] for the buckets): a table that read cannot
//! return is an ARGUMENT fault of the call that named it, so nothing is written and the client
//! is told at that call rather than handed a stream that reports healthy and can never commit.
//!
//! That read is one call, [`GLUE_TABLE_READ_ACTION`], and BOTH halves of its answer are the
//! same fault here, because they are the same fault to the caller: the Data Catalog may not
//! hold the table, and the permissions in force on the role may not allow the call. Firehose
//! words one sentence for both ([`crate::service::unreadable_glue_table_message`]), so a role
//! without the catalog read, a missing table and a missing database are answered alike.
//!
//! The two halves are grounded in the two stores that own them:
//!
//! * whether the catalog HOLDS the table is read from the Glue service's own store, which is
//!   where the delivery path reads a destination table's columns and metadata pointer from
//!   ([`crate::state::delivery::iceberg`]), so a table reported as resolvable here is a table a
//!   commit can find;
//! * whether the role MAY read it is IAM's answer about that role's own permissions
//!   (`service_common::iam_role::role_withholds_action`), the same question this crate puts for
//!   the retrieval from a configured source
//!   ([`crate::state::kinesis_source::unreadable_source`]) and for the write a delivery makes.
//!
//! ## Lock order
//!
//! The Data Catalog is read with ITS own lock and nothing else is held while it is: every
//! caller resolves the target BEFORE it takes this service's own store, and the authorization
//! question is put to IAM with no store locked at all.

use std::collections::BTreeSet;

use crate::ops::types::{DestinationTableConfiguration, IcebergDestinationConfiguration};
use crate::state::delivery::iceberg::{addressed_catalog_id, MAX_DESTINATION_TABLES};
use crate::state::DestinationConfigKind;

/// The call Firehose makes for a destination table: AWS Glue's own read of a table, as a
/// policy names it. The one action this resolution asks about, which is also the call the
/// refusal states.
pub const GLUE_TABLE_READ_ACTION: &str = "glue:GetTable";

/// The ARN vendor segment of a Data Catalog resource.
const GLUE_ARN_SERVICE: &str = "glue";

/// The ARN resource-type segment of a Data Catalog table, whose resource portion carries the
/// table's database as well as its own name (`table/<database>/<table>`).
const GLUE_TABLE_ARN_SEGMENT: &str = "table";

/// Why the Data Catalog read of the destination tables the Iceberg configuration `config`
/// names cannot be made, worded as the refusal states it; `None` when every table it lists is
/// one Firehose can read, and for a destination of any other kind.
///
/// The configuration is read as the delivery path reads it — the catalog the
/// `CatalogConfiguration` names, the tables the list names and the role the destination reads
/// them with — so one reading answers for the create that writes a destination and for the
/// update that replaces one.
///
/// A destination that asks Firehose to CREATE its tables itself (`TableCreationConfiguration`,
/// "the configuration to enable automatic table creation", which `CatalogConfiguration`'s own
/// `WarehouseLocation` is configured for) does not need them to EXIST yet, so a table absent
/// under that configuration is not a fault — but the role still has to be allowed to make the
/// read, because a destination whose tables Firehose creates is one Firehose reads them back
/// from.
pub async fn unresolvable_destination_table<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    config: &DestinationConfigKind,
) -> Option<String> {
    let DestinationConfigKind::Iceberg(config) = config else {
        return None;
    };
    unresolvable_tables(
        engine,
        ctx,
        region,
        config.destination_table_configuration_list.as_deref(),
        config.catalog_configuration.catalog_arn.as_deref(),
        &config.role_arn,
        creates_its_tables(config),
    )
    .await
}

/// Whether the destination creates its destination tables itself.
fn creates_its_tables(config: &IcebergDestinationConfiguration) -> bool {
    config
        .table_creation_configuration
        .as_ref()
        .is_some_and(|configured| configured.enabled)
}

/// The same resolution over the parts a caller holds separately: the tables the call names, the
/// Data Catalog they live in, and the role the destination reads them with.
///
/// A configuration that lists NO table still has that read made: the tables such a destination
/// delivers to are only known per record, so there is no table to look up, but the role still
/// has to be allowed to read the tables of the catalog the configuration names — which is
/// exactly what `glue:GetTable` on that CATALOG is granted or withheld for. The same goes for a
/// destination whose tables Firehose CREATES itself (`creates_its_tables`): no table of it has
/// to exist yet, and the role still has to be allowed the read.
///
/// WHAT THIS WALKS is bounded twice over, because the list is the CALLER's: a configuration
/// naming more targets than one delivery of this emulator walks
/// ([`MAX_DESTINATION_TABLES`]) is resolved not at all rather than walked, and the targets
/// inside that bound are reduced to the DISTINCT ones — so the names are read, folded and
/// collected once each, before the Data Catalog's lock is taken. The critical section is then
/// one bounded series of index lookups holding no allocation, and at most that many
/// authorization questions follow it, so a configuration cannot decide how long another caller
/// of that catalog waits.
pub async fn unresolvable_tables<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    tables: Option<&[DestinationTableConfiguration]>,
    catalog_arn: Option<&str>,
    role_arn: &str,
    creates_its_tables: bool,
) -> Option<String> {
    let tables = tables.unwrap_or_default();
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
    let catalog_id = ::service_com_amazonaws_glue_awsglue::state::resolve_catalog_id(
        addressed_catalog_id(catalog_arn).as_ref(),
    );
    // The DISTINCT targets the configuration names, each folded the way the catalog stores and
    // looks a name up, built before either store is touched: a list naming one table several
    // times is one question for each of them.
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
    if !creates_its_tables && absent_from_catalog(engine, &catalog_id, &targets) {
        return Some(crate::service::unreadable_glue_table_message(role_arn));
    }
    // And the read is made UNDER the configuration's role, so what that role's permissions
    // allow decides it just as the catalog's contents do. Asked of IAM itself, with no store
    // held, once per resource the read addresses — a policy written for one table grants that
    // table and says nothing about another.
    for resource in authorized_resources(region, &catalog_id, catalog_arn, &targets) {
        if ::service_common::iam_role::role_withholds_action(
            ctx,
            region,
            role_arn,
            GLUE_TABLE_READ_ACTION,
            &resource,
        )
        .await
        {
            return Some(crate::service::unreadable_glue_table_message(role_arn));
        }
    }
    None
}

/// The resources the destination's catalog read is authorized against: one ARN per distinct
/// destination table the configuration names, or the CATALOG's own ARN where no table of it can
/// be addressed on its own.
///
/// `glue:GetTable` is granted on a catalog, on a database or on a table, so the catalog is what a
/// destination is answered about when it names no table at all — its per-record routing decides
/// them — and equally when its tables live in a catalog RESOURCE inside the account's Data
/// Catalog, whose `CatalogId` is that account and the catalog's name together
/// (`child_catalog_id`) and so addresses no table ARN: the catalog the configuration carries is
/// the one resource such a destination does name.
fn authorized_resources(
    region: &str,
    catalog_id: &str,
    catalog_arn: Option<&str>,
    targets: &BTreeSet<(String, String)>,
) -> Vec<String> {
    // A `CatalogId` that is an account number addresses that account's own Data Catalog, whose
    // tables are addressed as `table/<database>/<table>` of that account — the shared reading of
    // that form is `service_common::arn::is_account_id`.
    let addresses_its_tables =
        !targets.is_empty() && ::service_common::arn::is_account_id(catalog_id);
    if !addresses_its_tables {
        return vec![match catalog_arn.filter(|arn| !arn.is_empty()) {
            // The ARN the configuration carries IS how a policy names that catalog.
            Some(arn) => arn.to_string(),
            None => ::service_com_amazonaws_glue_awsglue::state::catalog_arn(region, catalog_id),
        }];
    }
    targets
        .iter()
        .map(|(database, table)| glue_table_arn(region, catalog_id, database, table))
        .collect()
}

/// Whether the Data Catalog holds no table for one of `targets`, read under the catalog's own
/// lock and nothing else.
///
/// `false` — refuse nothing — where the question could not be grounded in the Data Catalog's own
/// store: with no Glue mounted to read, nothing is known about the table and the call proceeds
/// (worth a warning, because that is a deployment where this half of the gate does not run).
fn absent_from_catalog<Eng: Send + Sync + 'static>(
    engine: &Eng,
    catalog_id: &str,
    targets: &BTreeSet<(String, String)>,
) -> bool {
    let Some(state) = ::service_com_amazonaws_glue_awsglue::state::service_state(engine) else {
        ::tracing::warn!(
            "the destination's Apache Iceberg tables could not be resolved against the Data \
             Catalog"
        );
        return false;
    };
    let store = state.lock();
    targets.iter().any(|(database, table)| {
        !store
            .database(catalog_id, database)
            .is_some_and(|database| database.tables.contains_key(table))
    })
}

/// The ARN of the Data Catalog table `table` of `database`, held in the catalog `catalog_id`
/// and addressed in `region` — the resource a policy grants or withholds the catalog read on.
///
/// The canonical layout and partition come from the shared minter, so this ARN matches every
/// other ARN this emulator emits; only the ACCOUNT field is replaced, because a destination may
/// name the catalog of another account and that account is what such an ARN names.
fn glue_table_arn(region: &str, catalog_id: &str, database: &str, table: &str) -> String {
    let canonical = ::engine::emulator_arn(
        GLUE_ARN_SERVICE,
        region,
        &format!("{GLUE_TABLE_ARN_SEGMENT}/{database}/{table}"),
    );
    match ::service_common::arn::parse(&canonical) {
        Some(fields) => format!(
            "arn:{}:{}:{}:{catalog_id}:{}",
            fields.partition, fields.vendor, fields.region, fields.resource
        ),
        None => canonical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a destination table is addressed by the catalog that holds it in the ARN's
    /// account field and by its database beside its own name in the resource field, which is
    /// the resource a `glue:GetTable` grant is written about.
    ///
    /// The rows span what the form generalises over: the Region the request reached, the
    /// catalog the destination named (this account's own and another's), and the database and
    /// table names.
    #[test]
    fn a_destination_table_is_addressed_by_its_catalog_database_and_name() {
        let account = ::engine::emulator_account_id();
        for (region, catalog, database, table) in [
            ("us-west-2", account, "lake", "events"),
            ("eu-west-1", account, "iceberg_delivery_db", "present"),
            ("ap-southeast-2", "111122223333", "shared", "profiles"),
        ] {
            let built = glue_table_arn(region, catalog, database, table);
            let expected = format!("arn:aws:glue:{region}:{catalog}:table/{database}/{table}");
            assert!(
                built == expected,
                "case {region:?}/{catalog:?}: expected {expected:?}, got {built:?}"
            );
        }
    }
}
