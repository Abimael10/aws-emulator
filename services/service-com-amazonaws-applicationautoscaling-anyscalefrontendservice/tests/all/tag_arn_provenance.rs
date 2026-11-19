// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal the ARN-addressed tagging family answers a `ResourceARN` that names no
//! registered scalable target with, and what decides between the two.
//!
//! RULE under test: `TagResource`, `UntagResource` and `ListTagsForResource` address a scalable
//! target by the ARN this service minted for it, so what a `ResourceARN` naming no REGISTERED
//! target earns is decided by WHICH ENDPOINT that ARN names, not by the lookup miss both cases
//! share:
//!
//! * an ARN of the partition, account and Region serving the request names a scalable target of
//!   this endpoint, and the answer is that there is none — the resource is reported ABSENT, by
//!   the modelled `ResourceNotFoundException`
//!   (`smithy_query(query="operation",operation="TagResource,UntagResource,ListTagsForResource")`
//!   declares it on all three), the fixed sentence "Scalable Target not found" and the ARN
//!   asked about in the shape's own `ResourceName` member. Whether this service ever minted
//!   that ARN does not enter into it;
//! * an ARN of another partition, account or Region names no resource this endpoint could hold,
//!   so the tagging
//!   call is refused without the service speaking about the resource at all: the modelled
//!   `ValidationException`'s code at its 400, and the fixed sentence "Access Denied", which
//!   carries nothing of the ARN, the account or the Region asked about.
//!
//! The model lists both errors on the mutations but says which covers which condition for
//! neither.
//!
//! The rows span what the rule generalises over: the OPERATION (the read and both mutations,
//! each with payloads of its own) crossed with the endpoint the ARN names — several ways an ARN
//! can name no registered target here (including one of exactly the form this service mints, so
//! the rule cannot be read off an id's shape), and several targets whose ARNs this Region
//! minted and has since retired. A registered target stands beside every row and round-trips a
//! tag through the same three operations, so no row passes because the family refuses
//! everything.

use serde_json::{json, Value};

use crate::emulator::{error_code, error_message, Emulator, ACCOUNT, REGION};

/// The sentence an ARN of this endpoint that names no registered target earns. Written out
/// here rather than read off the code under test: the WORDING is part of the rule.
const TARGET_NOT_FOUND: &str = "Scalable Target not found";

/// The code that answer carries.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole sentence an ARN of another endpoint earns — fixed text, interpolating nothing
/// from the request or from state.
const ACCESS_DENIED: &str = "Access Denied";

/// The code that answer carries.
const VALIDATION: &str = "ValidationException";

/// The status both are served on: the client-error status a request that will not be served
/// carries.
const CLIENT_ERROR: u16 = 400;

/// One request per operation of the family, all three addressed to `resource_arn` with a
/// payload of its own: the answer is about the resource the ARN names, so neither the tags
/// written nor the keys removed change it. Every key and value here is a legal one, so no row
/// is refused for its payload.
fn family(resource_arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "TagResource",
            json!({"ResourceARN": resource_arn, "Tags": {"env": "sim2real"}}),
        ),
        (
            "TagResource",
            json!({
                "ResourceARN": resource_arn,
                "Tags": {"env": "sim2real", "owner": "orders-team", "cost-center": "1234"},
            }),
        ),
        (
            "UntagResource",
            json!({"ResourceARN": resource_arn, "TagKeys": ["env"]}),
        ),
        (
            "UntagResource",
            json!({"ResourceARN": resource_arn, "TagKeys": ["env", "owner"]}),
        ),
        (
            "UntagResource",
            json!({"ResourceARN": resource_arn, "TagKeys": []}),
        ),
        ("ListTagsForResource", json!({"ResourceARN": resource_arn})),
    ]
}

/// ARNs that satisfy the modelled `@pattern` on `ResourceARN`
/// (`^arn:.+:application-autoscaling:.+:[0-9]+:scalable-target\/[a-zA-Z0-9-]+$`), so each one
/// reaches the handler rather than the boundary validator, and that name no registered target.
///
/// `live` is the ARN of a target that IS registered here, which the last rows vary one field of.
/// The flag is whether the ARN names THIS endpoint — its partition, account and Region — which
/// is what decides which refusal it earns: an ARN of the very form this service mints, differing
/// from a minted one only in not having been minted, earns the ABSENT answer just as a retired
/// one does, which is what tells this rule apart from one keyed on an id's shape or on its
/// provenance.
fn unresolvable(live: &str) -> Vec<(String, String, bool)> {
    let twin = {
        let mut twin = live.to_string();
        twin.truncate(live.len() - 4);
        twin.push_str("abcd");
        twin
    };
    vec![
        (
            "an id this service never minted".to_string(),
            format!(
                "arn:aws:application-autoscaling:{REGION}:{ACCOUNT}:scalable-target/\
                 0123456789abcdef0123456789abcdef0123"
            ),
            true,
        ),
        (
            "an id in another form".to_string(),
            format!(
                "arn:aws:application-autoscaling:{REGION}:{ACCOUNT}:scalable-target/\
                 00000000-0000-0000-0000-000000000000"
            ),
            true,
        ),
        (
            "the shortest id the pattern admits".to_string(),
            format!("arn:aws:application-autoscaling:{REGION}:{ACCOUNT}:scalable-target/a"),
            true,
        ),
        (
            "an ARN of another account".to_string(),
            format!(
                "arn:aws:application-autoscaling:{REGION}:111122223333:scalable-target/\
                 0123456789abcdef0123456789abcdef0123"
            ),
            false,
        ),
        (
            "an ARN of another Region".to_string(),
            format!(
                "arn:aws:application-autoscaling:eu-west-1:{ACCOUNT}:scalable-target/\
                 0123456789abcdef0123456789abcdef0123"
            ),
            false,
        ),
        (
            "an ARN of another partition".to_string(),
            format!(
                "arn:aws-us-gov:application-autoscaling:us-gov-west-1:{ACCOUNT}:scalable-target/\
                 0123456789abcdef0123456789abcdef0123"
            ),
            false,
        ),
        (
            "a registered target's ARN with another account in it".to_string(),
            live.replace(ACCOUNT, "111122223333"),
            false,
        ),
        (
            "a registered target's ARN with another Region in it".to_string(),
            live.replace(REGION, "eu-west-1"),
            false,
        ),
        (
            "an ARN of the form this service mints, that it did not mint".to_string(),
            twin,
            true,
        ),
    ]
}

/// Assert `body` is the refusal of an ARN of another endpoint: the access-denied sentence,
/// stating nothing about the resource asked about.
fn assert_access_denied(case: &str, status: u16, body: &Value, arn: &str) {
    assert_eq!(
        status, CLIENT_ERROR,
        "case {case:?}: expected HTTP {CLIENT_ERROR}, got {status} with {body:?}"
    );
    let code = error_code(body);
    assert_eq!(
        code, VALIDATION,
        "case {case:?}: expected {VALIDATION}, got {code:?} in {body:?}"
    );
    let message = error_message(body);
    assert_eq!(
        message, ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED:?}, got {message:?}"
    );
    // The service discloses nothing about a resource the caller does not reach: neither the ARN
    // asked about nor any part of it is reported back, in the sentence or in a member of its
    // own.
    assert!(
        body.get("ResourceName").is_none(),
        "case {case:?}: expected no resource to be named in the refusal, got {body:?}"
    );
    let id = arn.rsplit('/').next().unwrap_or(arn);
    assert!(
        !message.contains(arn) && !message.contains(id) && !message.contains("scalable-target"),
        "case {case:?}: expected the sentence to carry nothing of the ARN, got {message:?}"
    );
}

/// Assert `body` reports the resource `arn` names as absent.
fn assert_target_not_found(case: &str, status: u16, body: &Value, arn: &str) {
    assert_eq!(
        status, CLIENT_ERROR,
        "case {case:?}: expected HTTP {CLIENT_ERROR}, got {status} with {body:?}"
    );
    let code = error_code(body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND,
        "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {code:?} in {body:?}"
    );
    let message = error_message(body);
    assert_eq!(
        message, TARGET_NOT_FOUND,
        "case {case:?}: expected {TARGET_NOT_FOUND:?}, got {message:?}"
    );
    // The ARN asked about is reported in the modelled member rather than spliced into the
    // sentence, which states the condition and nothing else.
    assert_eq!(
        body["ResourceName"],
        Value::String(arn.to_string()),
        "case {case:?}: expected the ARN asked about in ResourceName, got {body:?}"
    );
    assert!(
        !message.contains(arn),
        "case {case:?}: expected the sentence to echo nothing of the ARN, got {message:?}"
    );
}

/// Read the tag set of `arn` back, through the implemented read.
async fn tags_of(emulator: &Emulator, arn: &str, case: &str) -> Value {
    let (status, body) = emulator
        .call("ListTagsForResource", json!({"ResourceARN": arn}))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListTagsForResource to answer 200, got {status} with {body:?}"
    );
    body["Tags"].clone()
}

/// RULE: an ARN that names no registered scalable target is answered by the ENDPOINT it names:
/// one of this account and Region is the resource reported absent, whether this service minted
/// it or not, and one of another account, Region or partition is refused as a tagging call the
/// service will not serve — the access-denied sentence, disclosing nothing about whether such a
/// resource exists. Every operation of the family answers alike, and neither refusal writes
/// anything.
#[tokio::test]
async fn an_unresolvable_arn_is_answered_by_the_endpoint_it_names() {
    let emulator = Emulator::empty();

    // One target that stays registered, so each row below is about the ARN it names.
    let resident = emulator
        .given_target("dynamodb:table:ReadCapacityUnits", 1, 10)
        .await;
    let resident_arn = emulator.target_arn(&resident).await;
    let (status, body) = emulator
        .call(
            "TagResource",
            json!({"ResourceARN": resident_arn, "Tags": {"env": "sim2real"}}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"tag the resident target\": expected HTTP 200, got {status} with {body:?}"
    );

    for (reason, arn, served_here) in unresolvable(&resident_arn) {
        assert_ne!(
            arn, resident_arn,
            "case {reason:?}: expected a row that is not the registered target's own ARN"
        );
        for (operation, request) in family(&arn) {
            let case = format!("{reason} on {operation}");
            let (status, body) = emulator.call(operation, request).await;
            if served_here {
                assert_target_not_found(&case, status, &body, &arn);
            } else {
                assert_access_denied(&case, status, &body, &arn);
            }
        }
    }

    // None of those refusals touched the store: the target that IS registered still carries the
    // tag written before them, and still gives it up again — so no row passed because the
    // family refuses everything.
    let stored = tags_of(&emulator, &resident_arn, "the resident target's tags").await;
    assert_eq!(
        stored,
        json!({"env": "sim2real"}),
        "case \"the resident target's tags\": expected the refusals to change nothing, got \
         {stored:?}"
    );
    let (status, body) = emulator
        .call(
            "UntagResource",
            json!({"ResourceARN": resident_arn, "TagKeys": ["env"]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"untag the resident target\": expected HTTP 200, got {status} with {body:?}"
    );
    let stored = tags_of(&emulator, &resident_arn, "the emptied tag set").await;
    assert_eq!(
        stored,
        json!({}),
        "case \"the emptied tag set\": expected an empty tag set, got {stored:?}"
    );
}

/// RULE: an ARN this Region MINTED, for a target it no longer holds, is the resource reported
/// absent by every operation of the family — and the refusal does not revive the target.
#[tokio::test]
async fn an_arn_minted_for_a_target_that_is_gone_reports_the_absent_resource() {
    let emulator = Emulator::empty();

    // Several targets, each registered and then deregistered, so the rows differ in the target
    // whose ARN was retired rather than in one identity's spelling. One stays registered
    // beside them.
    let resident = emulator
        .given_target("dynamodb:table:ReadCapacityUnits", 1, 10)
        .await;
    let resident_arn = emulator.target_arn(&resident).await;

    let mut retired: Vec<(String, String)> = Vec::new();
    for dimension in [
        "dynamodb:table:WriteCapacityUnits",
        "ecs:service:DesiredCount",
        "dynamodb:index:ReadCapacityUnits",
    ] {
        let target = emulator.given_target(dimension, 1, 10).await;
        let arn = emulator.target_arn(&target).await;
        // The ARN answers while the target is registered: what changes below is the target's
        // existence, not the ARN's readability.
        let (status, body) = emulator
            .call("ListTagsForResource", json!({"ResourceARN": arn}))
            .await;
        assert_eq!(
            status, 200,
            "case {dimension:?}: expected ListTagsForResource to answer 200 while the target is \
             registered, got {status} with {body:?}"
        );
        let (status, body) = emulator
            .call("DeregisterScalableTarget", target.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {dimension:?}: expected DeregisterScalableTarget to answer 200, got {status} \
             with {body:?}"
        );
        retired.push((format!("the retired ARN of a {dimension} target"), arn));
    }

    for (reason, arn) in &retired {
        for (operation, request) in family(arn) {
            let case = format!("{reason} on {operation}");
            let (status, body) = emulator.call(operation, request).await;
            assert_target_not_found(&case, status, &body, arn);
        }
    }

    // A refused tag call creates nothing: the deregistered identities are still unregistered,
    // and the resident target still answers.
    let (status, body) = emulator
        .call(
            "DescribeScalableTargets",
            json!({"ServiceNamespace": "ecs"}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the deregistered namespace\": expected HTTP 200, got {status} with {body:?}"
    );
    assert_eq!(
        body["ScalableTargets"],
        json!([]),
        "case \"the deregistered namespace\": expected the refusals to revive no target, got \
         {body:?}"
    );
    let stored = tags_of(&emulator, &resident_arn, "the resident target's tags").await;
    assert_eq!(
        stored,
        json!({}),
        "case \"the resident target's tags\": expected the untagged target to still answer, got \
         {stored:?}"
    );
}

/// RULE: an endpoint answers for the scalable targets of its own Region, so an ARN of another
/// Region is one it does not serve, before and after that
/// target is deregistered.
#[tokio::test]
async fn an_arn_is_served_only_by_the_region_it_names() {
    // Region names are opaque to the rule, so the rows differ in the shape of the pair: two
    // Regions of one geography, and a pair whose home is not the Region an unscoped request
    // falls back to.
    for (case, home, other) in [
        ("two Regions of one geography", "us-east-1", "us-west-2"),
        ("a home Region that is not the fallback one", "sa-east-1", "us-east-1"),
    ] {
        let emulator = Emulator::empty();
        let (status, body) = emulator
            .call_dynamodb_in(
                home,
                "CreateTable",
                json!({
                    "TableName": "catalog",
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "BillingMode": "PROVISIONED",
                    "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateTable in {home} to answer 200, got {status} with \
             {body:?}"
        );
        let target = json!({
            "ServiceNamespace": "dynamodb",
            "ResourceId": "table/catalog",
            "ScalableDimension": "dynamodb:table:WriteCapacityUnits",
        });
        let mut registration = target.clone();
        registration["MinCapacity"] = json!(1);
        registration["MaxCapacity"] = json!(10);
        let (status, body) = emulator
            .call_in(home, "RegisterScalableTarget", registration)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RegisterScalableTarget in {home} to answer 200, got \
             {status} with {body:?}"
        );
        let arn = body["ScalableTargetARN"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // The Region that minted it serves the tag write; the other Region does not serve it.
        let (status, body) = emulator
            .call_in(
                home,
                "TagResource",
                json!({"ResourceARN": arn, "Tags": {"env": "sim2real"}}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected TagResource in {home} to answer 200, got {status} with \
             {body:?}"
        );
        for (operation, request) in family(&arn) {
            let (status, body) = emulator.call_in(other, operation, request).await;
            assert_access_denied(
                &format!("{case}: a {home} ARN asked at {other} on {operation}"),
                status,
                &body,
                &arn,
            );
        }

        // Deregistering it in its own Region stops it resolving THERE: that Region reports the
        // resource absent, and the other Region still does not serve it.
        let (status, body) = emulator
            .call_in(home, "DeregisterScalableTarget", target.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeregisterScalableTarget in {home} to answer 200, got \
             {status} with {body:?}"
        );
        for (operation, request) in family(&arn) {
            let (status, body) = emulator.call_in(home, operation, request.clone()).await;
            assert_target_not_found(
                &format!("{case}: the retired ARN asked at {home} on {operation}"),
                status,
                &body,
                &arn,
            );
            let (status, body) = emulator.call_in(other, operation, request).await;
            assert_access_denied(
                &format!("{case}: the retired ARN asked at {other} on {operation}"),
                status,
                &body,
                &arn,
            );
        }
    }
}

/// RULE: what this endpoint HOLDS decides, so an id of this account and Region that no
/// registered target answers to is the resource reported absent whatever form that id takes and
/// whoever wrote it — the same answer an ARN this endpoint issued earns once the target it named
/// is gone.
///
/// The rows differ along the id's FORM, which is what a rule keyed on an identifier's shape
/// would split on: the form this service's own mint draws from, a UUID, the shortest id the
/// modelled pattern admits, and a long hyphenated name.
#[tokio::test]
async fn an_id_this_endpoint_never_issued_reports_the_target_absent() {
    let emulator = Emulator::empty();

    // A registered target stands beside the rows, so a refusal below is about the ARN it names
    // and not about the family refusing everything.
    let resident = emulator
        .given_target("dynamodb:table:ReadCapacityUnits", 1, 10)
        .await;
    let resident_arn = emulator.target_arn(&resident).await;
    let (status, body) = emulator
        .call(
            "TagResource",
            json!({"ResourceARN": resident_arn, "Tags": {"env": "sim2real"}}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"tag the resident target\": expected HTTP 200, got {status} with {body:?}"
    );

    for (case, id) in [
        (
            "an id of the form this service mints",
            "0123456789abcdef0123456789abcdef0123",
        ),
        ("a UUID", "00000000-0000-0000-0000-000000000000"),
        ("the shortest id the pattern admits", "a"),
        (
            "a long hyphenated name",
            "probe-scalable-target-that-was-never-registered-0000",
        ),
    ] {
        let arn =
            format!("arn:aws:application-autoscaling:{REGION}:{ACCOUNT}:scalable-target/{id}");
        assert_ne!(
            arn, resident_arn,
            "case {case:?}: expected a row that is not the registered target's own ARN"
        );
        for (operation, request) in family(&arn) {
            let (status, body) = emulator.call(operation, request).await;
            assert_target_not_found(&format!("{case} on {operation}"), status, &body, &arn);
        }
    }

    // None of those refusals touched the store: the resident target still carries the tag
    // written before them.
    let stored = tags_of(&emulator, &resident_arn, "the resident target's tags").await;
    assert_eq!(
        stored,
        json!({"env": "sim2real"}),
        "case \"the resident target's tags\": expected the refusals to change nothing, got \
         {stored:?}"
    );
}

/// RULE: registering a deregistered identity again is a NEW resource with a NEW ARN, and the
/// superseded ARN keeps reporting the resource it names as absent — the store, not the ARN's
/// history, is what decides.
#[tokio::test]
async fn a_superseded_arn_still_reports_the_target_absent() {
    let emulator = Emulator::empty();
    // Several identities, so the rule is not read off one dimension's spelling.
    for dimension in [
        "dynamodb:table:WriteCapacityUnits",
        "ecs:service:DesiredCount",
        "dynamodb:index:ReadCapacityUnits",
    ] {
        let target = emulator.given_target(dimension, 1, 10).await;
        let first = emulator.target_arn(&target).await;
        let (status, body) = emulator
            .call("DeregisterScalableTarget", target.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {dimension:?}: expected DeregisterScalableTarget to answer 200, got {status} \
             with {body:?}"
        );

        let mut registration = target.clone();
        registration["MinCapacity"] = json!(1);
        registration["MaxCapacity"] = json!(10);
        let (status, body) = emulator
            .call("RegisterScalableTarget", registration)
            .await;
        assert_eq!(
            status, 200,
            "case {dimension:?}: expected the re-registration to answer 200, got {status} with \
             {body:?}"
        );
        let reissued = emulator.target_arn(&target).await;
        assert_ne!(
            reissued, first,
            "case {dimension:?}: expected the re-registration to mint a new ARN"
        );

        // The new ARN answers, and every operation of the family reports the superseded one's
        // resource as absent.
        let (status, body) = emulator
            .call("ListTagsForResource", json!({"ResourceARN": reissued}))
            .await;
        assert_eq!(
            status, 200,
            "case {dimension:?}: expected the re-registered target to answer 200, got {status} \
             with {body:?}"
        );
        for (operation, request) in family(&first) {
            let (status, body) = emulator.call(operation, request).await;
            assert_target_not_found(
                &format!("the superseded ARN of {dimension} on {operation}"),
                status,
                &body,
                &first,
            );
        }
    }
}
