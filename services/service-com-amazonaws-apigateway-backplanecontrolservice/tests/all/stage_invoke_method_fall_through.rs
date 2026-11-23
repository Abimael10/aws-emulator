// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH resource of a deployed stage serves a call when several of its
//! resources match the path: the resolution is of a METHOD, not of a resource.
//!
//! THE RULE UNDER TEST: a deployed stage resolves a request against its matching resource
//! paths most specific first — a literal segment before a `{parameter}`, a `{parameter}`
//! before a greedy `{proxy+}` — and the first of them that DECLARES the request's verb serves
//! it, binding the literal segments the request carried as that resource's path parameters.
//! So a resource wins the call only for the verbs it declares: a verb it does not declare
//! falls through to the next matching resource. Only when NO matching resource declares the
//! verb does the stage answer the refusal it answers an unplaceable route with — `403`
//! `{"message":"Missing Authentication Token"}` for a call carrying no credential.
//!
//! The rows span what the rule generalises over: which of the matching resources declares the
//! verb (the literal one, only the parameterised sibling, only the greedy sibling, both, and
//! neither), the shape of the sibling the call falls through to (a path parameter and a greedy
//! one), the segment bound as the path parameter's value, and a request addressed at a segment
//! no literal resource names at all.
//!
//! Every case drives the real wire path — an HTTP request on the api's own invoke host through
//! the multi-service router — and every record is built through this service's control plane,
//! so what is asserted is what a client of the published invoke URL sees.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{deploy, invoke, mock_method, Invoked};
use crate::rest_api_lifecycle::{create_api, resource_under, Gateway};

/// The stage the api under test is deployed to.
const STAGE: &str = "s";

/// The collection whose literal child and parameterised child are the two candidates a call
/// resolves against.
const COLLECTION: &str = "orders";

/// The literal child of [`COLLECTION`], which declares `GET` and `POST` and no other verb.
const LITERAL: &str = "pending";

/// The parameterised sibling of [`LITERAL`], which declares `GET` and `PUT`.
const PARAMETER: &str = "{orderId}";

/// The collection whose literal child's sibling is a GREEDY parameter.
const FILES: &str = "files";

/// The literal child of [`FILES`], which declares `GET` alone.
const FILE: &str = "report";

/// The greedy sibling of [`FILE`], which declares `DELETE`.
const GREEDY: &str = "{proxy+}";

/// The response template every method here answers through: the resource path TEMPLATE the
/// call was resolved against (`$context.resourcePath`) and the value each of the two path
/// parameters was bound to, so an answer says which resource served the call and what the
/// request's own segment was bound as.
fn route_template() -> Value {
    json!({
        "application/json": "{\"resourcePath\":\"$context.resourcePath\",\
                             \"orderId\":\"$input.params('orderId')\",\
                             \"proxy\":\"$input.params('proxy')\"}"
    })
}

/// The api under test: a literal child and a parameterised sibling under one collection, and a
/// literal child and a GREEDY sibling under another, each declaring its own verbs.
async fn deployed(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "method-fall-through").await;
    let collection = resource_under(gateway, &api, &root, COLLECTION).await;
    let literal = resource_under(gateway, &api, &collection, LITERAL).await;
    let parameter = resource_under(gateway, &api, &collection, PARAMETER).await;
    for verb in ["GET", "POST"] {
        mock_method(gateway, &api, &literal, verb, "200", route_template()).await;
    }
    for verb in ["GET", "PUT"] {
        mock_method(gateway, &api, &parameter, verb, "200", route_template()).await;
    }
    let files = resource_under(gateway, &api, &root, FILES).await;
    let file = resource_under(gateway, &api, &files, FILE).await;
    let greedy = resource_under(gateway, &api, &files, GREEDY).await;
    mock_method(gateway, &api, &file, "GET", "200", route_template()).await;
    mock_method(gateway, &api, &greedy, "DELETE", "200", route_template()).await;
    deploy(gateway, &api, STAGE).await;
    api
}

/// One member of a served answer's JSON body.
fn member(answered: &Invoked, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(&answered.body)
        .ok()?
        .get(name)?
        .as_str()
        .map(str::to_owned)
}

/// What a row expects of the answer.
#[derive(Debug)]
enum Expect {
    /// A method served it: the resource path template that resolved the call, and the
    /// `(parameter name, bound value)` the request's own segment was bound as — empty when
    /// the serving resource declares no path parameter.
    Served {
        resource_path: &'static str,
        bound: (&'static str, &'static str),
    },
    /// No matching resource declares the verb, so the stage answers the refusal it answers an
    /// unplaceable route with.
    MissingToken,
}

/// RULE: the first matching resource that DECLARES the request's verb serves the call, with
/// the literal segment bound as the path parameter it fell through to; only a verb NO matching
/// resource declares earns the missing-token refusal.
#[tokio::test]
async fn a_verb_the_literal_resource_does_not_declare_falls_through_to_its_sibling() {
    let gateway = Gateway::new();
    let api = deployed(&gateway).await;

    // Rows differ along which of the matching resources declares the verb, the shape of the
    // sibling fallen through to, and the segment bound as its path parameter.
    type Row<'r> = (&'r str, &'r str, String, Expect);
    let rows: Vec<Row<'_>> = vec![
        (
            "a verb the literal resource declares",
            "GET",
            format!("/{COLLECTION}/{LITERAL}"),
            Expect::Served {
                resource_path: "/orders/pending",
                bound: ("orderId", ""),
            },
        ),
        (
            "another verb the literal resource declares",
            "POST",
            format!("/{COLLECTION}/{LITERAL}"),
            Expect::Served {
                resource_path: "/orders/pending",
                bound: ("orderId", ""),
            },
        ),
        (
            "a verb only the parameterised sibling declares",
            "PUT",
            format!("/{COLLECTION}/{LITERAL}"),
            Expect::Served {
                resource_path: "/orders/{orderId}",
                bound: ("orderId", LITERAL),
            },
        ),
        (
            "that same verb at a segment no literal resource names",
            "PUT",
            format!("/{COLLECTION}/9f2c"),
            Expect::Served {
                resource_path: "/orders/{orderId}",
                bound: ("orderId", "9f2c"),
            },
        ),
        (
            "a verb the parameterised sibling declares and the literal one also does",
            "GET",
            format!("/{COLLECTION}/9f2c"),
            Expect::Served {
                resource_path: "/orders/{orderId}",
                bound: ("orderId", "9f2c"),
            },
        ),
        (
            "a verb neither the literal resource nor its sibling declares",
            "DELETE",
            format!("/{COLLECTION}/{LITERAL}"),
            Expect::MissingToken,
        ),
        (
            "a verb the literal resource declares, beside a greedy sibling",
            "GET",
            format!("/{FILES}/{FILE}"),
            Expect::Served {
                resource_path: "/files/report",
                bound: ("proxy", ""),
            },
        ),
        (
            "a verb only the greedy sibling declares",
            "DELETE",
            format!("/{FILES}/{FILE}"),
            Expect::Served {
                resource_path: "/files/{proxy+}",
                bound: ("proxy", FILE),
            },
        ),
        (
            "a verb neither the literal resource nor its greedy sibling declares",
            "POST",
            format!("/{FILES}/{FILE}"),
            Expect::MissingToken,
        ),
    ];

    for (case, verb, path, expected) in &rows {
        let answered = invoke(&gateway, &api, verb, &format!("/{STAGE}{path}"), &[]).await;
        match expected {
            Expect::Served {
                resource_path,
                bound,
            } => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: expected 200 from the resource that declares {verb}, got {} \
                     with {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    member(&answered, "resourcePath").as_deref(),
                    Some(*resource_path),
                    "case {case:?}: expected the call to be served by {resource_path:?}, got {:?}",
                    answered.body
                );
                let (name, value) = bound;
                assert_eq!(
                    member(&answered, name).as_deref(),
                    Some(*value),
                    "case {case:?}: expected {name} bound to {value:?}, got {:?}",
                    answered.body
                );
            }
            Expect::MissingToken => {
                assert_eq!(
                    answered.status, 403,
                    "case {case:?}: expected 403, got {} with {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.message().as_deref(),
                    Some("Missing Authentication Token"),
                    "case {case:?}: expected the missing-token sentence, got {:?}",
                    answered.body
                );
            }
        }
    }
}
