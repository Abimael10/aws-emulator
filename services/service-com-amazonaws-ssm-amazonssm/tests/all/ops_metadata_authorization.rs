// SPDX-License-Identifier: CC0-1.0

//! WHO MAY CALL the Application Manager OpsMetadata operations: every one of them is decided
//! against the calling principal's identity-based IAM policies before this service resolves
//! anything the request names.
//!
//! RULE under test: **AWS decides a signed call at the IAM layer before the service resolves
//! what the request names, so a call whose caller holds no grant for `ssm:<OperationName>` on
//! the resource it addresses is refused `AccessDeniedException` (HTTP 400) naming the
//! principal, the action, the resource and the rule that withheld the grant, and changes
//! nothing — while a caller a readable document allows is served, and one an explicit `Deny`
//! covers is refused however much else it is allowed.** The resource is the `opsmetadata`
//! object the request NAMES, and every `opsmetadata` object of the Region for the two calls
//! that name none: the create, whose object's ARN does not exist until it mints it, and the
//! listing.
//!
//! Why it matters beyond the refusal itself: a function, a pipeline or an operator registering
//! application metadata runs as a ROLE, and the point of scoping that role's policy is that a
//! call outside the scope fails. An emulator that serves such a call reports success for work
//! AWS refuses, and the caller's own handling of the refusal is never exercised.
//!
//! Every case goes through the front door: `AmazonSSM.<Operation>` over `awsJson1_1`, signed
//! with the access key id of the principal under test, with the roles and their policies made
//! through IAM's own `awsQuery` operations and the sessions minted by STS's `AssumeRole` — so
//! the documents this service reads are the ones those services wrote.
//!
//! The rows differ along what the rule generalises over: WHICH operation is called (the
//! create, the three calls that name one object, and the listing), the KIND of principal (the
//! account's own published credential and assumed-role sessions), what the principal's policy
//! says (nothing, every action of this service, this service's actions on the Region's
//! OpsMetadata objects, another action only, and an explicit deny on top of a grant of
//! everything), and — in the second test — the RESOURCE a grant is scoped to.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::just_in_time_node_access::{assumed_role, call_as, emulator, Principal};
use crate::parameter_store::{call, error_code, error_message, ACCOUNT, REGION};

/// The access key id the account's own published credential presents, which is what the
/// harness signs its setup calls with: neither IAM nor STS holds a record for it, so it
/// resolves to the account's ambient identity, which carries no identity-based policy.
const ACCOUNT_KEY: &str = "AKIDTEST";

/// The ARN of the OpsMetadata object registered for the application `name`, written out here
/// independently of the service: the `opsmetadata` resource of this account, under the path
/// Application Manager mints one in.
fn object_arn(name: &str) -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:opsmetadata/aws/ssm/{name}/opsmetadata")
}

/// The resource a call that names no single object is decided about: every `opsmetadata`
/// resource of the Region, the object path spelled with IAM's own wildcard.
fn every_object() -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:opsmetadata/*")
}

/// What the rule says about one principal's call: it is served, or refused with the rule that
/// withheld the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// A readable document allows the action on the resource (or there is no document the
    /// decision can bound the caller by at all).
    Served,
    /// No readable document states the action: the sentence names the action it was asked
    /// about.
    NoGrant,
    /// A readable document DENIES it explicitly, which refuses the call whatever else the
    /// principal carries.
    ExplicitDeny,
}

/// The sentence a refusal carries for one principal, action and resource — IAM's own wording,
/// with the clause of the rule that withheld the grant at the end.
fn not_authorized(principal: &str, operation: &str, resource: &str, decided: Decided) -> String {
    let action = format!("ssm:{operation}");
    let reason = match decided {
        Decided::NoGrant => format!("because no identity-based policy allows the {action} action"),
        Decided::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Decided::Served => String::new(),
    };
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// One call under test: the operation, the body that calls it, and the resource the decision
/// about it is taken on.
struct Call {
    case: &'static str,
    operation: &'static str,
    body: Value,
    resource: String,
}

/// The five calls of this plane, for a row whose own object is `application` and whose create
/// registers `new_application`.
fn calls(application: &str, new_application: &str) -> Vec<Call> {
    vec![
        Call {
            case: "the create",
            operation: "CreateOpsMetadata",
            body: json!({ "ResourceId": new_application, "Metadata": { "Key": { "Value": "v" } } }),
            resource: every_object(),
        },
        Call {
            case: "the read",
            operation: "GetOpsMetadata",
            body: json!({ "OpsMetadataArn": object_arn(application) }),
            resource: object_arn(application),
        },
        Call {
            case: "the update",
            operation: "UpdateOpsMetadata",
            body: json!({
                "OpsMetadataArn": object_arn(application),
                "MetadataToUpdate": { "Written": { "Value": "by the update" } },
            }),
            resource: object_arn(application),
        },
        Call {
            case: "the listing",
            operation: "ListOpsMetadata",
            body: json!({}),
            resource: every_object(),
        },
        Call {
            case: "the delete",
            operation: "DeleteOpsMetadata",
            body: json!({ "OpsMetadataArn": object_arn(application) }),
            resource: object_arn(application),
        },
    ]
}

/// Register the object a row addresses, with the account's own credential — the caller the
/// decision cannot bound, so the seeding says nothing about the row.
async fn seed(router: &MultiServiceRouter, application: &str) {
    let (status, body) = call(
        router,
        "CreateOpsMetadata",
        &json!({ "ResourceId": format!("/probe/{application}"), "Metadata": { "Seeded": { "Value": "yes" } } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "seeding {application:?}: expected CreateOpsMetadata to succeed, got {status} {body}"
    );
    assert_eq!(
        body["OpsMetadataArn"].as_str().unwrap_or_default(),
        object_arn(application),
        "seeding {application:?}: expected the create to mint the ARN the rows address, got {body}"
    );
}

/// What the object `application` holds, read with the account's own credential: `None` when
/// this Region holds no object under its ARN.
async fn held(router: &MultiServiceRouter, application: &str) -> Option<Value> {
    let (status, body) = call(
        router,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": object_arn(application) }),
    )
    .await;
    (status == 200).then_some(body)
}

/// RULE: every call of this plane is decided against the caller's identity-based policies,
/// about the object it names (or the Region's objects when it names none), and a refused call
/// changes nothing.
#[tokio::test]
async fn every_ops_metadata_call_is_decided_against_the_callers_identity_policies() {
    let router = emulator();

    // Every action of this service on every resource: the grant an administrator carries.
    let allow_every_action = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;
    // The same actions scoped to the Region's OpsMetadata objects, which is what a role
    // scoped to the resources it needs carries.
    let allow_these_objects = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:*","Resource":"{}"}}]}}"#,
        every_object()
    );
    // Another action of this service entirely, which states nothing about these calls.
    let allow_another_action = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:GetParameter","Resource":"*"}]}"#;
    // A grant of everything with an explicit deny of this plane's actions on top of it.
    let deny_the_plane = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"},
        {"Effect":"Deny","Action":["ssm:CreateOpsMetadata","ssm:GetOpsMetadata",
            "ssm:UpdateOpsMetadata","ssm:DeleteOpsMetadata","ssm:ListOpsMetadata"],
         "Resource":"*"}]}"#;

    // (why, the object this row addresses, the principal, what the rule says about it)
    let rows: Vec<(&str, &str, Principal, Decided)> = vec![
        (
            "the account's own published credential, which holds no identity policy",
            "row-account",
            Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: format!("arn:aws:iam::{ACCOUNT}:root"),
            },
            Decided::Served,
        ),
        (
            "a session carrying no policy at all",
            "row-nopolicy",
            assumed_role(&router, "ops-metadata-nopolicy", None).await,
            Decided::NoGrant,
        ),
        (
            "a session allowed every action of this service",
            "row-admin",
            assumed_role(&router, "ops-metadata-admin", Some(allow_every_action)).await,
            Decided::Served,
        ),
        (
            "a session allowed this service's actions on the Region's OpsMetadata objects",
            "row-scoped",
            assumed_role(&router, "ops-metadata-scoped", Some(&allow_these_objects)).await,
            Decided::Served,
        ),
        (
            "a session allowed another action of this service only",
            "row-elsewhere",
            assumed_role(&router, "ops-metadata-elsewhere", Some(allow_another_action)).await,
            Decided::NoGrant,
        ),
        (
            "a session allowed everything with an explicit deny of this plane",
            "row-denied",
            assumed_role(&router, "ops-metadata-denied", Some(deny_the_plane)).await,
            Decided::ExplicitDeny,
        ),
    ];

    for (why, application, principal, decided) in &rows {
        seed(&router, application).await;
        let minted = format!("{application}-minted");
        for call_under_test in calls(application, &format!("/probe/{minted}")) {
            let case = format!("{why}, {}", call_under_test.case);
            let (status, body) = call_as(
                &router,
                &principal.access_key_id,
                REGION,
                call_under_test.operation,
                &call_under_test.body,
            )
            .await;
            if *decided == Decided::Served {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the allowed caller's call to be served, got \
                     {status} {body}"
                );
                continue;
            }
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "AccessDeniedException"),
                "case {case:?}: expected the call to be refused, got {status} {body}"
            );
            assert_eq!(
                error_message(&body),
                not_authorized(
                    &principal.arn,
                    call_under_test.operation,
                    &call_under_test.resource,
                    *decided
                ),
                "case {case:?}: expected the refusal to carry IAM's own sentence, got {body}"
            );
        }

        // A refused call writes nothing: the row's object is still there holding what the
        // seeding persisted, and the create's own application was never registered.
        if *decided != Decided::Served {
            let object = held(&router, application).await.unwrap_or_else(|| {
                panic!("case {why:?}: expected the refused calls to leave the object in place")
            });
            assert_eq!(
                object["Metadata"]["Seeded"]["Value"].as_str(),
                Some("yes"),
                "case {why:?}: expected the object to hold what the seeding persisted, got \
                 {object}"
            );
            assert!(
                held(&router, &minted).await.is_none(),
                "case {why:?}: expected the refused create to register no object"
            );
        }
    }
}

/// RULE: the resource half of the decision — a grant scoped to ONE object allows the calls
/// that NAME that object and no other, and does not allow the two that address every object of
/// the Region.
#[tokio::test]
async fn a_grant_scoped_to_one_object_covers_that_object_alone() {
    let router = emulator();
    seed(&router, "scoped-granted").await;
    seed(&router, "scoped-other").await;

    let allow_one_object = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:*","Resource":"{}"}}]}}"#,
        object_arn("scoped-granted")
    );
    let principal = assumed_role(&router, "ops-metadata-one-object", Some(&allow_one_object)).await;

    // The object the grant names is read, written and finally removed.
    for (case, operation, body) in [
        (
            "the granted object's read",
            "GetOpsMetadata",
            json!({ "OpsMetadataArn": object_arn("scoped-granted") }),
        ),
        (
            "the granted object's update",
            "UpdateOpsMetadata",
            json!({
                "OpsMetadataArn": object_arn("scoped-granted"),
                "MetadataToUpdate": { "Written": { "Value": "by the grant" } },
            }),
        ),
        (
            "the granted object's delete",
            "DeleteOpsMetadata",
            json!({ "OpsMetadataArn": object_arn("scoped-granted") }),
        ),
    ] {
        let (status, answered) =
            call_as(&router, &principal.access_key_id, REGION, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the grant on that object to allow it, got {status} \
             {answered}"
        );
    }

    // ...and nothing else is allowed: another object, and the two calls that address every
    // object of the Region.
    for (case, operation, body, resource) in [
        (
            "another object's read",
            "GetOpsMetadata",
            json!({ "OpsMetadataArn": object_arn("scoped-other") }),
            object_arn("scoped-other"),
        ),
        (
            "the create, which names no object",
            "CreateOpsMetadata",
            json!({ "ResourceId": "/probe/scoped-minted" }),
            every_object(),
        ),
        (
            "the listing, which names no object",
            "ListOpsMetadata",
            json!({}),
            every_object(),
        ),
    ] {
        let (status, answered) =
            call_as(&router, &principal.access_key_id, REGION, operation, &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?}: expected a grant scoped to one object not to allow it, got {status} \
             {answered}"
        );
        assert_eq!(
            error_message(&answered),
            not_authorized(&principal.arn, operation, &resource, Decided::NoGrant),
            "case {case:?}: expected the refusal to name the resource the request addressed, \
             got {answered}"
        );
    }

    // The object the grant did not cover is untouched, and the refused create registered
    // nothing.
    let other = held(&router, "scoped-other")
        .await
        .expect("the object the grant does not cover is still held");
    assert_eq!(
        other["Metadata"]["Seeded"]["Value"].as_str(),
        Some("yes"),
        "expected the ungranted object to hold what the seeding persisted, got {other}"
    );
    assert!(
        held(&router, "scoped-minted").await.is_none(),
        "expected the refused create to register no object"
    );
}
