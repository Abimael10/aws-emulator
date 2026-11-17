// SPDX-License-Identifier: CC0-1.0

//! How this service answers a stack identifier written as a stack's UNIQUE ID — the segment
//! a stack id (`arn:…:stack/<name>/<unique-id>`) ends in — over the awsQuery wire.
//!
//! **The rule every case here holds the service to:** the forms CloudFormation takes as
//! naming a stack are the ones its own validator states — a stack NAME or a stack ARN — and
//! the unique id a stack id ENDS in is neither, so a request carrying one alone never reports
//! a stack: a value the name alternative does not admit (one led by a digit) is answered for
//! its own shape, with the constraint refusal stating that expression at the member path the
//! operation names the identifier at, BEFORE any stack is resolved, and one the name
//! alternative does admit (led by a letter) is resolved and reported as the stack it does not
//! name. The forms a stack IS addressed by — its name and its whole stack id — still reach the
//! lookup and are served.
//!
//! What the identifier check reads is the FORM of the value alone, so each row's answer is
//! decided by the expression and not by the store. Both sides of the leading character are
//! rows below.
//!
//! The tables span the class rather than one recorded request: five unique ids differing
//! along what the rule generalises over (a live stack's own led by a digit and by a letter,
//! that digit-led value in the other case of hexadecimal, and two nothing ever minted)
//! crossed with every operation whose identifier member this service checks itself. The
//! values that are only NEARLY a unique id are rows of the malformed-identifier table in
//! `stack_identifier_refusals.rs`, which is the one suite that pins the shape of that
//! refusal.

use crate::harness::{call, create_stack, member, router, wait_for_the_transition, xml_text};

/// A template with one resource, so every read a row makes has something to report for the
/// control cases.
const TEMPLATE: &str = r#"{"Resources":{
     "First":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"unique-id-first"}}}}"#;

/// A stack policy body, for the row that reaches `SetStackPolicy`.
const POLICY: &str = r#"{"Statement":[{"Effect":"Allow","Action":"Update:*",
     "Principal":"*","Resource":"*"}]}"#;

/// The expression CloudFormation's own input validation states a stack identifier has to
/// match, quoted in a constraint refusal exactly as the service states it — the clause EVERY
/// row of this suite answers, since no value here is a form the service resolves.
///
/// Source: the service's own constant (`state::stack_identifier::STACK_IDENTIFIER_PATTERN`),
/// spelled out here because the sentence IS the requirement — a reworded message must fail
/// this suite.
const IDENTIFIER_PATTERN: &str = "[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*";

/// The refusal a value the expression does not match is answered with, at the member path the
/// operation states it at.
fn constraint_refusal(path: &str, value: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {IDENTIFIER_PATTERN}"
    )
}

/// One operation that takes a stack identifier this service checks itself: the members it
/// needs besides `StackName`, the member path its refusal states the identifier at, and
/// whether a request naming a LIVE stack is served (a mutation is left out of that control,
/// since it would move the stack the other rows read).
struct UniqueIdRow {
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
    path: &'static str,
    serves_a_live_stack: bool,
}

const ROWS: &[UniqueIdRow] = &[
    UniqueIdRow {
        action: "ListStackResources",
        extra: &[],
        // The one operation whose validator names the member `StackNameOrId`.
        path: "stackNameOrId",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackResources",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStacks",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "GetTemplate",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackResource",
        extra: &[("LogicalResourceId", "First")],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackEvents",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "GetStackPolicy",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "CancelUpdateStack",
        extra: &[],
        path: "stackName",
        serves_a_live_stack: false,
    },
    // Two operations that WRITE: a unique id is no identifier for them either, so the rule
    // holds for the mutating reads of a stack as well as the reporting ones. Neither is
    // crossed with the live stack, since a write would move the stack the rows above read.
    UniqueIdRow {
        action: "SetStackPolicy",
        extra: &[("StackPolicyBody", POLICY)],
        path: "stackName",
        serves_a_live_stack: false,
    },
    UniqueIdRow {
        action: "UpdateStack",
        extra: &[("TemplateBody", TEMPLATE)],
        path: "stackName",
        serves_a_live_stack: false,
    },
];

/// A unique id nothing ever minted, in the 8-4-4-4-12 hexadecimal form a stack id ends in and
/// starting with a digit.
const UNMINTED_UNIQUE_ID: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

/// A value in the same form that starts with a LETTER, which nothing ever minted either.
const LETTER_LED_UNIQUE_ID: &str = "af2504e0-4f89-11d3-9a0c-0305e82c3301";

fn members_of(row: &UniqueIdRow, identifier: &str) -> Vec<(String, String)> {
    let mut members = vec![member("StackName", identifier)];
    members.extend(row.extra.iter().map(|(key, value)| member(key, value)));
    members
}

/// The unique id a stack id ends in: everything after the last `/`.
fn unique_id_of(stack_id: &str) -> String {
    stack_id
        .rsplit('/')
        .next()
        .expect("a stack id's last segment")
        .to_string()
}

#[tokio::test]
async fn a_stack_unique_id_is_refused_for_its_own_shape() {
    let router = router();
    let (status, body) = create_stack(&router, "unique-id-live", TEMPLATE, &[]).await;
    assert_eq!(
        status, 200,
        "expected CreateStack to be accepted, got {status} {body}"
    );
    wait_for_the_transition().await;

    let (status, described) = call(
        &router,
        "DescribeStacks",
        &[member("StackName", "unique-id-live")],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the stack to be readable, got {status} {described}"
    );
    let stack_id = xml_text(&described, "StackId").expect("the stack's own id");
    let minted = unique_id_of(&stack_id);
    // The minted id's own leading character is random hexadecimal, so both sides of the
    // character the value BEGINS with are built from it: the same 8-4-4-4-12 value led by a
    // digit and led by a letter. Neither addresses the stack — only a name or a whole stack id
    // does — and which refusal each earns is decided by the expression the service states: a
    // letter-led value is a NAME by that expression, so it is resolved and reported absent,
    // while a digit-led one matches no alternative of it and is refused for its own shape.
    let digit_led = format!("0{}", &minted[1..]);
    let letter_led = format!("a{}", &minted[1..]);
    let upper = digit_led.to_ascii_uppercase();

    // Every unique id: the live stack's own led by a digit and by a letter, that digit-led
    // value in the other case of hexadecimal, and two nothing ever minted. The third member of
    // each row is whether the expression admits the value as a name.
    for (case, identifier, admitted_as_a_name) in [
        ("that unique id led by a digit", digit_led.as_str(), false),
        ("that unique id led by a letter", letter_led.as_str(), true),
        (
            "that unique id in upper-case hexadecimal",
            upper.as_str(),
            false,
        ),
        ("a unique id nothing ever minted", UNMINTED_UNIQUE_ID, false),
        (
            "a letter-led value nothing ever minted",
            LETTER_LED_UNIQUE_ID,
            true,
        ),
    ] {
        for row in ROWS {
            let case = format!("{} naming {case}", row.action);
            let (status, body) = call(&router, row.action, &members_of(row, identifier)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            assert_eq!(
                xml_text(&body, "Code").as_deref(),
                Some("ValidationError"),
                "case {case:?}: expected <Code>ValidationError</Code>, got {body}"
            );
            let message = xml_text(&body, "Message").unwrap_or_default();
            if admitted_as_a_name {
                // The expression's name alternative takes it, so it reaches the lookup and
                // the stack it names is the one that is not there.
                assert!(
                    message.contains("does not exist") && message.contains(identifier),
                    "case {case:?}: expected the stack-not-found sentence naming \
                     {identifier:?}, got {message:?}"
                );
            } else {
                assert_eq!(
                    message,
                    constraint_refusal(row.path, identifier),
                    "case {case:?}: expected the constraint refusal stating the expression, \
                     got {message:?}"
                );
                assert!(
                    !message.contains("does not exist"),
                    "case {case:?}: a value the expression does not admit is refused for its \
                     own shape rather than reported as a stack that is not there, got \
                     {message:?}"
                );
            }
        }
    }

    // ...and the forms that DO address the stack are still served, so refusing the unique id
    // for its shape costs none of them.
    for (case, identifier) in [
        ("its name", "unique-id-live"),
        ("its whole stack id", stack_id.as_str()),
    ] {
        for row in ROWS {
            if !row.serves_a_live_stack {
                continue;
            }
            let case = format!("{} naming {case}", row.action);
            let (status, body) = call(&router, row.action, &members_of(row, identifier)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} {body}"
            );
        }
    }
}

/// RULE: the refusal the bare unique id earns is NOT the not-found sentence the operation
/// answers an identifier it resolved and found nothing for — which is what makes the value
/// refused rather than resolved. Asserted on the listing, whose own refusal names the
/// identifier exactly as sent.
#[tokio::test]
async fn the_bare_unique_id_earns_another_refusal_than_an_absent_stack() {
    let router = router();
    let (status, body) = create_stack(&router, "unique-id-sentence", TEMPLATE, &[]).await;
    assert_eq!(
        status, 200,
        "expected CreateStack to be accepted, got {status} {body}"
    );
    wait_for_the_transition().await;
    let (status, described) = call(
        &router,
        "DescribeStacks",
        &[member("StackName", "unique-id-sentence")],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the stack to be readable, got {status} {described}"
    );
    let minted = unique_id_of(&xml_text(&described, "StackId").expect("the stack's own id"));
    // The minted id's leading character is random hexadecimal, and a value led by a LETTER is
    // a name by the expression the service states; this case is about the one the expression
    // does not admit, so the value is that unique id led by a digit.
    let digit_led = format!("0{}", &minted[1..]);

    // A NAME nothing created is resolved and reported absent; a unique id the expression does
    // not admit is no identifier at all, so it earns the constraint refusal instead.
    for (case, identifier, expected) in [
        (
            "the live stack's bare unique id led by a digit",
            digit_led.as_str(),
            constraint_refusal("stackNameOrId", digit_led.as_str()),
        ),
        (
            "a name nothing created",
            "unique-id-never-created",
            "Stack with id unique-id-never-created does not exist".to_string(),
        ),
        (
            "a unique id nothing ever minted",
            UNMINTED_UNIQUE_ID,
            constraint_refusal("stackNameOrId", UNMINTED_UNIQUE_ID),
        ),
    ] {
        let (status, body) = call(
            &router,
            "ListStackResources",
            &[member("StackName", identifier)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            xml_text(&body, "Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }
}
