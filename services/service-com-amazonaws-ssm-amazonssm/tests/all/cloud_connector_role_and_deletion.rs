// SPDX-License-Identifier: CC0-1.0

//! The cloud connector's write half beyond the create: the IAM role a create is resolved
//! against, and the delete that removes a connector again.
//!
//! Two rules are asserted here:
//!
//! 1. a connector "uses [the IAM role `RoleArn` names] to communicate with the third-party
//!    cloud environment" (`smithy_query(query="doc", shape="CreateCloudConnectorRequest",
//!    member="RoleArn")`), so the create resolves that role against the account's IAM records
//!    first: an ARN IAM holds no role under, one whose trust policy withholds the assume from
//!    this service, and one naming another account are each refused HTTP 400
//!    `ValidationException` and NO connector is minted — the same reading of a
//!    caller-supplied role `CreateActivation` takes, and grounded in what IAM ANSWERED, so an
//!    emulator with no IAM mounted to ask refuses nothing. (The other half of what the create
//!    resolves about that role — whether it may obtain the token the cross-cloud federation
//!    needs — is the sibling suite's ([`crate::cloud_connector_validation`]), so every role
//!    held here is permitted that call and the trust policy is what the rows vary.)
//! 2. `DeleteCloudConnector` removes the connector its `CloudConnectorId` names at the Region
//!    the request was signed for, and an id this Region holds no connector under — one
//!    nothing created, one created through another Region, or one already deleted — is
//!    refused `ResourceNotFoundException` with the sentence every operation of this service
//!    that resolves a connector id states.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`])
//! — and every role is created through IAM's OWN `CreateRole`, reached through the helpers the
//! sibling role suite owns ([`crate::hybrid_activation_role::router_with_iam`],
//! [`crate::hybrid_activation_role::create_role`]): nothing reaches into either store, and
//! neither the router nor the awsQuery encoding is written a second time here.
//!
//! The rows span what the rules generalise over: where IAM holds the role and what its trust
//! policy says, which account and path the ARN names, how many connectors the Region holds
//! and which Region holds them, and whether the id was created, deleted or never existed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_connector_validation::{
    create_federating_role, enable_outbound_federation, federation_permitted, listed,
    put_role_policy, trusted_by_ssm,
};
use crate::hybrid_activation_role::{create_role, router_with_iam};
use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router, ACCOUNT, REGION,
};

/// A second Region, to pin that a connector is deleted through the Region it was created in.
const OTHER_REGION: &str = "us-east-1";

/// An account other than the one this emulator serves.
const OTHER_ACCOUNT: &str = "123456789012";

/// The action a trust policy grants the service principal it admits.
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The sentence a connector id that names no connector is refused with, written out here
/// independently of the service: fixed text for the class, naming the resource class and
/// interpolating nothing from the request.
const NOT_FOUND: &str = "CloudConnector not found for account.";

/// An id of the form `CloudConnectorId` models (a 36-character lowercase-hex UUID) that this
/// service never minted.
const UNMINTED_ID: &str = "0ad1e69c-b8c1-4b4b-9e1b-0c5f5d0a5f20";

/// A second one, so a case can name two ids nothing created.
const OTHER_UNMINTED_ID: &str = "11111111-2222-3333-4444-555555555555";

/// An Azure tenant id: `AzureConfiguration$TenantId` is a hex UUID.
const TENANT_ID: &str = "22222222-3333-4444-5555-666666666666";

/// See [`TENANT_ID`]: `AzureConfiguration$ApplicationId` has the same shape.
const APPLICATION_ID: &str = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";

/// A create body naming `role_arn` as the role the connector acts under.
fn create_body(role_arn: &str) -> Value {
    json!({
        "DisplayName": "Azure production tenant",
        "RoleArn": role_arn,
        "ConfigConnectorArn": format!("arn:aws:config:{REGION}:{ACCOUNT}:connector/production"),
        "Configuration": {
            "AzureConfiguration": {
                "TenantId": TENANT_ID,
                "ApplicationId": APPLICATION_ID,
            },
        },
    })
}

/// Create a connector at `region` naming a role IAM holds, trusts and permits to federate,
/// answering its id.
async fn create_connector(router: &MultiServiceRouter, case: &str, region: &str, role: &str) -> String {
    create_federating_role(router, case, role).await;
    let (status, created) = call_in_region(
        router,
        region,
        "CreateCloudConnector",
        &create_body(&format!("arn:aws:iam::{ACCOUNT}:role/{role}")),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be served, got {status} with {created}"
    );
    created["CloudConnectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served create reports an id, got {created}"))
        .to_string()
}

/// RULE: a create is served only for a role IAM holds and whose trust policy lets this
/// service assume it; every other answer IAM gives refuses the create, and nothing is minted.
#[tokio::test]
async fn a_connector_is_minted_only_for_a_role_this_service_may_assume() {
    /// One row: the role IAM is asked about, and whether the create is served.
    struct Row {
        case: &'static str,
        /// The role to create in IAM, with its trust policy, or `None` to create none.
        held: Option<(&'static str, Value)>,
        /// The `RoleArn` the create names.
        role_arn: String,
        served: bool,
    }

    let trusted = trusted_by_ssm();

    let rows = vec![
        Row {
            case: "a role IAM holds and trusts",
            held: Some(("SsmCloudConnectorRole", trusted.clone())),
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
            served: true,
        },
        Row {
            case: "a role IAM holds under a path and trusts",
            held: Some(("SsmPathRole", trusted.clone())),
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/service-role/SsmPathRole"),
            served: true,
        },
        Row {
            case: "an ARN IAM holds no role under",
            held: None,
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/NobodyCreatedThis"),
            served: false,
        },
        Row {
            case: "a role trusting another service only",
            held: Some((
                "SsmWrongPrincipal",
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "ec2.amazonaws.com" },
                        "Action": ASSUME_ROLE,
                    }],
                }),
            )),
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/SsmWrongPrincipal"),
            served: false,
        },
        Row {
            case: "a role whose trust policy grants nothing",
            held: Some((
                "SsmNoStatement",
                json!({ "Version": "2012-10-17", "Statement": [] }),
            )),
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/SsmNoStatement"),
            served: false,
        },
        Row {
            case: "an ARN naming another account",
            held: Some(("SsmCloudConnectorRole", trusted.clone())),
            role_arn: format!("arn:aws:iam::{OTHER_ACCOUNT}:role/SsmCloudConnectorRole"),
            served: false,
        },
    ];

    for row in rows {
        let case = row.case;
        // A router per row, so each row's IAM store holds only its own role.
        let router = router_with_iam();
        if let Some((role, trust)) = row.held.as_ref() {
            create_role(&router, case, "/", role, trust).await;
            // Every held role is permitted to obtain the web identity token the create's
            // federation step needs, so what each row varies is the TRUST half of the
            // question and nothing else.
            put_role_policy(&router, case, role, &federation_permitted()).await;
        }
        // The token is vended only while the ACCOUNT has outbound web identity federation
        // on, which every row here holds constant.
        enable_outbound_federation(&router, case).await;

        let (status, body) = call(&router, "CreateCloudConnector", &create_body(&row.role_arn)).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: the create must be served, got {status} with {body}"
            );
            let id = body["CloudConnectorId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: a served create reports an id, got {body}")
            });
            assert_eq!(
                listed(&router, REGION).await,
                vec![id.to_string()],
                "case {case:?}: the minted connector is reported"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: the create must be refused, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {body}"
            );
            let expected = format!(
                "Nonexistent role or missing ssm service principal in trust policy: {}",
                row.role_arn
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                error_message(&body)
            );
            assert!(
                listed(&router, REGION).await.is_empty(),
                "case {case:?}: a refused create mints no connector"
            );
        }
    }
}

/// RULE, grounded: the refusal rests on what IAM ANSWERED about the role, so an emulator with
/// no IAM mounted to ask refuses nothing — the create is served, whatever role it names.
///
/// That is not in tension with the row above that refuses an ARN naming ANOTHER account
/// without asking: there the ARN names no role of this account at all, which the value itself
/// settles, while here the question is what IAM holds under a name of this account — and
/// nothing answered it.
#[tokio::test]
async fn a_create_is_served_where_no_iam_answers_about_the_role() {
    let router = router();
    let (status, body) = call(
        &router,
        "CreateCloudConnector",
        &create_body(&format!("arn:aws:iam::{ACCOUNT}:role/NobodyCreatedThis")),
    )
    .await;
    assert_eq!(
        status, 200,
        "with no IAM to ask, the create is served, got {status} with {body}"
    );
    assert!(
        body["CloudConnectorId"].as_str().is_some(),
        "a served create reports an id, got {body}"
    );
}

/// RULE: a delete removes the connector its id names at the Region the request was signed
/// for, reports that id back, and leaves every other connector where it is.
#[tokio::test]
async fn a_delete_removes_the_connector_its_id_names() {
    let case = "two connectors in one Region and one in another";
    let router = router_with_iam();
    let first = create_connector(&router, case, REGION, "SsmFirstRole").await;
    let second = create_connector(&router, case, REGION, "SsmSecondRole").await;
    let elsewhere = create_connector(&router, case, OTHER_REGION, "SsmThirdRole").await;

    let (status, deleted) = call(
        &router,
        "DeleteCloudConnector",
        &json!({ "CloudConnectorId": first }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {deleted}"
    );
    assert_eq!(
        deleted["CloudConnectorId"].as_str(),
        Some(first.as_str()),
        "the delete reports the id it removed, got {deleted}"
    );

    // What each Region holds afterwards.
    assert_eq!(
        listed(&router, REGION).await,
        vec![second.clone()],
        "the Region holds only the connector the delete did not name"
    );
    assert_eq!(
        listed(&router, OTHER_REGION).await,
        vec![elsewhere.clone()],
        "another Region's connector is left alone"
    );

    // The read of the deleted connector is the not-found refusal too.
    let (status, body) = call(
        &router,
        "GetCloudConnector",
        &json!({ "CloudConnectorId": first }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the deleted connector must not be readable, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "expected the not-found refusal, got {body}"
    );
}

/// RULE: an id this Region holds no connector under is refused `ResourceNotFoundException`
/// with the one sentence for the class, and the delete removes nothing.
#[tokio::test]
async fn a_delete_is_refused_for_an_id_that_names_no_connector() {
    let case = "one connector held here and one held elsewhere";
    let router = router_with_iam();
    let held = create_connector(&router, case, REGION, "SsmHeldRole").await;
    let elsewhere = create_connector(&router, case, OTHER_REGION, "SsmElsewhereRole").await;
    // One connector created here and deleted again, so an id that WAS minted names nothing.
    let deleted = create_connector(&router, case, REGION, "SsmDeletedRole").await;
    let (status, body) = call(
        &router,
        "DeleteCloudConnector",
        &json!({ "CloudConnectorId": deleted }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the first delete must be served, got {status} with {body}"
    );

    // (case, the id the delete names)
    let rows: Vec<(&str, String)> = vec![
        ("an id nothing created", UNMINTED_ID.to_string()),
        ("another id nothing created", OTHER_UNMINTED_ID.to_string()),
        ("an id this service minted and deleted", deleted.clone()),
        (
            "an id created through another Region",
            elsewhere.clone(),
        ),
    ];

    for (case, id) in rows {
        let (status, body) = call(
            &router,
            "DeleteCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the delete must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND:?}, got {:?}",
            error_message(&body)
        );
    }

    // Every refused delete left both Regions exactly as they were.
    assert_eq!(
        listed(&router, REGION).await,
        vec![held],
        "the connector this Region holds survives every refused delete"
    );
    assert_eq!(
        listed(&router, OTHER_REGION).await,
        vec![elsewhere],
        "the connector the other Region holds survives them too"
    );
}
