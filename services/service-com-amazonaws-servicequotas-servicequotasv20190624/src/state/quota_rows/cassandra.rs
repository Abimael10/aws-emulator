// SPDX-License-Identifier: CC0-1.0

//! Amazon Keyspaces (for Apache Cassandra)'s published default-quota catalog.

use super::*;

/// The page Amazon Keyspaces publishes its quotas on, which the service's own quota
/// descriptions point a reader at.
///
/// Keyspaces describes a limit and then REFERS to its quotas page for the detail, so the
/// rows whose text the recorded listing shows carrying that reference are built through
/// [`with_quotas_reference`] rather than each repeating the sentence and the URL.
const KEYSPACES_QUOTAS_PAGE: &str = "https://docs.aws.amazon.com/keyspaces/latest/devguide/quotas.html";

/// One Amazon Keyspaces description with the service's quotas-page reference on the end.
fn with_quotas_reference(limit: &str) -> String {
    format!("{limit} For more information, see {KEYSPACES_QUOTAS_PAGE}")
}

/// Amazon Keyspaces (for Apache Cassandra)'s published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `cassandra` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code cassandra` reports, which
/// is the "Quotas for Amazon Keyspaces" page of the Amazon Keyspaces Developer Guide), and
/// the recorded default-quota listing of that code for which `QuotaCode` names which
/// quota, for the `Max …` wording the service publishes its rows under and for each row's
/// unit and adjustability. Every row carries the `L-` code AWS publishes the quota under,
/// so a client that read a code out of AWS's catalog resolves the same quota here.
///
/// Keyspaces publishes most of its limits as RAISABLE — the recorded listing reports the
/// sizes, depths and counts of the data model as adjustable, not only the throughput and
/// resource planes — so a row is fixed here only where that listing shows the service
/// publishes it as one that cannot be raised.
pub(in crate::state) fn cassandra_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Max clustering key size", 850.0)
            .code("L-0659E12E")
            .unit("Bytes")
            .fixed()
            .describe(&with_quotas_reference(
                "The maximum combined size of all clustering columns in a row.",
            )),
        quota("Account-level read throughput quota", 80_000.0)
            .code("L-089904FB")
            .describe(&with_quotas_reference(
                "The maximum read throughput, in read capacity units, provisioned across \
                 all tables in this account in the current Region.",
            )),
        quota("Table-level read throughput quota", 40_000.0)
            .code("L-17766544")
            .describe(&with_quotas_reference(
                "The maximum read throughput, in read capacity units, provisioned for a \
                 single table.",
            )),
        quota("Account-level write throughput quota", 80_000.0)
            .code("L-29E90199")
            .fixed()
            .describe(&with_quotas_reference(
                "The maximum write throughput, in write capacity units, provisioned across \
                 all tables in this account in the current Region.",
            )),
        quota("Table-level write throughput quota", 40_000.0)
            .code("L-2C5B14BD")
            .describe(&with_quotas_reference(
                "The maximum write throughput, in write capacity units, provisioned for a \
                 single table.",
            )),
        quota("Max number of keyspaces per AWS Region", 256.0)
            .code("L-2FC1B9A1")
            .describe(
                "The maximum number of keyspaces that can be created per region in this \
                 account.",
            ),
        quota("Max number of tables per AWS Region", 256.0)
            .code("L-324B5396")
            .fixed()
            .describe(
                "The maximum number of tables that can be created per region in this \
                 account.",
            ),
        quota("Max row size", 1.0)
            .code("L-3D8ED127")
            .unit("Megabytes")
            .describe("The maximum size of a row, excluding static column data."),
        quota("Max partition key size", 2048.0)
            .code("L-4C49F3DB")
            .unit("Bytes")
            .describe("The maximum combined size of all partition key columns in a row."),
        quota("Max static data per logical partition", 1.0)
            .code("L-50DCCF0B")
            .unit("Megabytes")
            .describe("The maximum size of all static data in a logical partition."),
        quota("Max number of columns per table", 225.0)
            .code("L-5823D982")
            .fixed()
            .describe("The maximum number of columns that a table can have."),
        quota("Max number of concurrent table operations", 10.0)
            .code("L-6632AD49")
            .describe(
                "The maximum number of concurrent table creations, deletions and updates in \
                 this account in the current Region.",
            ),
        quota("Max number of concurrent restore operations", 4.0)
            .code("L-66AD5703")
            .fixed()
            .describe(
                "The maximum number of concurrent table restores in this account in the \
                 current Region.",
            ),
        quota("Max number of tags per resource", 50.0)
            .code("L-66D0104D")
            .fixed()
            .describe("The maximum number of tags that a keyspace or table can have."),
        quota("Max TTL", 630_720_000.0)
            .code("L-677FFD22")
            .unit("Seconds")
            .describe("The maximum Time to Live that can be set on a row or a column."),
        quota("Max number of UDTs per AWS Region", 256.0)
            .code("L-7A88D508")
            .describe(
                "The maximum number of UDTs that can be created per region across all \
                 keyspaces.",
            ),
        quota("Max nested depth per UDT", 10.0)
            .code("L-7F2EAE05")
            .describe("The maximum depth that a UDT can be nested to."),
        quota("Max number of fields per UDT", 100.0)
            .code("L-964C49BD")
            .describe("The maximum number of fields supported for a UDT."),
        quota("Max number of UDTs per table", 50.0)
            .code("L-96FEFC6D")
            .describe("The maximum number of UDTs that a table can reference."),
        quota("Max length of a UDT name", 48.0)
            .code("L-BF48748A")
            .unit("Bytes")
            .describe("The maximum length of the name of a UDT."),
        quota("Max number of multi-Region replicas per table", 6.0)
            .code("L-C63C913D")
            .describe("The maximum number of Regions that a table can be replicated to."),
        quota("Max size of a single column value", 1.0)
            .code("L-D5E49767")
            .unit("Megabytes")
            .fixed()
            .describe("The maximum size of the value of a single column."),
        quota("Max number of CQL statements per batch", 30.0)
            .code("L-E08A04C1")
            .fixed()
            .describe("The maximum number of statements supported in a single batch."),
        quota("Max number of partition keys per IN clause", 100.0)
            .code("L-E7DB0CFF")
            .describe("The maximum number of partition keys supported in an IN clause."),
        quota("Max number of simultaneous CQL connections", 3000.0)
            .code("L-F41E662B")
            .describe(
                "The maximum number of simultaneous CQL client connections in this account \
                 in the current Region.",
            ),
        quota("Max amount of direct child UDTs per UDT", 10.0)
            .code("L-F90953AC")
            .describe("The maximum number of child UDTs supported for a UDT."),
        quota("Max number of columns per partition key", 10.0)
            .code("L-FDBA801F")
            .describe("The maximum number of columns supported in a partition key."),
    ]
}
