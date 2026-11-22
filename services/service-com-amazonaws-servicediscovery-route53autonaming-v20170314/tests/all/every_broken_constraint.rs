// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CONSTRAINTS a refused request is told about: all of them.
//!
//! RULE: a request outside the constraints the model declares is refused with one counted
//! envelope carrying one clause per constraint it broke — the value the caller SUBMITTED,
//! the member path it sat at and the requirement it failed — so a value breaking two
//! constraints is reported as two clauses rather than as the first one found. Two halves of
//! that the framework's own boundary walk cannot state:
//!
//! * a modelled `@pattern` carrying LOOK-AROUND, which its engine cannot compile and
//!   therefore treats as satisfied — three members of this model carry one
//!   (`smithy_query(query="shape", shape="NamespaceNameHttp,NamespaceNamePrivate,
//!   ServiceName")`);
//! * the ORDER of two clauses about one member: the member's `@pattern` requirement is
//!   stated before its `@length` requirement.
//!
//! The rows differ along what the rule generalises over: which member carries the
//! expression, how many constraints the value breaks (one, two), which way it leaves the
//! expression (the reserved `arn:` prefix, a character the class excludes, a run past the
//! length the expression itself bounds), and the SIZE of the value — including one past the
//! ceiling the framework's own clause elides a value at, which an AWS clause never does. The
//! last table is the positive control: a value every declared constraint admits is served,
//! and nothing the refusals above named was stored.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use serde_json::{json, Value};

use crate::cloud_map_core::{call, emulator, ok, refusal};

/// The VPC a private DNS namespace is created for; nothing in these cases turns on it.
const VPC: &str = "vpc-0a1b2c3d";

/// The one error this model declares for a bad request, which is what the service's main
/// endpoint refuses one with.
const INVALID_INPUT: &str = "InvalidInput";

/// The `@pattern` and `@length` maximum the model declares on `NamespaceNamePrivate`
/// (`CreatePrivateDnsNamespace$Name`): `smithy_query(query="shape",
/// shape="NamespaceNamePrivate")`. A clause quotes the expression as the model states it.
const PRIVATE_NAME_PATTERN: &str = "^(?!arn:)[!-~]{1,253}$";
const PRIVATE_NAME_MAX: usize = 253;

/// The same pair on `NamespaceNameHttp` (`CreateHttpNamespace$Name`):
/// `smithy_query(query="shape", shape="NamespaceNameHttp")`.
const HTTP_NAME_PATTERN: &str = "^(?!arn:)[!-~]{1,1024}$";
const HTTP_NAME_MAX: usize = 1024;

/// The `@pattern` the model declares on `ServiceName` (`CreateService$Name`), which bounds
/// the name's own length with a look-ahead and declares no `@length` beside it:
/// `smithy_query(query="shape", shape="ServiceName")`.
const SERVICE_NAME_PATTERN: &str =
    r"^((?=^.{1,127}$)^([a-zA-Z0-9_][a-zA-Z0-9-_]{0,61}[a-zA-Z0-9_]|[a-zA-Z0-9])(\.([a-zA-Z0-9_][a-zA-Z0-9-_]{0,61}[a-zA-Z0-9_]|[a-zA-Z0-9]))*$)|(^\.$)$";

/// The `@length` maximum of `NamespaceNamePublic` (`CreatePublicDnsNamespace$Name`), whose
/// expression the boundary validator DOES compile: `smithy_query(query="shape",
/// shape="NamespaceNamePublic")`.
const PUBLIC_NAME_MAX: usize = 253;

/// One clause: the value the request submitted, the member path the walk states it at, and
/// the requirement it failed.
fn clause(submitted: &str, path: &str, requirement: &str) -> String {
    format!("Value '{submitted}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement a `@pattern` states, quoting the expression as the model declares it.
fn pattern_requirement(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The requirement a `@length` maximum states.
fn length_requirement(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The counted envelope over `clauses`, as the framework words it: `1 validation error
/// detected: …` for one, `<n> validation errors detected: …; …` for several.
fn envelope(clauses: &[String]) -> String {
    match clauses.len() {
        1 => format!("1 validation error detected: {}", clauses[0]),
        n => format!("{n} validation errors detected: {}", clauses.join("; ")),
    }
}

/// A `Name` of `length` characters the character classes of every name shape admit.
fn name_of(length: usize) -> String {
    "x".repeat(length)
}

/// A dotted DNS name of `length` characters whose labels the public DNS name pattern
/// admits: labels of 60 characters, so no label passes the 63 a label grammar allows.
fn dotted_name_of(length: usize) -> String {
    let mut name = String::new();
    while name.len() < length {
        if !name.is_empty() {
            name.push('.');
        }
        let room = length - name.len();
        name.push_str(&"a".repeat(room.min(60)));
    }
    name
}

/// RULE: every constraint one value breaks is stated, the member's pattern clause first, and
/// the value is echoed exactly as submitted — over the members whose expression the
/// boundary validator declares and cannot apply.
#[tokio::test]
async fn a_refusal_states_every_constraint_the_value_broke() {
    let router = emulator();

    // A name one character past the run the private name's own expression admits, which is
    // also one past its `@length` maximum: it breaks BOTH.
    let private_over = name_of(PRIVATE_NAME_MAX + 1);
    // Far past both, and past the 256-byte ceiling the framework's own walk elides a clause
    // value at — an AWS clause quotes the value in full, whatever its size.
    let private_far_over = name_of(301);
    // The reserved prefix, inside the length: the pattern alone.
    let private_reserved = "arn:aws:servicediscovery:us-west-2:000000000000:namespace/ns-0";
    // A character the class `[!-~]` excludes, inside the length: the pattern alone.
    let private_space = "has a space.internal";
    // The HTTP name's own maximum is four times larger, so its over-length row is a value
    // no clause could carry unelided unless the echo really is the submitted one.
    let http_over = name_of(HTTP_NAME_MAX + 1);
    let http_reserved = "arn:aws:iam::000000000000:role/example";
    // A service name one character past the length its look-ahead bounds, written in labels
    // the rest of its expression admits, so the look-ahead is the only thing it breaks.
    let service_over = dotted_name_of(128);
    // A public DNS name past its `@length` maximum whose labels its expression admits: the
    // boundary applies that expression itself, so the length clause is the whole refusal.
    let public_over = dotted_name_of(PUBLIC_NAME_MAX + 1);

    // (case, operation, the request, the clauses the refusal states in order)
    let rows: Vec<(&str, &str, Value, Vec<String>)> = vec![
        (
            "a private DNS name one past both its bounds",
            "CreatePrivateDnsNamespace",
            json!({ "Name": private_over, "Vpc": VPC }),
            vec![
                clause(&private_over, "name", &pattern_requirement(PRIVATE_NAME_PATTERN)),
                clause(&private_over, "name", &length_requirement(PRIVATE_NAME_MAX)),
            ],
        ),
        (
            "a private DNS name far past both, past the framework's echo ceiling",
            "CreatePrivateDnsNamespace",
            json!({ "Name": private_far_over, "Vpc": VPC }),
            vec![
                clause(
                    &private_far_over,
                    "name",
                    &pattern_requirement(PRIVATE_NAME_PATTERN),
                ),
                clause(&private_far_over, "name", &length_requirement(PRIVATE_NAME_MAX)),
            ],
        ),
        (
            "a private DNS name carrying the prefix the expression reserves",
            "CreatePrivateDnsNamespace",
            json!({ "Name": private_reserved, "Vpc": VPC }),
            vec![clause(
                private_reserved,
                "name",
                &pattern_requirement(PRIVATE_NAME_PATTERN),
            )],
        ),
        (
            "a private DNS name carrying a character the class excludes",
            "CreatePrivateDnsNamespace",
            json!({ "Name": private_space, "Vpc": VPC }),
            vec![clause(
                private_space,
                "name",
                &pattern_requirement(PRIVATE_NAME_PATTERN),
            )],
        ),
        (
            "an HTTP namespace name one past both its bounds",
            "CreateHttpNamespace",
            json!({ "Name": http_over }),
            vec![
                clause(&http_over, "name", &pattern_requirement(HTTP_NAME_PATTERN)),
                clause(&http_over, "name", &length_requirement(HTTP_NAME_MAX)),
            ],
        ),
        (
            "an HTTP namespace name carrying the reserved prefix",
            "CreateHttpNamespace",
            json!({ "Name": http_reserved }),
            vec![clause(
                http_reserved,
                "name",
                &pattern_requirement(HTTP_NAME_PATTERN),
            )],
        ),
        (
            "a service name past the length its own expression bounds",
            "CreateService",
            json!({ "Name": service_over, "NamespaceId": "ns-0000000000000000" }),
            vec![clause(
                &service_over,
                "name",
                &pattern_requirement(SERVICE_NAME_PATTERN),
            )],
        ),
        (
            "a public DNS name past its length, whose expression the boundary applies",
            "CreatePublicDnsNamespace",
            json!({ "Name": public_over }),
            vec![clause(
                &public_over,
                "name",
                &length_requirement(PUBLIC_NAME_MAX),
            )],
        ),
    ];

    for (case, operation, request, clauses) in rows {
        let (status, code, message) = refusal(&router, operation, request).await;
        assert_eq!(
            (status, code.as_str()),
            (400, INVALID_INPUT),
            "case {case:?} on {operation}: expected 400 {INVALID_INPUT}, got {status} {code} \
             {message:?}"
        );
        let expected = envelope(&clauses);
        assert_eq!(
            message.as_deref(),
            Some(expected.as_str()),
            "case {case:?} on {operation}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE, at the service's OTHER endpoint: the operations the model hosts on an endpoint of
/// their own apply the same set of modelled constraints — the ones the boundary validator
/// cannot compile included — and differ only in the error CODE the generic validation layer
/// in front of that endpoint names.
#[tokio::test]
async fn the_data_endpoint_applies_the_same_expressions_under_its_own_code() {
    /// The error code that layer refuses a request with; this model declares no such shape.
    const VALIDATION_EXCEPTION: &str = "ValidationException";

    let router = emulator();
    let (_, namespace_id) = crate::cloud_map_core::namespace(&router, "discover-ns").await;
    crate::cloud_map_core::service(&router, &namespace_id, "discover-svc").await;

    // A service name past the length its own expression bounds, and one carrying a
    // character its label grammar excludes: both are values `DiscoverInstances$ServiceName`
    // targets `ServiceName` for.
    for (case, bad) in [
        ("past the length the expression bounds", dotted_name_of(128)),
        ("a character the label grammar excludes", "has a space".to_string()),
    ] {
        for operation in ["DiscoverInstances", "DiscoverInstancesRevision"] {
            let (status, code, message) = refusal(
                &router,
                operation,
                json!({ "NamespaceName": "discover-ns", "ServiceName": bad }),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, VALIDATION_EXCEPTION),
                "case {case:?} on {operation}: expected 400 {VALIDATION_EXCEPTION}, got {status} \
                 {code} {message:?}"
            );
            let expected = envelope(&[clause(
                &bad,
                "serviceName",
                &pattern_requirement(SERVICE_NAME_PATTERN),
            )]);
            assert_eq!(
                message.as_deref(),
                Some(expected.as_str()),
                "case {case:?} on {operation}: expected {expected:?}, got {message:?}"
            );
        }
    }

    // A name the expression admits reaches the operation behind that layer, which answers
    // from the store rather than refusing the request.
    let (status, answer) = call(
        &router,
        "DiscoverInstances",
        json!({ "NamespaceName": "discover-ns", "ServiceName": "discover-svc" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a service name the expression admits is served, got {status}: {answer}"
    );
}

/// RULE, the positive control: a value every declared constraint admits is served — at each
/// published bound and inside it — and none of the names the refusals above named reached
/// the store.
#[tokio::test]
async fn a_value_every_constraint_admits_is_served_and_a_refused_name_is_not_stored() {
    let router = emulator();

    // (case, operation, the request)
    let served: Vec<(&str, &str, Value)> = vec![
        (
            "a private DNS name at its published maximum",
            "CreatePrivateDnsNamespace",
            json!({ "Name": name_of(PRIVATE_NAME_MAX), "Vpc": VPC }),
        ),
        (
            "a private DNS name carrying the reserved letters elsewhere in the value",
            "CreatePrivateDnsNamespace",
            json!({ "Name": "my.arn:name.internal", "Vpc": VPC }),
        ),
        (
            "an HTTP namespace name at its published maximum",
            "CreateHttpNamespace",
            json!({ "Name": name_of(HTTP_NAME_MAX) }),
        ),
        (
            "an HTTP namespace name of one character",
            "CreateHttpNamespace",
            json!({ "Name": "x" }),
        ),
        (
            "a public DNS name at its published maximum",
            "CreatePublicDnsNamespace",
            json!({ "Name": dotted_name_of(PUBLIC_NAME_MAX) }),
        ),
    ];
    for (case, operation, request) in served {
        let (status, answer) = call(&router, operation, request).await;
        assert_eq!(
            status, 200,
            "case {case:?} on {operation}: every declared constraint admits this value, got \
             {status}: {answer}"
        );
    }

    // A service name at the length its expression bounds is served too, in a namespace that
    // exists.
    let (_, namespace_id) = crate::cloud_map_core::namespace(&router, "admitted-ns").await;
    let (status, answer) = call(
        &router,
        "CreateService",
        json!({ "Name": dotted_name_of(127), "NamespaceId": namespace_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a service name at the length its expression bounds is served, got {status}: {answer}"
    );

    // Nothing a refusal named was stored: the names above are the ones the first table
    // refused.
    let refused = [
        name_of(PRIVATE_NAME_MAX + 1),
        name_of(301),
        name_of(HTTP_NAME_MAX + 1),
    ];
    for name in refused {
        let (status, _, _) = refusal(
            &router,
            "CreatePrivateDnsNamespace",
            json!({ "Name": name, "Vpc": VPC }),
        )
        .await;
        assert_eq!(
            status, 400,
            "a name outside the declared constraints stays refused, got {status}"
        );
    }
    let listed = ok(&router, "ListNamespaces", json!({})).await;
    let names: Vec<String> = listed["Namespaces"]
        .as_array()
        .expect("ListNamespaces reports its namespaces")
        .iter()
        .filter_map(|summary| summary["Name"].as_str().map(str::to_string))
        .collect();
    for refused in [
        name_of(PRIVATE_NAME_MAX + 1),
        name_of(301),
        name_of(HTTP_NAME_MAX + 1),
    ] {
        assert!(
            !names.contains(&refused),
            "a refused name must not be stored: {} characters of it are listed",
            refused.len()
        );
    }
}
