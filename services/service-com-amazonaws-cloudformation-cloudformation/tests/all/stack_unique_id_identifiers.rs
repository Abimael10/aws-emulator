// SPDX-License-Identifier: CC0-1.0

//! How this service answers a stack identifier written as a stack's UNIQUE ID — the segment
//! a stack id (`arn:…:stack/<name>/<unique-id>`) ends in — over the awsQuery wire.
//!
//! **The rule every case here holds the service to:** the expression CloudFormation's own
//! input validation states for a stack identifier describes the WHOLE value, not some run
//! inside it, so a value that IS a stack name reaches the stack lookup and a value that is
//! only partly one is refused for its own shape. A bare unique id is wholly a name run when
//! it begins with a hexadecimal LETTER, so it is admitted — and since only a stack's name and
//! its whole stack id address a stack, such a request is answered as the stack it does not
//! name (`Stack with id <value> does not exist`); a unique id beginning with a DIGIT is no
//! name at all, so it earns the constraint refusal instead, in either case of hexadecimal.
//! The forms a stack IS addressed by still reach the lookup and are served.
//!
//! What the identifier check reads is the FORM of the value alone, so each row's answer is
//! decided by the expression and not by the store. The shape of the constraint refusal itself
//! is pinned by the malformed-identifier table of `stack_identifier_refusals.rs`, which is the
//! one suite that states it.
//!
//! The tables span the class rather than one recorded request: five unique ids differing along
//! what the rule generalises over (a live stack's own led by a letter and by a digit, that
//! digit-led value in the other case of hexadecimal, and two nothing ever minted) crossed with
//! every operation whose identifier member this service checks itself.

use crate::harness::{call, create_stack, member, router, wait_for_the_transition, xml_text};

/// A template with one resource, so every read a row makes has something to report for the
/// control cases.
const TEMPLATE: &str = r#"{"Resources":{
     "First":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"unique-id-first"}}}}"#;

/// A stack policy body, for the row that reaches `SetStackPolicy`.
const POLICY: &str = r#"{"Statement":[{"Effect":"Allow","Action":"Update:*",
     "Principal":"*","Resource":"*"}]}"#;

/// One operation that takes a stack identifier this service checks itself: the members it
/// needs besides `StackName`, and whether a request naming a LIVE stack is served (a mutation
/// is left out of that control, since it would move the stack the other rows read).
struct UniqueIdRow {
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
    serves_a_live_stack: bool,
}

const ROWS: &[UniqueIdRow] = &[
    UniqueIdRow {
        action: "ListStackResources",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackResources",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStacks",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "GetTemplate",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackResource",
        extra: &[("LogicalResourceId", "First")],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "DescribeStackEvents",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "GetStackPolicy",
        extra: &[],
        serves_a_live_stack: true,
    },
    UniqueIdRow {
        action: "CancelUpdateStack",
        extra: &[],
        serves_a_live_stack: false,
    },
    // Two operations that WRITE: a unique id is no identifier for them either, so the rule
    // holds for the mutating reads of a stack as well as the reporting ones. Neither is
    // crossed with the live stack, since a write would move the stack the rows above read.
    UniqueIdRow {
        action: "SetStackPolicy",
        extra: &[("StackPolicyBody", POLICY)],
        serves_a_live_stack: false,
    },
    UniqueIdRow {
        action: "UpdateStack",
        extra: &[("TemplateBody", TEMPLATE)],
        serves_a_live_stack: false,
    },
];

/// A unique id nothing ever minted, in the 8-4-4-4-12 hexadecimal form a stack id ends in and
/// starting with a DIGIT, so no part of the expression describes the whole of it.
const UNMINTED_UNIQUE_ID: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

/// A value in the same form that starts with a LETTER, which nothing ever minted either: the
/// whole of it is a name run, so it reaches the lookup.
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
async fn a_stack_unique_id_is_answered_by_its_own_form() {
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
    // letter and led by a digit. Neither addresses the stack — only a name or a whole stack id
    // does — so the letter-led value, which is wholly a name run, is resolved and reported as
    // the stack that is not there, while the digit-led one is no name at all and is refused
    // for its shape, in either case of hexadecimal.
    let digit_led = format!("0{}", &minted[1..]);
    let letter_led = format!("a{}", &minted[1..]);
    let upper = digit_led.to_ascii_uppercase();

    /// Which answer a row's identifier earns: the stack lookup's, or the constraint check's.
    #[derive(Clone, Copy)]
    enum Answer {
        /// The value is wholly a name run, so it is resolved and answered as a missing stack.
        NotFound,
        /// The value is no name at all, so it is refused for its own shape.
        Malformed,
    }

    for (case, identifier, answer) in [
        (
            "that unique id led by a letter",
            letter_led.as_str(),
            Answer::NotFound,
        ),
        (
            "a letter-led value nothing ever minted",
            LETTER_LED_UNIQUE_ID,
            Answer::NotFound,
        ),
        (
            "that unique id led by a digit",
            digit_led.as_str(),
            Answer::Malformed,
        ),
        (
            "that unique id in upper-case hexadecimal",
            upper.as_str(),
            Answer::Malformed,
        ),
        (
            "a digit-led unique id nothing ever minted",
            UNMINTED_UNIQUE_ID,
            Answer::Malformed,
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
            match answer {
                Answer::NotFound => assert!(
                    message.contains("does not exist") && message.contains(identifier),
                    "case {case:?}: expected the stack-not-found sentence naming \
                     {identifier:?}, got {message:?}"
                ),
                Answer::Malformed => assert!(
                    message.contains("validation error")
                        && message.contains("regular expression pattern"),
                    "case {case:?}: expected the constraint refusal, got {message:?}"
                ),
            }
        }
    }

    // ...and the forms that DO address the stack are still served, so admitting a value that
    // is wholly a name run costs none of them.
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

/// RULE: the refusal an admitted bare unique id earns is the one sentence an operation answers
/// an identifier it resolved and found nothing for — the same answer a name nothing created
/// earns, naming the identifier exactly as sent. Asserted on the listing, whose own refusal
/// states that sentence.
#[tokio::test]
async fn an_admitted_unique_id_earns_the_same_refusal_as_an_absent_stack() {
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
    // The minted id's leading character is random hexadecimal, so the value asserted on is
    // built from it: the same unique id led by a letter, which the whole of the name
    // alternative describes.
    let letter_led = format!("a{}", &minted[1..]);

    for (case, identifier) in [
        (
            "the live stack's bare unique id led by a letter",
            letter_led.as_str(),
        ),
        ("a name nothing created", "unique-id-never-created"),
        (
            "a letter-led unique id nothing ever minted",
            LETTER_LED_UNIQUE_ID,
        ),
    ] {
        let expected = format!("Stack with id {identifier} does not exist");
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
