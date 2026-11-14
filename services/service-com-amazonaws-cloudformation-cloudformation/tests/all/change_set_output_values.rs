// SPDX-License-Identifier: CC0-1.0

//! The OUTPUT VALUES a stack deployed by an EXECUTED CHANGE SET reports, end to end over the
//! awsQuery wire.
//!
//! RULE: a stack reports one `Output` per output its DEPLOYED template declares, and every one
//! of them carries the `OutputValue` the output's `Value` expression resolves to against the
//! configuration in force on that stack — whichever operation deployed the template, so a
//! stack an executed change set drove to its completed status reports exactly what the same
//! template deployed by `CreateStack` would. `Ref AWS::NotificationARNs` is one such piece of
//! configuration: it "returns the list of notification Amazon Resource Names (ARNs) for the
//! current stack", a LIST, so the functions that read a list (`Fn::Join`, `Fn::Select`) read
//! the topics the change set put on the stack — the comma-joined list in general, and the
//! empty string for a stack carrying no topic at all.
//!
//! The recorded trace this suite was written for is ONE row of the table below (a `CREATE`
//! change set carrying a single topic, whose one output joins the pseudo parameter). The rows
//! differ along what the rule generalises over:
//!
//! * how many topics the change set carries — none, one, and the five
//!   `CreateChangeSetInput$NotificationARNs` admits at most (Smithy: `NotificationARNs`,
//!   length 0-5);
//! * the expression each output reads — the joined list, one entry of it, a literal that reads
//!   no configuration at all, and a `Ref` to a resource the template declares;
//! * which executed change set deployed the template — the `CREATE` change set that brought
//!   the stack out of `REVIEW_IN_PROGRESS`, and then an `UPDATE` change set that CHANGES the
//!   topic list, so the outputs follow the configuration now in force rather than the one the
//!   stack was created with.
//!
//! Every assertion drives the emulator through its own wire — form-encoded body in, XML
//! envelope out — so what is asserted is what a client reading `Stacks[0].Outputs` sees.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, execute_change_set, settle_the_store};
use crate::harness::{call, member, router_with_engine, xml_text, xml_texts};
use crate::stack_configuration_and_provider_limits::{notification_members, topic_arn};
use crate::stack_lifecycle::list_members;

/// The literal an output that reads no configuration at all publishes, so every row also
/// pins an output whose value owes nothing to the pseudo parameter.
const LITERAL: &str = "a-plain-value";

/// How many settling reads one case is allowed before its stack is declared stuck: the
/// store's own due instants are pulled into the past by each one, so a stack settles in as
/// many reads as its deployment has levels and never in a measured span of wall clock.
const SETTLING_READS: usize = 20;

/// The template one revision deploys: `resources` wait-condition handles, an output joining
/// `AWS::NotificationARNs`, one reading its first entry where the revision's stack has one,
/// a literal output and an output reading a declared resource.
///
/// Deliberately its own document rather than the one
/// [`super::stack_configuration_and_provider_limits`] deploys: a revision here declares a
/// VARYING number of resources (so the `UPDATE` change set below has a change to plan) and
/// no transform at all, since nothing in this table reads a language-extensions directive.
fn template(resources: usize, reads_an_entry: bool) -> String {
    let declared: serde_json::Map<String, Value> = (0..resources)
        .map(|index| {
            (
                format!("Handle{index}"),
                json!({"Type": "AWS::CloudFormation::WaitConditionHandle"}),
            )
        })
        .collect();
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "Topics".to_string(),
        json!({"Value": {"Fn::Join": [",", {"Ref": "AWS::NotificationARNs"}]}}),
    );
    if reads_an_entry {
        outputs.insert(
            "FirstTopic".to_string(),
            json!({"Value": {"Fn::Select": ["0", {"Ref": "AWS::NotificationARNs"}]}}),
        );
    }
    outputs.insert("Literal".to_string(), json!({"Value": LITERAL}));
    outputs.insert(
        "HandleRef".to_string(),
        json!({"Value": {"Ref": "Handle0"}}),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(declared),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// `DescribeStacks` for one stack, settled: the status it rests in and the whole body.
///
/// Each attempt pulls every due instant the store owes into the past and lets an ordinary
/// read materialise it, so a deployment of several levels settles in several reads and the
/// case waits for the CONDITION rather than for a span of wall clock.
async fn settled(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..SETTLING_READS {
        settle_the_store(router, engine).await;
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack_name)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DescribeStacks to answer for {stack_name:?}, \
             got {code} with {body}"
        );
        let status = xml_text(&body, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") {
            return (status, body);
        }
        last = (status, body);
    }
    panic!(
        "case {case:?}: {stack_name:?} never left an in-progress status; last was {:?} with {}",
        last.0, last.1
    );
}

/// The outputs a described stack reports, read as the `Output` ENTRIES they are: one
/// `<OutputKey>`/`<OutputValue>` pair per `<member>` of the `<Outputs>` list — the shared
/// list scan ([`list_members`]) — so an entry carrying no `OutputValue` at all is visible as
/// the missing member it is rather than being papered over by zipping two independent lists.
fn reported_outputs(body: &str) -> Vec<(String, Option<String>)> {
    list_members(body, "Outputs")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "OutputKey").unwrap_or_default(),
                xml_text(entry, "OutputValue"),
            )
        })
        .collect()
}

/// Assert that every output the stack reports carries BOTH members, and answer them as the
/// map a client builds from `Stacks[0].Outputs`.
fn readable_outputs(case: &str, body: &str) -> BTreeMap<String, String> {
    let entries = reported_outputs(body);
    assert!(
        !entries.is_empty(),
        "case {case:?}: expected the stack to report its declared outputs, got none in {body}"
    );
    let missing: Vec<&str> = entries
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "case {case:?}: expected every reported output to carry an OutputValue, \
         got none for {missing:?} in {body}"
    );
    assert_eq!(
        xml_texts(body, "OutputKey").len(),
        xml_texts(body, "OutputValue").len(),
        "case {case:?}: expected as many OutputValue members as OutputKey members in {body}"
    );
    entries
        .into_iter()
        .map(|(key, value)| (key, value.unwrap_or_default()))
        .collect()
}

/// What the outputs of a stack carrying `topics` must read, for a template that declares
/// `reads_an_entry` outputs.
fn expected_outputs(topics: &[&str], reads_an_entry: bool) -> BTreeMap<String, String> {
    let mut expected = BTreeMap::new();
    expected.insert(
        "Topics".to_string(),
        topics
            .iter()
            .map(|topic| topic_arn(topic))
            .collect::<Vec<String>>()
            .join(","),
    );
    if reads_an_entry {
        if let Some(first) = topics.first() {
            expected.insert("FirstTopic".to_string(), topic_arn(first));
        }
    }
    expected.insert("Literal".to_string(), LITERAL.to_string());
    expected
}

/// Assert the outputs a settled stack reports against `expected`, plus the one output whose
/// value the deployment MINTED (the handle's own identifier, which is not a value a case can
/// spell).
fn assert_outputs(case: &str, body: &str, expected: &BTreeMap<String, String>) {
    let outputs = readable_outputs(case, body);
    for (key, value) in expected {
        assert_eq!(
            outputs.get(key),
            Some(value),
            "case {case:?}: expected output {key:?} to read {value:?}, got {outputs:?}"
        );
    }
    let handle = outputs.get("HandleRef").map(String::as_str).unwrap_or("");
    assert!(
        !handle.is_empty(),
        "case {case:?}: expected the declared resource's Ref to resolve to its identifier, \
         got {outputs:?}"
    );
}

/// One row of the table: the topics the `CREATE` change set carries, and the ones the
/// `UPDATE` change set executed after it replaces them with.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    created_with: &'static [&'static str],
    updated_to: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a change set carrying no topic at all",
        stack_name: "cs-outputs-none",
        created_with: &[],
        updated_to: &["promoted"],
    },
    Row {
        case: "a change set carrying one topic, the recorded trace's own case",
        stack_name: "cs-outputs-one",
        created_with: &["deployments"],
        updated_to: &["audit", "oncall"],
    },
    Row {
        case: "a change set carrying the five topics the member admits at most",
        stack_name: "cs-outputs-five",
        created_with: &["one", "two", "three", "four", "five"],
        updated_to: &["deployments"],
    },
];

/// Every output of a stack an executed change set deployed is reported with its
/// `OutputValue`, resolved against the topic list that change set put on the stack.
#[tokio::test]
async fn an_executed_change_sets_stack_reports_every_output_with_its_value() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        // The `Fn::Select` output is declared only where BOTH revisions of this row's stack
        // carry a topic to select: an index no list of that length has is not a value this
        // table is about.
        let reads_an_entry = !row.created_with.is_empty() && !row.updated_to.is_empty();

        // The `CREATE` change set: the stack it leaves in review carries the template and the
        // topics only once this execution applies them.
        let mut members = vec![
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(1, reads_an_entry)),
        ];
        members.extend(notification_members(row.created_with));
        let (code, body) =
            create_change_set(&router, row.stack_name, "the-create-plan", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the CREATE change set to be accepted, got {code} with {body}"
        );
        settle_the_store(&router, &engine).await;
        let (code, body) =
            execute_change_set(&router, "the-create-plan", Some(row.stack_name)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the CREATE change set to execute, got {code} with {body}"
        );
        let (status, body) = settled(&router, &engine, case, row.stack_name).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the executed change set to complete the stack, \
             got {status} with {body}"
        );
        assert_outputs(
            case,
            &body,
            &expected_outputs(row.created_with, reads_an_entry),
        );

        // ...and an `UPDATE` change set that declares one more resource (so the plan carries a
        // change) and states a DIFFERENT topic list: the outputs the stack reports after it
        // executes are the ones resolved against the list now in force.
        let mut members = vec![
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &template(2, reads_an_entry)),
        ];
        members.extend(notification_members(row.updated_to));
        let (code, body) =
            create_change_set(&router, row.stack_name, "the-update-plan", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the UPDATE change set to be accepted, got {code} with {body}"
        );
        settle_the_store(&router, &engine).await;
        let (code, body) =
            execute_change_set(&router, "the-update-plan", Some(row.stack_name)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the UPDATE change set to execute, got {code} with {body}"
        );
        let (status, body) = settled(&router, &engine, case, row.stack_name).await;
        assert_eq!(
            status, "UPDATE_COMPLETE",
            "case {case:?}: expected the executed update to complete the stack, \
             got {status} with {body}"
        );
        assert_outputs(
            case,
            &body,
            &expected_outputs(row.updated_to, reads_an_entry),
        );
        // The stack reports the topics the update put on it, which is what the outputs above
        // resolved against.
        let reported: Vec<String> = list_members(&body, "NotificationARNs")
            .iter()
            .map(|entry| entry.trim().to_string())
            .collect();
        let expected: Vec<String> = row.updated_to.iter().map(|t| topic_arn(t)).collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stack to report the updated topics {expected:?}, \
             got {reported:?}"
        );
    }
}
