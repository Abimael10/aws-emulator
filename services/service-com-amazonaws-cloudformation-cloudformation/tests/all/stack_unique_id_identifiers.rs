// SPDX-License-Identifier: CC0-1.0

//! How this service answers a stack identifier written as a stack's UNIQUE ID — the segment
//! a stack id (`arn:…:stack/<name>/<unique-id>`) ends in — over the awsQuery wire.
//!
//! **The rule every case here holds the service to:** the bare unique id CloudFormation mints
//! for a stack is one of the forms the service takes as addressing a stack, beside the stack
//! name and the stack ARN, so such a value reaches the stack LOOKUP — and since only a stack's
//! name and its whole stack id address a stack, the request is answered as the stack it does
//! not name (`Stack with id <value> does not exist`). Which hexadecimal character the id begins
//! with is no part of that decision: the alphabet spans digits and letters, so one id of the
//! kind is every id of the kind, in either case of hexadecimal and whether the digits are
//! grouped 8-4-4-4-12 or not. A hexadecimal run that is NOT that id's width or grouping is no
//! identifier of any form, and earns the constraint refusal instead.
//!
//! What the identifier check reads is the FORM of the value alone, so each row's answer is
//! decided by the form and not by the store. The shape of the constraint refusal itself
//! is pinned by the malformed-identifier table of `stack_identifier_refusals.rs`, which is the
//! one suite that states it.
//!
//! The tables span the class rather than one recorded request: the admitted side over seven
//! unique ids differing along what the rule generalises over (a live stack's own as minted and
//! rewritten to begin with a digit and with a letter, in either case of hexadecimal, with its
//! digits grouped and ungrouped, and values nothing ever minted), and the refused side over
//! hexadecimal runs that are a character short, a character long or grouped elsewhere — each
//! crossed with every operation whose identifier member this service checks itself.

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
/// starting with a DIGIT.
const UNMINTED_DIGIT_LED_UNIQUE_ID: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

/// A value in the same form that starts with a LETTER, which nothing ever minted either.
const UNMINTED_LETTER_LED_UNIQUE_ID: &str = "af2504e0-4f89-11d3-9a0c-0305e82c3301";

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

/// Stand up one stack and answer its name, its whole stack id and the bare unique id that id
/// ends in.
async fn a_live_stack(name: &str) -> (smithy_server_runtime::MultiServiceRouter, String, String) {
    let router = router();
    let (status, body) = create_stack(&router, name, TEMPLATE, &[]).await;
    assert_eq!(
        status, 200,
        "expected CreateStack to be accepted, got {status} {body}"
    );
    wait_for_the_transition().await;

    let (status, described) = call(&router, "DescribeStacks", &[member("StackName", name)]).await;
    assert_eq!(
        status, 200,
        "expected the stack to be readable, got {status} {described}"
    );
    let stack_id = xml_text(&described, "StackId").expect("the stack's own id");
    let unique_id = unique_id_of(&stack_id);
    (router, stack_id, unique_id)
}

#[tokio::test]
async fn a_stack_unique_id_is_resolved_whatever_character_it_begins_with() {
    let (router, stack_id, minted) = a_live_stack("unique-id-live").await;

    // The minted id's own leading character is random hexadecimal, so both sides of the
    // character the value BEGINS with are built from it: the same 8-4-4-4-12 value led by a
    // digit and led by a letter. Neither addresses the stack — only a name or a whole stack id
    // does — so each is resolved and reported as the stack that is not there, in either case
    // of hexadecimal and with the digits grouped or not.
    let digit_led = format!("0{}", &minted[1..]);
    let letter_led = format!("a{}", &minted[1..]);
    let upper = digit_led.to_ascii_uppercase();
    let ungrouped = minted.replace('-', "");

    for (case, identifier) in [
        ("the live stack's own unique id", minted.as_str()),
        ("that unique id led by a digit", digit_led.as_str()),
        ("that unique id led by a letter", letter_led.as_str()),
        (
            "that digit-led unique id in upper-case hexadecimal",
            upper.as_str(),
        ),
        ("that unique id with its digits ungrouped", ungrouped.as_str()),
        (
            "a digit-led unique id nothing ever minted",
            UNMINTED_DIGIT_LED_UNIQUE_ID,
        ),
        (
            "a letter-led unique id nothing ever minted",
            UNMINTED_LETTER_LED_UNIQUE_ID,
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
            assert!(
                message.contains("does not exist") && message.contains(identifier),
                "case {case:?}: expected the stack-not-found sentence naming {identifier:?}, \
                 got {message:?}"
            );
        }
    }

    // ...and the forms that DO address the stack are still served, so admitting the bare
    // unique id costs none of them.
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

/// RULE, the other side: a hexadecimal run that is not the WIDTH or the GROUPING of a minted
/// unique id is no identifier of any form this service takes, so it is refused for its own
/// shape ahead of the lookup.
#[tokio::test]
async fn a_hexadecimal_run_that_is_no_unique_id_is_refused_for_its_shape() {
    let (router, _stack_id, minted) = a_live_stack("unique-id-shape").await;
    let digit_led = format!("0{}", &minted[1..]);
    let short = digit_led[..digit_led.len() - 1].to_string();
    let long = format!("{digit_led}0");
    let regrouped = format!("{}-{}", &digit_led[..6], &digit_led[7..]);

    for (case, identifier) in [
        ("a run one character short of a unique id", short.as_str()),
        ("a run one character longer", long.as_str()),
        ("a unique id's digits grouped elsewhere", regrouped.as_str()),
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
            assert!(
                message.contains("validation error")
                    && message.contains("regular expression pattern"),
                "case {case:?}: expected the constraint refusal, got {message:?}"
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
    let (router, _stack_id, minted) = a_live_stack("unique-id-sentence").await;
    // The minted id's leading character is random hexadecimal, so both leading classes are
    // built from it and asserted on beside the names nothing created.
    let digit_led = format!("0{}", &minted[1..]);
    let letter_led = format!("a{}", &minted[1..]);

    for (case, identifier) in [
        ("the live stack's bare unique id", minted.as_str()),
        ("that unique id led by a digit", digit_led.as_str()),
        ("that unique id led by a letter", letter_led.as_str()),
        ("a name nothing created", "unique-id-never-created"),
        (
            "a letter-led unique id nothing ever minted",
            UNMINTED_LETTER_LED_UNIQUE_ID,
        ),
        (
            "a digit-led unique id nothing ever minted",
            UNMINTED_DIGIT_LED_UNIQUE_ID,
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
