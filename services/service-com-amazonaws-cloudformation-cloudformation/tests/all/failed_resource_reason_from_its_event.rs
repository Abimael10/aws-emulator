// SPDX-License-Identifier: CC0-1.0

//! Where a per-resource view takes `ResourceStatusReason` from, end to end over the awsQuery
//! wire.
//!
//! RULE under test: **a resource in a FAILED status reports the diagnosis of that failure in
//! every per-resource view (`ListStackResources`, `DescribeStackResources`,
//! `DescribeStackResource`), and the diagnosis it reports is the one the resource's OWN EVENT
//! carries — so a view never reports that a resource failed while the stack's event log says
//! why. A resource in a success status reports no reason at all, whatever an earlier attempt on
//! it said.**
//!
//! Why it matters: the per-resource reason is the only machine-readable explanation of why a
//! deployment failed. A failed row without it reports that a resource failed and nothing about
//! the cause, so a client — or `cdk deploy`'s own output — reports a failure an operator cannot
//! act on, and a row carrying one for a resource that SUCCEEDED makes "is there a reason?"
//! useless as the test for "did this resource fail?".
//!
//! `failed_resource_status_reason.rs` is the suite for the reason a failure WRITES on the
//! record; this one is about where a view reads it from, which is what keeps the two from
//! disagreeing: the rows blank the record's own copy — the state any path that records a
//! resource's status again leaves — and read all three views back.

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, member, router_with_engine, store, xml_blocks, xml_text, REGION};
use crate::stack_lifecycle::describe_stack;

/// The stack every case here deploys: one parameter the create makes, and one restating that
/// same name, whose creation CloudFormation itself refuses — so the stack holds a resource
/// that FAILED beside one that succeeded, with the failure kept where a caller reads it
/// ("DO_NOTHING — if the stack creation fails, do nothing").
const STACK: &str = "reason-from-the-event";

fn template() -> String {
    format!(
        r#"{{"Resources":{{
            "Made":{{"Type":"AWS::SSM::Parameter",
                     "Properties":{{"Name":"/{STACK}/made","Type":"String","Value":"made"}}}},
            "Clash":{{"Type":"AWS::SSM::Parameter","DependsOn":"Made",
                      "Properties":{{"Name":"/{STACK}/made","Type":"String",
                                     "Value":"twice"}}}}}}}}"#
    )
}

/// The logical ids the template declares: the one whose create is refused, and the one that
/// succeeds beside it.
const FAILED: &str = "Clash";
const SUCCEEDED: &str = "Made";

/// The `ResourceStatusReason` each view reports for one logical id, read out of the member
/// that carries it: `(the view's name, the reason it reports)`.
async fn reported_reasons(
    router: &MultiServiceRouter,
    logical_id: &str,
) -> Vec<(&'static str, Option<String>)> {
    let mut read = Vec::new();
    for (view, block) in [
        ("ListStackResources", "member"),
        ("DescribeStackResources", "member"),
    ] {
        let (code, body) = call(router, view, &[member("StackName", STACK)]).await;
        assert_eq!(code, 200, "{view} must answer 200, got {code} {body}");
        let row = xml_blocks(&body, block)
            .into_iter()
            .find(|row| xml_text(row, "LogicalResourceId").as_deref() == Some(logical_id))
            .unwrap_or_else(|| panic!("{view}: expected a row for {logical_id:?}, got {body}"));
        read.push((view, xml_text(&row, "ResourceStatusReason")));
    }
    let view = "DescribeStackResource";
    let (code, body) = call(
        router,
        view,
        &[
            member("StackName", STACK),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(code, 200, "{view} must answer 200, got {code} {body}");
    read.push((view, xml_text(&body, "ResourceStatusReason")));
    read
}

#[tokio::test]
async fn a_failed_resource_reports_the_reason_its_own_event_carries() {
    let (router, engine) = router_with_engine();
    let (code, body) = create_stack(
        &router,
        STACK,
        &template(),
        &[member("OnFailure", "DO_NOTHING")],
    )
    .await;
    assert_eq!(code, 200, "CreateStack must be accepted, got {code} {body}");
    crate::change_set_lifecycle::settle_the_store(&router, &engine).await;
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_FAILED),
        "the failed state must be kept for the views to be read, got {described}"
    );

    // What the failure WROTE: the record's own diagnosis, which every view reports as it
    // stands. Read once, so the rows below assert the views report THAT sentence rather than
    // one of this suite's own.
    let written = reported_reasons(&router, FAILED).await;
    let diagnosis = written
        .first()
        .and_then(|(_, reason)| reason.clone())
        .unwrap_or_default();
    assert!(
        !diagnosis.is_empty(),
        "the failed resource must carry a diagnosis to begin with, got {written:?}"
    );
    for (view, reason) in &written {
        assert_eq!(
            reason.as_deref(),
            Some(diagnosis.as_str()),
            "case \"the record carries its own diagnosis\" on {view}: expected \
             {diagnosis:?}, got {reason:?}"
        );
    }

    // ...and the resource that SUCCEEDED carries none, in any view.
    for (view, reason) in reported_reasons(&router, SUCCEEDED).await {
        assert_eq!(
            reason, None,
            "case \"a resource whose status speaks for itself\" on {view}: expected no \
             reason, got {reason:?}"
        );
    }

    // Now the state any path that records a resource's status again leaves: the record in its
    // failed status with no diagnosis of its own. The event log still carries the failure, so
    // every view goes on reporting it — the views and the event stream cannot disagree.
    {
        let state = store(&engine);
        let mut held = state.lock();
        let stack = held
            .resolve_mut(REGION, STACK)
            .expect("the stack just deployed");
        let record = stack
            .resources
            .iter_mut()
            .find(|deployed| deployed.resource.logical_id == FAILED)
            .expect("a record for the failed resource");
        assert_eq!(
            record.status, resource_status::CREATE_FAILED,
            "the record under test must be the failed one, got {:?}",
            record.status
        );
        record.status_reason = None;
    }
    for (view, reason) in reported_reasons(&router, FAILED).await {
        assert_eq!(
            reason.as_deref(),
            Some(diagnosis.as_str()),
            "case \"the record carries no diagnosis of its own\" on {view}: expected the \
             event's {diagnosis:?}, got {reason:?}"
        );
    }

    // ...and an EMPTY one is no diagnosis either: a client reads the member to learn why a
    // resource failed, so an empty string is the same absence as a missing member.
    {
        let state = store(&engine);
        let mut held = state.lock();
        let stack = held
            .resolve_mut(REGION, STACK)
            .expect("the stack just deployed");
        let record = stack
            .resources
            .iter_mut()
            .find(|deployed| deployed.resource.logical_id == FAILED)
            .expect("a record for the failed resource");
        record.status_reason = Some(String::new());
    }
    for (view, reason) in reported_reasons(&router, FAILED).await {
        assert_eq!(
            reason.as_deref(),
            Some(diagnosis.as_str()),
            "case \"the record carries an empty diagnosis\" on {view}: expected the event's \
             {diagnosis:?}, got {reason:?}"
        );
    }

    // The fallback is a FAILED resource's: a resource whose record says it SUCCEEDED reports
    // nothing, even where an earlier failure of it is still in the event log.
    {
        let state = store(&engine);
        let mut held = state.lock();
        let stack = held
            .resolve_mut(REGION, STACK)
            .expect("the stack just deployed");
        let record = stack
            .resources
            .iter_mut()
            .find(|deployed| deployed.resource.logical_id == FAILED)
            .expect("a record for the failed resource");
        record.status = resource_status::CREATE_COMPLETE.to_string();
        record.status_reason = None;
    }
    for (view, reason) in reported_reasons(&router, FAILED).await {
        assert_eq!(
            reason, None,
            "case \"a record in a success status with a failure in the log\" on {view}: \
             expected no reason, got {reason:?}"
        );
    }
}
