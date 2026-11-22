// SPDX-License-Identifier: CC0-1.0

//! The EXPRESSION a constraint refusal of this service quotes back, driven end to end over
//! HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct and an answer that is
//! not computed from persisted state cannot pass.
//!
//! The rule under test: a clause about a value outside its member's `@pattern` quotes the
//! expression this service PUBLISHES for that member's SHAPE. For almost every shape of this
//! model that is the model's text, anchors and all; for the shapes whose published
//! expression is anchored at its start only it is that text without the `$` that closes it;
//! and for the shapes published as the bare grammar a value is made of it is that text
//! without either anchor. Which of the three a shape is published with belongs to the shape,
//! so it holds for every member targeting it, in whatever operation and whatever binding
//! carried the value, and the CHECK is unaffected: the values each expression admits are
//! exactly the values it admitted before.
//!
//! It is asserted over a TABLE spanning all three forms — the two shapes this service
//! publishes without their closing anchor (a directory alias and an OCSP URL, reached through
//! different operations, one of them inside a structure the request nests), the one it
//! publishes without either anchor (an event topic name, reached through BOTH operations that
//! carry it) and two it publishes as declared (two id spaces) — with a value each expression
//! ADMITS as the positive control, so no row can pass by matching one recorded message.

use serde_json::{json, Value};

use crate::harness::{call, service};

/// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`) this account never creates: every row
/// below is decided before any handler resolves it.
const ABSENT_DIRECTORY_ID: &str = "d-0123456789";

/// One row: the operation a refused value is sent to, the member path the clause names, the
/// value that is refused, and the expression the clause must quote.
struct PublishedCase {
    case: &'static str,
    operation: &'static str,
    member: &'static str,
    value: &'static str,
    /// The expression the refusal publishes. Each row's modelled text is named in the
    /// comment beside it, from `smithy_query(query="shape",shape="<Shape>")`.
    published: &'static str,
    body: fn(&str) -> Value,
}

/// The shapes this rule is asserted over, both forms together.
fn published_rows() -> Vec<PublishedCase> {
    vec![
        // `AliasName`: the model declares `^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*$` and the
        // service publishes it without the closing anchor.
        PublishedCase {
            case: "a directory alias",
            operation: "CreateAlias",
            member: "alias",
            value: "SIM2REAL_Invalid_Alias",
            published: r"^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*",
            body: |value| json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "Alias": value }),
        },
        // `OCSPUrl`: the model declares
        // `^(https?|ftp|file|ldaps?)://[-a-zA-Z0-9+&@#/%?=~_|!:,.;()]*[-a-zA-Z0-9+&@#/%=~_|()]$`
        // and the service publishes it without the closing anchor. The member sits inside a
        // structure the request nests, so the clause names it by its dotted path.
        PublishedCase {
            case: "an OCSP URL inside a registration's client certificate settings",
            operation: "RegisterCertificate",
            member: "clientCertAuthSettings.oCSPUrl",
            value: "not-a-valid-url",
            published: r"^(https?|ftp|file|ldaps?)://[-a-zA-Z0-9+&@#/%?=~_|!:,.;()]*[-a-zA-Z0-9+&@#/%=~_|()]",
            body: |value| {
                json!({
                    "DirectoryId": ABSENT_DIRECTORY_ID,
                    "CertificateData": crate::certificates::certificate_pem(
                        Some("published.example.com"),
                        (2033, 8, 9),
                    ).0,
                    "ClientCertAuthSettings": { "OCSPUrl": value }
                })
            },
        },
        // `CertificateId`: published as the model declares it, anchors included.
        PublishedCase {
            case: "a certificate id",
            operation: "DescribeCertificate",
            member: "certificateId",
            value: "cert-0123456789",
            published: r"^c-[0-9a-f]{10}$",
            body: |value| {
                json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "CertificateId": value })
            },
        },
        // `AssessmentId`: the same, in another id space.
        PublishedCase {
            case: "an assessment id",
            operation: "DescribeADAssessment",
            member: "assessmentId",
            value: "da-0123456789",
            published: r"^da-[0-9a-f]{18}$",
            body: |value| json!({ "AssessmentId": value }),
        },
        // `TopicName`: the model declares `^[a-zA-Z0-9_-]+$` and the service publishes the
        // bare grammar — neither anchor — for it.
        PublishedCase {
            case: "an event topic name",
            operation: "RegisterEventTopic",
            member: "topicName",
            value: "sim2real.behav.topic",
            published: r"[a-zA-Z0-9_-]+",
            body: |value| {
                json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "TopicName": value })
            },
        },
        // The same shape in the other operation that carries it: what is published belongs
        // to the shape, not to the entry point.
        PublishedCase {
            case: "an event topic name a deregistration names",
            operation: "DeregisterEventTopic",
            member: "topicName",
            value: "sim2real topic",
            published: r"[a-zA-Z0-9_-]+",
            body: |value| {
                json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "TopicName": value })
            },
        },
    ]
}

/// The whole clause a refused value is answered with.
fn published_clause(value: &str, member: &str, published: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {published}"
    )
}

/// RULE: the clause quotes the expression this service publishes for the member's shape.
#[tokio::test]
async fn a_pattern_clause_quotes_the_expression_the_service_publishes() {
    let router = service();

    for row in published_rows() {
        let (status, parsed) = call(&router, row.operation, (row.body)(row.value)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the value to be refused, got {status} with {parsed}",
            row.case
        );
        assert_eq!(
            parsed["__type"],
            json!("ValidationException"),
            "case {:?}: expected the boundary's validation envelope, got {parsed}",
            row.case
        );
        let stated = parsed["message"].as_str().unwrap_or_default();
        assert_eq!(
            stated,
            published_clause(row.value, row.member, row.published),
            "case {:?}: expected the published expression, got {stated:?}",
            row.case
        );
    }
}

/// The expression this service publishes for the `TopicName` shape, whose model text is
/// `^[a-zA-Z0-9_-]+$` (`smithy_query(query="shape",shape="TopicName")`): the bare grammar a
/// topic name is made of, which is what the recorded refusal of an event-topic registration
/// quotes back.
const PUBLISHED_TOPIC_NAME: &str = r"[a-zA-Z0-9_-]+";

/// The ways a topic name can fall outside the class its expression admits, one row per form
/// rather than one recorded value.
///
/// The last two rows are the ones the PUBLISHED expression would match somewhere inside: each
/// CONTAINS a run of admitted characters, so they are refused only because the CHECK is still
/// the model's anchored text — which is what ties the quoted expression to the one the walk
/// applies.
fn topic_names_outside_the_class() -> [(&'static str, &'static str); 7] {
    [
        ("dots between the labels", "sim2real.behav.topic"),
        ("a space", "sim2real behav"),
        ("a slash", "sim2real/behav"),
        ("a colon", "topic:behav"),
        ("a non-ASCII letter", "sim2réal"),
        ("an admitted run with a trailing dot", "sim2real-behav."),
        ("an admitted run with a leading bang", "!sim2real_behav"),
    ]
}

/// RULE (the same rule read along the value and the operation): what is published belongs to
/// the member's SHAPE, so every value that shape's expression refuses draws the same
/// published text in either operation that carries the member, while the CHECK stays the
/// model's own anchored expression.
#[tokio::test]
async fn every_refused_topic_name_publishes_the_same_expression() {
    let router = service();

    for operation in ["RegisterEventTopic", "DeregisterEventTopic"] {
        for (form, value) in topic_names_outside_the_class() {
            let case = format!("{operation}: {form}");
            let (status, parsed) = call(
                &router,
                operation,
                json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "TopicName": value }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the value to be refused, got {status} with {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("ValidationException"),
                "case {case:?}: expected the boundary's validation envelope, got {parsed}"
            );
            let stated = parsed["message"].as_str().unwrap_or_default();
            assert_eq!(
                stated,
                published_clause(value, "topicName", PUBLISHED_TOPIC_NAME),
                "case {case:?}: expected the published expression, got {stated:?}"
            );
        }
    }
}

/// RULE (the positive control for the same rule): a value its expression ADMITS is not
/// refused by that expression at all — the publishing of the text changed no check, so the
/// call gets as far as resolving the directory it names and is refused on THAT instead.
#[tokio::test]
async fn a_value_its_published_expression_admits_is_not_refused_by_it() {
    let router = service();

    // One admitted value per shape the rows above refuse a value of, including the two
    // published without their closing anchor: an alias and an OCSP URL the whole expression
    // matches.
    let (pem, _) = crate::certificates::certificate_pem(Some("admitted.example.com"), (2033, 8, 9));
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "an alias the expression admits",
            "CreateAlias",
            json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "Alias": "sim2real-behav-1" }),
        ),
        (
            "an OCSP URL the expression admits",
            "RegisterCertificate",
            json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "CertificateData": pem,
                "ClientCertAuthSettings": { "OCSPUrl": "http://ocsp.example.com/check" }
            }),
        ),
        (
            "a topic name the expression admits",
            "RegisterEventTopic",
            json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "TopicName": "sim2real-behav-topic" }),
        ),
    ];

    for (case, operation, body) in rows {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the absent directory to be refused, got {status} with \
             {parsed}"
        );
        let wire_type = parsed["__type"].as_str().unwrap_or_default();
        assert!(
            !wire_type.contains("ValidationException"),
            "case {case:?}: expected the resolution failure rather than a constraint \
             refusal, got {parsed}"
        );
    }
}
