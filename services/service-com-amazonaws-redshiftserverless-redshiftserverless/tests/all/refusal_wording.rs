// SPDX-License-Identifier: CC0-1.0

//! The WORDING of four refusals, end to end over the wire.
//!
//! A refusal's message is part of its contract: a client that reads it to tell one
//! failure of an error class from another only can when the sentence is the service's
//! own. The four rules under test, each asserted over a TABLE of inputs rather than one
//! request:
//!
//! 1. `CreateSnapshot` against a namespace with no compute answers a sentence that is
//!    entirely FIXED — it names no resource, because the resource travels in the modelled
//!    `ResourceNotFoundException$resourceName` member — so the same text comes back
//!    whatever the namespace is called and whichever Region the call is signed for.
//! 2. `CreateUsageLimit` over a `resourceArn` that resolves to nothing answers a sentence
//!    whose ONE variable slot is the id segment the submitted ARN ends in, never the whole
//!    ARN.
//! 3. `CreateWorkgroup` for a `workgroupName` another workgroup already holds answers the
//!    modelled `ConflictException` with the sentence this service words a taken workgroup
//!    name with, whose ONE variable slot is the name the request asked for — however the
//!    rest of the request is shaped, and whatever status the workgroup holding the name has
//!    settled to.
//! 4. `CreateWorkgroup` into a namespace that already has compute answers the same modelled
//!    error with the OTHER sentence of that class, naming the workgroup already associated
//!    with the namespace — so a client reading the text tells the two apart.
//!
//! The harness (emulator, signed call, error-code reader, create helpers) is shared with
//! `namespace_workgroup_lifecycle`, which owns it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::namespace_workgroup_lifecycle::{
    call, call_in, create_namespace, create_workgroup, emulator, error_code, ACCOUNT,
};

/// The fixed sentence this service refuses a snapshot of a workgroupless namespace with.
const NO_WORKGROUP: &str =
    "You can't create a snapshot because there isn't a workgroup created for the namespace.";

// ---------------------------------------------------------------------------
// Rule 1 — the workgroupless-namespace refusal carries no name.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_snapshot_without_compute_refusal_is_one_fixed_sentence() {
    // The rows differ along what the sentence could have interpolated and does not: the
    // namespace name (short, long, digit-laden, hyphenated) and the Region the call is
    // signed for. The snapshot name and the retention period vary with them, so no row
    // repeats another's request.
    let cases: [(&str, &str, &str, Option<i32>); 4] = [
        ("us-east-1", "nsw", "snap-nsw", None),
        (
            "eu-west-2",
            "a-namespace-with-a-rather-long-name-0123456789",
            "snap-long",
            Some(1),
        ),
        ("ap-southeast-2", "ns-42-7", "snap-digits", Some(3653)),
        ("us-west-2", "x-y-z-ns", "snap-hyphens", Some(-1)),
    ];

    let router = emulator();
    for (region, namespace, snapshot, retention) in cases {
        let case = format!("region={region} namespace={namespace}");
        let (status, body) = call_in(
            &router,
            region,
            "CreateNamespace",
            json!({ "namespaceName": namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateNamespace expected 200, got {status} with {body}"
        );

        let mut request = json!({ "namespaceName": namespace, "snapshotName": snapshot });
        if let Some(retention) = retention {
            request["retentionPeriod"] = json!(retention);
        }
        let (status, body) = call_in(&router, region, "CreateSnapshot", request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case}: CreateSnapshot without compute got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(NO_WORKGROUP),
            "case {case}: the refusal is the fixed sentence, got {:?}",
            body["message"]
        );
        assert!(
            !body["message"]
                .as_str()
                .unwrap_or_default()
                .contains(namespace),
            "case {case}: the sentence must not carry the namespace name, got {:?}",
            body["message"]
        );
        // The resource the refusal is about travels in the member modelled for it.
        assert_eq!(
            body["resourceName"],
            json!(namespace),
            "case {case}: resourceName names the namespace, got {:?}",
            body["resourceName"]
        );

        // Attaching compute makes the identical request succeed, so the refusal was about
        // the missing workgroup and nothing else.
        let (status, body) = call_in(
            &router,
            region,
            "CreateWorkgroup",
            json!({ "workgroupName": format!("{namespace}-wg"), "namespaceName": namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateWorkgroup expected 200, got {status} with {body}"
        );
        let (status, body) = call_in(&router, region, "CreateSnapshot", request).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateSnapshot with compute expected 200, got {status} with {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — the unresolvable-resourceArn refusal names the ARN's id segment.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_unresolvable_resource_arn_refusal_names_the_arn_id_segment() {
    let router = emulator();
    let namespace = create_namespace(&router, "wording-ns").await;
    let workgroup = create_workgroup(&router, "wording-wg", "wording-ns").await;
    let real_workgroup_arn = workgroup["workgroupArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let real_namespace_arn = namespace["namespaceArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let real_workgroup_id = workgroup["workgroupId"].as_str().unwrap_or_default();

    // A UUID that names nothing: distinct from anything the emulator minted.
    let absent_id = "3f8c1a7e-5b2d-4c6e-9a10-7d4e2f6b8c90";
    // Every row is an ARN that resolves to no resource of this account in this Region, and
    // they differ along the part that makes them unresolvable: a fresh id, another
    // resource kind, another account, another Region, another partition, and — the rows
    // that carry no `/` at all — a value whose whole text is its own id segment.
    let cases: Vec<(String, String)> = vec![
        (
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:workgroup/{absent_id}"),
            absent_id.to_string(),
        ),
        (
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:namespace/{absent_id}"),
            absent_id.to_string(),
        ),
        (
            format!("arn:aws:redshift-serverless:us-east-1:{ACCOUNT}:snapshot/{absent_id}"),
            absent_id.to_string(),
        ),
        (
            format!(
                "arn:aws:redshift-serverless:us-east-1:999999999999:workgroup/{real_workgroup_id}"
            ),
            real_workgroup_id.to_string(),
        ),
        (
            format!(
                "arn:aws:redshift-serverless:eu-central-1:{ACCOUNT}:workgroup/{real_workgroup_id}"
            ),
            real_workgroup_id.to_string(),
        ),
        (
            format!(
                "arn:aws-us-gov:redshift-serverless:us-gov-west-1:{ACCOUNT}:workgroup/\
                 {real_workgroup_id}"
            ),
            real_workgroup_id.to_string(),
        ),
        ("not-an-arn".to_string(), "not-an-arn".to_string()),
        (String::new(), String::new()),
    ];

    for (resource_arn, id_segment) in &cases {
        let (status, body) = call(
            &router,
            "CreateUsageLimit",
            json!({
                "resourceArn": resource_arn,
                "usageType": "serverless-compute",
                "amount": 60,
                "period": "daily",
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case resourceArn={resource_arn:?}: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!(
                "The Resource ARN is not valid as the workgroup ID {id_segment}."
            )),
            "case resourceArn={resource_arn:?}: got {:?}",
            body["message"]
        );
    }

    // And the ARNs that DO resolve are accepted, so the sentence above is the answer to an
    // ARN that resolves to nothing rather than to this request's shape.
    for (resource_arn, usage_type) in [
        (&real_workgroup_arn, "serverless-compute"),
        (&real_namespace_arn, "cross-region-datasharing"),
    ] {
        let (status, body) = call(
            &router,
            "CreateUsageLimit",
            json!({
                "resourceArn": resource_arn,
                "usageType": usage_type,
                "amount": 60,
                "period": "daily",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case resourceArn={resource_arn:?}: expected 200, got {status} with {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rules 3 and 4 — the two CreateWorkgroup conflicts are told apart by their text.
// ---------------------------------------------------------------------------

/// The sentence this service refuses a `workgroupName` another workgroup already holds with:
/// fixed text either side of the ONE slot, which carries the name the request asked for.
fn workgroup_taken(workgroup_name: &str) -> String {
    format!("The workgroup with name {workgroup_name} already exists.")
}

/// The sentence this service refuses a create for a namespace that already has compute with:
/// fixed text either side of the ONE slot, which carries the workgroup already associated
/// with the namespace.
fn namespace_has_compute(resident_workgroup_name: &str) -> String {
    format!("There is already a workgroup {resident_workgroup_name} associated with the namespace.")
}

/// `CreateWorkgroup` with `workgroup` in `namespace`, carrying every member of `extra`.
async fn create_workgroup_request(
    router: &MultiServiceRouter,
    region: &str,
    workgroup: &str,
    namespace: &str,
    extra: &Value,
) -> (u16, Value) {
    let mut request = json!({ "workgroupName": workgroup, "namespaceName": namespace });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    call_in(router, region, "CreateWorkgroup", request).await
}

/// The workgroup `workgroup`, read back once the status transition the clock owes it has
/// fallen due. The transition is owed by the CLOCK rather than by any call, so the condition
/// is polled for — with a deadline generous enough for a loaded host — rather than waited out.
async fn settled_workgroup(router: &MultiServiceRouter, region: &str, workgroup: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = call_in(
            router,
            region,
            "GetWorkgroup",
            json!({ "workgroupName": workgroup }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetWorkgroup {workgroup}: expected 200, got {status} with {body}"
        );
        if body["workgroup"]["status"] == json!("AVAILABLE") {
            return body["workgroup"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "GetWorkgroup {workgroup}: never reported AVAILABLE, last status {:?}",
            body["workgroup"]["status"]
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_taken_workgroup_name_is_refused_with_the_name_conflict_sentence() {
    // The rows differ along what the sentence interpolates and what the refused create asks
    // for, neither of which the wording depends on: the workgroup's name (the shortest the
    // `WorkgroupName` pattern admits, a long one, a digit-laden one, a hyphenated one), the
    // Region the call is signed for, and the members the second create names.
    let cases: [(&str, &str, &str, Value); 4] = [
        ("us-east-1", "taken-ns-a", "wg1", json!({})),
        (
            "eu-west-2",
            "taken-ns-b",
            "a-workgroup-with-a-rather-long-name-0123456789",
            json!({ "baseCapacity": 32 }),
        ),
        (
            "ap-southeast-2",
            "taken-ns-c",
            "wg-42-7",
            json!({ "publiclyAccessible": true, "port": 8191 }),
        ),
        (
            "us-west-2",
            "taken-ns-d",
            "x-y-z-wg",
            json!({ "baseCapacity": 256, "maxCapacity": 512 }),
        ),
    ];

    let router = emulator();
    for (region, namespace, workgroup, second) in cases {
        let case = format!("region={region} workgroup={workgroup}");
        let (status, body) = call_in(
            &router,
            region,
            "CreateNamespace",
            json!({ "namespaceName": namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateNamespace expected 200, got {status} with {body}"
        );
        let (status, body) = create_workgroup_request(
            &router,
            region,
            workgroup,
            namespace,
            &json!({ "baseCapacity": 8 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateWorkgroup expected 200, got {status} with {body}"
        );

        // The name is taken from the moment the workgroup exists — while it is still
        // settling …
        let (status, body) =
            create_workgroup_request(&router, region, workgroup, namespace, &second).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (409, "ConflictException"),
            "case {case} while the workgroup is settling: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(workgroup_taken(workgroup)),
            "case {case} while the workgroup is settling: got {:?}",
            body["message"]
        );

        // … and it is still taken once the clock has settled that workgroup to AVAILABLE.
        let settled = settled_workgroup(&router, region, workgroup).await;
        let (status, body) =
            create_workgroup_request(&router, region, workgroup, namespace, &second).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (409, "ConflictException"),
            "case {case} once the workgroup is AVAILABLE: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(workgroup_taken(workgroup)),
            "case {case} once the workgroup is AVAILABLE: got {:?}",
            body["message"]
        );

        // A workgroup's name is its identity in the account, so the same name is refused for
        // a namespace that has no compute at all — the name conflict, not the one rule 4
        // asserts.
        let other_namespace = format!("{namespace}-other");
        let (status, body) = call_in(
            &router,
            region,
            "CreateNamespace",
            json!({ "namespaceName": other_namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateNamespace {other_namespace} expected 200, got {status} \
             with {body}"
        );
        let (status, body) =
            create_workgroup_request(&router, region, workgroup, &other_namespace, &second).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (409, "ConflictException"),
            "case {case} in another namespace: got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(workgroup_taken(workgroup)),
            "case {case} in another namespace: got {:?}",
            body["message"]
        );

        // None of the refusals wrote anything: the workgroup still reads back exactly as it
        // was created, the refused request's members nowhere in it.
        let read_back = settled_workgroup(&router, region, workgroup).await;
        assert_eq!(
            read_back, settled,
            "case {case}: a refused create left the workgroup as it was, got {read_back:?}"
        );
        assert_eq!(
            read_back["baseCapacity"],
            json!(8),
            "case {case}: baseCapacity is the accepted create's, got {:?}",
            read_back["baseCapacity"]
        );
    }
}

#[tokio::test]
async fn a_namespace_that_already_has_compute_is_refused_with_the_association_sentence() {
    // The rows differ along the resident workgroup's name (which the sentence carries), the
    // name the refused create asks for, the Region, and the members that create names.
    let cases: [(&str, &str, &str, &str, Value); 4] = [
        ("us-east-1", "resident-ns-a", "wg2", "second-wg", json!({})),
        (
            "eu-west-2",
            "resident-ns-b",
            "a-resident-workgroup-with-a-rather-long-name-0123",
            "other-wg",
            json!({ "baseCapacity": 32 }),
        ),
        (
            "ap-southeast-2",
            "resident-ns-c",
            "wg-7-11",
            "wg-7-12",
            json!({ "publiclyAccessible": true, "port": 5431 }),
        ),
        (
            "us-west-2",
            "resident-ns-d",
            "zz-resident-wg",
            "aa-new-wg",
            json!({ "baseCapacity": 256, "maxCapacity": 512 }),
        ),
    ];

    let router = emulator();
    for (region, namespace, resident, wanted, extra) in cases {
        let case = format!("region={region} resident={resident} wanted={wanted}");
        let (status, body) = call_in(
            &router,
            region,
            "CreateNamespace",
            json!({ "namespaceName": namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateNamespace expected 200, got {status} with {body}"
        );
        let (status, body) =
            create_workgroup_request(&router, region, resident, namespace, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateWorkgroup {resident} expected 200, got {status} with {body}"
        );

        // The namespace has its compute from the moment that workgroup exists, and still has
        // it once the clock has settled the workgroup to AVAILABLE.
        for when in ["while the resident workgroup is settling", "settled"] {
            if when == "settled" {
                settled_workgroup(&router, region, resident).await;
            }
            let (status, body) =
                create_workgroup_request(&router, region, wanted, namespace, &extra).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (409, "ConflictException"),
                "case {case} ({when}): got {status} with {body}"
            );
            assert_eq!(
                body["message"],
                json!(namespace_has_compute(resident)),
                "case {case} ({when}): got {:?}",
                body["message"]
            );
            // The refused create stored nothing: the name it asked for resolves to no
            // workgroup.
            let (status, body) = call_in(
                &router,
                region,
                "GetWorkgroup",
                json!({ "workgroupName": wanted }),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case} ({when}): the refused name resolves to nothing, got {status} \
                 with {body}"
            );
        }

        // Freeing the namespace's compute makes the identical request succeed, so the refusal
        // was about the resident workgroup and nothing else.
        let (status, body) = call_in(
            &router,
            region,
            "DeleteWorkgroup",
            json!({ "workgroupName": resident }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteWorkgroup {resident} expected 200, got {status} with {body}"
        );
        let (status, body) =
            create_workgroup_request(&router, region, wanted, namespace, &extra).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateWorkgroup {wanted} once the namespace is free expected 200, \
             got {status} with {body}"
        );
        assert_eq!(
            body["workgroup"]["namespaceName"],
            json!(namespace),
            "case {case}: the workgroup is created in the namespace asked for, got {:?}",
            body["workgroup"]["namespaceName"]
        );
    }
}
