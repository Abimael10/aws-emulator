// SPDX-License-Identifier: CC0-1.0

//! How this service classifies a value a `resourceArns` member hands it.
//!
//! The rule the rows below pin: a value taken as the ARN of a resource this service
//! shares is classified by KIND before any store is read — a value that is no ARN, and
//! an ARN of a resource type this service cannot share, are both refused with
//! `MalformedArnException`, the second with the fixed text AWS words that refusal with,
//! which names nothing from the request. Only an entry that passes reaches the state
//! question — does a share of this account hold this resource? — which
//! `GetResourcePolicies` answers with `ResourceArnNotFoundException` and the mutations
//! with their own refusals.
//!
//! So the rows vary along what the rule generalises over:
//!
//! * WHICH kind of ARN is named: one of this service's own control-plane resources (a
//!   share, a permission, an invitation), a resource of a service that shares nothing,
//!   and a resource of a service that shares OTHER types than the one named;
//! * HOW a shareable ARN spells its type: leading the resource field, nested in it,
//!   the whole field, hyphenated against the published spelling, with and without a
//!   Region, in another account — and every type the catalog publishes;
//! * WHERE the entry sits in the list, and whether the store holds the named entity
//!   anyway;
//! * WHICH operation takes the member: the read of a resource's policies and the three
//!   mutations that associate resources with a share.

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::RESOURCE_TYPES;
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

/// The text AWS answers with when an ARN names a resource type RAM cannot share. Fixed:
/// it interpolates no ARN, id, account or Region.
const NOT_SHAREABLE: &str = "You cannot share the selected resource type.";

/// An account outside the one that owns the shares, so a principal in it is external
/// and the resource it reaches carries an instantiated policy.
const FOREIGN_ACCOUNT: &str = "111122223333";

/// One row of the classification table: an ARN, and what naming it means.
struct ArnCase {
    case: &'static str,
    arn: String,
}

async fn get_policies(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getresourcepolicies", Some(body)).await
}

/// ARNs of kinds this service cannot share, one per reason it cannot.
fn non_shareable_arns() -> Vec<ArnCase> {
    vec![
        ArnCase {
            case: "this service's own resource-share resource",
            arn: format!(
                "arn:aws:ram:{REGION}:{ACCOUNT}:resource-share/f581604e-e6be-88ab-82f1-c82793469f1c"
            ),
        },
        ArnCase {
            case: "this service's own managed-permission resource",
            arn: "arn:aws:ram::aws:permission/AWSRAMDefaultPermissionSubnet".to_string(),
        },
        ArnCase {
            case: "this service's own invitation resource",
            arn: format!(
                "arn:aws:ram:{REGION}:{ACCOUNT}:resource-share-invitation/\
                 00000000-0000-4000-8000-000000000000"
            ),
        },
        ArnCase {
            case: "an identity rather than a resource",
            arn: format!("arn:aws:iam::{ACCOUNT}:role/Reader"),
        },
        ArnCase {
            case: "a resource of a service that shares nothing",
            arn: "arn:aws:s3:::shared-bucket".to_string(),
        },
        ArnCase {
            case: "a queue, whose resource field is the name alone",
            arn: format!("arn:aws:sqs:{REGION}:{ACCOUNT}:orders"),
        },
        ArnCase {
            case: "a type a sharing service does not share",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d4e5f60718"),
        },
        ArnCase {
            case: "another type a sharing service does not share",
            arn: format!("arn:aws:glue:{REGION}:{ACCOUNT}:connection/warehouse"),
        },
        ArnCase {
            case: "a shareable resource code under the wrong service",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:database/analytics"),
        },
    ]
}

/// ARNs of resource types this service shares, one per form a resource ARN takes.
/// Every type named here is one `RESOURCE_TYPES` publishes — the catalog
/// `ListResourceTypes` reports.
fn shareable_arns() -> Vec<ArnCase> {
    vec![
        ArnCase {
            case: "the resource code leads the resource field",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-1111"),
        },
        ArnCase {
            case: "the resource code is hyphenated where the catalog spells it in caps",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:prefix-list/pl-0a1b2c3d4e5f60718"),
        },
        ArnCase {
            case: "the resource field is the resource code alone",
            arn: format!("arn:aws:glue:{REGION}:{ACCOUNT}:catalog"),
        },
        ArnCase {
            case: "the resource field nests the id under the code",
            arn: format!("arn:aws:glue:{REGION}:{ACCOUNT}:table/analytics/events"),
        },
        ArnCase {
            case: "the resource code is nested under another resource",
            arn: format!(
                "arn:aws:s3-outposts:{REGION}:{ACCOUNT}:outpost/op-0a1b2c3d/bucket/reports"
            ),
        },
        ArnCase {
            case: "the service code carries a hyphen",
            arn: format!(
                "arn:aws:license-manager:{REGION}:{ACCOUNT}:license-configuration/lic-0a1b2c3d"
            ),
        },
        ArnCase {
            case: "the resource field carries a version path",
            arn: format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:component/hardening/1.0.0/1"),
        },
        ArnCase {
            case: "the ARN names no Region",
            arn: format!(
                "arn:aws:networkmanager::{ACCOUNT}:global-network/global-network-01231231231231231"
            ),
        },
        ArnCase {
            case: "the ARN names another account",
            arn: format!("arn:aws:vpc-lattice:{REGION}:111122223333:servicenetwork/sn-0a1b2c3d"),
        },
        ArnCase {
            case: "a type whose service shares several",
            arn: format!(
                "arn:aws:route53resolver:{REGION}:{ACCOUNT}:resolver-rule/rslvr-rr-0a1b2c3d"
            ),
        },
    ]
}

/// Assert `body` is the kind refusal: 400, `MalformedArnException`, and the fixed text.
fn assert_not_shareable(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        "MalformedArnException",
        "case {case:?}: expected the ARN-kind refusal, got {body:?}"
    );
    assert_eq!(
        error_message(body),
        NOT_SHAREABLE,
        "case {case:?}: expected the fixed non-shareable text, got {:?}",
        error_message(body)
    );
}

#[tokio::test]
async fn a_policy_read_refuses_an_arn_of_a_type_this_service_cannot_share() {
    let router = emulator();
    for row in non_shareable_arns() {
        let (status, body) = get_policies(&router, &json!({"resourceArns": [row.arn]})).await;
        assert_not_shareable(row.case, status, &body);
    }
}

#[tokio::test]
async fn a_policy_read_sends_a_shareable_arn_on_to_the_state_lookup() {
    let router = emulator();
    for row in shareable_arns() {
        let (status, body) = get_policies(&router, &json!({"resourceArns": [row.arn]})).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status} with {body:?}",
            row.case
        );
        // Nothing shares it, so the refusal is the state one — and it names the ARN,
        // which the kind refusal never does.
        assert_eq!(
            error_code(&body),
            "ResourceArnNotFoundException",
            "case {:?}: expected the state refusal, got {body:?}",
            row.case
        );
        assert!(
            error_message(&body).contains(&row.arn),
            "case {:?}: expected the message to name {:?}, got {:?}",
            row.case,
            row.arn,
            error_message(&body)
        );
    }
}

#[tokio::test]
async fn the_kind_refusal_does_not_depend_on_what_the_store_holds_or_on_list_position() {
    let router = emulator();
    let shared = format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-1111");
    // A live share of a resource this service can share, plus an external identity, so
    // the read really does report a document for that resource.
    let created = create_ok(
        &router,
        "a live share",
        &json!({
            "name": "classification",
            "resourceArns": [shared.clone()],
            "principals": [FOREIGN_ACCOUNT],
        }),
    )
    .await;
    let share_arn = arn_of("a live share", &created);

    let (status, body) = get_policies(&router, &json!({"resourceArns": [shared.clone()]})).await;
    assert_eq!(
        status, 200,
        "case \"the shared resource\": expected 200, got {status} with {body:?}"
    );
    assert_eq!(
        body.get("policies").and_then(Value::as_array).map(Vec::len),
        Some(1),
        "case \"the shared resource\": expected one document, got {body:?}"
    );

    // The share's own ARN is an entity the store HOLDS, and the answer is still the
    // kind refusal: the classification asks what the ARN is, not what the store has.
    for (case, arns) in [
        ("the stored share's own ARN", vec![share_arn.clone()]),
        (
            "a non-shareable entry after a shareable one",
            vec![shared.clone(), share_arn.clone()],
        ),
        (
            "a non-shareable entry before a shareable one",
            vec![share_arn.clone(), shared.clone()],
        ),
        (
            "the same non-shareable entry named twice",
            vec![share_arn.clone(), share_arn.clone()],
        ),
    ] {
        let (status, body) = get_policies(&router, &json!({"resourceArns": arns})).await;
        assert_not_shareable(case, status, &body);
    }

    // And the read mutated nothing: the shared resource still reports its document.
    let (status, body) = get_policies(&router, &json!({"resourceArns": [shared]})).await;
    assert_eq!(
        (status, body.get("policies").and_then(Value::as_array).map(Vec::len)),
        (200, Some(1)),
        "case \"after the refusals\": the read must still report the document, got {body:?}"
    );
}

#[tokio::test]
async fn the_mutations_that_take_resource_arns_classify_them_the_same_way() {
    let router = emulator();
    let shared = format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-1111");
    let created = create_ok(
        &router,
        "a live share",
        &json!({"name": "mutations", "resourceArns": [shared.clone()]}),
    )
    .await;
    let share_arn = arn_of("a live share", &created);

    for row in non_shareable_arns() {
        let (status, body) = create(
            &router,
            &json!({"name": format!("share-of-{}", row.case), "resourceArns": [row.arn.clone()]}),
        )
        .await;
        assert_not_shareable(&format!("create with {}", row.case), status, &body);

        let (status, body) = associate(
            &router,
            &json!({"resourceShareArn": share_arn, "resourceArns": [row.arn.clone()]}),
        )
        .await;
        assert_not_shareable(&format!("associate {}", row.case), status, &body);

        let (status, body) = disassociate(
            &router,
            &json!({"resourceShareArn": share_arn, "resourceArns": [row.arn.clone()]}),
        )
        .await;
        assert_not_shareable(&format!("disassociate {}", row.case), status, &body);
    }

    // The share the refusals were aimed at still holds exactly what it was created
    // with: a refused mutation changed nothing.
    let (status, body) = get_associations(
        &router,
        &json!({"associationType": "RESOURCE", "resourceShareArns": [share_arn]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the share after the refusals\": expected 200, got {status} with {body:?}"
    );
    let entities: Vec<String> = associations_of("the share after the refusals", &body)
        .iter()
        .map(|association| {
            association
                .get("associatedEntity")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string()
        })
        .collect();
    assert_eq!(
        entities,
        vec![shared.clone()],
        "case \"the share after the refusals\": expected only the resource it was created with"
    );
}

#[tokio::test]
async fn every_form_of_a_shareable_arn_is_accepted_by_the_mutations() {
    let router = emulator();
    let forms: Vec<String> = shareable_arns().into_iter().map(|row| row.arn).collect();
    let created = create_ok(
        &router,
        "every shareable form",
        &json!({"name": "forms", "resourceArns": forms.clone()}),
    )
    .await;
    let share_arn = arn_of("every shareable form", &created);

    // Read back what the share holds: every form was stored, so each one survived the
    // classification and reached the store.
    let listed = listed_resources(&router, "every shareable form", &share_arn).await;
    let mut expected = forms.clone();
    expected.sort();
    assert_eq!(
        listed, expected,
        "case \"every shareable form\": the share must hold every resource it was created with"
    );

    // One of the nested forms taken out and put back: the mutation that removes and the
    // one that adds classify it the same way the create did.
    let nested = forms
        .iter()
        .find(|arn| arn.contains("s3-outposts"))
        .expect("the shareable rows include a nested resource field")
        .clone();
    for (case, body) in [
        (
            "disassociate a nested form",
            json!({"resourceShareArn": share_arn, "resourceArns": [nested.clone()]}),
        ),
        (
            "associate it again",
            json!({"resourceShareArn": share_arn, "resourceArns": [nested.clone()]}),
        ),
    ] {
        let (status, answered) = if case.starts_with("disassociate") {
            disassociate(&router, &body).await
        } else {
            associate(&router, &body).await
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {answered:?}"
        );
    }
    assert_eq!(
        listed_resources(&router, "after the round trip", &share_arn).await,
        expected,
        "case \"after the round trip\": the share must hold every resource again"
    );
}

/// The resources a share holds, sorted, read back through
/// `GetResourceShareAssociations`.
async fn listed_resources(
    router: &MultiServiceRouter,
    case: &str,
    share_arn: &str,
) -> Vec<String> {
    let (status, body) = get_associations(
        router,
        &json!({
            "associationType": "RESOURCE",
            "resourceShareArns": [share_arn],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body:?}"
    );
    let mut entities: Vec<String> = associations_of(case, &body)
        .iter()
        .map(|association| {
            association
                .get("associatedEntity")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string()
        })
        .collect();
    entities.sort();
    entities
}

#[tokio::test]
async fn every_resource_type_the_catalog_publishes_is_one_a_resource_arn_may_name() {
    let router = emulator();
    assert!(
        RESOURCE_TYPES.len() > 1,
        "the catalog must publish more than one resource type, got {}",
        RESOURCE_TYPES.len()
    );
    for (index, entry) in RESOURCE_TYPES.iter().enumerate() {
        let case = entry.resource_type;
        // A resource ARN of that type, built the way the owning service builds one: the
        // service code is the ARN's service field and the resource code the first
        // segment of its resource field.
        let (service, code) = entry
            .resource_type
            .split_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: {case:?} is not a resource type"));
        let arn = format!(
            "arn:aws:{service}:{REGION}:{ACCOUNT}:{}/shared-{index}",
            code.to_ascii_lowercase()
        );

        // A share of it is accepted, and the read of its policies reaches the state
        // lookup rather than the kind refusal.
        let created = create_ok(
            &router,
            case,
            &json!({"name": format!("catalog-{index}"), "resourceArns": [arn.clone()]}),
        )
        .await;
        let share_arn = arn_of(case, &created);
        assert!(
            !share_arn.is_empty(),
            "case {case:?}: a share of {arn:?} must report its ARN"
        );

        let (status, body) = get_policies(&router, &json!({"resourceArns": [arn.clone()]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a share of {arn:?} must have made it readable, got {body:?}"
        );
        // The share added no identity outside the owning account, so the resource
        // carries no instantiated policy — an empty list, not a refusal.
        assert_eq!(
            body.get("policies").and_then(Value::as_array),
            Some(&vec![]),
            "case {case:?}: expected no documents for {arn:?}, got {body:?}"
        );
    }
}
