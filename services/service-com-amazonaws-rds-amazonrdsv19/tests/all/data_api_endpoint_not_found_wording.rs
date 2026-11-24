// SPDX-License-Identifier: CC0-1.0

//! The sentence `EnableHttpEndpoint` and `DisableHttpEndpoint` refuse an unresolvable
//! `ResourceArn` with.
//!
//! The rule under test: when a Data API endpoint toggle's `ResourceArn` names no DB cluster
//! this account holds in the request's Region, Amazon RDS answers `ResourceNotFoundFault`
//! (HTTP 404 — `smithy_query(query="operation", operation="EnableHttpEndpoint")`) whose
//! `Message` is one sentence ABOUT the cluster: the fixed text `The DB cluster `, the value
//! the request's `ResourceArn` carried, exactly as it was sent, and the fixed text
//! ` wasn't found.` — not the `<type> not found: <id>` form this service answers the
//! cluster-identifier calls' `DBClusterNotFoundFault` with.
//!
//! The expectation is written out HERE rather than imported from the code under test, so the
//! assertions pin the wording itself and not whatever the emulator currently formats.
//!
//! The table spans what the rule generalises over — why the ARN resolves to nothing (never
//! created, created and then deleted, pointing at another Region, account or partition, or
//! spelled in a case the store never holds), the length and spelling of the identifier inside
//! it, and both operations of the pair — because the sentence is the same for all of them and
//! its only variable part is the value the member carried.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, text, wait_for_settle, REGION,
};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A second Region, so an ARN can point at a cluster that exists somewhere else.
const OTHER_REGION: &str = "eu-west-1";

/// The two halves of the sentence Amazon RDS words this fault with, as recorded behaviour
/// gives them: everything outside the member's value is fixed text.
const MESSAGE_PREFIX: &str = "The DB cluster ";
const MESSAGE_SUFFIX: &str = " wasn't found.";

/// The `arn:<partition>:rds:<region>:<account>:cluster:<identifier>` form Amazon RDS gives a
/// DB cluster ("Constructing an ARN for Amazon RDS"), written out here rather than taken from
/// the code under test.
fn cluster_arn(region: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{region}:{ACCOUNT}:cluster:{identifier}")
}

/// `CreateDBCluster` for one Aurora cluster of `region`.
async fn create_cluster(router: &MultiServiceRouter, region: &str, identifier: &str) -> String {
    expect_ok(
        identifier,
        call_in(
            router,
            region,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "aurora-mysql"),
            ],
        )
        .await,
    )
}

/// One `ResourceArn` that resolves to no DB cluster of this account in the request's Region.
struct MissCase {
    case: &'static str,
    resource_arn: String,
}

/// Every refusal of the pair carries the one sentence, with the ARN the request sent in it.
#[tokio::test]
async fn an_unresolvable_resource_arn_is_refused_with_the_rds_sentence() {
    let router = emulator();

    // Clusters in both Regions, plus one that is deleted again, so a miss is a miss because
    // of what the ARN points AT rather than because the store is empty.
    create_cluster(&router, REGION, "emu-dataapi-word-here").await;
    create_cluster(&router, OTHER_REGION, "emu-dataapi-word-elsewhere").await;
    create_cluster(&router, REGION, "emu-dataapi-word-gone").await;
    wait_for_settle();
    expect_ok(
        "the deleted cluster",
        call(
            &router,
            "DeleteDBCluster",
            &[
                ("DBClusterIdentifier", "emu-dataapi-word-gone"),
                ("SkipFinalSnapshot", "true"),
            ],
        )
        .await,
    );
    wait_for_settle();

    let misses = [
        MissCase {
            case: "an identifier no create ever used",
            resource_arn: cluster_arn(REGION, "emu-dataapi-word-absent"),
        },
        MissCase {
            case: "a one-character identifier",
            resource_arn: cluster_arn(REGION, "x"),
        },
        MissCase {
            case: "a long identifier",
            resource_arn: cluster_arn(REGION, &format!("emu-{}", "a".repeat(59))),
        },
        MissCase {
            case: "a cluster that was created and then deleted",
            resource_arn: cluster_arn(REGION, "emu-dataapi-word-gone"),
        },
        MissCase {
            case: "a stored cluster's identifier spelled in upper case",
            resource_arn: cluster_arn(REGION, "EMU-DATAAPI-WORD-HERE"),
        },
        MissCase {
            case: "a stored cluster's ARN with another Region in it",
            resource_arn: cluster_arn(OTHER_REGION, "emu-dataapi-word-here"),
        },
        MissCase {
            case: "a cluster that exists only in another Region",
            resource_arn: cluster_arn(OTHER_REGION, "emu-dataapi-word-elsewhere"),
        },
        MissCase {
            case: "a stored cluster's ARN under another account",
            resource_arn: format!(
                "arn:aws:rds:{REGION}:210987654321:cluster:emu-dataapi-word-here"
            ),
        },
        MissCase {
            case: "a stored cluster's ARN in another partition",
            resource_arn: format!(
                "arn:aws-us-gov:rds:{REGION}:{ACCOUNT}:cluster:emu-dataapi-word-here"
            ),
        },
    ];

    for row in &misses {
        for action in ["EnableHttpEndpoint", "DisableHttpEndpoint"] {
            let case = row.case;
            let arn = row.resource_arn.as_str();
            let refused = expect_error(
                case,
                call(&router, action, &[("ResourceArn", arn)]).await,
                404,
                "ResourceNotFoundFault",
            );
            let message = text(&refused, "Message");
            let expected = format!("{MESSAGE_PREFIX}{arn}{MESSAGE_SUFFIX}");
            assert_eq!(
                message, expected,
                "case {case:?} on {action}: expected the message {expected:?}, got {message:?}"
            );
            // The same sentence read as its parts: fixed text around the one variable part,
            // which is the member's value whole — the ARN, not the identifier inside it.
            let named = message
                .strip_prefix(MESSAGE_PREFIX)
                .and_then(|rest| rest.strip_suffix(MESSAGE_SUFFIX));
            assert_eq!(
                named,
                Some(arn),
                "case {case:?} on {action}: expected the sentence to name the ResourceArn the \
                 request sent, got {message:?}"
            );
        }
    }

    // The cluster that IS in the request's Region still serves both toggles, so the refusals
    // above are about the ARN and not about the operation.
    for (action, expected) in [
        ("EnableHttpEndpoint", "true"),
        ("DisableHttpEndpoint", "false"),
    ] {
        let arn = cluster_arn(REGION, "emu-dataapi-word-here");
        let body = expect_ok(
            action,
            call(&router, action, &[("ResourceArn", arn.as_str())]).await,
        );
        let enabled = text(&body, "HttpEndpointEnabled");
        assert_eq!(
            enabled, expected,
            "case {action:?}: expected the resolvable cluster to report HttpEndpointEnabled \
             {expected}, got {enabled:?}"
        );
    }
}
