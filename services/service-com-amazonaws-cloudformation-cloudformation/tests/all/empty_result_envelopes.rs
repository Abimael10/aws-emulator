// SPDX-License-Identifier: CC0-1.0

//! The ENVELOPE an operation whose modelled output is `Unit` answers with, over the awsQuery
//! wire.
//!
//! **The rule every case here holds the service to: an operation whose output shape carries no
//! member still answers in its OWN envelope — `<<Operation>Response>` holding
//! `ResponseMetadata.RequestId` and nothing else — because that envelope is what an SDK reads
//! the call's success out of. An empty body, or another operation's element name, is not an
//! answer a client can parse.**
//!
//! The rows span the operations of this service whose output shape is `smithy.api#Unit`
//! (`smithy_query(query="operation",operation="SetStackPolicy,CancelUpdateStack,DeleteStack")`
//! reports `Unit` for each) that are driven from one stack: the policy write, the cancel of an
//! update in flight and the deletion. The other three the model declares
//! (`SignalResource`, `DeleteGeneratedTemplate`, `ExecuteStackRefactor`) each need a resource
//! of their own — a signalled resource, a generated template, a refactor — and are covered by
//! the suites that create those.

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::change_set_lifecycle::settle_the_store;
use crate::stack_lifecycle::{call, create_stack, member, router_with_engine, xml_text};

/// A template over a resource no owning service has to provision.
fn template(value: &str) -> String {
    format!(
        r#"{{"Resources":{{"Param":{{"Type":"AWS::SSM::Parameter","Properties":{{
             "Name":"/empty-result-envelopes/p","Type":"String","Value":"{value}"}}}}}}}}"#
    )
}

/// The stack policy a write submits: the document shape `SetStackPolicy` takes.
const POLICY: &str = r#"{"Statement":[{"Effect":"Allow","Action":"Update:*","Principal":"*","Resource":"*"}]}"#;

/// Assert one answer is the operation's own empty result: its envelope, a `RequestId` in it,
/// and no error element.
fn assert_empty_result(case: &str, operation: &str, http_status: u16, body: &str) {
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected {operation} to be answered, got {http_status} {body}"
    );
    let envelope = format!("<{operation}Response");
    assert!(
        body.contains(&envelope),
        "case {case:?}: expected the {envelope}> envelope, got {body:?}"
    );
    let request_id = xml_text(body, "RequestId").unwrap_or_default();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the envelope to carry a RequestId, got {body:?}"
    );
    assert!(
        !body.contains("<Error>"),
        "case {case:?}: an answered call carries no error element, got {body:?}"
    );
}

#[tokio::test]
async fn an_operation_with_no_output_members_answers_its_own_envelope() {
    let stack = "empty-result-envelopes";
    let (router, engine) = router_with_engine();

    let (http_status, body) = create_stack(&router, stack, &template("before"), &[]).await;
    assert_eq!(
        http_status, 200,
        "expected CreateStack to be accepted, got {http_status} {body}"
    );
    settle_the_store(&router, &engine).await;

    let (http_status, body) = call(
        &router,
        "SetStackPolicy",
        &[
            member("StackName", stack),
            member("StackPolicyBody", POLICY),
        ],
    )
    .await;
    assert_empty_result("the policy write", "SetStackPolicy", http_status, &body);

    // The cancel is answered only while an update is in flight, so one is started for it.
    let (http_status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template("after")),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "expected UpdateStack to be accepted, got {http_status} {body}"
    );
    let (http_status, body) =
        call(&router, "CancelUpdateStack", &[member("StackName", stack)]).await;
    assert_empty_result(
        "the cancel of an update in flight",
        "CancelUpdateStack",
        http_status,
        &body,
    );
    settle_the_store(&router, &engine).await;
    let settled = crate::stack_lifecycle::settled_stack(&router, "the cancelled update", stack)
        .await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_ROLLBACK_COMPLETE),
        "expected the cancelled update to be rolled back, got {settled}"
    );

    let (http_status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_empty_result("the deletion", "DeleteStack", http_status, &body);
}
