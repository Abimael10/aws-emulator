// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the one rule a tenant database's whole existence hangs off: a
//! tenant database is a pluggable database INSIDE a DB instance, reached only through that
//! instance, so it FOLLOWS that instance.
//!
//! Two things follow from that rule, and both are asserted here over the wire:
//!
//! * an instance that is RENAMED carries its tenant databases over to the identifier it is
//!   now called by — each of them is addressed by its instance, so a report narrowed to the
//!   new identifier answers with them and they report it as their `DBInstanceIdentifier`;
//! * an instance that is DELETED takes its tenant databases with it, so no report answers
//!   with a tenant database whose DB instance nothing can resolve any more — and only its
//!   own go: another instance's tenant databases are untouched.
//!
//! The rows span what the rule generalises over: an instance holding one tenant database
//! and one holding several, created out of name order, and a second instance in the same
//! region whose tenant databases neither operation may touch.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_error, expect_ok, optional_text, subtrees, text, texts};
use crate::lifecycle_poll_loops::poll_until;

/// The instance class every DB instance below is created with.
const INSTANCE_CLASS: &str = "db.m5.large";

/// One of the Oracle container-database engines, which is what makes an instance one that
/// holds tenant databases at all.
const CDB_ENGINE: &str = "oracle-ee-cdb";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// A `DescribeTenantDatabases` call that narrows nothing: every tenant database the signed
/// region holds.
fn every_tenant_database() -> Vec<(&'static str, String)> {
    Vec::new()
}

/// `CreateDBInstance` on a container-database engine, followed by a poll loop until it
/// reports `available`, and then one `CreateTenantDatabase` per name in `tenants`.
async fn instance_holding(
    router: &MultiServiceRouter,
    identifier: &str,
    tenants: &[&str],
) -> Vec<String> {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => CDB_ENGINE,
                "DBInstanceClass" => INSTANCE_CLASS,
                "MasterUsername" => "oracleadmin",
                "MasterUserPassword" => "a-strong-password",
                "MultiTenant" => "true",
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => identifier];
    poll_until(
        identifier,
        "the DB instance reaching available",
        || call(router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    let mut resource_ids = Vec::new();
    for tenant in tenants {
        let created = expect_ok(
            identifier,
            call(
                router,
                "CreateTenantDatabase",
                &params![
                    "DBInstanceIdentifier" => identifier,
                    "TenantDBName" => tenant,
                    "MasterUsername" => "tenantadmin",
                    "MasterUserPassword" => "a-strong-password",
                ],
            )
            .await,
        );
        resource_ids.push(text(&created, "TenantDatabaseResourceId"));
    }
    resource_ids
}

#[tokio::test]
async fn a_renamed_db_instance_carries_its_tenant_databases() {
    let router = crate::db_instances::emulator();
    let case = "an instance holding several tenant databases, renamed";
    // Created out of name order, so the report's own order is the store's and not the
    // order the creates arrived in.
    let created = instance_holding(&router, "emu-cdb-rename", &["tenantb", "tenanta"]).await;
    let untouched = instance_holding(&router, "emu-cdb-other", &["tenantc"]).await;

    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-cdb-rename",
                "NewDBInstanceIdentifier" => "emu-cdb-renamed",
                "ApplyImmediately" => "true",
            ],
        )
        .await,
    );

    // The tenant databases answer under the identifier the instance is now called by, with
    // the identity they were created with.
    let read = params!["DBInstanceIdentifier" => "emu-cdb-renamed"];
    let (reported, _) = poll_until(
        case,
        "the renamed instance reporting its tenant databases",
        || call(&router, "DescribeTenantDatabases", &read),
        |status, body| status == 200 && subtrees(body, "TenantDatabase").len() == 2,
    )
    .await;
    assert_eq!(
        texts(&reported, "TenantDBName"),
        vec!["tenanta".to_string(), "tenantb".to_string()],
        "case {case:?}: expected both tenant databases of the renamed instance, got {reported}"
    );
    let mut moved: Vec<String> = texts(&reported, "TenantDatabaseResourceId");
    moved.sort();
    let mut expected = created.clone();
    expected.sort();
    assert_eq!(
        moved, expected,
        "case {case:?}: a rename moves the SAME tenant databases, got {reported}"
    );
    for envelope in subtrees(&reported, "TenantDatabase") {
        assert_eq!(
            text(&envelope, "DBInstanceIdentifier"),
            "emu-cdb-renamed",
            "case {case:?}: a tenant database reports the instance that holds it NOW: \
             {envelope}"
        );
    }
    // And nothing is left behind under the identifier the instance used to have: that
    // identifier names no DB instance at all.
    expect_error(
        case,
        call(
            &router,
            "DescribeTenantDatabases",
            &params!["DBInstanceIdentifier" => "emu-cdb-rename"],
        )
        .await,
        404,
        "DBInstanceNotFound",
    );
    // The other instance's tenant database is untouched by the rename.
    let others = expect_ok(
        case,
        call(
            &router,
            "DescribeTenantDatabases",
            &params!["DBInstanceIdentifier" => "emu-cdb-other"],
        )
        .await,
    );
    assert_eq!(
        texts(&others, "TenantDatabaseResourceId"),
        untouched,
        "case {case:?}: another instance's tenant databases are no part of this rename: \
         {others}"
    );
}

#[tokio::test]
async fn a_deleted_db_instance_takes_its_tenant_databases_with_it() {
    let router = crate::db_instances::emulator();
    let case = "an instance holding several tenant databases, deleted";
    instance_holding(&router, "emu-cdb-deleted", &["tenanta", "tenantb"]).await;
    let untouched = instance_holding(&router, "emu-cdb-kept", &["tenantc"]).await;

    expect_ok(
        case,
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-cdb-deleted",
                "SkipFinalSnapshot" => "true",
            ],
        )
        .await,
    );
    // Once the instance has left the service, its tenant databases are gone with it: the
    // report that narrows nothing answers with the OTHER instance's tenant database alone.
    let read = every_tenant_database();
    let (reported, _) = poll_until(
        case,
        "the deleted instance's tenant databases going with it",
        || call(&router, "DescribeTenantDatabases", &read),
        |status, body| status == 200 && subtrees(body, "TenantDatabase").len() == 1,
    )
    .await;
    assert_eq!(
        texts(&reported, "TenantDatabaseResourceId"),
        untouched,
        "case {case:?}: only the surviving instance's tenant database is left, got {reported}"
    );
    // And the deleted instance's own identifier resolves nothing to report on.
    expect_error(
        case,
        call(
            &router,
            "DescribeTenantDatabases",
            &params!["DBInstanceIdentifier" => "emu-cdb-deleted"],
        )
        .await,
        404,
        "DBInstanceNotFound",
    );
}
