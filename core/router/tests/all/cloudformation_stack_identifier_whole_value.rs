// SPDX-License-Identifier: CC0-1.0

//! A stack NAME a stack could not carry, sent to the operation that would create it and to
//! the operations that would read it, on the assembled emulator.
//!
//! The rule under test: **a constraint expression is satisfied by the WHOLE of the value, so
//! a value only PART of which is a stack name is refused for its own shape** — by the create,
//! which holds a new stack's name to the rule CloudFormation documents for it ("A stack name
//! can contain only alphanumeric characters (case sensitive) and hyphens. It must start with
//! an alphabetical character"), and by every read that takes a stack IDENTIFIER, which holds
//! the value to the name-or-ARN expression CloudFormation's own input validation states and
//! refuses it BEFORE the stack is resolved. A caller that sent a name no stack could carry is
//! told by both halves of the round trip that its request is malformed, rather than being
//! told by the read that the stack it was just refused does not exist.
//!
//! The table spans what the rule generalises over: a character outside a stack name's
//! alphabet at the END of an otherwise well-formed name, at its START and in its MIDDLE, over
//! five such characters (underscore, dot, space, slash and plus), plus a value whose run is
//! ARN-only without the `arn:` prefix — each crossed with the create and with the six reads
//! that take a stack identifier. The controls are the two forms that ARE wholly addressable: a
//! well-formed name, created and read back, and the stack's own id.
//!
//! It lives here rather than in the CloudFormation crate because the stack it stands up
//! really creates its declared resource in the owning service (Amazon SNS), so the control
//! rows need both services mounted on one engine.
//!
//! Source of the two expressions: CloudFormation's own input validation, which the service
//! states as `[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*` for an identifier and
//! `[a-zA-Z][-a-zA-Z0-9]*` for a new stack's name — the model carries no `@pattern` for
//! `StackName` (`smithy_query(query="shape", shape="StackName")`), so the service is what
//! states it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The text of the first `<name>` element of an answer, with the five entities the XML
/// serializer escapes decoded back by the shared reader ([`elements`]) — a refusal quotes the
/// value the request sent, which carries characters the serializer escapes.
fn message_of(body: &str, name: &str) -> Option<String> {
    elements(body, name).into_iter().next()
}

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a stack is given to reach a terminal status, and how long between two looks: the
/// CONDITION is what is waited for, and the deadline is only a safety net so a wait that can
/// never be satisfied still ends.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The template the control rows deploy, so the name the rule admits really creates a stack.
const TEMPLATE: &str = r#"{"Resources":{"First":{"Type":"AWS::SNS::Topic",
     "Properties":{"DisplayName":"whole-value-first"}}}}"#;

/// The expression CloudFormation states for a stack IDENTIFIER, quoted in a read's refusal
/// exactly as the service states it.
const IDENTIFIER_PATTERN: &str = "[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*";

/// The expression the CREATE states for a new stack's NAME, which has no ARN alternative: a
/// stack is created by name, never by ARN.
const NAME_PATTERN: &str = "[a-zA-Z][-a-zA-Z0-9]*";

/// One read that takes a stack identifier: the action, the member path its refusal states the
/// identifier at, and the members it needs beside the identifier.
struct Read {
    action: &'static str,
    path: &'static str,
    extra: &'static [(&'static str, &'static str)],
}

/// Every read that takes a stack identifier.
const READS: &[Read] = &[
    Read {
        action: "DescribeStacks",
        path: "stackName",
        extra: &[],
    },
    Read {
        action: "DescribeStackEvents",
        path: "stackName",
        extra: &[],
    },
    Read {
        action: "GetTemplate",
        path: "stackName",
        extra: &[],
    },
    Read {
        action: "GetStackPolicy",
        path: "stackName",
        extra: &[],
    },
    Read {
        action: "ListStackResources",
        path: "stackNameOrId",
        extra: &[],
    },
    Read {
        action: "DescribeStackResource",
        path: "stackName",
        extra: &[("LogicalResourceId", "First")],
    },
];

/// Values only part of which is a stack name: a character outside the alphabet at every
/// position a value can carry one, over five such characters.
const PARTLY_A_NAME: &[(&str, &str)] = &[
    (
        "an underscore and a dot after the name run",
        "whole-value-bad_invalid.name",
    ),
    ("an underscore at the end", "whole-value-bad_"),
    ("an underscore at the start", "_whole-value-bad"),
    ("an underscore in the middle", "whole_value_bad"),
    ("a dot at the end", "whole-value-bad."),
    ("a dot in the middle", "whole.value.bad"),
    ("a space in the middle", "whole value bad"),
    ("a trailing space", "whole-value-bad "),
    ("a slash in the middle", "whole/value/bad"),
    // A character only the ARN alternative admits, after a name run: neither alternative
    // describes the whole of the value.
    ("a plus after the name run", "whole-value-bad+1"),
    (
        "an ARN-only run with no arn: prefix",
        "aws:cloudformation:stack/x",
    ),
];

/// The emulator with the two services these rows exercise, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names CloudFormation, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/cloudformation/\
         aws4_request, SignedHeaders=host, Signature=00000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization())
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// How a refusal of `value` at `path` against `pattern` reads, in the framework's envelope.
fn constraint_refusal(path: &str, value: &str, pattern: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {pattern}"
    )
}

/// Create `name`'s stack and wait until it is out of its `_IN_PROGRESS` status, answering the
/// status it settled at.
async fn create_and_settle(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", name), ("TemplateBody", TEMPLATE)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected CreateStack to be accepted for {name:?}, got {status} {body}"
    );
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", name)]).await;
        assert_eq!(
            status, 200,
            "expected the stack to be readable, got {status} {body}"
        );
        let settled = element(&body, "StackStatus");
        if !settled.ends_with("_IN_PROGRESS") {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "the stack never left its in-progress status, last seen {settled:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_name_only_part_of_which_is_a_name_is_refused_by_the_create_and_by_every_read() {
    let router = build_router();
    // A stack that really is there, so no row passes by the emulator being unable to serve
    // anything: the VALUE is what each row changes.
    let settled = create_and_settle(&router, "whole-value-live").await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "expected the control stack to be created, got {settled:?}"
    );

    for (case, value) in PARTLY_A_NAME {
        // The CREATE refuses the name for the name rule it documents, which has no ARN
        // alternative.
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", value), ("TemplateBody", TEMPLATE)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected CreateStack to be refused, got {status} {body}"
        );
        assert_eq!(
            message_of(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {case:?}: expected <Code>ValidationError</Code>, got {body}"
        );
        let expected = constraint_refusal("stackName", value, NAME_PATTERN);
        assert_eq!(
            message_of(&body, "Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {body}"
        );

        // ...and every READ refuses it for the identifier expression, ahead of the lookup:
        // the answer names the constraint rather than a stack that is not there.
        for read in READS {
            let mut members: Vec<(&str, &str)> = vec![("StackName", value)];
            members.extend_from_slice(read.extra);
            let (status, body) = cloudformation(&router, read.action, &members).await;
            let case = format!("{} naming {case}", read.action);
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            assert_eq!(
                message_of(&body, "Code").as_deref(),
                Some("ValidationError"),
                "case {case:?}: expected <Code>ValidationError</Code>, got {body}"
            );
            let expected = constraint_refusal(read.path, value, IDENTIFIER_PATTERN);
            assert_eq!(
                message_of(&body, "Message").as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {body}"
            );
        }
    }
}

/// RULE: the control — a value the expression describes WHOLLY still reaches the stack, so the
/// refusals above are the constraint and not a blanket rejection. Both addressable forms are
/// read: the stack's name and its whole stack id.
#[tokio::test]
async fn a_value_the_expression_describes_wholly_still_reaches_the_stack() {
    let router = build_router();
    let settled = create_and_settle(&router, "whole-value-served").await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "expected the stack to be created, got {settled:?}"
    );
    let (status, described) =
        cloudformation(&router, "DescribeStacks", &[("StackName", "whole-value-served")]).await;
    assert_eq!(
        status, 200,
        "expected the stack to be readable, got {status} {described}"
    );
    let stack_id = element(&described, "StackId");

    for (case, identifier) in [
        ("its name", "whole-value-served"),
        ("its whole stack id", stack_id.as_str()),
    ] {
        for read in READS {
            let mut members: Vec<(&str, &str)> = vec![("StackName", identifier)];
            members.extend_from_slice(read.extra);
            let (status, body) = cloudformation(&router, read.action, &members).await;
            let case = format!("{} naming {case}", read.action);
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} {body}"
            );
        }
    }
}
