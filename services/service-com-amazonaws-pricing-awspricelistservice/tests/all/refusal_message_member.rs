// SPDX-License-Identifier: CC0-1.0

//! Which member a refusal of this service carries its text in, and which value set it
//! quotes for an enumerated member.
//!
//! Two rules, both about the body of a failure rather than about any one operation:
//!
//! * every error body this service answers whose class its model declares reports the
//!   failure's text in `Message` — the member every error shape of this model declares,
//!   and so the member the `awsJson1_1` wire carries it under — whether the text was
//!   chosen by a handler or by the framework's pre-handler refusal of the request; and
//! * a clause about an ENUMERATED member quotes the value set the service validates
//!   against, which for this API's filter type is wider than the subset the published
//!   model lists.
//!
//! The rows below span the constraint classes this model can refuse a request with (an
//! absent required member, a length, a range, a pattern, an enumerated value set, a
//! collection's own length), the operations that carry them, and the failures a HANDLER
//! answers with, so neither rule is pinned to one recorded request.

use serde_json::{json, Value};

use super::pricing_support::{all_items, call, ok, router, JAN_1_2024};

/// The member every error shape of this model declares its text in (`Message`, targeting
/// `errorMessage`, with no `@jsonName`), and so the member this service's wire carries it
/// under.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The member the framework's own pre-handler refusal renderer writes that text in.
const RENDERED_MESSAGE_MEMBER: &str = "message";

/// The `FilterType` values this service names in a refusal, in the order it states them —
/// the five its model declares plus the `TERM_CONTAIN` the service itself lists; the one
/// statement of that set is `src/service/enum_value_set.rs`, which records where it comes
/// from.
const FILTER_TYPE_SET_AS_STATED: &str =
    "[TERM_CONTAIN, ANY_OF, TERM_MATCH, NONE_OF, CONTAINS, EQUALS]";

/// The sentence this API refuses a `FormatVersion` it does not publish with: the parameter
/// as the wire spells it, and the one version this catalog publishes (the model documents
/// `aws_v1` as the member's only valid value). It is not a constraint clause: the value is
/// one the model's own `@length` admits, so no declared constraint was broken.
const UNACCEPTED_FORMAT_VERSION_MESSAGE: &str =
    "Input Parameters are invalid. formatVersion should be aws_v1";

/// A well-formed filter, for the rows that break one member of a longer list.
fn term_match(field: &str, value: &str) -> Value {
    json!({ "Type": "TERM_MATCH", "Field": field, "Value": value })
}

/// The text an error body carries under `member`, or `None` when it carries no such
/// member.
fn text_under<'a>(body: &'a Value, member: &str) -> Option<&'a str> {
    body.get(member).and_then(Value::as_str)
}

#[tokio::test]
async fn every_modelled_refusal_reports_its_text_in_the_modelled_message_member() {
    let router = router();

    // Each row: the case, the operation, the request, and a fragment the stated text must
    // contain — one row per constraint class the framework refuses a request with, spread
    // over every operation of this service that declares a constrained member.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "an unmodelled filter type",
            "GetProducts",
            json!({
                "ServiceCode": "AmazonEC2",
                "Filters": [term_match("ServiceCode", "AmazonEC2"),
                            { "Type": "EXACT", "Field": "instanceType", "Value": "t3.nano" }],
            }),
            "Member must satisfy enum value set",
        ),
        (
            "an absent required member",
            "GetProducts",
            json!({ "Filters": [term_match("ServiceCode", "AmazonEC2")] }),
            "Member must not be null",
        ),
        (
            "a filter field below its length",
            "GetProducts",
            json!({
                "ServiceCode": "AmazonEC2",
                "Filters": [{ "Type": "TERM_MATCH", "Field": "", "Value": "t3.nano" }],
            }),
            "Member must have length greater than or equal to 1",
        ),
        (
            "a filter value above its length",
            "GetProducts",
            json!({
                "ServiceCode": "AmazonEC2",
                "Filters": [term_match("instanceType", &"t".repeat(1025))],
            }),
            "Member must have length less than or equal to 1024",
        ),
        (
            "a format version above its length",
            "GetProducts",
            json!({ "ServiceCode": "AmazonEC2", "FormatVersion": "v".repeat(33) }),
            "Member must have length less than or equal to 32",
        ),
        (
            "a page size below its range",
            "GetProducts",
            json!({ "ServiceCode": "AmazonEC2", "MaxResults": 0 }),
            "Member must have value greater than or equal to 1",
        ),
        (
            "a page size above its range",
            "DescribeServices",
            json!({ "MaxResults": 101 }),
            "Member must have value less than or equal to 100",
        ),
        (
            "a collection above its own length",
            "GetProducts",
            json!({
                "ServiceCode": "AmazonEC2",
                "Filters": (0..51).map(|index| term_match("instanceType",
                    &format!("t3.filter{index}"))).collect::<Vec<Value>>(),
            }),
            "Member must have length less than or equal to 50",
        ),
        (
            "a value outside a pattern",
            "ListPriceLists",
            json!({
                "ServiceCode": "AmazonEC2",
                "EffectiveDate": JAN_1_2024,
                "CurrencyCode": "usd",
            }),
            "Member must satisfy regular expression pattern",
        ),
        (
            // The widest page bound this model declares (1..10000), one past its maximum.
            "a page size above the widest range this model declares",
            "GetAttributeValues",
            json!({
                "ServiceCode": "AmazonEC2",
                "AttributeName": "instanceType",
                "MaxResults": 10001,
            }),
            "Member must have value less than or equal to 10000",
        ),
        (
            "a service code above its length",
            "ListPriceLists",
            json!({
                "ServiceCode": "A".repeat(33),
                "EffectiveDate": JAN_1_2024,
                "CurrencyCode": "USD",
            }),
            "Member must have length less than or equal to 32",
        ),
        (
            "a price-list ARN outside its pattern",
            "GetPriceListFileUrl",
            json!({ "PriceListArn": "not-an-arn", "FileFormat": "json" }),
            "Member must satisfy regular expression pattern",
        ),
    ];

    for (case, operation, body, fragment) in rows {
        let (status, parsed) = call(&router, operation, &body).await;
        assert_eq!(
            (status, parsed.get("__type").and_then(Value::as_str)),
            (400, Some("InvalidParameterException")),
            "case {case:?}: expected 400 InvalidParameterException, got {status} with {parsed}"
        );
        let stated = text_under(&parsed, MODELLED_MESSAGE_MEMBER);
        assert!(
            stated.is_some_and(|text| text.contains(fragment)),
            "case {case:?}: expected {MODELLED_MESSAGE_MEMBER} to state {fragment:?}, got \
             {parsed}"
        );
        assert!(
            stated.is_some_and(|text| text.starts_with("1 validation error detected: ")
                || text.contains(" validation errors detected: ")),
            "case {case:?}: expected the validation envelope, got {parsed}"
        );
        assert!(
            text_under(&parsed, RENDERED_MESSAGE_MEMBER).is_none(),
            "case {case:?}: the text must be carried under ONE member, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_handler_refusal_reports_its_text_in_the_same_member() {
    let router = router();

    // The other layer that answers a failure: a modelled error a HANDLER returns, which is
    // serialized from the model's own shape. Same member, same single spelling — one row
    // per handler-answered class this service has, and for the format version every
    // operation that takes the member states the one sentence.
    let rows: Vec<(&str, &str, Value, u16, &str, &str)> = vec![
        (
            "a format version GetProducts does not publish",
            "GetProducts",
            json!({ "ServiceCode": "AmazonEC2", "FormatVersion": "aws_v2" }),
            400,
            "InvalidParameterException",
            UNACCEPTED_FORMAT_VERSION_MESSAGE,
        ),
        (
            "a format version DescribeServices does not publish",
            "DescribeServices",
            json!({ "FormatVersion": "v1.0" }),
            400,
            "InvalidParameterException",
            UNACCEPTED_FORMAT_VERSION_MESSAGE,
        ),
        (
            "a next token this service never issued",
            "GetProducts",
            json!({ "ServiceCode": "AmazonEC2", "NextToken": "not-a-token" }),
            400,
            "InvalidNextTokenException",
            "NextToken is invalid.",
        ),
        (
            "a price list ARN that resolves to nothing",
            "GetPriceListFileUrl",
            json!({
                "PriceListArn":
                    "arn:aws:pricing:::price-list/aws/AmazonEC2/USD/19700101000000/us-east-1",
                "FileFormat": "json",
            }),
            400,
            "NotFoundException",
            "arn:aws:pricing:::price-list/",
        ),
    ];

    for (case, operation, body, status, code, fragment) in rows {
        let (actual_status, parsed) = call(&router, operation, &body).await;
        assert_eq!(
            (actual_status, parsed.get("__type").and_then(Value::as_str)),
            (status, Some(code)),
            "case {case:?}: expected {status} {code}, got {actual_status} with {parsed}"
        );
        assert!(
            text_under(&parsed, MODELLED_MESSAGE_MEMBER)
                .is_some_and(|text| text.contains(fragment)),
            "case {case:?}: expected {MODELLED_MESSAGE_MEMBER} to state {fragment:?}, got \
             {parsed}"
        );
        assert!(
            text_under(&parsed, RENDERED_MESSAGE_MEMBER).is_none(),
            "case {case:?}: the text must be carried under ONE member, got {parsed}"
        );
    }
}

#[tokio::test]
async fn an_enumerated_clause_quotes_the_value_set_the_service_states() {
    let router = router();

    // Each row: the case, the filter list, and the member path the clause names — the bad
    // element at the head of the list, in the middle of one, and at its end, so the rule
    // holds wherever the offending element sits.
    let rows: Vec<(&str, Vec<Value>, &str)> = vec![
        (
            "the only filter",
            vec![json!({ "Type": "EXACT", "Field": "instanceType", "Value": "t3.nano" })],
            "filters.1.member.type",
        ),
        (
            "the second of two filters",
            vec![
                term_match("ServiceCode", "AmazonEC2"),
                json!({ "Type": "EXACT", "Field": "instanceType", "Value": "t3.nano" }),
            ],
            "filters.2.member.type",
        ),
        (
            "the last of three filters",
            vec![
                term_match("ServiceCode", "AmazonEC2"),
                term_match("instanceType", "t3.nano"),
                json!({ "Type": "term_match", "Field": "location", "Value": "US East" }),
            ],
            "filters.3.member.type",
        ),
    ];

    for (case, filters, path) in rows {
        let body = json!({ "ServiceCode": "AmazonEC2", "Filters": filters });
        let (status, parsed) = call(&router, "GetProducts", &body).await;
        assert_eq!(
            (status, parsed.get("__type").and_then(Value::as_str)),
            (400, Some("InvalidParameterException")),
            "case {case:?}: expected 400 InvalidParameterException, got {status} with {parsed}"
        );
        let stated = text_under(&parsed, MODELLED_MESSAGE_MEMBER).unwrap_or_default();
        assert!(
            stated.contains(&format!("at '{path}' ")),
            "case {case:?}: expected the clause to name {path:?}, got {stated:?}"
        );
        assert!(
            stated.ends_with(&format!(
                "Member must satisfy enum value set: {FILTER_TYPE_SET_AS_STATED}"
            )),
            "case {case:?}: expected the set {FILTER_TYPE_SET_AS_STATED}, got {stated:?}"
        );
    }

    // The quoting moved, the CHECK did not: every value the model declares is still served,
    // and each one filters the catalog rather than being refused.
    for filter_type in ["TERM_MATCH", "EQUALS", "CONTAINS", "ANY_OF", "NONE_OF"] {
        let body = json!({
            "ServiceCode": "AmazonEC2",
            "Filters": [{
                "Type": filter_type,
                "Field": "instanceType",
                "Value": "t3.nano",
            }],
        });
        let served = ok(&router, "GetProducts", &body).await;
        assert!(
            served.get("PriceList").is_some(),
            "case {filter_type}: a modelled filter type must be served, got {served}"
        );
    }

    // And a value the service NAMES in that set but its model does not declare is still
    // refused by the model's own check, with the one code this model declares for a bad
    // input — the set this service quotes is the rendering of the requirement, not a
    // widening of it.
    let (status, parsed) = call(
        &router,
        "GetProducts",
        &json!({
            "ServiceCode": "AmazonEC2",
            "Filters": [{ "Type": "TERM_CONTAIN", "Field": "instanceType", "Value": "t3" }],
        }),
    )
    .await;
    assert_eq!(
        (status, parsed.get("__type").and_then(Value::as_str)),
        (400, Some("InvalidParameterException")),
        "case a value outside the model's own set: expected 400 InvalidParameterException, \
         got {status} with {parsed}"
    );

    // The refusals above are the only thing that changed: a well-formed query is still
    // answered from the stored catalog, paged to the end.
    let products = all_items(
        &router,
        "GetProducts",
        &json!({ "ServiceCode": "AmazonEC2" }),
        "PriceList",
    )
    .await;
    assert!(
        !products.is_empty(),
        "case a well-formed query: expected the stored offers, got none"
    );
}
