// SPDX-License-Identifier: CC0-1.0

//! Redeploying a stack through change sets that all carry ONE name, which is what a deployment
//! tool does: before it plans the next deployment it discards the change set the last one
//! deployed under, because "the name must be unique among all change sets that are associated
//! with the specified stack".
//!
//! RULE: a change set is a RECORD that lives until a caller discards it, and `DeleteChangeSet`
//! discards the PLAN it describes — so a record whose plan was already applied stops being
//! reported by every read of the stack's change sets, and the next deployment can plan under
//! the name the last one deployed under. Every round therefore reads back as a change set of
//! its own (its own ARN, its own plan), the stack keeps every change its executions carried
//! out, and the name resolves to the record the LAST round created. What the store KEEPS of a
//! discarded record is bounded, oldest reclaimed first, so deploying in a loop cannot grow it
//! without limit.
//!
//! The table below is the rounds: the first brings the stack into existence from a `CREATE`
//! change set and each later one updates it from an `UPDATE` change set, so the rule is held
//! over both change-set types and over a name re-used more than once, rather than over one
//! deployment.

use std::collections::BTreeSet;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    execution_status, status, MAX_DISCARDED_CHANGE_SETS_RETAINED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{call, member, refusal, router_with_engine, store, xml_text, xml_texts};
use crate::stack_lifecycle::describe_stack;

/// The ONE change-set name every round of the deployment plans under.
const DEPLOY_SET: &str = "deploy-change-set";

/// The template of one round: the topic every round declares, plus one resource per round so
/// each round's plan is a change against what the round before it left in force.
fn template(round: usize) -> String {
    let mut resources = vec![r#""A":{"Type":"AWS::SNS::Topic"}"#.to_string()];
    for step in 1..=round {
        resources.push(format!(
            r#""Added{step}":{{"Type":"AWS::SNS::Topic"}}"#
        ));
    }
    format!(r#"{{"Resources":{{{}}}}}"#, resources.join(","))
}

async fn delete_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    stack: &str,
) -> (u16, String) {
    call(
        router,
        "DeleteChangeSet",
        &[
            member("ChangeSetName", change_set),
            member("StackName", stack),
        ],
    )
    .await
}

/// The logical ids `DescribeStackResources` reports for one stack.
async fn deployed_logical_ids(router: &MultiServiceRouter, stack: &str) -> BTreeSet<String> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "DescribeStackResources must succeed, got {code} {body}"
    );
    xml_texts(&body, "LogicalResourceId").into_iter().collect()
}

#[tokio::test]
async fn a_redeployment_plans_under_the_name_the_last_one_deployed_under() {
    let (router, engine) = router_with_engine();
    let stack = "redeployed-under-one-name";
    // The ARNs each round's change set reported, so no round can be answered with another
    // round's record.
    let mut arns: Vec<String> = Vec::new();

    for round in 0..3usize {
        let case = format!("round {round}");
        // Every round after the first discards the record the round before it deployed under
        // — the executed change set — before it plans anything.
        if round > 0 {
            let (code, body) = delete_change_set(&router, DEPLOY_SET, stack).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the record the last round deployed under must be discardable, \
                 got {code} {body}"
            );
            let (code, body) = describe_change_set(&router, DEPLOY_SET, Some(stack)).await;
            assert_eq!(
                (code, refusal(&body).0),
                (404, "ChangeSetNotFound".to_string()),
                "case {case:?}: the discarded record must read back as not found, got {code} \
                 {body}"
            );
        }

        let change_set_type = if round == 0 { "CREATE" } else { "UPDATE" };
        let (code, body) = create_change_set(
            &router,
            stack,
            DEPLOY_SET,
            &[
                member("ChangeSetType", change_set_type),
                member("TemplateBody", &template(round)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the round must plan under the shared name, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        let (code, described) = describe_change_set(&router, DEPLOY_SET, Some(stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the planned change set must be readable, got {code} {described}"
        );
        let arn = xml_text(&described, "ChangeSetId").unwrap_or_default();
        assert!(
            !arns.contains(&arn),
            "case {case:?}: expected a record of its own, got the ARN {arn:?} a round before \
             it already reported"
        );
        assert_eq!(
            xml_text(&described, "ExecutionStatus").unwrap_or_default(),
            execution_status::AVAILABLE,
            "case {case:?}: expected a plan ready to execute, got {described}"
        );
        arns.push(arn.clone());

        let (code, body) = execute_change_set(&router, DEPLOY_SET, Some(stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the round's plan must execute, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        // The round deployed: the stack carries every resource the rounds so far declared and
        // reports the status this round's deployment reaches, and the record reports the
        // execution it carried out.
        let expected_status = if round == 0 {
            status::CREATE_COMPLETE
        } else {
            status::UPDATE_COMPLETE
        };
        let (code, body) = describe_stack(&router, stack).await;
        assert_eq!(
            (code, xml_text(&body, "StackStatus").unwrap_or_default()),
            (200, expected_status.to_string()),
            "case {case:?}: expected the stack in {expected_status}, got {code} {body}"
        );
        let expected_ids: BTreeSet<String> = ["A".to_string()]
            .into_iter()
            .chain((1..=round).map(|step| format!("Added{step}")))
            .collect();
        assert_eq!(
            deployed_logical_ids(&router, stack).await,
            expected_ids,
            "case {case:?}: expected the stack to carry every round's resources"
        );
        let (code, described) = describe_change_set(&router, DEPLOY_SET, Some(stack)).await;
        assert_eq!(
            (
                code,
                xml_text(&described, "ChangeSetId").unwrap_or_default(),
                xml_text(&described, "ExecutionStatus").unwrap_or_default(),
            ),
            (
                200,
                arn.clone(),
                execution_status::EXECUTE_COMPLETE.to_string()
            ),
            "case {case:?}: the name must resolve to this round's executed record, got \
             {described}"
        );
    }
}

/// The bound on what the store KEEPS of the records a delete discarded: past
/// `MAX_DISCARDED_CHANGE_SETS_RETAINED` the oldest is reclaimed, so a caller that deploys and
/// discards in a loop cannot grow the store without limit. The records are seeded in the store
/// rather than deployed one by one, because what is asserted is the store's own reclamation and
/// a few hundred deployments would say nothing more about it.
#[tokio::test]
async fn what_is_kept_of_the_discarded_records_is_bounded() {
    let (router, engine) = router_with_engine();
    let stack = "discarded-bound";

    // One real deployment, discarded the way a redeployment discards it: the record this bound
    // is about.
    let (code, body) = create_change_set(
        &router,
        stack,
        DEPLOY_SET,
        &[
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(0)),
        ],
    )
    .await;
    assert_eq!(code, 200, "the change set must be created, got {code} {body}");
    settle_the_store(&router, &engine).await;
    let (code, body) = execute_change_set(&router, DEPLOY_SET, Some(stack)).await;
    assert_eq!(code, 200, "the change set must execute, got {code} {body}");
    settle_the_store(&router, &engine).await;
    let (code, body) = delete_change_set(&router, DEPLOY_SET, stack).await;
    assert_eq!(code, 200, "the executed record must be discardable, got {code} {body}");

    // ...and as many more of them as it takes to put the store past the bound, each a record of
    // its own.
    let seeded = MAX_DISCARDED_CHANGE_SETS_RETAINED + 50;
    {
        let state = store(&engine);
        let mut held = state.lock();
        let discarded = held
            .change_sets
            .iter()
            .find(|change_set| change_set.discarded)
            .cloned()
            .expect("the delete above leaves one discarded record");
        for index in 0..seeded {
            let mut copy = discarded.clone();
            copy.change_set_id = format!("{}-{index}", discarded.change_set_id);
            held.change_sets.push(copy);
        }
    }

    // Any call settles the store, which is where the retention is applied.
    let (code, body) = call(&router, "ListStacks", &[]).await;
    assert_eq!(code, 200, "the settling read must succeed, got {code} {body}");
    let state = store(&engine);
    let held = state.lock();
    let kept = held
        .change_sets
        .iter()
        .filter(|change_set| change_set.discarded)
        .count();
    assert_eq!(
        kept, MAX_DISCARDED_CHANGE_SETS_RETAINED,
        "expected the store to keep {MAX_DISCARDED_CHANGE_SETS_RETAINED} discarded records of \
         the {} it was given, got {kept}",
        seeded + 1
    );
    // What is reclaimed is the OLDEST, so the records the last deletes left are the ones kept.
    let newest = format!("-{}", seeded - 1);
    assert!(
        held.change_sets
            .iter()
            .any(|change_set| change_set.change_set_id.ends_with(&newest)),
        "expected the newest discarded record to survive the reclamation"
    );
}
