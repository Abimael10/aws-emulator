// SPDX-License-Identifier: CC0-1.0

//! Which refusal a node read answers when its `SyncName` names no sync it can read, and when
//! the account has not enabled the Systems Manager integrated experience.
//!
//! The RULE under test: the two conditions are DISTINCT and each has its own modelled error,
//! so the sync a request NAMES is resolved first. "The name of the supported resource data
//! sync is AWS-QuickSetup-ManagedNode" (`ListNodesRequest$SyncName`), so any other name names
//! no sync a node read can read and is refused `ResourceDataSyncNotFoundException` — carrying
//! the `SyncName` the request asked for, so the client is told WHICH sync was missing —
//! whether or not the account has the experience; `UnsupportedOperationException` is left for
//! what it is about, a request this account cannot run at all, which is one that names no
//! sync of its own (or names the supported one) at an endpoint without the experience.
//!
//! Both node reads are held to it: `ListNodes` and `ListNodesSummary` read the same account
//! configuration and the same sync.
//!
//! Every call goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — and the experience is turned on through this service's
//! OWN `CreateResourceDataSync`, so what a read answers follows a write a caller made rather
//! than a flag a test planted.
//!
//! The rows differ along what the rule generalises over: the sync name (one nothing created,
//! one a create of ANOTHER sync type did create, the supported name in another case, the
//! supported name with a suffix), whether the account has the experience, and which of the
//! two reads is called.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router, REGION};

/// The name of the resource data sync the integrated experience puts in an account: "the
/// name of the supported resource data sync is AWS-QuickSetup-ManagedNode"
/// (`smithy_query(query="doc", shape="ListNodesRequest", member="SyncName")`).
const MANAGED_NODE_SYNC: &str = "AWS-QuickSetup-ManagedNode";

/// The sync type a node read's sync is of: it aggregates data "from multiple Amazon Web
/// Services Regions" (`CreateResourceDataSync`'s `SyncType` doc), which is the type the
/// refusal reports beside the name.
const FROM_SOURCE: &str = "SyncFromSource";

/// The `UnsupportedOperationException` sentence an account without the experience is answered
/// with, as the service words it.
const NOT_ENABLED: &str = "This is an unsupported operation for this account. You must first \
                           enable the Systems Manager integrated experience in your account.";

/// The two node reads the rule holds for, with the members each needs beside `SyncName`:
/// `ListNodesSummary` requires an `Aggregators` list (`len 1..2`), `ListNodes` requires
/// nothing.
const READS: [(&str, Option<&str>); 2] = [("ListNodes", None), ("ListNodesSummary", Some("Region"))];

/// How this service words a sync a request addresses that the Region holds none of.
fn not_found(sync_name: &str) -> String {
    format!("ResourceDataSync with name {sync_name} synctype {FROM_SOURCE} does not exists")
}

/// The body one of [`READS`] is called with for `sync_name`.
fn body(aggregated_by: Option<&str>, sync_name: Option<&str>) -> Value {
    let mut body = json!({});
    if let Some(attribute) = aggregated_by {
        body["Aggregators"] = json!([{
            "AggregatorType": "Count",
            "TypeName": "Instance",
            "AttributeName": attribute,
        }]);
    }
    if let Some(sync_name) = sync_name {
        body["SyncName"] = json!(sync_name);
    }
    body
}

/// Turn the integrated experience on at [`REGION`]'s endpoint, through this service's own
/// `CreateResourceDataSync`.
async fn enable_experience(router: &MultiServiceRouter) {
    let (status, response) = call(
        router,
        "CreateResourceDataSync",
        &json!({
            "SyncName": MANAGED_NODE_SYNC,
            "SyncType": FROM_SOURCE,
            "SyncSource": {
                "SourceType": "SingleAccountMultiRegions",
                "SourceRegions": [REGION],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the managed node sync must be created, got {status} {response}"
    );
}

/// Create a sync of the OTHER sync type under `sync_name`, so a row names a sync the account
/// really holds and that a node read still cannot read.
async fn create_other_sync(router: &MultiServiceRouter, sync_name: &str) {
    let (status, response) = call(
        router,
        "CreateResourceDataSync",
        &json!({
            "SyncName": sync_name,
            "S3Destination": {
                "BucketName": "nodes",
                "SyncFormat": "JsonSerDe",
                "Region": REGION,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the second sync must be created, got {status} {response}"
    );
}

/// RULE: a `SyncName` naming no sync a node read can read is the sync-not-found refusal,
/// carrying that name — before the account's experience is looked at, and so whether or not
/// the account has it.
#[tokio::test]
async fn a_sync_name_naming_no_readable_sync_is_reported_as_missing() {
    // (case, the sync name the request names)
    let names: [(&str, &str); 4] = [
        ("a name nothing created", "never-created"),
        ("a sync of the other sync type", "inventory-sync"),
        ("the supported name in another case", "aws-quicksetup-managednode"),
        ("the supported name with a suffix", "AWS-QuickSetup-ManagedNodes"),
    ];
    // (case, whether the account has the integrated experience)
    let accounts: [(&str, bool); 2] = [
        ("an account without the experience", false),
        ("an account with the experience", true),
    ];

    for (account, enabled) in accounts {
        for (operation, aggregated_by) in READS {
            let router = router();
            create_other_sync(&router, "inventory-sync").await;
            if enabled {
                enable_experience(&router).await;
            }

            for (named, sync_name) in names {
                let case = format!("{operation} for {named:?} at {account}");
                let (status, response) =
                    call(&router, operation, &body(aggregated_by, Some(sync_name))).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} {response}"
                );
                assert_eq!(
                    error_code(&response),
                    "ResourceDataSyncNotFoundException",
                    "case {case:?}: expected the sync-not-found refusal, got {response}"
                );
                assert_eq!(
                    error_message(&response),
                    not_found(sync_name),
                    "case {case:?}: expected the refusal to name the sync asked for, got \
                     {response}"
                );
                assert_eq!(
                    (
                        response["SyncName"].as_str(),
                        response["SyncType"].as_str()
                    ),
                    (Some(sync_name), Some(FROM_SOURCE)),
                    "case {case:?}: expected the refused sync to be echoed, got {response}"
                );
            }
        }
    }
}

/// RULE, the other half: the unsupported-operation refusal is what a request this account
/// cannot run at all is answered with — one naming no sync, or naming the supported sync the
/// account does not hold — and once the experience is enabled the same request is served.
#[tokio::test]
async fn an_account_without_the_experience_is_refused_the_unsupported_operation() {
    for (operation, aggregated_by) in READS {
        let router = router();

        // (case, the sync the request names)
        let rows: [(&str, Option<&str>); 2] = [
            ("naming no sync at all", None),
            ("naming the supported sync", Some(MANAGED_NODE_SYNC)),
        ];
        for (named, sync_name) in rows {
            let case = format!("{operation} {named}");
            let (status, response) = call(&router, operation, &body(aggregated_by, sync_name)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "UnsupportedOperationException",
                "case {case:?}: expected the unsupported-operation refusal, got {response}"
            );
            assert_eq!(
                error_message(&response),
                NOT_ENABLED,
                "case {case:?}: expected the not-enabled sentence, got {response}"
            );
        }

        // The refusal is about the account's configuration, so the write that configures it
        // is what turns the same request into a served read.
        enable_experience(&router).await;
        for (named, sync_name) in rows {
            let case = format!("{operation} {named}, once enabled");
            let (status, response) = call(&router, operation, &body(aggregated_by, sync_name)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the read to be served, got {status} {response}"
            );
        }
    }
}
