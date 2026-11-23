// SPDX-License-Identifier: CC0-1.0

//! The RECORD a create leaves behind when a Lambda-backed custom resource's provider answers
//! `FAILED`: what the failed resource's own event says, word for word, and in what ORDER the
//! resources beside it were made and unmade.
//!
//! RULE under test: **a custom resource whose provider answers its `Create` with `Status`
//! `FAILED` takes `ResourceStatus` `CREATE_FAILED` with a `ResourceStatusReason` of the one
//! sentence CloudFormation words for an answered failure — `Received response status [FAILED]
//! from custom resource. Message returned: <the message that answer returned> (RequestId: <the
//! id of the request it answered>)`, whose only variable parts are that message (the provider's
//! own `Reason`, or this service's sentence for an answer that states none) and that minted
//! request id — and the create is then rolled back, so every resource it had ALREADY made
//! reports `CREATE_COMPLETE` BEFORE that failure and `DELETE_COMPLETE` after it, is gone from
//! the service that owns it, and the stack settles `ROLLBACK_COMPLETE`.**
//!
//! Why the ORDER matters, and not only the set: a client reading `DescribeStackEvents` to find
//! out what a failed deploy did reads it as a history. A record that reports the sibling
//! created and deleted, but not in that sequence around the failure, cannot be told from one
//! where the rollback ran before the provider was ever asked — and a reason assembled in any
//! other wording is one no deployment tool's matcher finds.
//!
//! Three crates have to agree for that to hold, which is why the suite lives here:
//! CloudFormation mints the request, records the event stream and rolls the create back; AWS
//! Lambda runs the provider the stack itself declared from inline code; and Amazon SNS is the
//! service that OWNS the topics the create made before the provider was asked, so whether they
//! survived is only answerable through SNS's own wire.
//!
//! The harness is the crate's own: the emulator, the awsQuery caller, the dispatcher and the
//! settling poll come from [`crate::cloudformation_completed_stack_output_values`], so nothing
//! here is a second copy of one of them. What no sibling suite pins, and this one does, is the
//! SEQUENCE of that record and the failed resource's reason read WHOLE —
//! [`crate::cloudformation_failed_custom_resource_rollback_removes_queues`] pins that a
//! rollback removes what the create had made,
//! [`crate::cloudformation_custom_resource_answer_decides_the_stacks_fate`] that the answer's
//! `Status` alone decides the terminal status, and
//! [`crate::cloudformation_custom_resource_provider_run_log_request_id`] that the id in the
//! reason is the one the provider's own run logged.
//!
//! The rows differ along what the rule generalises over: how many resources of the owning
//! service the create had already made when the provider answered (one and two), how the
//! custom resource's type is spelled (the generic `AWS::CloudFormation::CustomResource` and an
//! author's own `Custom::<Name>`), and what the answer states as its message — a plain clause,
//! a diagnostic sentence carrying its own parentheses and punctuation, and NOTHING at all,
//! which is the service's own sentence for an unstated reason.
//!
//! No expected value here comes from a recording: every name and message is the row's own, and
//! the request id asserted on is whatever this run's own CloudFormation minted — only its FORM
//! is pinned. Nothing sleeps a span on a service's behalf: every wait polls for the CONDITION
//! under a deadline far longer than the spans these services choose, and nothing asserts on
//! elapsed time.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, emulator, reported_failures, settled_status, Answered,
    ACCOUNT, CREATE_COMPLETE, REGION,
};
use crate::query_wire::{elements, members_of, outputs_of};

/// The terminal status a create whose rollback was carried out reaches.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The resource statuses the record is read for.
const CREATE_FAILED: &str = "CREATE_FAILED";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The logical id every template below declares its custom resource under.
const CUSTOM: &str = "Probe";

/// The `Status` values a provider answers with.
const ANSWER_FAILED: &str = "FAILED";
const ANSWER_SUCCESS: &str = "SUCCESS";

/// The property that tells the row's provider whether to state a `Reason` of its own, and the
/// value that asks it to.
const STATE_REASON: &str = "StateReason";
const STATES_A_REASON: &str = "yes";

/// What CloudFormation reports as the message of a failure whose answer stated no `Reason`:
/// this service's own sentence for an unstated one. Spelled out here rather than imported from
/// the code that writes it — an expectation taken from the implementation's own constant could
/// not fail when that constant changes, which is the one thing this suite is for.
const UNSTATED_REASON: &str = "The custom resource provider reported FAILED";

/// The sentence CloudFormation reports a custom resource whose provider ANSWERED `FAILED`
/// with: its fixed text, whose only variable parts are the message that answer returned and
/// the `RequestId` of the request it answered. Spelled out here for the same reason.
fn answered_failure_reason(message: &str, request_id: &str) -> String {
    format!(
        "Received response status [FAILED] from custom resource. Message returned: {message} \
         (RequestId: {request_id})"
    )
}

/// The provider every row deploys: it answers the `Create` with the `Status` its own
/// declaration asks for — stating the declared `Reason` only where the declaration asks it to
/// — and carries every other request (the `Delete` a rollback owes) out, which is what an
/// ordinary responder does.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    properties = event["ResourceProperties"]
    physical = event.get("PhysicalResourceId") or properties["AnswerId"]
    if event["RequestType"] != "Create":
        answer = {"Status": "SUCCESS", "PhysicalResourceId": physical}
    else:
        answer = {
            "Status": properties["ResponseStatus"],
            "PhysicalResourceId": physical,
        }
        if properties["ResponseStatus"] == "SUCCESS":
            answer["Data"] = {"Echo": properties["AnswerId"]}
        elif properties["StateReason"] == "yes":
            answer["Reason"] = properties["FailureReason"]
    body = json.dumps(answer).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// One row: what the provider answers, whether it states a reason of its own, how the custom
/// resource is declared, and which topics the same create makes before the provider is asked.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Status` the provider answers the `Create` with.
    response_status: &'static str,
    /// The `Reason` it states, where it states one at all.
    reason: Option<&'static str>,
    resource_type: &'static str,
    /// The `PhysicalResourceId` the answer reports, which is also what `Ref` resolves to on a
    /// completed row.
    answer_id: &'static str,
    /// The topics the template declares, which the custom resource's own declaration refers to
    /// — so each is created, in Amazon SNS, before the provider is asked anything.
    topics: &'static [&'static str],
}

impl Row {
    /// The message the failure reports for this row: the provider's own `Reason`, or the
    /// service's sentence for an answer that stated none.
    fn message(&self) -> &'static str {
        self.reason.unwrap_or(UNSTATED_REASON)
    }
}

/// The logical id the `index`-th topic of a row is declared under.
fn topic_logical_id(index: usize) -> String {
    format!("Bystander{index}")
}

/// The ARN Amazon SNS holds the topic `name` under, which is how a topic the rollback removed
/// is asked for once the stack that published it is gone.
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{name}")
}

const FAILED_ROWS: &[Row] = &[
    Row {
        case: "one topic already made, a provider stating a plain reason",
        stack: "cr-failure-record-one",
        response_status: ANSWER_FAILED,
        reason: Some("the provisioner could not make the thing"),
        resource_type: "AWS::CloudFormation::CustomResource",
        answer_id: "cr-failure-record-one-id",
        topics: &["cr-failure-record-bystander-one"],
    },
    Row {
        case: "two topics already made, a provider stating a punctuated diagnostic",
        stack: "cr-failure-record-two",
        response_status: ANSWER_FAILED,
        reason: Some("Error: the upstream answered 409 (ResourceConflict); see the log stream."),
        resource_type: "Custom::Provisioner",
        answer_id: "cr-failure-record/two-id",
        topics: &[
            "cr-failure-record-bystander-two-a",
            "cr-failure-record-bystander-two-b",
        ],
    },
    // The same rule where the answer states NO reason: the message the sentence quotes is then
    // the service's own for an unstated one, and everything else about the record is unchanged.
    Row {
        case: "a provider answering FAILED and stating no reason at all",
        stack: "cr-failure-record-silent",
        response_status: ANSWER_FAILED,
        reason: None,
        resource_type: "Custom::Provisioner",
        answer_id: "cr-failure-record-silent-id",
        topics: &["cr-failure-record-bystander-silent"],
    },
];

/// The control: the SAME template whose provider answers `SUCCESS`, so no row above could pass
/// against an emulator that tears a stack down whatever its provider answered.
const COMPLETED_ROW: Row = Row {
    case: "the same template whose provider answers SUCCESS",
    stack: "cr-failure-record-answered",
    response_status: ANSWER_SUCCESS,
    reason: None,
    resource_type: "Custom::Provisioner",
    answer_id: "cr-failure-record-answered-id",
    topics: &["cr-failure-record-bystander-answered"],
};

/// The template one row deploys: the execution role the provider assumes, the provider itself
/// from inline code, the row's topics, and the custom resource whose declaration REFERS to each
/// of them — which is what makes those topics exist before the provider is asked anything.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Action": "sts:AssumeRole",
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                    }],
                },
            },
        }),
    );
    resources.insert(
        "Provider".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Code": {"ZipFile": PROVIDER_SOURCE},
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                "Runtime": "python3.12",
                "Timeout": 60,
            },
        }),
    );
    let mut published = Vec::new();
    for (index, name) in row.topics.iter().enumerate() {
        resources.insert(
            topic_logical_id(index),
            json!({
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": name},
            }),
        );
        published.push(json!({"Ref": topic_logical_id(index)}));
    }
    let states_a_reason = if row.reason.is_some() {
        STATES_A_REASON
    } else {
        "no"
    };
    resources.insert(
        CUSTOM.to_string(),
        json!({
            "Type": row.resource_type,
            "Properties": {
                "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                "ResponseStatus": row.response_status,
                "FailureReason": row.reason.unwrap_or("unused by this row"),
                STATE_REASON: states_a_reason,
                "AnswerId": row.answer_id,
                "Bystanders": Value::Array(published),
            },
        }),
    );
    json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            "ProbeRef": {"Value": {"Ref": CUSTOM}},
        },
    })
    .to_string()
}

/// Deploy `row`'s template, asserting only that the submission was ACCEPTED: what the stack
/// then does is what the row is about.
async fn deploy(router: &MultiServiceRouter, row: &Row) {
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template(row)),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {:?}: expected HTTP 200 from CreateStack, got {} with {}",
        row.case,
        created.status,
        created.text()
    );
}

/// Call one Amazon SNS operation over its awsQuery wire, with whatever status it answered: a
/// read of a topic the rollback removed is a REFUSAL, so the status comes back rather than
/// being asserted here.
async fn sns(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Answered {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(key));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("sns.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("sns")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One event of a stack's record: which resource it is about, the status it reports and the
/// reason it gives.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    logical_id: String,
    status: String,
    reason: String,
}

/// The stack's whole event record, OLDEST FIRST — the history a client reads it as.
///
/// `DescribeStackEvents` answers "in reverse chronological order", so the answer is reversed
/// here: the rule this suite asserts is about the sequence the operation happened in.
async fn recorded_events(router: &MultiServiceRouter, stack: &str) -> Vec<Recorded> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    let mut oldest_first: Vec<Recorded> = members_of(&body)
        .into_iter()
        .filter_map(|member| {
            Some(Recorded {
                logical_id: elements(member, "LogicalResourceId").into_iter().next()?,
                status: elements(member, "ResourceStatus").into_iter().next()?,
                reason: elements(member, "ResourceStatusReason")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            })
        })
        .collect();
    oldest_first.reverse();
    oldest_first
}

/// Where in `record` the resource `logical_id` first reports `status`, or `None` when it never
/// does.
fn position_of(record: &[Recorded], logical_id: &str, status: &str) -> Option<usize> {
    record
        .iter()
        .position(|event| event.logical_id == logical_id && event.status == status)
}

/// Whether `id` has the form CloudFormation gives a request id — the 8-4-4-4-12 grouping of
/// lowercase hexadecimal digits every identifier this service hands out carries.
///
/// The id itself is this run's own, so only its form is pinned: a value one recording happened
/// to carry would say nothing about any other run.
fn is_a_request_id(id: &str) -> bool {
    let lowercase_hex =
        |character: char| character.is_ascii_hexdigit() && !character.is_ascii_uppercase();
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(width, part)| part.len() == *width)
        && parts
            .iter()
            .all(|part| part.chars().all(lowercase_hex))
}

/// The request id `reason` names, read out of the sentence itself: everything between the
/// opening of its `(RequestId: ` clause and the closing bracket that ends the sentence.
fn request_id_in(reason: &str) -> Option<&str> {
    let opens = "(RequestId: ";
    let at = reason.rfind(opens)? + opens.len();
    reason[at..].strip_suffix(')')
}

#[tokio::test]
async fn an_answered_failure_is_recorded_as_the_sentence_cloudformation_words_for_it() {
    for row in FAILED_ROWS {
        let case = row.case;
        let router = emulator();
        deploy(&router, row).await;

        // 1. The create was rolled back, and the rollback was carried out: the terminal status
        //    a clean failure leaves, never one still in flight and never ROLLBACK_FAILED.
        let (settled, described) = settled_status(&router, row.stack).await;
        if settled != ROLLBACK_COMPLETE {
            let failures = reported_failures(&router, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got \
                 {settled:?}; the deployment recorded {failures:?}; last answer {described}"
            );
        }

        // 2. What failed is the custom resource, and its reason is the ONE sentence
        //    CloudFormation words for an answered failure. The id in it is this run's own, so
        //    it is read out of the sentence and its FORM pinned — and the sentence is then
        //    compared WHOLE, because anything added to either end is a different sentence.
        let record = recorded_events(&router, row.stack).await;
        let failures: Vec<&Recorded> = record
            .iter()
            .filter(|event| event.logical_id == CUSTOM && event.status == CREATE_FAILED)
            .collect();
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {CREATE_FAILED} event for {CUSTOM}, got \
             {failures:?} out of {record:?}"
        );
        let reported = &failures[0].reason;
        let request_id = request_id_in(reported).unwrap_or_default();
        assert!(
            is_a_request_id(request_id),
            "case {case:?}: expected the reason to name the request it answered under a minted \
             8-4-4-4-12 id, got {request_id:?} out of {reported:?}"
        );
        assert_eq!(
            reported,
            &answered_failure_reason(row.message(), request_id),
            "case {case:?}: expected the reason CloudFormation words for an answered failure, \
             got {reported:?}"
        );

        // 3. The resources the create had ALREADY made are recorded created before that
        //    failure and deleted after it: the history a client reads the rollback as.
        let failed_at = position_of(&record, CUSTOM, CREATE_FAILED).unwrap_or_default();
        for index in 0..row.topics.len() {
            let logical_id = topic_logical_id(index);
            let made_at = position_of(&record, &logical_id, CREATE_COMPLETE);
            let unmade_at = position_of(&record, &logical_id, DELETE_COMPLETE);
            assert!(
                made_at.is_some_and(|made| made < failed_at),
                "case {case:?}: expected {logical_id} to be recorded {CREATE_COMPLETE} before \
                 {CUSTOM} was recorded {CREATE_FAILED} at {failed_at}, got {made_at:?} out of \
                 {record:?}"
            );
            assert!(
                unmade_at.is_some_and(|unmade| unmade > failed_at),
                "case {case:?}: expected the rollback to record {logical_id} {DELETE_COMPLETE} \
                 after that failure, got {unmade_at:?} out of {record:?}"
            );
        }

        // 4. ...and the claim the stack's own record cannot make: the service that OWNS each of
        //    them no longer holds it.
        for name in row.topics {
            let arn = topic_arn(name);
            let answered = sns(&router, "GetTopicAttributes", &[("TopicArn", &arn)]).await;
            assert_eq!(
                answered.status,
                404,
                "case {case:?}: expected Amazon SNS to hold no topic {arn:?} after the rollback \
                 removed it, got {} with {}",
                answered.status,
                answered.text()
            );
        }

        // 5. A rolled-back create published nothing: outputs are what a completed operation
        //    leaves behind.
        let outputs = outputs_of(&described);
        assert!(
            outputs.is_empty(),
            "case {case:?}: expected a rolled-back stack to publish no outputs, got {outputs:?}"
        );
    }
}

#[tokio::test]
async fn the_same_template_answered_success_records_no_failure_and_unmakes_nothing() {
    let row = &COMPLETED_ROW;
    let case = row.case;
    let router = emulator();
    deploy(&router, row).await;

    // The control settles COMPLETE, so the rows above distinguish the two answers rather than
    // describing an emulator that rolls every create back.
    let (settled, described) = settled_status(&router, row.stack).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, row.stack).await;
        panic!(
            "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got {settled:?}; \
             the deployment recorded {failures:?}; last answer {described}"
        );
    }
    let outputs = outputs_of(&described);
    assert_eq!(
        outputs.get("ProbeRef").map(String::as_str),
        Some(row.answer_id),
        "case {case:?}: expected Ref on {CUSTOM} to resolve to the answered PhysicalResourceId, \
         got {outputs:?}"
    );

    // Its record carries no failure at all, and the topics it made are recorded created before
    // the custom resource completed — the same ordering, with nothing unmade.
    let record = recorded_events(&router, row.stack).await;
    assert!(
        !record.iter().any(|event| event.status.contains("FAILED")),
        "case {case:?}: expected the record of a completed create to report no failure, got \
         {record:?}"
    );
    let completed_at = position_of(&record, CUSTOM, CREATE_COMPLETE);
    assert!(
        completed_at.is_some(),
        "case {case:?}: expected {CUSTOM} to be recorded {CREATE_COMPLETE}, got {record:?}"
    );
    for index in 0..row.topics.len() {
        let logical_id = topic_logical_id(index);
        let made_at = position_of(&record, &logical_id, CREATE_COMPLETE);
        assert!(
            made_at.is_some_and(|made| Some(made) < completed_at),
            "case {case:?}: expected {logical_id} to be recorded {CREATE_COMPLETE} before \
             {CUSTOM} was, got {made_at:?} against {completed_at:?} out of {record:?}"
        );
        assert!(
            position_of(&record, &logical_id, DELETE_COMPLETE).is_none(),
            "case {case:?}: expected a completed create to unmake nothing, it recorded \
             {logical_id} {DELETE_COMPLETE} out of {record:?}"
        );
    }

    // ...and Amazon SNS still holds each of them.
    for name in row.topics {
        let arn = topic_arn(name);
        let answered = sns(&router, "GetTopicAttributes", &[("TopicArn", &arn)]).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected Amazon SNS to still hold {arn:?} after the create \
             completed, got {} with {}",
            answered.status,
            answered.text()
        );
        assert!(
            answered.text().contains(&arn),
            "case {case:?}: expected the reported attributes of {arn:?} to name it, got {}",
            answered.text()
        );
    }
}
