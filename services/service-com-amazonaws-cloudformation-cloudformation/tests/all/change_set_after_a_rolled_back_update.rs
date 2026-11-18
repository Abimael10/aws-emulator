// SPDX-License-Identifier: CC0-1.0

//! What a change set of a stack whose UPDATE FAILED is computed against.
//!
//! **The rule every case here holds the emulator to:** a change set is "a list of changes that
//! will be applied to a stack", so it is computed against the configuration the stack's
//! resources really CARRY — their properties and the stack TAG SET they were given. A failed
//! update that rolled back deployed nothing — "if the stack update fails, CloudFormation rolls
//! back the stack to the previous stack configuration" — so the configuration carried is the
//! one in force before that update; a failed update that was left standing
//! (`DisableRollback`) left each resource it could not change carrying that same
//! configuration. Either way a change set of the revision the stack carries proposes nothing:
//! it settles `FAILED` with CloudFormation's no-changes reason, reports an empty change list
//! and is not offered for execution. The revision the failed update submitted, and a revision
//! stating the tags differently, are the ones that propose changes.
//!
//! Why it matters: a client that re-plans after a failed deploy reads the change set to learn
//! whether the stack still differs from the revision it means to have. A change set that
//! reports work against a revision the stack already carries sends that client into an
//! execution with nothing to do — and one that silently plans the FAILED revision back in
//! would re-apply the very change that broke the stack.
//!
//! The rows span what the rule generalises over: HOW MANY resources the failed update broke
//! (one, two, three), whether the update also carried a change the owning service APPLIED
//! beside the ones it refused (the functions' `Description`), how the request asked the
//! failure to be handled (the stack rolling itself back, and the failed state kept), and how
//! many tags the stack carries (none, one, two). Each row also drives the ADMITTED side on the
//! same stack: a change set of a revision the stack does NOT carry is offered for execution
//! with the change it proposes. The broken property is AWS Lambda's own: an alias
//! pointed at a version its function never published is refused by the service that owns it.
//!
//! Every call goes over the awsQuery wire, and each alias is read back through AWS Lambda's own
//! wire, so what is asserted is what a client sees.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::failed_update_resource_rollback::{
    alias_version, function_name, template, CREATED_DESCRIPTION, HEALTHY_VERSION,
    UPDATED_DESCRIPTION,
};
use crate::harness::{call, create_stack, member, xml_blocks, xml_text, xml_texts};
use crate::stack_lifecycle::acknowledging_iam;

/// CloudFormation's own `StatusReason` on a change set that would change nothing.
const NO_CHANGES_REASON: &str = "The submitted information didn't contain changes. Submit \
                                 different information to create a change set.";

/// How long a case waits for a stack operation to settle before giving up: generous, since
/// nothing here is timed — the wait polls for the CONDITION it needs.
const SETTLING_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: the stack, the version each of its aliases is pointed at by the failed update, and
/// whether that update also changed the functions' `Description`.
struct Row {
    case: &'static str,
    stack: &'static str,
    missing_versions: &'static [&'static str],
    changed_the_description: bool,
}

/// The class the rule covers.
const ROWS: &[Row] = &[
    Row {
        case: "one resource the update could not change",
        stack: "cfnrb-diff-one",
        missing_versions: &["593"],
        changed_the_description: false,
    },
    Row {
        case: "two resources the update could not change",
        stack: "cfnrb-diff-two",
        missing_versions: &["4", "17"],
        changed_the_description: false,
    },
    Row {
        case: "three resources, beside a change the owning service applied",
        stack: "cfnrb-diff-three",
        missing_versions: &["31", "77", "901"],
        changed_the_description: true,
    },
];

/// One change a `DescribeChangeSet` reports: the logical id, the action, the replacement
/// decision and the names of the targets it details.
type ReportedChange = (String, String, String, Vec<String>);

/// The changes a `DescribeChangeSet` response carries, in the order it reports them.
fn reported_changes(xml: &str) -> Vec<ReportedChange> {
    xml_blocks(xml, "ResourceChange")
        .iter()
        .map(|change| {
            let details = xml_blocks(change, "Details")
                .first()
                .map(|details| {
                    xml_blocks(details, "Target")
                        .iter()
                        .filter_map(|target| xml_text(target, "Name"))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            (
                xml_text(change, "LogicalResourceId").unwrap_or_default(),
                xml_text(change, "Action").unwrap_or_default(),
                xml_text(change, "Replacement").unwrap_or_default(),
                details,
            )
        })
        .collect()
}

/// The `StackStatus` the stack reports right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status_code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {status_code} {body}"
    );
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// Settle the store and answer the stack's status once no operation is in flight.
async fn settled_status(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> String {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let status = stack_status(router, case, stack).await;
        if !status.ends_with("_IN_PROGRESS") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled, still {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Deploy the healthy revision of the Lambda estate and then break it, letting the stack roll
/// ITSELF back: every alias is pointed at a version its function never published.
///
/// Answers once the stack rests in `UPDATE_ROLLBACK_COMPLETE`, having asserted that AWS Lambda
/// holds each alias at the version it had before — the configuration the rule below is about.
async fn rolled_back(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    row: &Row,
) {
    let healthy: Vec<&str> = row
        .missing_versions
        .iter()
        .map(|_| HEALTHY_VERSION)
        .collect();
    let deployed = template(row.stack, &healthy, CREATED_DESCRIPTION);
    let (status_code, body) =
        create_stack(router, row.stack, &deployed, &acknowledging_iam(&[])).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
    );
    let created = settled_status(router, engine, case, row.stack).await;
    assert_eq!(
        created, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {created:?}"
    );

    let description = if row.changed_the_description {
        UPDATED_DESCRIPTION
    } else {
        CREATED_DESCRIPTION
    };
    let broken = template(row.stack, row.missing_versions, description);
    let mut members = vec![
        member("StackName", row.stack),
        member("TemplateBody", &broken),
    ];
    members.extend(acknowledging_iam(&[]));
    let (status_code, body) = call(router, "UpdateStack", &members).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {status_code} {body}"
    );
    let settled = settled_status(router, engine, case, row.stack).await;
    assert_eq!(
        settled, "UPDATE_ROLLBACK_COMPLETE",
        "case {case:?}: expected the failed update to roll the stack back, got {settled:?}"
    );
    for index in 0..row.missing_versions.len() {
        let function = function_name(row.stack, index);
        let invoked = alias_version(router, &function).await;
        assert_eq!(
            invoked.as_deref(),
            Some(HEALTHY_VERSION),
            "case {case:?}: expected AWS Lambda to hold the alias of {function:?} invoking \
             {HEALTHY_VERSION:?}, got {invoked:?}"
        );
    }
}

/// One row of the tag table: the case, the stack it names, the tags that stack carries and
/// the version its alias is broken with.
type TagRow = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

/// RULE, on the stack that keeps its failed state: the stack's own TAG SET is part of the
/// configuration a change set is computed against, so a revision restating the tags in force
/// plans no tag change — whatever status the stack's last operation left it in. A stack parked
/// in `UPDATE_FAILED` (`DisableRollback`) is the case where that is not the template's doing:
/// its resources carry the set in force, and the revision that restates it proposes nothing.
#[tokio::test]
async fn a_change_set_restating_the_tags_in_force_on_a_parked_stack_proposes_nothing() {
    // (the case, the stack, the tags it carries, the version its alias is broken with)
    let rows: [TagRow; 3] = [
        ("one tag", "cfnrb-tags-one", &[("owner", "platform")], "593"),
        (
            "two tags",
            "cfnrb-tags-two",
            &[("owner", "platform"), ("tier", "gold")],
            "4",
        ),
        ("no tags at all", "cfnrb-tags-none", &[], "17"),
    ];

    for (case, stack, tags, missing_version) in rows {
        let (router, engine) = crate::harness::router_with_engine();
        let tag_members: Vec<(String, String)> = tags
            .iter()
            .enumerate()
            .flat_map(|(index, (key, value))| {
                let position = index + 1;
                [
                    member(&format!("Tags.member.{position}.Key"), key),
                    member(&format!("Tags.member.{position}.Value"), value),
                ]
            })
            .collect();

        let stable = template(stack, &[HEALTHY_VERSION], CREATED_DESCRIPTION);
        let mut created_members = acknowledging_iam(&[]);
        created_members.extend(tag_members.clone());
        let (status_code, body) = create_stack(&router, stack, &stable, &created_members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
        );
        let created = settled_status(&router, &engine, case, stack).await;
        assert_eq!(
            created, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {created:?}"
        );

        // The update is refused by AWS Lambda and the failed state is KEPT, so the stack's
        // resources carry the configuration — and the tag set — in force.
        let broken = template(stack, &[missing_version], CREATED_DESCRIPTION);
        let mut members = vec![
            member("StackName", stack),
            member("TemplateBody", &broken),
            member("DisableRollback", "true"),
        ];
        members.extend(acknowledging_iam(&[]));
        members.extend(tag_members.clone());
        let (status_code, body) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected UpdateStack to be accepted, got {status_code} {body}"
        );
        let settled = settled_status(&router, &engine, case, stack).await;
        assert_eq!(
            settled, "UPDATE_FAILED",
            "case {case:?}: expected the failed update to park the stack, got {settled:?}"
        );

        // The change set of the revision the stack carries, restating its tags.
        let mut change_set_members = acknowledging_iam(&[
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &stable),
        ]);
        change_set_members.extend(tag_members.clone());
        let (status_code, created) = create_change_set(
            &router,
            stack,
            "of-the-carried-revision",
            &change_set_members,
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-the-carried-revision", Some(stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        let reported = reported_changes(&described);
        assert!(
            reported.is_empty(),
            "case {case:?}: expected the carried revision to propose nothing, got {reported:?}"
        );
        assert_eq!(
            (
                xml_text(&described, "Status").unwrap_or_default(),
                xml_text(&described, "StatusReason").unwrap_or_default(),
            ),
            ("FAILED".to_string(), NO_CHANGES_REASON.to_string()),
            "case {case:?}: expected the no-changes failure, got {described}"
        );

        // The admitted side, on the same stack: a revision that states the tags DIFFERENTLY
        // plans the tag change on every resource the stack holds, and nothing else.
        let mut other_members = acknowledging_iam(&[
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &stable),
            member("Tags.member.1.Key", "owner"),
            member("Tags.member.1.Value", "another-team"),
        ]);
        other_members.retain(|(key, _)| key != "Tags.member.2.Key" && key != "Tags.member.2.Value");
        let (status_code, created) =
            create_change_set(&router, stack, "of-another-tag-set", &other_members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-another-tag-set", Some(stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        let reported = reported_changes(&described);
        assert!(
            !reported.is_empty()
                && reported
                    .iter()
                    .all(|(_, action, _, details)| action == "Modify" && details.is_empty()),
            "case {case:?}: expected one tag modification per held resource, got {reported:?}"
        );
        assert_eq!(
            xml_text(&described, "Status").unwrap_or_default(),
            "CREATE_COMPLETE",
            "case {case:?}: expected a change set with changes to be offered, got {described}"
        );
    }
}

/// RULE: a change set of the revision the rollback put back proposes nothing — and one of a
/// revision the stack does not carry is offered for execution with the change it proposes.
#[tokio::test]
async fn a_change_set_of_a_rolled_back_stack_is_computed_against_the_restored_revision() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = crate::harness::router_with_engine();
        rolled_back(&router, &engine, case, row).await;

        // ---- The change set of the revision the rollback put back. ----
        let healthy: Vec<&str> = row
            .missing_versions
            .iter()
            .map(|_| HEALTHY_VERSION)
            .collect();
        let stable = template(row.stack, &healthy, CREATED_DESCRIPTION);
        let (status_code, created) = create_change_set(
            &router,
            row.stack,
            "of-the-restored-revision",
            &acknowledging_iam(&[
                member("ChangeSetType", "UPDATE"),
                member("TemplateBody", &stable),
            ]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-the-restored-revision", Some(row.stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        let reported = reported_changes(&described);
        assert!(
            reported.is_empty(),
            "case {case:?}: expected the restored revision to propose nothing, got {reported:?}"
        );
        assert_eq!(
            (
                xml_text(&described, "Status").unwrap_or_default(),
                xml_text(&described, "StatusReason").unwrap_or_default(),
                xml_text(&described, "ExecutionStatus").unwrap_or_default(),
            ),
            (
                "FAILED".to_string(),
                NO_CHANGES_REASON.to_string(),
                "UNAVAILABLE".to_string()
            ),
            "case {case:?}: expected a change set with no changes to fail with the no-changes \
             reason and no plan to execute, got {described}"
        );

        // ---- The admitted side, on the same stack: a revision it does NOT carry. ----
        let revised = template(row.stack, &healthy, UPDATED_DESCRIPTION);
        let (status_code, created) = create_change_set(
            &router,
            row.stack,
            "of-another-revision",
            &acknowledging_iam(&[
                member("ChangeSetType", "UPDATE"),
                member("TemplateBody", &revised),
            ]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-another-revision", Some(row.stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        let expected: Vec<ReportedChange> = (0..row.missing_versions.len())
            .map(|index| {
                (
                    format!("Fn{index}"),
                    "Modify".to_string(),
                    "False".to_string(),
                    vec!["Description".to_string()],
                )
            })
            .collect();
        assert_eq!(
            reported_changes(&described),
            expected,
            "case {case:?}: expected the other revision to propose {expected:?}, got {described}"
        );
        assert_eq!(
            (
                xml_text(&described, "Status").unwrap_or_default(),
                xml_text(&described, "ExecutionStatus").unwrap_or_default(),
            ),
            ("CREATE_COMPLETE".to_string(), "AVAILABLE".to_string()),
            "case {case:?}: expected a change set with changes to be offered for execution, \
             got {described}"
        );
        // Nothing of the FAILED revision is deployed, so the stack still reports the restored
        // one: the change sets above read it, they do not write it.
        let (status_code, body) = call(
            &router,
            "GetTemplate",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected GetTemplate to answer, got {status_code} {body}"
        );
        let in_force = xml_texts(&body, "TemplateBody").join("");
        for version in row.missing_versions {
            assert!(
                !in_force.contains(&format!("\"FunctionVersion\":\"{version}\"")),
                "case {case:?}: expected the restored template to carry none of the failed \
                 revision's versions, got {in_force}"
            );
        }
    }
}
