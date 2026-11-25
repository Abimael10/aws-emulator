// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the tagging APIs' not-found refusal, end to end over the wire.
//!
//! The rule under test: when a tagging call's `resourceArn` resolves to no stored
//! resource, the modelled `ResourceNotFoundException` (HTTP 404) carries a sentence that
//! is entirely FIXED text, worded from the resource KIND the ARN's resource-type segment
//! names — never any part of the request. So the same sentence comes back for every ARN of
//! one kind, whatever id, account, Region or partition it carries, and the three tagging
//! operations (`ListTagsForResource`, `TagResource`, `UntagResource`) word it identically,
//! while two ARNs of DIFFERENT kinds are told apart by their text. The ARN the call
//! submitted travels in the `ResourceNotFoundException$resourceName` member instead.
//!
//! The kinds in the table are this service's own taggable resources — the ARN forms its
//! creates mint, `namespace/<id>`, `workgroup/<id>` and `snapshot/<id>` (see
//! `smithy_query(query="operation", operation="CreateNamespace,CreateWorkgroup,CreateSnapshot")`
//! for the `namespaceArn` / `workgroupArn` / `snapshotArn` members they return) — plus the
//! forms that name no taggable resource of this service at all: another resource type of
//! this service, another service's ARN, and values that carry no resource-type segment.
//!
//! The harness (the emulator, the signed call and the error-code reader) is shared with
//! `namespace_workgroup_lifecycle`, which owns it.

use serde_json::{json, Value};

use crate::namespace_workgroup_lifecycle::{call, emulator, error_code, ACCOUNT};

/// One row of the wording table: an ARN that resolves to nothing, and the fixed sentence
/// every tagging call must refuse it with.
#[derive(Debug)]
struct WordingCase {
    /// What distinguishes this row.
    label: &'static str,
    /// The `resourceArn` the call submits.
    arn: String,
    /// The whole message the refusal must carry, character for character.
    message: &'static str,
}

/// The rows: every taggable kind this service mints an ARN for, each reached through
/// several unresolvable ARNs, and the forms that name no taggable resource of this service.
fn wording_cases() -> Vec<WordingCase> {
    // Ids no create minted: they resolve to nothing in every row that carries one.
    let absent = "7a1e5c90-2b34-4d68-9e07-3f5c81b2a46d";
    let other_absent = "0d4b9f21-8c6e-4a53-b172-9e8f3a6c5d04";
    vec![
        WordingCase {
            label: "a namespace ARN whose id names nothing",
            arn: format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:namespace/{absent}"),
            message: "Namespace not found",
        },
        WordingCase {
            label: "another namespace ARN, another id, another Region",
            arn: format!(
                "arn:aws:redshift-serverless:eu-west-2:{ACCOUNT}:namespace/{other_absent}"
            ),
            message: "Namespace not found",
        },
        WordingCase {
            label: "a namespace ARN of another account in another partition",
            arn: format!(
                "arn:aws-us-gov:redshift-serverless:us-gov-west-1:999999999999:namespace/{absent}"
            ),
            message: "Namespace not found",
        },
        WordingCase {
            label: "a workgroup ARN whose id names nothing",
            arn: format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:workgroup/{absent}"),
            message: "Workgroup not found",
        },
        WordingCase {
            label: "another workgroup ARN, another id, another Region",
            arn: format!(
                "arn:aws:redshift-serverless:ap-southeast-2:{ACCOUNT}:workgroup/{other_absent}"
            ),
            message: "Workgroup not found",
        },
        WordingCase {
            label: "a snapshot ARN whose id names nothing",
            arn: format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:snapshot/{absent}"),
            message: "Snapshot not found",
        },
        WordingCase {
            label: "another snapshot ARN, another id, another account",
            arn: format!(
                "arn:aws:redshift-serverless:us-east-1:123456789012:snapshot/{other_absent}"
            ),
            message: "Snapshot not found",
        },
        WordingCase {
            label: "a resource type of this service that holds no tags",
            arn: format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:usagelimit/{absent}"),
            message: "Resource not found",
        },
        WordingCase {
            label: "another service's ARN",
            arn: format!("arn:aws:s3:::a-bucket-{absent}"),
            message: "Resource not found",
        },
        WordingCase {
            label: "a value that is no ARN at all",
            arn: "not-an-arn".to_string(),
            message: "Resource not found",
        },
        WordingCase {
            label: "an ARN with no resource segment",
            arn: format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:"),
            message: "Resource not found",
        },
    ]
}

/// The three tagging calls over `arn`, each named with the operation it drives.
fn tagging_requests(arn: &str) -> [(&'static str, Value); 3] {
    [
        ("ListTagsForResource", json!({ "resourceArn": arn })),
        (
            "TagResource",
            json!({ "resourceArn": arn, "tags": [{ "key": "env", "value": "prod" }] }),
        ),
        (
            "UntagResource",
            json!({ "resourceArn": arn, "tagKeys": ["env"] }),
        ),
    ]
}

#[tokio::test]
async fn the_tagging_not_found_refusal_is_worded_from_the_arns_resource_kind() {
    let router = emulator();
    for case in wording_cases() {
        for (operation, request) in tagging_requests(&case.arn) {
            let named = format!("{} / {operation} ({})", case.label, case.arn);
            let (status, body) = call(&router, operation, request).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {named}: got {status} with {body}"
            );
            assert_eq!(
                body["message"],
                json!(case.message),
                "case {named}: expected {:?}, got {:?}",
                case.message,
                body["message"]
            );
            // The sentence is fixed text: none of the ARN's variable parts — its id, its
            // account or its Region — may appear in it, and the ARN travels in the member
            // modelled for the resource that could not be found instead.
            let message = body["message"].as_str().unwrap_or_default().to_string();
            for part in case.arn.split([':', '/']).filter(|part| part.len() > 4) {
                assert!(
                    !message.contains(part),
                    "case {named}: the sentence must carry no part of the ARN, \
                     but {message:?} carries {part:?}"
                );
            }
            assert_eq!(
                body["resourceName"],
                json!(case.arn),
                "case {named}: resourceName names the resource that was not found, got {:?}",
                body["resourceName"]
            );
        }
    }
}

#[tokio::test]
async fn the_refusal_is_about_the_arn_resolving_to_nothing_and_not_the_request() {
    let router = emulator();

    // One resource of each taggable kind, created so its ARN DOES resolve.
    let (status, body) = call(
        &router,
        "CreateNamespace",
        json!({ "namespaceName": "wording-ns", "tags": [{ "key": "env", "value": "prod" }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: CreateNamespace expected 200, got {status} with {body}"
    );
    let namespace_arn = arn(&body["namespace"], "namespaceArn");

    let (status, body) = call(
        &router,
        "CreateWorkgroup",
        json!({ "workgroupName": "wording-wg", "namespaceName": "wording-ns" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: CreateWorkgroup expected 200, got {status} with {body}"
    );
    let workgroup_arn = arn(&body["workgroup"], "workgroupArn");

    let (status, body) = call(
        &router,
        "CreateSnapshot",
        json!({ "snapshotName": "wording-snap", "namespaceName": "wording-ns" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: CreateSnapshot expected 200, got {status} with {body}"
    );
    let snapshot_arn = arn(&body["snapshot"], "snapshotArn");

    for (kind, resolving) in [
        ("namespace", &namespace_arn),
        ("workgroup", &workgroup_arn),
        ("snapshot", &snapshot_arn),
    ] {
        // The identical requests the table above was refused with are SERVED for an ARN
        // that resolves, so the refusal is a fact about state, not about the request.
        for (operation, request) in tagging_requests(resolving) {
            let (status, body) = call(&router, operation, request).await;
            assert_eq!(
                status, 200,
                "case {kind} / {operation}: a resolving ARN expected 200, got {status} \
                 with {body}"
            );
        }

        // And replacing only the id segment of that very ARN makes it unresolvable again,
        // refused with the sentence worded from the same kind.
        let unresolvable = replace_id(resolving, "9f3c6a28-7b45-4e19-8d02-1a6e5c7b3f40");
        let (status, body) = call(
            &router,
            "ListTagsForResource",
            json!({ "resourceArn": unresolvable }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {kind} / a fresh id in the same ARN: got {status} with {body}"
        );
        let expected = format!(
            "{} not found",
            match kind {
                "namespace" => "Namespace",
                "workgroup" => "Workgroup",
                _ => "Snapshot",
            }
        );
        assert_eq!(
            body["message"],
            json!(expected),
            "case {kind} / a fresh id in the same ARN: expected {expected:?}, got {:?}",
            body["message"]
        );
    }
}

/// The ARN member `member` of a created resource, asserted present.
fn arn(resource: &Value, member: &str) -> String {
    resource[member]
        .as_str()
        .unwrap_or_else(|| panic!("case setup: {member} must be a string, got {resource}"))
        .to_string()
}

/// `arn` with everything after its last `/` replaced by `id`.
fn replace_id(arn: &str, id: &str) -> String {
    match arn.rsplit_once('/') {
        Some((head, _)) => format!("{head}/{id}"),
        None => id.to_string(),
    }
}
