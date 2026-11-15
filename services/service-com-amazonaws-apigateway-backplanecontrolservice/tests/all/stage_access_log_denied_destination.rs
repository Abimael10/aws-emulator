// SPDX-License-Identifier: CC0-1.0

//! What TWO stages of one api do when one of them is pointed at an access-log destination the
//! account holds no log group under: the negative-delivery half of stage access logging, beside
//! the sibling stage that is configured correctly.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A stage's `accessLogSettings` `destinationArn` names a log group of the ACCOUNT's, so a
//! > delivery writes into the group CloudWatch Logs already holds and brings none into
//! > existence. A stage whose destination names no group therefore FAILS CLOSED — its records
//! > are lost, `DescribeLogGroups` goes on listing no group of that name, and nothing of that
//! > stage's traffic leaks into any other destination — while a SIBLING stage of the same api
//! > pointed at a group the account created keeps receiving exactly one record per request it
//! > answered. Each stage writes only into the destination IT names, both stages serve their
//! > traffic exactly as they would with no logging configured at all, and a destination the
//! > account creates later takes only what is delivered once it exists.
//!
//! Every call this drives — the account's role, each stage's configuration, each invoke and each
//! question put to CloudWatch Logs — reaches its terminal answer inside the request, so the
//! negative guarantee is OBSERVABLE: a client scripting "point one stage at a group that is not
//! there, point its sibling at one that is, then compare the two destinations" gets an answer for
//! both rather than waiting on one that never comes.
//!
//! Only a cross-service suite can pin this: the api, its stages and their logging configuration
//! are API Gateway's, the role the account writes its logs under is AWS IAM's, and whether a
//! group exists at all is CloudWatch Logs'. All three are mounted on ONE engine, as
//! `core/router/src/lib.rs` mounts them, and every assertion reads the outcome back through
//! CloudWatch Logs' own `FilterLogEvents` and `DescribeLogGroups`, so a group conjured up by a
//! delivery cannot pass.
//!
//! What the account role's own POLICY grants or denies is NOT under test here, and no assertion
//! below turns on it: a vended-logs write is made as the account's role, and which writes that
//! role's policies would admit is not something this emulator decides. The role is created with
//! the inline `Deny` the recorded flow installs — scoped to the destination one stage names — so
//! that the setup is the client's, and what each row then reads is the one thing that does decide
//! the outcome: whether CloudWatch Logs holds a group of that name at all.
//!
//! The rows differ along what the rule generalises over: WHICH of the two stages is pointed at
//! the absent destination, which published form of the log-group ARN names it (the group itself
//! and everything under it), the shape of the group names, and how many requests each stage
//! serves under the configuration.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::account_logging_role::{form_encoded, iam_query};
use crate::execute_api_data_plane::{deploy, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, gateway_with_iam_and_logs, Gateway};
// The destination ARN and the account's logging role are the sibling suite's readings of the
// same two things ([`crate::stage_access_log_absent_destination`]), reused rather than copied:
// one ARN form, and one role installed the one way, for every suite about what a stage delivers.
use crate::stage_access_log_absent_destination::{
    destination_arn, install_account_role, ROLE_NAME,
};
use crate::stage_access_logging::{create_log_group, delivered, invoke, logs};

/// The two stages the api is deployed to: a row points one of them at a destination that is
/// there and the other at one that is not, either way round.
const ALPHA: &str = "alpha";
const BETA: &str = "beta";

/// The path part of the api's one resource and the path a request addresses it by.
const PATH_PART: &str = "ping";
const RESOURCE_PATH: &str = "/ping";

/// The one `MOCK` method each stage serves — "for testing without actually invoking the
/// backend" (`IntegrationType`) — so what is delivered is the stage's own doing and no
/// backend's.
const VERB: &str = "GET";
const STATUS: &str = "200";
const BODY: &str = r#"{"pong":true}"#;

/// The inline policy embedded on that role, which each row re-puts scoped to its OWN absent
/// destination: one name, so the put replaces the document rather than adding a second one.
const POLICY_NAME: &str = "deny-access-log-destination";

/// The format every row renders its records by. The rule under test is about the DESTINATION,
/// so one format naming the request and the stage that answered it is enough: it makes a record
/// say which stage wrote it, so a record delivered into the wrong destination cannot pass.
const FORMAT: &str = "$context.requestId $context.stage $context.status";

/// One row: which stage is pointed at a destination CloudWatch Logs holds no group under, which
/// stage is pointed at one the account created, and how many requests each of them serves.
struct Row {
    case: &'static str,
    /// The stage whose `destinationArn` names a group that is not there.
    absent_stage: &'static str,
    /// That group's name, which the account never creates.
    absent_group: &'static str,
    /// Whether the absent destination is named by everything UNDER the group (the `:*` form)
    /// rather than by the group itself: both published forms name the same group.
    everything_under: bool,
    /// The sibling stage, pointed at a group the account creates first.
    held_stage: &'static str,
    /// That group's name.
    held_group: &'static str,
    /// How many requests EACH stage serves under this configuration.
    requests: usize,
}

/// The rows: each stage in turn pointed at the absent destination, in each published ARN form,
/// with one and with several requests served under the configuration.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the first stage pointed at a destination the account holds no group under",
            absent_stage: ALPHA,
            absent_group: "/apigw/denied/absent-alpha",
            everything_under: false,
            held_stage: BETA,
            held_group: "/apigw/denied/held-beta",
            requests: 1,
        },
        Row {
            case: "the second stage pointed at an absent destination, several requests each",
            absent_stage: BETA,
            absent_group: "/apigw/denied/absent-beta",
            everything_under: false,
            held_stage: ALPHA,
            held_group: "/apigw/denied/held-alpha",
            requests: 2,
        },
        Row {
            case: "an absent destination named by everything under the group",
            absent_stage: ALPHA,
            absent_group: "/apigw/denied/absent-everything-under",
            everything_under: true,
            held_stage: BETA,
            held_group: "/apigw/denied/held-everything-under",
            requests: 1,
        },
        Row {
            case: "destinations whose names carry no path at all",
            absent_stage: BETA,
            absent_group: "apigw-denied-absent-flat",
            everything_under: false,
            held_stage: ALPHA,
            held_group: "apigw-denied-held-flat",
            requests: 2,
        },
    ]
}

/// Embed an inline policy on that role DENYING the creation of, and the writes into, the log
/// group `destination` names — the configuration a client makes a destination mis-permissioned
/// with. What the policy says is IAM's record; it is the destination's absence that decides
/// what is delivered, which is what the rows then read.
async fn deny_destination(gateway: &Gateway, destination: &str) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": destination,
        }],
    })
    .to_string();
    iam_query(
        gateway,
        &format!("PutRolePolicy({POLICY_NAME})"),
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName={}&PolicyDocument={}",
            form_encoded(ROLE_NAME),
            form_encoded(POLICY_NAME),
            form_encoded(&document)
        ),
    )
    .await;
}

/// Point `stage`'s access logging at `destination`, rendering by [`FORMAT`].
///
/// Accepted whatever CloudWatch Logs holds: the configuration is API Gateway's record, and
/// whether the group is there is read when a record is delivered.
async fn point_at(gateway: &Gateway, api: &str, stage: &str, destination: &str, case: &str) {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": "/accessLogSettings/destinationArn",
                        "value": destination,
                    },
                    { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
                ],
            })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateStage to store the logging configuration of stage \
         {stage:?} whether or not the destination exists, got {} with {:?}",
        answer.status, answer.body
    );
}

/// The names of the log groups `DescribeLogGroups` reports under `prefix`, which is the question
/// a client puts to CloudWatch Logs to establish whether a group is there at all.
async fn groups_named(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": prefix }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLogGroups({prefix:?}): expected HTTP 200, got {status} with {body}"
    );
    body.get("logGroups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group.get("logGroupName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One invoke of `stage` that has to be served, answering the id API Gateway processed it under.
async fn served(router: &MultiServiceRouter, api: &str, stage: &str, case: &str) -> String {
    let answered = invoke(router, api, stage, VERB, RESOURCE_PATH).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected stage {stage:?} to serve its traffic whatever its access-log \
         destination, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .request_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: stage {stage:?} named no request id"))
}

/// The record [`FORMAT`] renders for a request `stage` answered under `request_id`.
fn record(request_id: &str, stage: &str) -> String {
    format!("{request_id} {stage} 200")
}

/// An api with one `MOCK` resource, deployed to BOTH stages. Answers the api id.
async fn deployed_to_both_stages(gateway: &Gateway, name: &str) -> String {
    let (api, root) = create_api(gateway, name).await;
    mock_resource(
        gateway,
        &api,
        &root,
        PATH_PART,
        VERB,
        STATUS,
        json_template(BODY),
    )
    .await;
    for stage in [ALPHA, BETA] {
        deploy(gateway, &api, stage).await;
    }
    api
}

/// RULE: the stage pointed at a destination the account holds no group under delivers nothing
/// and brings no group into existence, while its sibling keeps receiving one record per request
/// it answered — and both stages serve their traffic.
#[tokio::test]
async fn an_absent_destination_takes_nothing_while_its_sibling_stage_keeps_delivering() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed_to_both_stages(&gateway, "access-log-denied-destination").await;

    for row in rows() {
        let case = row.case;
        // Only the sibling's destination is created: a destination is a log group of the
        // account's, so the other one stays absent unless a delivery conjures it up.
        create_log_group(&gateway.router, row.held_group).await;
        let absent = destination_arn(row.absent_group, row.everything_under);
        deny_destination(&gateway, &absent).await;
        point_at(&gateway, &api, row.absent_stage, &absent, case).await;
        point_at(
            &gateway,
            &api,
            row.held_stage,
            &destination_arn(row.held_group, false),
            case,
        )
        .await;

        // The two stages serve interleaved traffic, so a record of either could in principle
        // reach either destination.
        let mut held_records: Vec<String> = Vec::new();
        for _ in 0..row.requests {
            served(&gateway.router, &api, row.absent_stage, case).await;
            let request_id = served(&gateway.router, &api, row.held_stage, case).await;
            held_records.push(record(&request_id, row.held_stage));
        }

        // The absent destination: no group, so nothing to read and nothing listed.
        let reported = delivered(&gateway.router, row.absent_group).await;
        assert_eq!(
            reported, None,
            "case {case:?}: expected CloudWatch Logs to hold no group {:?} at all after {} \
             request(s) to stage {:?}, got {reported:?}",
            row.absent_group, row.requests, row.absent_stage
        );
        let listed = groups_named(&gateway.router, row.absent_group).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected DescribeLogGroups to list no group named {:?}, got {listed:?}",
            row.absent_group
        );

        // The sibling destination: exactly the records of the requests THAT stage answered,
        // and nothing of the stage whose destination was absent.
        let records = delivered(&gateway.router, row.held_group)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the destination {:?} the account created to still \
                     be there",
                    row.held_group
                )
            });
        assert_eq!(
            records, held_records,
            "case {case:?}: expected one record per request stage {:?} answered in {:?}, got \
             {records:?}",
            row.held_stage, row.held_group
        );
        let listed = groups_named(&gateway.router, row.held_group).await;
        assert_eq!(
            listed,
            vec![row.held_group.to_string()],
            "case {case:?}: expected DescribeLogGroups to list the destination the account \
             created, got {listed:?}"
        );
    }
}

/// RULE, the other half: failing closed is about the group's EXISTENCE, not about the stage —
/// once the account creates the destination, the records of the requests served from then on
/// land in it, and the ones served while it was absent are gone rather than held back.
#[tokio::test]
async fn a_destination_created_later_takes_only_what_is_delivered_once_it_exists() {
    let case = "a destination the account creates after the stage was pointed at it";
    let group = "/apigw/denied/created-later";
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed_to_both_stages(&gateway, "access-log-destination-created-later").await;
    let destination = destination_arn(group, false);
    deny_destination(&gateway, &destination).await;
    point_at(&gateway, &api, ALPHA, &destination, case).await;

    // Served while the account holds no such group: the record is lost and the group stays
    // absent.
    served(&gateway.router, &api, ALPHA, case).await;
    assert_eq!(
        delivered(&gateway.router, group).await,
        None,
        "case {case:?}: expected no group {group:?} while the account had not created it"
    );

    // The account creates it, and the stage keeps serving.
    create_log_group(&gateway.router, group).await;
    let request_id = served(&gateway.router, &api, ALPHA, case).await;
    let records = delivered(&gateway.router, group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the created group {group:?} to be read"));
    assert_eq!(
        records,
        vec![record(&request_id, ALPHA)],
        "case {case:?}: expected only the request served once {group:?} existed to be in it, \
         got {records:?}"
    );
}
