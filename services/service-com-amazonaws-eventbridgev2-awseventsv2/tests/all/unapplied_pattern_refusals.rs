// SPDX-License-Identifier: CC0-1.0

//! The modelled `@pattern` of an input member, applied where a client sees it: off the
//! `rpcv2Cbor` wire.
//!
//! RULE — a value a member's modelled `@pattern` does not admit is refused BEFORE any state
//! this service holds is looked at, with `400` and the `InvalidInputException` its model
//! lists as a common error of every operation, whose `Message` carries AWS's
//! constraint-violation envelope: the violation count, the value as submitted, the member as
//! a validation message names it, and the expression the model declares for it. That holds
//! for the WHOLE expression, look-around included — which is what the engine behind
//! `@pattern` at the request boundary cannot compile, and therefore treats as satisfied — and
//! it holds whether the expression constrains the member's own value or each element of a
//! collection it holds. A value the expression DOES admit is passed on untouched, to the
//! operation that serves it.
//!
//! The rows span what the rule generalises over: the member and the shape it targets
//! (`EventSourceName`, whose expression opens with the look-ahead reserving the `aws.`
//! prefix; `Description`, whose expression closes with a look-ahead standing for the end of
//! the value; `TagKey`, carried by the elements of a list), the half of the expression the
//! value breaks (the look-around, or the character grammar beside it), the near misses the
//! look-around does NOT exclude, and the position the value sits at (a member of the input,
//! an element of a list member).
//!
//! The ORDER is asserted the only way a client can see it: every refused row names a bus
//! that does not exist, so a service that looked the state up first would answer that
//! lookup's `404` instead of the `400` below, and a row whose value is admitted gets exactly
//! that `404`.
//!
//! Nothing here waits on the wall clock: the one wait is for a CONDITION (a bus reporting
//! `ACTIVE`), polled by the harness under a generous deadline. The wire harness, the input
//! builders and the refusal decoder are `modelled_input_refusal.rs`'s, reused rather than
//! copied.

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::types::{DescribeEventSourceInput, UntagResourceInput};
use eventsv2::ops::untag_resource::UntagResource;

use crate::modelled_input_refusal::{
    constraint_envelope, create_source, refusal, rendered, unminted_arn, Wire,
    INVALID_INPUT_EXCEPTION, REGION,
};

/// The status `InvalidInputException`'s own `@httpError` declares, and the status
/// `ResourceNotFoundException` declares — the answer a request that passes this door and
/// names a bus that is not there earns from the lookup.
const INVALID_INPUT_STATUS: u16 = 400;
const NOT_FOUND_STATUS: u16 = 404;

/// The status an operation of this service that is still scaffolded answers with: a request
/// that passed this door and reached `UntagResource`, which nobody has written yet, is told
/// so rather than refused.
const NOT_IMPLEMENTED_STATUS: u16 = 501;

/// The error shape a lookup of a resource that is not there answers with.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// `EventSourceName`'s modelled `@pattern`, as a refusal quotes it: the model declares
/// `^(?!aws\.)[A-Za-z0-9][\.\-_A-Za-z0-9]*$` and AWS quotes the expression without the
/// anchor pair a whole-value pattern wraps it in.
/// Source: `smithy_query(query="shape", shape="EventSourceName")`.
const EVENT_SOURCE_NAME_PATTERN: &str = r"(?!aws\.)[A-Za-z0-9][\.\-_A-Za-z0-9]*";

/// `Description`'s modelled `@pattern`, as a refusal quotes it: the model anchors its start
/// and closes with a look-ahead rather than with `$`, so there is no anchor pair to drop and
/// the expression is quoted as declared.
/// Source: `smithy_query(query="shape", shape="Description")`.
const DESCRIPTION_PATTERN: &str = r"^[^\u0000-\u001F\u007F-\u009F\u2028\u2029]*(?![\s\S])";

/// `TagKey`'s modelled `@pattern`, quoted the same way and for the same reason.
/// Source: `smithy_query(query="shape", shape="TagKey")`.
const TAG_KEY_PATTERN: &str = r"^\S([\s\S]*\S)?(?![\s\S])";

/// `TagValue`'s modelled `@pattern`, quoted the same way: the same grammar as a key's, with
/// the whole value optional because its `@length` minimum is 0.
/// Source: `smithy_query(query="shape", shape="TagMap")`.
const TAG_VALUE_PATTERN: &str = r"^(\S([\s\S]*\S)?)?(?![\s\S])";

/// A tag key every row of the tag-value table sends its value under — itself a key the
/// modelled `TagKey` grammar admits, so the only thing a row varies is the VALUE.
const TAG_KEY: &str = "owner";

/// `Description`'s modelled `@length` maximum, the bound the request boundary applies itself.
/// Source: `smithy_query(query="shape", shape="Description")`.
const DESCRIPTION_MAX: usize = 512;

/// The identifier segment of an ARN this service never minted, which the modelled ARN
/// patterns pin at exactly 25 characters of `[a-z0-9]`.
/// Source: `smithy_query(query="shape", shape="EventBusArn")`.
const UNMINTED_ID: &str = "a1b2c3d4e5f6a7b8c9d0e1f2a";

/// One clause of the constraint envelope, as the framework spells a member path and AWS
/// words the `@pattern` requirement.
fn pattern_clause(value: &str, member: &str, pattern: &str) -> String {
    format!(
        "Value '{value}' at '{member}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {pattern}"
    )
}

/// RULE over `CreateEventSource$Name`: the whole modelled `EventSourceName` grammar decides
/// the request, the reserved-prefix look-ahead included, and it decides it before the bus
/// lookup.
#[tokio::test]
async fn an_event_source_name_is_held_to_its_whole_modelled_grammar() {
    let wire = Wire::new();
    let bus = wire
        .active_bus("the bus the admitted names are created onto", "name-grammar")
        .await;
    // A well-formed `EventBusArn` naming a bus that does not exist: what a refused row sends,
    // so the 400 below can only have been decided ahead of the lookup.
    let missing = unminted_arn(REGION, "name-grammar-missing", UNMINTED_ID);

    // Each row is a name and whether the modelled grammar admits it.
    let rows: [(&str, &str, bool); 8] = [
        ("a plain name", "orders", true),
        ("every separator the class adds", "a.b-c_d9", true),
        // The look-ahead reserves the prefix `aws.` and nothing else.
        ("the reserved letters with no dot", "awsreserved", true),
        ("the reserved prefix not at the start", "my.aws.source", true),
        ("the reserved prefix in another case", "AWS.reserved", true),
        ("the reserved prefix", "aws.reserved", false),
        ("a leading separator the class forbids", ".orders", false),
        ("a character outside the class", "orders!", false),
    ];

    for (case, name, admitted) in rows {
        let (status, body) = wire
            .call::<CreateEventSource<InMemoryEngine>>(&create_source(
                name,
                &missing,
                Some(&format!("token-for-{name}")),
            ))
            .await;
        if admitted {
            // The name passed this door, so what answers is the bus lookup.
            assert_eq!(
                status,
                NOT_FOUND_STATUS,
                "case {case:?}: a name the grammar admits must reach the bus lookup, got \
                 {status} with {:?}",
                rendered(&body)
            );
            let (code, _) = refusal(case, &body);
            assert_eq!(
                code, RESOURCE_NOT_FOUND_EXCEPTION,
                "case {case:?}: the answer must be the lookup's own error shape"
            );
            continue;
        }
        assert_eq!(
            status,
            INVALID_INPUT_STATUS,
            "case {case:?}: a name the grammar refuses must be refused ahead of the lookup, \
             got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: the refusal must name the modelled error shape"
        );
        let expected =
            constraint_envelope(&[pattern_clause(name, "name", EVENT_SOURCE_NAME_PATTERN)]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // Every admitted name is a name this service really stores: created onto a bus that IS
    // there and read back through the operation that reports it.
    for (case, name, admitted) in rows {
        if !admitted {
            continue;
        }
        let created = wire
            .ok::<CreateEventSource<InMemoryEngine>>(
                case,
                &create_source(name, &bus, Some(&format!("stored-{name}"))),
            )
            .await;
        let arn = created
            .event_source_arn
            .unwrap_or_else(|| panic!("case {case:?}: the create must report an EventSourceArn"));
        let reported = wire
            .ok::<DescribeEventSource<InMemoryEngine>>(
                case,
                &DescribeEventSourceInput {
                    event_source_arn: arn.clone(),
                },
            )
            .await
            .name;
        assert_eq!(
            reported.as_deref(),
            Some(name),
            "case {case:?}: the stored EventSource at {arn} must report the name it was \
             created with"
        );
    }
}

/// RULE over `CreateEventSource$Description`: the same door applies the `Description`
/// grammar, whose look-ahead stands for the end of the value, and applies it ahead of the
/// bus lookup.
#[tokio::test]
async fn a_description_is_held_to_its_whole_modelled_grammar() {
    let wire = Wire::new();
    let missing = unminted_arn(REGION, "description-grammar-missing", UNMINTED_ID);

    // The character classes the expression excludes are the C0 controls, the C1 controls and
    // the two Unicode separators; one row per class, plus the printable text and the empty
    // value the expression admits.
    let control = format!("orders{}s3", '\u{1}');
    let delete = format!("orders{}s3", '\u{7F}');
    let c1 = format!("orders{}s3", '\u{9F}');
    let line_separator = format!("orders{}s3", '\u{2028}');
    let rows: [(&str, &str, bool); 6] = [
        ("printable text", "events forwarded from S3", true),
        ("the empty description", "", true),
        ("a C0 control character", control.as_str(), false),
        ("the delete character", delete.as_str(), false),
        ("a C1 control character", c1.as_str(), false),
        ("the Unicode line separator", line_separator.as_str(), false),
    ];

    for (case, description, admitted) in rows {
        let mut input = create_source("description-grammar", &missing, Some("token-for-row"));
        input.description = Some(description.to_string());
        let (status, body) = wire
            .call::<CreateEventSource<InMemoryEngine>>(&input)
            .await;
        if admitted {
            assert_eq!(
                status,
                NOT_FOUND_STATUS,
                "case {case:?}: a description the grammar admits must reach the bus lookup, \
                 got {status} with {:?}",
                rendered(&body)
            );
            continue;
        }
        assert_eq!(
            status,
            INVALID_INPUT_STATUS,
            "case {case:?}: a description the grammar refuses must be refused ahead of the \
             lookup, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: the refusal must name the modelled error shape"
        );
        let expected = constraint_envelope(&[pattern_clause(
            description,
            "description",
            DESCRIPTION_PATTERN,
        )]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE over `UntagResource$TagKeys`: an expression carried by a collection's ELEMENTS is
/// applied to each element, and a clause names the element by its 1-based position — the
/// same position a validation message spells.
#[tokio::test]
async fn a_tag_key_is_held_to_its_whole_modelled_grammar() {
    let wire = Wire::new();
    let target = unminted_arn(REGION, "tag-key-grammar", UNMINTED_ID);

    // The expression rules out a key whose first or last character is whitespace, and nothing
    // else: inner whitespace, and every other printable key, satisfy it.
    let rows: [(&str, &str, bool); 5] = [
        ("a plain key", "environment", true),
        ("a key holding inner whitespace", "cost centre", true),
        ("a key holding an inner line break", "cost\ncentre", true),
        ("a key opening with whitespace", " environment", false),
        ("a key closing with whitespace", "environment ", false),
    ];

    for (case, key, admitted) in rows {
        let (status, body) = wire
            .call::<UntagResource<InMemoryEngine>>(&UntagResourceInput {
                resource_arn: target.clone(),
                tag_keys: vec![key.to_string()],
            })
            .await;
        if admitted {
            // The key passed this door, so what answers is the operation itself — which for
            // `UntagResource` is still the scaffold's "nobody has written this yet", not a
            // refusal of the request.
            assert_eq!(
                status,
                NOT_IMPLEMENTED_STATUS,
                "case {case:?}: a key the grammar admits must pass this door and reach the \
                 operation, got {status} with {:?}",
                rendered(&body)
            );
            continue;
        }
        assert_eq!(
            status,
            INVALID_INPUT_STATUS,
            "case {case:?}: a key the grammar refuses must be refused, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: the refusal must name the modelled error shape"
        );
        let expected =
            constraint_envelope(&[pattern_clause(key, "tagKeys.1.member", TAG_KEY_PATTERN)]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE over `CreateEventSource$Tags`: the same element position inside a MAP — the values of
/// a tag map carry `TagValue`'s expression, and a clause names the entry by the key it was
/// sent under (`tags.<key>`), which is how the collecting walk spells a map entry.
#[tokio::test]
async fn a_tag_value_is_held_to_its_whole_modelled_grammar() {
    let wire = Wire::new();
    let missing = unminted_arn(REGION, "tag-value-grammar-missing", UNMINTED_ID);

    // `TagValue`'s expression admits the empty value (its own `@length` minimum is 0) and any
    // value whose first and last character is not whitespace; it rules out the rest.
    let rows: [(&str, &str, bool); 5] = [
        ("a plain value", "finance", true),
        ("the empty value", "", true),
        ("a value holding inner whitespace", "cost centre", true),
        ("a value opening with whitespace", " finance", false),
        ("a value closing with whitespace", "finance ", false),
    ];

    for (case, value, admitted) in rows {
        let mut input = create_source("tag-value-grammar", &missing, Some("token-for-row"));
        input.tags = Some(
            [(TAG_KEY.to_string(), value.to_string())]
                .into_iter()
                .collect(),
        );
        let (status, body) = wire.call::<CreateEventSource<InMemoryEngine>>(&input).await;
        if admitted {
            assert_eq!(
                status,
                NOT_FOUND_STATUS,
                "case {case:?}: a tag value the grammar admits must reach the bus lookup, got \
                 {status} with {:?}",
                rendered(&body)
            );
            continue;
        }
        assert_eq!(
            status,
            INVALID_INPUT_STATUS,
            "case {case:?}: a tag value the grammar refuses must be refused ahead of the \
             lookup, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: the refusal must name the modelled error shape"
        );
        let expected = constraint_envelope(&[pattern_clause(
            value,
            &format!("tags.{TAG_KEY}"),
            TAG_VALUE_PATTERN,
        )]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE over a request that breaks BOTH classes at once: the clauses are counted in ONE
/// envelope and stated in the order the input shape declares the members they are about —
/// which is the order the framework's own walk states the ones it found in — so a clause
/// about an expression that walk could not compile is not pushed behind the ones it could.
#[tokio::test]
async fn one_request_breaking_both_classes_is_one_ordered_refusal() {
    let wire = Wire::new();
    let missing = unminted_arn(REGION, "mixed-grammar-missing", UNMINTED_ID);

    // `Name` breaks the expression the boundary cannot compile (the reserved prefix);
    // `Description` breaks its `@length` maximum, which the boundary applies itself.
    // `CreateEventSourceRequest` declares `Name` before `Description`
    // (`smithy_query(query="operation", operation="CreateEventSource")`), so that is the
    // order the one envelope states them in.
    let name = "aws.mixed";
    let description = "d".repeat(DESCRIPTION_MAX + 1);
    let mut input = create_source(name, &missing, Some("token-for-mixed"));
    input.description = Some(description.clone());

    let case = "a reserved name and an over-long description in one request";
    let (status, body) = wire.call::<CreateEventSource<InMemoryEngine>>(&input).await;
    assert_eq!(
        status,
        INVALID_INPUT_STATUS,
        "case {case:?}: both classes must be refused ahead of the lookup, got {status} with \
         {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, INVALID_INPUT_EXCEPTION,
        "case {case:?}: the refusal must name the modelled error shape"
    );
    let expected = constraint_envelope(&[
        pattern_clause(name, "name", EVENT_SOURCE_NAME_PATTERN),
        format!(
            "Value '{description}' at 'description' failed to satisfy constraint: Member must \
             have length less than or equal to {DESCRIPTION_MAX}"
        ),
    ]);
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}
