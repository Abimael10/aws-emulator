// SPDX-License-Identifier: CC0-1.0

//! The zero-ETL integration's AUTHORIZATION and its state machine, end to end
//! over the awsQuery wire.
//!
//! Two rules, both about a `CreateIntegration`/`DeleteIntegration` pair whose
//! answer depends on something other than the request's own syntax.
//!
//! **A zero-ETL integration is authorized by the TARGET data warehouse's own
//! resource policy**: the account attaches a policy to the warehouse naming the
//! principal that may create an inbound integration into it
//! (`redshift:CreateInboundIntegration`) and the source Amazon Redshift may then
//! admit the flow from (`redshift:AuthorizeInboundIntegration`, narrowed by the
//! global source condition keys), so a target whose policy does not grant the
//! caller BOTH halves for THIS source — a target carrying no policy at all
//! included — refuses the create synchronously with HTTP 400
//! `InvalidParameterValue` and the one data-warehouse-access sentence, the same
//! sentence a warehouse that does not exist earns, because which of the two held
//! is what a caller that may not read the warehouse is not told.
//!
//! **An integration takes one conditional operation at a time, and a delete is a
//! transition like any other**: a delete against an integration whose own CREATE
//! has not settled is refused 400 `IntegrationConflictOperationFault` and the
//! integration keeps the status it was in, an accepted delete reports and keeps
//! reporting `deleting` until the delete's span has passed, and only then does
//! every operation on that ARN answer 404 `IntegrationNotFoundFault`.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter` the
//! production router builds, so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize. The policy rows differ
//! along what the rule generalises over — which half of the grant is missing, who
//! the grant names, which source it is conditioned on, and whether a deny stands
//! beside it — and the state-machine rows along which conditional operation is in
//! flight and which operation then collides with it.
//!
//! Every sentence this suite pins is reproduced here literally rather than read
//! back from the handler's own constant, so a change to the wording fails a case
//! instead of following it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// The cluster whose namespace every row of this suite integrates INTO.
const CLUSTER: &str = "zetl-auth-target";

/// The Amazon S3 source the authorized rows replicate FROM.
const SOURCE: &str = "arn:aws:s3:::zetl-auth-landing";

/// Another source of the same kind, for the row whose policy admits only the
/// first one — and for the state-machine row that needs a (source, target) pair of
/// its own, since only one integration may replicate between one pair.
const OTHER_SOURCE: &str = "arn:aws:s3:::zetl-auth-other";

/// The source the row naming the account ROOT as the create principal replicates
/// from: one integration per (source, target) pair, so each row that gets as far as
/// recording one brings its own source.
const ROOT_PRINCIPAL_SOURCE: &str = "arn:aws:s3:::zetl-auth-root";

/// The source the row whose inbound grant is narrowed to this ACCOUNT's sources
/// replicates from; see [`ROOT_PRINCIPAL_SOURCE`].
const ACCOUNT_CONDITION_SOURCE: &str = "arn:aws:s3:::zetl-auth-account";

/// The ONE sentence a data warehouse the caller cannot reach is refused with,
/// read from the sibling suite that already pins it so the wording lives in one
/// place across this crate's suites.
use crate::zero_etl_integration_targets::DATA_WAREHOUSE_ACCESS_MESSAGE as DATA_WAREHOUSE_ACCESS;

/// The sentence a delete blocked on a target that has not finished taking the
/// integration is refused with.
const DELETE_NOT_ACTIVE: &str =
    "Unable to perform delete operation on the specified zero-ETL integration because its \
     target Redshift data warehouse is not active. Wait until the data warehouse is active \
     and try again.";

/// The sentence an operation that collides with another one already in flight
/// against the same integration is refused with.
const CONFLICTING_OPERATION: &str =
    "A conflicting conditional operation is currently in progress against this resource. \
     This typically occurs when there are multiple requests being made to the same \
     resource at the same time, and these requests conflict with each other.";

/// The sentence every operation answers for an `IntegrationArn` that resolves to
/// no integration.
const NOT_FOUND: &str = "The requested integration doesn't exist.";

/// Drive one action with owned parameters.
async fn call(emulator: &Emulator, action: &str, params: &[(String, String)]) -> Answer {
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.call(action, &borrowed).await
}

/// `CreateIntegration` with just the three required members.
async fn create_integration(
    emulator: &Emulator,
    name: &str,
    source: &str,
    target: &str,
) -> Answer {
    call(
        emulator,
        "CreateIntegration",
        &[
            ("IntegrationName".to_string(), name.to_string()),
            ("SourceArn".to_string(), source.to_string()),
            ("TargetArn".to_string(), target.to_string()),
        ],
    )
    .await
}

/// An emulator holding an AVAILABLE cluster, and that cluster's namespace ARN —
/// the target every row names. Advancing the clock by one provisioning span both
/// brings the cluster up and STOPS the clock, so a row that means to observe a
/// transient status keeps it however loaded the host is.
async fn seeded() -> (Emulator, String) {
    let emulator = Emulator::new();
    let created = emulator.create_minimal(CLUSTER).await;
    assert_eq!(
        created.status, 200,
        "case seed {CLUSTER}: expected 200, got {} with body {}",
        created.status, created.body
    );
    let namespace = created
        .text("ClusterNamespaceArn")
        .expect("case seed: the cluster reports its namespace ARN");
    emulator.advance_clock(state::PROVISIONING_SPAN);
    (emulator, namespace)
}

/// Attach `policy` to the data warehouse `target` names, through the operation
/// that owns resource policies.
async fn put_policy(emulator: &Emulator, case: &str, target: &str, policy: &str) {
    let answer = call(
        emulator,
        "PutResourcePolicy",
        &[
            ("ResourceArn".to_string(), target.to_string()),
            ("Policy".to_string(), policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: attaching the target's resource policy expected 200, got {} with body {}",
        answer.status, answer.body
    );
}

/// Attach the resource policy that AUTHORIZES an integration from [`SOURCE`] or
/// [`OTHER_SOURCE`] into `target`: the documented pair of grants, for a case whose
/// subject is the state machine rather than the grant. Both sources are authorized
/// because only one integration may replicate between one (source, target) pair, so
/// a case needing two integrations into one target needs two sources.
async fn authorize(emulator: &Emulator, target: &str) {
    let document = policy(vec![
        statement(
            "Allow",
            serde_json::json!({ "AWS": caller() }),
            "redshift:CreateInboundIntegration",
        ),
        authorize_statement(SOURCE),
        authorize_statement(OTHER_SOURCE),
    ]);
    put_policy(emulator, "authorize the target", target, &document).await;
}

/// The IAM principal this emulator's caller speaks for, from the one shared
/// definition of it — the principal a target policy has to name.
fn caller() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// One statement of a target data warehouse's resource policy, as JSON.
fn statement(effect: &str, principal: serde_json::Value, action: &str) -> serde_json::Value {
    serde_json::json!({
        "Effect": effect,
        "Principal": principal,
        "Action": action,
    })
}

/// The `redshift:AuthorizeInboundIntegration` grant Amazon Redshift needs,
/// narrowed to the ONE source the warehouse accepts a flow from by the global
/// source-ARN condition key.
fn authorize_statement(source: &str) -> serde_json::Value {
    let mut grant = statement(
        "Allow",
        serde_json::json!({ "Service": "redshift.amazonaws.com" }),
        "redshift:AuthorizeInboundIntegration",
    );
    grant["Condition"] = serde_json::json!({ "StringEquals": { "aws:SourceArn": source } });
    grant
}

/// The same grant, narrowed to every source of ONE account by the global
/// source-account condition key instead.
fn authorize_statement_for_account(account: &str) -> serde_json::Value {
    let mut grant = statement(
        "Allow",
        serde_json::json!({ "Service": "redshift.amazonaws.com" }),
        "redshift:AuthorizeInboundIntegration",
    );
    grant["Condition"] = serde_json::json!({ "StringEquals": { "aws:SourceAccount": account } });
    grant
}

/// A whole policy document from its statements.
fn policy(statements: Vec<serde_json::Value>) -> String {
    serde_json::json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// One row of the authorization table: the policy attached to the target, the
/// source the create replicates from — each row that is authorized brings its own,
/// since only one integration may replicate between one (source, target) pair — and
/// whether the create is then authorized.
struct PolicyRow {
    case: &'static str,
    name: &'static str,
    source: &'static str,
    policy: String,
    authorized: bool,
}

#[tokio::test]
async fn a_target_data_warehouse_authorizes_an_integration_through_its_resource_policy() {
    let (emulator, namespace) = seeded().await;

    // A target with NO policy attached grants nothing, so it authorizes no
    // integration: that is the control the rows below are read against.
    let unprotected = create_integration(&emulator, "zetlAuthNoPolicy", SOURCE, &namespace).await;
    assert_eq!(
        (
            unprotected.status,
            unprotected.error_code().as_deref(),
            unprotected.text("Message").as_deref()
        ),
        (
            400,
            Some("InvalidParameterValue"),
            Some(DATA_WAREHOUSE_ACCESS)
        ),
        "case a target with no resource policy: expected 400 InvalidParameterValue with the \
         data-warehouse-access sentence, got {} / {:?} / {:?} with body {}",
        unprotected.status,
        unprotected.error_code(),
        unprotected.text("Message"),
        unprotected.body
    );

    let caller_grant = statement(
        "Allow",
        serde_json::json!({ "AWS": caller() }),
        "redshift:CreateInboundIntegration",
    );
    let rows = vec![
        PolicyRow {
            case: "the documented pair, naming the caller and this source",
            name: "zetlAuthBoth",
            source: SOURCE,
            policy: policy(vec![caller_grant.clone(), authorize_statement(SOURCE)]),
            authorized: true,
        },
        PolicyRow {
            case: "the documented pair with the account root as the create principal",
            name: "zetlAuthAccountRoot",
            source: ROOT_PRINCIPAL_SOURCE,
            policy: policy(vec![
                statement(
                    "Allow",
                    serde_json::json!({
                        "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())
                    }),
                    "redshift:CreateInboundIntegration",
                ),
                authorize_statement(ROOT_PRINCIPAL_SOURCE),
            ]),
            authorized: true,
        },
        PolicyRow {
            case: "the inbound authorization narrowed to this account's sources",
            name: "zetlAuthByAccount",
            source: ACCOUNT_CONDITION_SOURCE,
            policy: policy(vec![
                caller_grant.clone(),
                authorize_statement_for_account(engine::emulator_account_id()),
            ]),
            authorized: true,
        },
        PolicyRow {
            case: "the inbound authorization narrowed to another account's sources",
            name: "zetlAuthOtherAccount",
            source: SOURCE,
            policy: policy(vec![
                caller_grant.clone(),
                authorize_statement_for_account("210987654321"),
            ]),
            authorized: false,
        },
        PolicyRow {
            case: "the create grant naming another principal",
            name: "zetlAuthOtherPrincipal",
            source: SOURCE,
            policy: policy(vec![
                statement(
                    "Allow",
                    serde_json::json!({ "AWS": "arn:aws:iam::210987654321:user/someone-else" }),
                    "redshift:CreateInboundIntegration",
                ),
                authorize_statement(SOURCE),
            ]),
            authorized: false,
        },
        PolicyRow {
            case: "the inbound authorization conditioned on another source",
            name: "zetlAuthOtherSource",
            source: SOURCE,
            policy: policy(vec![caller_grant.clone(), authorize_statement(OTHER_SOURCE)]),
            authorized: false,
        },
        PolicyRow {
            case: "the caller's create grant with no inbound authorization at all",
            name: "zetlAuthCreateOnly",
            source: SOURCE,
            policy: policy(vec![caller_grant.clone()]),
            authorized: false,
        },
        PolicyRow {
            case: "the inbound authorization with no create grant at all",
            name: "zetlAuthAuthorizeOnly",
            source: SOURCE,
            policy: policy(vec![authorize_statement(SOURCE)]),
            authorized: false,
        },
        PolicyRow {
            case: "the documented pair beside an explicit deny of the create",
            name: "zetlAuthDenied",
            source: SOURCE,
            policy: policy(vec![
                caller_grant.clone(),
                authorize_statement(SOURCE),
                statement(
                    "Deny",
                    serde_json::json!({ "AWS": caller() }),
                    "redshift:CreateInboundIntegration",
                ),
            ]),
            authorized: false,
        },
        PolicyRow {
            case: "a policy that grants an unrelated action only",
            name: "zetlAuthUnrelated",
            source: SOURCE,
            policy: policy(vec![statement(
                "Allow",
                serde_json::json!({ "AWS": caller() }),
                "redshift:GetResourcePolicy",
            )]),
            authorized: false,
        },
    ];

    for row in &rows {
        put_policy(&emulator, row.case, &namespace, &row.policy).await;
        let answer = create_integration(&emulator, row.name, row.source, &namespace).await;
        if row.authorized {
            assert_eq!(
                (answer.status, answer.text("Status").as_deref()),
                (200, Some(state::INTEGRATION_STATUS_CREATING)),
                "case {}: expected 200 and the creating status, got {} / {:?} with body {}",
                row.case,
                answer.status,
                answer.error_code(),
                answer.body
            );
        } else {
            assert_eq!(
                (
                    answer.status,
                    answer.error_code().as_deref(),
                    answer.text("Message").as_deref()
                ),
                (
                    400,
                    Some("InvalidParameterValue"),
                    Some(DATA_WAREHOUSE_ACCESS)
                ),
                "case {}: expected 400 InvalidParameterValue with the data-warehouse-access \
                 sentence, got {} / {:?} / {:?} with body {}",
                row.case,
                answer.status,
                answer.error_code(),
                answer.text("Message"),
                answer.body
            );
        }
    }

    // ROUND TRIP: only the authorized rows were recorded, so a refused create
    // left nothing behind.
    let expected: Vec<String> = rows
        .iter()
        .filter(|row| row.authorized)
        .map(|row| row.name.to_string())
        .collect();
    let listed = emulator.call("DescribeIntegrations", &[]).await;
    assert_eq!(
        all_text(&listed.body, "IntegrationName"),
        expected,
        "case read the authorization rows back: expected only the authorized ones, got body {}",
        listed.body
    );
}

/// One row of the state-machine table: which transient status the integration is
/// in when the delete arrives, and whether that status blocks it.
struct InFlightRow {
    case: &'static str,
    name: &'static str,
    /// The source this row's integration replicates from: one integration per
    /// (source, target) pair, so each row brings its own.
    source: &'static str,
    /// The action that puts the integration in its transient status, if any
    /// beyond the create itself.
    then: Option<&'static str>,
    /// The status the integration is in when the delete arrives.
    status: &'static str,
    /// Whether that status blocks the delete — only the one in which the target
    /// data warehouse has not finished taking the integration does.
    blocks: bool,
}

#[tokio::test]
async fn only_an_integration_still_being_created_takes_no_delete() {
    let (emulator, namespace) = seeded().await;
    authorize(&emulator, &namespace).await;

    let rows = [
        InFlightRow {
            case: "the create itself has not settled",
            name: "zetlFlightCreating",
            source: SOURCE,
            then: None,
            status: state::INTEGRATION_STATUS_CREATING,
            blocks: true,
        },
        InFlightRow {
            case: "a modify has not settled",
            name: "zetlFlightModifying",
            source: OTHER_SOURCE,
            then: Some("ModifyIntegration"),
            status: state::INTEGRATION_STATUS_MODIFYING,
            blocks: false,
        },
    ];

    for row in &rows {
        let created = create_integration(&emulator, row.name, row.source, &namespace).await;
        assert_eq!(
            created.status, 200,
            "case {}: seeding the integration expected 200, got {} with body {}",
            row.case, created.status, created.body
        );
        let arn = created
            .text("IntegrationArn")
            .expect("the create reports the ARN it minted");
        if let Some(action) = row.then {
            // The integration has to be settled before the second operation can
            // put it back in flight.
            emulator.advance_clock(state::PROVISIONING_SPAN);
            let changed = call(
                &emulator,
                action,
                &[
                    ("IntegrationArn".to_string(), arn.clone()),
                    ("Description".to_string(), "in flight".to_string()),
                ],
            )
            .await;
            assert_eq!(
                changed.status, 200,
                "case {}: {action} expected 200, got {} with body {}",
                row.case, changed.status, changed.body
            );
        }
        // The status the delete is about to arrive in, read back rather than
        // assumed.
        let before = call(
            &emulator,
            "DescribeIntegrations",
            &[("IntegrationArn".to_string(), arn.clone())],
        )
        .await;
        assert_eq!(
            (before.status, before.text("Status").as_deref()),
            (200, Some(row.status)),
            "case {}: expected the integration in its transient status before the delete, got \
             {} with body {}",
            row.case,
            before.status,
            before.body
        );

        let answer = call(
            &emulator,
            "DeleteIntegration",
            &[("IntegrationArn".to_string(), arn.clone())],
        )
        .await;
        if row.blocks {
            assert_eq!(
                (
                    answer.status,
                    answer.error_code().as_deref(),
                    answer.text("Message").as_deref()
                ),
                (
                    400,
                    Some("IntegrationConflictOperationFault"),
                    Some(DELETE_NOT_ACTIVE)
                ),
                "case {}: expected 400 IntegrationConflictOperationFault with the \
                 wait-for-the-warehouse sentence, got {} / {:?} / {:?} with body {}",
                row.case,
                answer.status,
                answer.error_code(),
                answer.text("Message"),
                answer.body
            );
        } else {
            assert_eq!(
                (answer.status, answer.text("Status").as_deref()),
                (200, Some(state::INTEGRATION_STATUS_DELETING)),
                "case {}: expected 200 and the deleting status, got {} / {:?} with body {}",
                row.case,
                answer.status,
                answer.error_code(),
                answer.body
            );
        }

        // Whichever way it went, the integration is still readable — in the
        // status it was already in when the delete was refused, and in `deleting`
        // when it was accepted.
        let read_back = call(
            &emulator,
            "DescribeIntegrations",
            &[("IntegrationArn".to_string(), arn.clone())],
        )
        .await;
        let expected_status = if row.blocks {
            row.status
        } else {
            state::INTEGRATION_STATUS_DELETING
        };
        assert_eq!(
            (read_back.status, read_back.text("Status").as_deref()),
            (200, Some(expected_status)),
            "case {}: expected the integration still readable in {expected_status}, got {} with \
             body {}",
            row.case,
            read_back.status,
            read_back.body
        );
    }
}

#[tokio::test]
async fn an_accepted_delete_keeps_reporting_the_integration_until_it_is_gone() {
    let (emulator, namespace) = seeded().await;
    authorize(&emulator, &namespace).await;

    let created = create_integration(&emulator, "zetlDeleting", SOURCE, &namespace).await;
    assert_eq!(
        created.status, 200,
        "case seed the integration: expected 200, got {} with body {}",
        created.status, created.body
    );
    let arn = created
        .text("IntegrationArn")
        .expect("the create reports the ARN it minted");
    emulator.advance_clock(state::PROVISIONING_SPAN);

    let deleted = call(
        &emulator,
        "DeleteIntegration",
        &[("IntegrationArn".to_string(), arn.clone())],
    )
    .await;
    assert_eq!(
        (deleted.status, deleted.text("Status").as_deref()),
        (200, Some(state::INTEGRATION_STATUS_DELETING)),
        "case delete an active integration: expected 200 and the deleting status, got {} with \
         body {}",
        deleted.status,
        deleted.body
    );

    // While the deletion is under way the integration is STILL reported, by both
    // views of it, in the status the delete put it in.
    for action in ["DescribeIntegrations", "DescribeInboundIntegrations"] {
        let during = call(
            &emulator,
            action,
            &[("IntegrationArn".to_string(), arn.clone())],
        )
        .await;
        assert_eq!(
            (during.status, during.text("Status").as_deref()),
            (200, Some(state::INTEGRATION_STATUS_DELETING)),
            "case {action} during the deletion: expected 200 and the deleting status, got {} / \
             {:?} with body {}",
            during.status,
            during.error_code(),
            during.body
        );
    }

    // ... and it takes no further operation while it is on its way out.
    for (action, extra) in [
        ("DeleteIntegration", None),
        ("ModifyIntegration", Some(("Description", "too late"))),
    ] {
        let mut params: Vec<(String, String)> =
            vec![("IntegrationArn".to_string(), arn.clone())];
        if let Some((name, value)) = extra {
            params.push((name.to_string(), value.to_string()));
        }
        let collided = call(&emulator, action, &params).await;
        assert_eq!(
            (
                collided.status,
                collided.error_code().as_deref(),
                collided.text("Message").as_deref()
            ),
            (
                400,
                Some("IntegrationConflictOperationFault"),
                Some(CONFLICTING_OPERATION)
            ),
            "case {action} against an integration being deleted: expected 400 \
             IntegrationConflictOperationFault with the conflicting-operation sentence, got {} \
             / {:?} / {:?} with body {}",
            collided.status,
            collided.error_code(),
            collided.text("Message"),
            collided.body
        );
    }

    // Once the delete's own span has passed the integration is GONE, and every
    // operation on that ARN answers the one not-found sentence.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    for (action, extra) in [
        ("DescribeIntegrations", None),
        ("DescribeInboundIntegrations", None),
        ("DeleteIntegration", None),
        ("ModifyIntegration", Some(("Description", "gone"))),
    ] {
        let mut params: Vec<(String, String)> =
            vec![("IntegrationArn".to_string(), arn.clone())];
        if let Some((name, value)) = extra {
            params.push((name.to_string(), value.to_string()));
        }
        let answer = call(&emulator, action, &params).await;
        assert_eq!(
            (
                answer.status,
                answer.error_code().as_deref(),
                answer.text("Message").as_deref()
            ),
            (404, Some("IntegrationNotFoundFault"), Some(NOT_FOUND)),
            "case {action} after the deletion: expected 404 IntegrationNotFoundFault with the \
             fixed not-found sentence, got {} / {:?} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.text("Message"),
            answer.body
        );
    }
    let listed = emulator.call("DescribeIntegrations", &[]).await;
    assert!(
        all_text(&listed.body, "IntegrationArn").is_empty(),
        "case list after the deletion: expected no integration, got body {}",
        listed.body
    );
}
