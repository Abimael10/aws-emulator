// SPDX-License-Identifier: CC0-1.0

//! A request that leaves out a member its operation requires in the QUERY STRING or in a
//! HEADER, end to end over HTTP, on every route of this model that binds one.
//!
//! The RULE under test: a request whose method and path are the ones an operation of this
//! service binds IS that operation's request, so a `@required` request-observable member it
//! does not carry is a statement about the REQUEST — this service's `ValidationException`
//! (HTTP 400) in the framework's own absent-member sentence, naming the member — and never
//! a report that the operation is not implemented (HTTP 501 `NotImplemented`, the claim
//! `src/scaffolded.rs` reserves for an operation nobody has written). The framework's
//! required-binding check exists to tell several operations apart on ONE method + URI
//! template by the shape of the request (`route_lookup`,
//! `vendor/smithy-schema-server/src/rest_xml.rs`); it picks BETWEEN operations and cannot
//! un-name the only operation a route has. Because such a request is refused before any
//! handler runs, it also changes nothing a later caller sees.
//!
//! The rows are DERIVED FROM THE MODEL rather than written out here: the suite walks this
//! service's own Smithy model for every operation whose input binds a `@required` member to
//! the query string or to a header, so the class is covered whole and an operation the model
//! adds to it is covered the day it arrives. They span what the rule generalises over: the
//! kind of binding (a query parameter, six headers), the HTTP method (DELETE, POST), which
//! resource kind the addressed ARN names (a `Domain` and a `Subscription`), the shapes
//! "absent" takes on the wire (no query string at all, an empty one, another parameter, a
//! parameter whose name merely starts with the bound one, several unrelated parameters) —
//! because the presence test compares parameter NAMES — and the parameter present, both
//! carrying a value and carrying none.
//!
//! The negative rows matter as much: a request that addresses NO route of this service — an
//! unknown path, a path one segment past a route's, the collection path with no label, a
//! method the route does not have — keeps the protocol's own routing answer, so the rule
//! cannot turn every unrouted request into a 400.
//!
//! Sources for every value written by hand here:
//!
//! * `smithy_query(query="operation",operation="UntagResource,StartMedicalScribeListeningSession",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`
//!   — the two operations of [`KNOWN_OPERATIONS`], `tagKeys` being `required` with
//!   `httpQuery: "tagKeys"` and the six `required` `httpHeader` members of the streaming
//!   start.
//! * The absent-member sentence is the FRAMEWORK's own
//!   (`aws_smithy_schema_server::missing_member_message`), rendered here through that
//!   function rather than restated, and the member path through the framework's own path
//!   renderer — so this suite cannot word the refusal differently from the boundary that
//!   raises it.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::domain_subscription_lifecycle::{error_code, string_at, Emulator};
use crate::list_tags_for_resource::list_tags;
use crate::resource_tag_removal::untag_path;
use crate::resource_tagging::{tag_resource, tags_path};

/// This service's Smithy model, so the routes below are the model's own rather than a copy
/// of them that can drift.
const MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/com-amazonaws-connecthealth-connecthealth.json"
));

/// The operations this model binds a `@required` member to the query string or to a header
/// on (see the module doc for the `smithy_query` they come from). Asserted to be a subset of
/// what the walk below finds, so a walk that silently stopped finding routes cannot pass for
/// a fix.
const KNOWN_OPERATIONS: &[&str] = &["StartMedicalScribeListeningSession", "UntagResource"];

/// The Region every request of this suite is signed for.
const REGION: &str = "eu-west-1";

/// One route of this service whose input binds a `@required` member to the query string or
/// to a header, as this model declares it.
#[derive(Debug)]
struct RequiredBindingRoute {
    /// The operation's shape name.
    operation: String,
    /// Its `@http` method.
    method: String,
    /// Its `@http` URI pattern, labels unfilled.
    uri: String,
    /// Each `@required` + `@httpQuery` member: the parameter name on the wire.
    query: Vec<String>,
    /// The member path each `@required` query-bound or header-bound member is named by in a
    /// refusal.
    member_paths: Vec<String>,
}

/// Every route of this model whose input binds a `@required` member to the query string or
/// to a header, in operation order.
///
/// Read off the model: an operation shape carrying `@http`, whose input declares a member
/// with `smithy.api#required` and either `smithy.api#httpQuery` or `smithy.api#httpHeader`.
/// Nothing about the route is written out here, so a model that moves a route, renames a
/// parameter or adds an operation to this class moves the rows with it.
fn required_binding_routes() -> Vec<RequiredBindingRoute> {
    let model: Value = serde_json::from_str(MODEL).expect("this service's Smithy model parses");
    let shapes = model["shapes"]
        .as_object()
        .expect("the model declares shapes")
        .clone();

    let mut routes: Vec<RequiredBindingRoute> = Vec::new();
    for (shape_id, shape) in &shapes {
        if shape["type"].as_str() != Some("operation") {
            continue;
        }
        let Some(http) = shape["traits"].get("smithy.api#http") else {
            continue;
        };
        let Some(input) = shape["input"]["target"]
            .as_str()
            .and_then(|target| shapes.get(target))
        else {
            continue;
        };
        let mut query: Vec<String> = Vec::new();
        let mut member_paths: Vec<String> = Vec::new();
        for (member_name, member) in input["members"].as_object().into_iter().flatten() {
            if member["traits"].get("smithy.api#required").is_none() {
                continue;
            }
            let bound_to_query = member["traits"].get("smithy.api#httpQuery");
            if bound_to_query.is_none() && member["traits"].get("smithy.api#httpHeader").is_none() {
                continue;
            }
            if let Some(parameter) = bound_to_query {
                // A trait carrying no value names the member itself, which is how the
                // framework's own binding table reads it.
                query.push(
                    parameter
                        .as_str()
                        .unwrap_or(member_name.as_str())
                        .to_string(),
                );
            }
            // The spelling a refusal states the member by, from the framework's own path
            // renderer rather than a second copy of its casing rule.
            member_paths.push(aws_smithy_schema_server::constraints::path::member(
                member_name,
            ));
        }
        if member_paths.is_empty() {
            continue;
        }
        routes.push(RequiredBindingRoute {
            operation: shape_id
                .rsplit('#')
                .next()
                .unwrap_or(shape_id.as_str())
                .to_string(),
            method: http["method"]
                .as_str()
                .expect("an @http trait states its method")
                .to_string(),
            uri: http["uri"]
                .as_str()
                .expect("an @http trait states its uri")
                .to_string(),
            query,
            member_paths,
        });
    }
    routes.sort_by(|left, right| left.operation.cmp(&right.operation));
    routes
}

/// The sentence a request that supplied nothing for the member at `member_path` is refused
/// with: the FRAMEWORK's own, so this suite cannot word it differently from the boundary
/// that raises it.
fn absent_member_sentence(member_path: &str) -> String {
    aws_smithy_schema_server::missing_member_message(member_path)
}

/// An `@httpLabel` value as a client sends it in one path segment: the same percent-encoding
/// the tagging suite addresses an ARN label with, so this suite and that one cannot encode
/// one ARN two ways.
fn encoded_label(value: &str) -> String {
    tags_path(value).trim_start_matches("/tags/").to_string()
}

/// The detail text of a refusal, read off the member every exception of this model declares.
fn message_of(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether `body` states one of `member_paths` as absent.
fn names_an_absent_member(body: &Value, member_paths: &[String]) -> bool {
    let message = message_of(body);
    member_paths
        .iter()
        .any(|path| message == absent_member_sentence(path))
}

/// The shapes "the request carries no `tagKeys`" takes on the wire, each as `(case, query)`.
///
/// The presence test the framework applies to a required query binding reads raw
/// `key[=value]` pairs off the query string and compares their NAMES
/// (`vendor/smithy-schema-server/src/rest_xml.rs`), so every row below supplies no pair named
/// `tagKeys` — whatever else it supplies — and every row has to be answered the same way.
const ABSENT_QUERIES: &[(&str, &str)] = &[
    ("no query string at all", ""),
    ("an empty query string", "?"),
    ("a query carrying only another parameter", "?resourceArn=env"),
    (
        "a parameter whose name only starts with the bound one",
        "?tagKeysExtra=env",
    ),
    ("several unrelated parameters", "?first=1&second=2"),
];

/// The tag set every subject starts from: more than one pair, so a refusal that removed some
/// keys and a removal that removed all of them are both visible.
fn starting_tags() -> Value {
    json!({ "env": "prod", "team": "clinical", "owner": "ops" })
}

/// [`starting_tags`] as a map, which is what a reader reports.
fn starting_map() -> BTreeMap<String, String> {
    starting_tags()
        .as_object()
        .expect("an object")
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.as_str().expect("a string value").to_string(),
            )
        })
        .collect()
}

/// The two ARNs a tagging call of this service addresses — a domain's and a subscription's —
/// each already carrying [`starting_tags`], as `(case, arn)`.
async fn tagged_subjects(emulator: &Emulator) -> Vec<(&'static str, String)> {
    let created = emulator
        .create_domain(&json!({ "name": "clinic", "tags": starting_tags() }))
        .await;
    let domain_id = string_at(&created, "domainId");
    let domain_arn = string_at(&created, "arn");

    let (status, subscription) = emulator
        .send(
            "POST",
            &format!("/domains/{domain_id}/subscriptions"),
            Some(&json!({})),
        )
        .await;
    assert_eq!(
        status, 201,
        "seeding a subscription expected 201, got {status} with {subscription}"
    );
    let subscription_arn = string_at(&subscription, "arn");
    tag_resource(
        emulator,
        "seeding the subscription",
        &subscription_arn,
        &starting_tags(),
    )
    .await;

    vec![
        ("a Domain ARN", domain_arn),
        ("a Subscription ARN", subscription_arn),
    ]
}

/// Every route this model binds a `@required` query parameter or header on refuses a request
/// that leaves it out with this service's modelled client-input error, naming the absent
/// member — never the scaffold's 501.
#[tokio::test]
async fn a_required_query_or_header_member_left_out_is_refused_as_a_client_error() {
    let emulator = Emulator::new(REGION);
    let routes = required_binding_routes();

    let found: Vec<&str> = routes
        .iter()
        .map(|route| route.operation.as_str())
        .collect();
    for operation in KNOWN_OPERATIONS {
        assert!(
            found.contains(operation),
            "case {operation:?}: the model walk must find every operation binding a required \
             query parameter or header, found {found:?}"
        );
    }

    // A label value of the form this service mints, addressing nothing any store holds: the
    // refusal under test is raised before any handler runs, so what a label addresses never
    // decides the answer.
    let label = format!(
        "arn:aws:health-agent:{REGION}:000000000000:domain/dom-aaaaaaaaaaaaaaaa"
    );
    for route in &routes {
        let target = route
            .uri
            .split('?')
            .next()
            .unwrap_or(&route.uri)
            .split('/')
            .map(|segment| match segment.starts_with('{') && segment.ends_with('}') {
                true => encoded_label(&label),
                false => segment.to_string(),
            })
            .collect::<Vec<String>>()
            .join("/");
        let case = format!("{} {} {target}", route.operation, route.method);

        // Every required query parameter and header left out — the only shape of "none" a
        // query-bound list has, since an empty list serializes to no parameter at all.
        let (status, body) = emulator.send(&route.method, &target, None).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a request leaving a required member out is the modelled client \
             refusal, got {status} with {body}"
        );
        assert!(
            names_an_absent_member(&body, &route.member_paths),
            "case {case:?}: the refusal must state one of {:?} as absent, got {body}",
            route.member_paths
        );

        // The same route with every query-bound member SUPPLIED, each carrying no value at
        // all: it is the parameter's name that routes the request, so the route matches and
        // whatever the answer is, it is neither the absent-member refusal for a parameter the
        // request carries nor the claim that the operation is unimplemented.
        if route.query.is_empty() {
            continue;
        }
        let supplied = format!("{target}?{}", route.query.join("&"));
        let (status, body) = emulator.send(&route.method, &supplied, None).await;
        assert_ne!(
            status, 501,
            "case {case:?}: a request carrying every required parameter reaches the \
             operation, got {status} with {body}"
        );
        for parameter in &route.query {
            let path = aws_smithy_schema_server::constraints::path::member(parameter);
            assert_ne!(
                message_of(&body),
                absent_member_sentence(&path),
                "case {case:?}: a parameter the request supplies must not be refused as \
                 absent, got {status} with {body}"
            );
        }
    }

    // The query-bound route, driven against subjects that really carry tags: every shape of
    // "absent" is the same refusal, it removes nothing, and the SAME route serves the removal
    // once the parameter is there.
    let subjects = tagged_subjects(&emulator).await;
    let absent_tag_keys = absent_member_sentence(
        &aws_smithy_schema_server::constraints::path::member("tagKeys"),
    );
    for (subject, arn) in &subjects {
        for (row, query) in ABSENT_QUERIES {
            let case = format!("{subject} / {row}");
            let target = format!("{}{query}", tags_path(arn));
            let (status, body) = emulator.send("DELETE", &target, None).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?} (DELETE {target}): a removal naming no key is the modelled \
                 client refusal, got {status} with {body}"
            );
            let message = message_of(&body);
            assert_eq!(
                message, absent_tag_keys,
                "case {case:?}: the refusal must name the absent member, got {message:?}"
            );

            // Refused before the handler ran, so every stored pair is still there.
            let reported = list_tags(&emulator, &case, arn).await;
            assert_eq!(
                reported,
                starting_map(),
                "case {case:?}: a refused removal must leave the stored set alone, got \
                 {reported:?}"
            );
        }

        // The SAME route with the parameter supplied: it serves, and it removes exactly the
        // key it names — so the refusals above reported a bad request, not a missing route.
        let case = format!("{subject} / the parameter supplied");
        let removed = "env".to_string();
        let (status, body) = emulator
            .send(
                "DELETE",
                &untag_path(arn, std::slice::from_ref(&removed)),
                None,
            )
            .await;
        assert_eq!(
            status, 204,
            "case {case:?}: the route is implemented and serves the removal, got {status} \
             with {body}"
        );
        let mut want = starting_map();
        want.remove(&removed);
        let reported = list_tags(&emulator, &case, arn).await;
        assert_eq!(
            reported, want,
            "case {case:?}: the removal must take exactly the key it names off, got \
             {reported:?}"
        );
    }
}

/// A request that addresses no route of this service keeps the protocol's own routing
/// answer, and a required parameter that IS on the query string routes whatever value it
/// carries — so only an ABSENT required member is read as a refusal of the request.
#[tokio::test]
async fn only_an_absent_required_member_turns_a_routing_failure_into_a_refusal() {
    let emulator = Emulator::new(REGION);
    let created = emulator
        .create_domain(&json!({ "name": "clinic", "tags": starting_tags() }))
        .await;
    let arn = string_at(&created, "arn");

    // (case, method, target): every row addresses no modelled route of this service, one way
    // each — an unknown path, the tagging collection with no label, a segment past the
    // route's path, and a method no route of that path has.
    for (case, method, target) in [
        ("a path no route of this service matches", "GET", "/nope".to_string()),
        ("the tagging path with no resource", "DELETE", "/tags".to_string()),
        (
            "a segment past the tagging route's path",
            "DELETE",
            format!("{}/extra", tags_path(&arn)),
        ),
        (
            "a method the tagging route does not have",
            "PUT",
            tags_path(&arn),
        ),
    ] {
        let (status, body) = emulator.send(method, &target, None).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (501, "NotImplemented"),
            "case {case:?} ({method} {target}): a request naming no modelled operation keeps \
             the protocol's routing answer, got {status} with {body}"
        );
    }

    // The parameter's NAME is what routes the request: present carrying no value at all, the
    // route matches and the VALUE is what is then judged — a different answer from the
    // absent-member refusal, and never the scaffold's 501.
    let absent_tag_keys = absent_member_sentence(
        &aws_smithy_schema_server::constraints::path::member("tagKeys"),
    );
    for (case, target) in [
        ("the parameter present carrying no value", format!("{}?tagKeys=", tags_path(&arn))),
        (
            "the parameter present carrying a key the resource does not carry",
            format!("{}?tagKeys=absent-key", tags_path(&arn)),
        ),
    ] {
        let (status, body) = emulator.send("DELETE", &target, None).await;
        assert_ne!(
            status, 501,
            "case {case:?} (DELETE {target}): a supplied parameter routes to the implemented \
             operation, got {status} with {body}"
        );
        let message = message_of(&body);
        assert_ne!(
            message, absent_tag_keys,
            "case {case:?}: a parameter the request supplies must not be refused as absent, \
             got {status} with {body}"
        );
    }

    // Nothing above named a key the domain carries, so its stored set is untouched.
    let reported = list_tags(&emulator, "after the routing rows", &arn).await;
    assert_eq!(
        reported,
        starting_map(),
        "a refused or empty removal must leave the stored set alone, got {reported:?}"
    );
}
