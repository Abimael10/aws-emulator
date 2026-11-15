// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the values a LAYER request carries in its URI, and what each one
//! has to be before anything is looked up.
//!
//! Three rules are asserted here, each over a table of inputs:
//!
//! 1. **a value bound into the request URI that cannot be CONVERTED to the modelled type of
//!    its member never becomes an input at all, so it is answered as the protocol's own
//!    failure** — HTTP 400 naming `SerializationException` in `x-amzn-errortype`, with the
//!    sentence `'<value>' can not be converted to <Type>` under the capitalized `Message`
//!    member the framework's envelope carries — rather than as a violation of a constraint
//!    this model declares;
//! 2. **a layer version number below the first one a publish assigns names no version that
//!    could exist**, so an operation addressing a layer version by number refuses it 400
//!    `InvalidParameterValueException` with Lambda's fixed sentence for it, ahead of the
//!    lookup, instead of reporting the version as one this account does not hold;
//! 3. **`ListLayers` continues only from a `Marker` it ISSUED**: the position travels inside
//!    an opaque token the service minted, so a value it never handed back is refused 400
//!    `InvalidParameterValueException` with the sentence that listing words for it, rather
//!    than silently restarting the walk.
//!
//! The rows differ along what each rule generalises over: the operation and the binding the
//! value arrives in (a path label, a query parameter) and the modelled type it is read into
//! for (1); the operation and the number for (2) — zero, negative, the extreme of the
//! member's own width, and the first and an absent version, which must NOT be refused that
//! way; and the way a marker can fail to be one this service issued for (3).
//!
//! Every suite drives real HTTP requests through the router, so the assertions are about the
//! wire behaviour (status, error type, message, body members) rather than any handler's
//! internals, and every refusal is checked against what the store still holds afterwards.
//! The harness (`Answer`, `json_call`, `router_on`) is the one `function_lifecycle.rs`
//! publishes.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{json_call, router_on, zip_file, Answer, ACCOUNT, REGION};

/// The status every refusal in this suite carries.
const REFUSAL_STATUS: u16 = 400;

/// The error type the framework's envelope names for a value the protocol could not
/// convert — NOT one of this model's error shapes, because the request never became an
/// input.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The error shape a modelled parameter refusal of these operations is answered as
/// (`smithy_query(query="operation", operation="GetLayerVersion")`: `httpError` 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The error shape a layer version this account does not hold is reported as (the same
/// query: `httpError` 404).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The class of fault every error body of this service names in its `Type` member for a
/// request the caller has to change.
const USER_FAULT: &str = "User";

/// The sentence Lambda refuses a layer version number below the first one with.
const BELOW_FIRST_VERSION: &str = "Layer Version Cannot be less than 1";

/// The sentence `ListLayers` refuses a `Marker` it did not issue with.
const INVALID_PAGINATION_KEY: &str = "Invalid pagination key.";

/// The layer every suite publishes one version of, so a refusal cannot be mistaken for the
/// answer about a layer that is not there.
const HELD: &str = "uri-value-layer";

/// Layer archive bytes of a given length; the content is irrelevant to the API, only its
/// length and digest are reported.
fn archive_bytes(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

/// `PublishLayerVersion` for `name`, answering the version it stored. Panics unless the
/// publish succeeded, because every assertion below is about what it stored.
async fn publish(router: &MultiServiceRouter, name: &str) -> Value {
    let body = json!({ "Content": { "ZipFile": zip_file(&archive_bytes(64)) } });
    let answer = json_call(
        router,
        "POST",
        &format!("/2018-10-31/layers/{name}/versions"),
        &body,
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "publishing {name:?}: expected 201 from PublishLayerVersion, got {} {}",
        answer.status,
        text(&answer)
    );
    answer.json()
}

/// The body of an answer, for a failing assertion to report.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The `message` of a wire error, under either casing the two envelopes use.
fn message(answer: &Answer) -> Option<String> {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
}

/// An emulator holding one layer with one published version.
async fn emulator_holding_a_layer() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    publish(&router, HELD).await;
    (engine, router)
}

/// One request a row drives.
struct Call {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
}

/// The body `AddLayerVersionPermission` needs for its own required members, so a row about
/// the URI is never refused for the body instead.
fn permission_body() -> Value {
    json!({
        "StatementId": "share",
        "Action": "lambda:GetLayerVersion",
        "Principal": ACCOUNT,
    })
}

/// Every operation of this service that addresses a layer version BY NUMBER, for the
/// version segment `version` of the layer `name`.
///
/// `DeleteLayerVersion` is deliberately absent from the number-range rows: it is
/// `@idempotent` (`smithy_query(query="operation", operation="DeleteLayerVersion")`) and
/// answers its modelled 204 for every version it does not hold, so it has nothing to
/// refuse. It IS among the coercion rows, because a version segment that is no number at
/// all never reaches it.
fn version_number_calls(name: &str, version: &str) -> Vec<Call> {
    vec![
        Call {
            case: "GetLayerVersion",
            method: "GET",
            uri: format!("/2018-10-31/layers/{name}/versions/{version}"),
            body: Value::Null,
        },
        Call {
            case: "GetLayerVersionPolicy",
            method: "GET",
            uri: format!("/2018-10-31/layers/{name}/versions/{version}/policy"),
            body: Value::Null,
        },
        Call {
            case: "AddLayerVersionPermission",
            method: "POST",
            uri: format!("/2018-10-31/layers/{name}/versions/{version}/policy"),
            body: permission_body(),
        },
        Call {
            case: "RemoveLayerVersionPermission",
            method: "DELETE",
            uri: format!("/2018-10-31/layers/{name}/versions/{version}/policy/share"),
            body: Value::Null,
        },
    ]
}

/// RULE: a value bound into the request URI that cannot be converted to the modelled type
/// of the member it is bound to never becomes an input, so it is refused as the protocol's
/// own failure — 400 naming `SerializationException`, with `'<value>' can not be converted
/// to <Type>` under the capitalized `Message` member — and not as a constraint violation of
/// this model.
///
/// The rows differ along the binding the value arrives in (a `@httpLabel` path segment, a
/// `@httpQuery` parameter), the modelled type it is read into (`LayerVersionNumber`, a long;
/// `MaxLayerListItems`, an integer), the operation, and the WAY the value fails to convert:
/// a word, a fractional number, a value one past the modelled width, and an empty one. The
/// last rows are values that DO convert, which must never be answered this way.
#[tokio::test]
async fn a_uri_value_that_cannot_become_its_modelled_type_is_refused_as_a_protocol_error() {
    let (_engine, router) = emulator_holding_a_layer().await;

    // (the call, the value as the request spelled it, the modelled type's name)
    let mut rows: Vec<(Call, String, &str)> = Vec::new();
    // Every operation taking the version number as a path label, for one unconvertible
    // value: the rule is the SERVICE's, not one operation's.
    for call in version_number_calls(HELD, "latest") {
        rows.push((call, "latest".to_string(), "Long"));
    }
    rows.push((
        Call {
            case: "DeleteLayerVersion",
            method: "DELETE",
            uri: format!("/2018-10-31/layers/{HELD}/versions/latest"),
            body: Value::Null,
        },
        "latest".to_string(),
        "Long",
    ));
    // The ways a long label can fail to convert, on the read the issue recorded.
    for value in ["v1", "1.5", "-", "9223372036854775808", "one"] {
        rows.push((
            Call {
                case: "GetLayerVersion",
                method: "GET",
                uri: format!("/2018-10-31/layers/{HELD}/versions/{value}"),
                body: Value::Null,
            },
            value.to_string(),
            "Long",
        ));
    }
    // The same rule in the other URI binding: `MaxItems` is an integer bound to the query
    // string on both layer listings.
    for (case, uri) in [
        ("ListLayers", "/2018-10-31/layers?MaxItems=lots".to_string()),
        (
            "ListLayerVersions",
            format!("/2018-10-31/layers/{HELD}/versions?MaxItems=lots"),
        ),
    ] {
        rows.push((
            Call {
                case,
                method: "GET",
                uri,
                body: Value::Null,
            },
            "lots".to_string(),
            "Integer",
        ));
    }
    rows.push((
        Call {
            case: "ListLayers past the integer width",
            method: "GET",
            uri: "/2018-10-31/layers?MaxItems=2147483648".to_string(),
            body: Value::Null,
        },
        "2147483648".to_string(),
        "Integer",
    ));
    rows.push((
        Call {
            case: "ListLayers with an empty MaxItems",
            method: "GET",
            uri: "/2018-10-31/layers?MaxItems=".to_string(),
            body: Value::Null,
        },
        String::new(),
        "Integer",
    ));

    for (call, value, named) in rows {
        let expected = format!("'{value}' can not be converted to {named}");
        let answer = json_call(&router, call.method, &call.uri, &call.body).await;
        assert_eq!(
            answer.status,
            REFUSAL_STATUS,
            "case {:?} ({}): expected {REFUSAL_STATUS}, got {} {}",
            call.case,
            call.uri,
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.header("x-amzn-errortype").as_deref(),
            Some(SERIALIZATION_EXCEPTION),
            "case {:?}: a value the protocol could not convert names {SERIALIZATION_EXCEPTION}, \
             got {:?}",
            call.case,
            answer.header("x-amzn-errortype")
        );
        assert_eq!(
            answer.string("Message").as_deref(),
            Some(expected.as_str()),
            "case {:?}: expected the capitalized Message {expected:?}, got {}",
            call.case,
            text(&answer)
        );
        // The framework's envelope, not this model's: a client reading the lowercase
        // member of a modelled error finds nothing here, which is how the two classes are
        // told apart.
        assert_eq!(
            answer.string("message"),
            None,
            "case {:?}: a protocol failure carries no modelled message member, got {}",
            call.case,
            text(&answer)
        );
    }

    // Values that DO convert are never answered this way: the published version reads
    // back, and each published `MaxItems` bound lists.
    for (case, uri) in [
        (
            "the published version",
            format!("/2018-10-31/layers/{HELD}/versions/1"),
        ),
        (
            "a version the layer does not hold",
            format!("/2018-10-31/layers/{HELD}/versions/9"),
        ),
        (
            "MaxItems at the minimum",
            "/2018-10-31/layers?MaxItems=1".to_string(),
        ),
        (
            "MaxItems at the maximum",
            "/2018-10-31/layers?MaxItems=50".to_string(),
        ),
        (
            "a negative version number",
            format!("/2018-10-31/layers/{HELD}/versions/-1"),
        ),
    ] {
        let answer = json_call(&router, "GET", &uri, &Value::Null).await;
        assert_ne!(
            answer.header("x-amzn-errortype").as_deref(),
            Some(SERIALIZATION_EXCEPTION),
            "case {case:?}: a value that converts must not be answered as a conversion \
             failure, got {}",
            text(&answer)
        );
    }
}

/// RULE: a layer version number below the first one a publish assigns names no version that
/// could exist, so every operation addressing a layer version by number refuses it 400
/// `InvalidParameterValueException` with Lambda's fixed sentence, ahead of the lookup —
/// which is what lets a caller tell "fix your input" from "not published yet".
///
/// The rows differ along the number (zero, negative, far negative, the extreme of the
/// member's own width) and the operation (the read, and all three layer-version policy
/// operations). The last rows are numbers that CAN name a version — the one published and
/// one the layer does not hold — which must be answered about the resource instead.
#[tokio::test]
async fn a_layer_version_number_below_the_first_is_refused_before_the_lookup() {
    let (_engine, router) = emulator_holding_a_layer().await;

    for version in ["0", "-1", "-9999", "-9223372036854775808"] {
        for call in version_number_calls(HELD, version) {
            let label = format!("{} at version {version}", call.case);
            let answer = json_call(&router, call.method, &call.uri, &call.body).await;
            assert_eq!(
                answer.status,
                REFUSAL_STATUS,
                "case {label:?}: expected {REFUSAL_STATUS}, got {} {}",
                answer.status,
                text(&answer)
            );
            assert!(
                answer.error_code().contains(INVALID_PARAMETER),
                "case {label:?}: expected {INVALID_PARAMETER}, got {}",
                answer.error_code()
            );
            let worded = message(&answer);
            assert_eq!(
                worded.as_deref(),
                Some(BELOW_FIRST_VERSION),
                "case {label:?}: expected {BELOW_FIRST_VERSION:?}, got {worded:?}"
            );
            assert_eq!(
                answer.string("Type").as_deref(),
                Some(USER_FAULT),
                "case {label:?}: a refusal of the request names the {USER_FAULT} fault \
                 class, got {}",
                text(&answer)
            );
        }
    }

    // A number that CAN name a version is answered about the resource: the sentence above
    // never appears for one. The numbers here name no version this layer holds, so no row
    // changes anything either.
    for version in ["2", "9223372036854775807"] {
        for call in version_number_calls(HELD, version) {
            let label = format!("{} at version {version}", call.case);
            let answer = json_call(&router, call.method, &call.uri, &call.body).await;
            assert_ne!(
                message(&answer).as_deref(),
                Some(BELOW_FIRST_VERSION),
                "case {label:?}: a number that can name a version must not be refused as \
                 one below the first, got {}",
                text(&answer)
            );
        }
    }

    // The first version itself still reads: the rule refuses the numbers below it and
    // nothing else.
    let first = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{HELD}/versions/1"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        first.status,
        200,
        "case the first version: expected 200, got {} {}",
        first.status,
        text(&first)
    );

    // A version the layer does not hold is the OTHER branch — the resource, at 404 — so
    // the two refusals a caller distinguishes are both reachable.
    let absent = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{HELD}/versions/9"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        absent.status,
        404,
        "case a version the layer does not hold: expected 404, got {} {}",
        absent.status,
        text(&absent)
    );
    assert!(
        absent.error_code().contains(NOT_FOUND),
        "case a version the layer does not hold: expected {NOT_FOUND}, got {}",
        absent.error_code()
    );

    // Nothing was created or granted by any refused call: the layer still holds exactly
    // the one version it was published with, and that version still holds no policy.
    let listed = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{HELD}/versions"),
        &Value::Null,
    )
    .await;
    let versions: Vec<i64> = listed
        .json()
        .get("LayerVersions")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("Version").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        versions,
        vec![1],
        "the refused calls must leave the layer with the one version it was published \
         with, got {}",
        text(&listed)
    );
    let policy = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{HELD}/versions/1/policy"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        policy.status,
        404,
        "the refused permission calls must grant nothing on the published version, got {} {}",
        policy.status,
        text(&policy)
    );
}

/// RULE: `ListLayers` continues only from a `Marker` it ISSUED — the position travels
/// inside an opaque token the service minted — so a value it never handed back names no
/// place in the order and is refused 400 `InvalidParameterValueException` with the sentence
/// that listing words for it.
///
/// The rows differ along the way a value fails to be a marker this service issued: the
/// position itself (the layer name a caller can read off the listing), a value in no token
/// alphabet, a value in the right alphabet that was never minted, a minted token with one
/// character changed, and the empty string. The walk that follows pins the other half of
/// the rule: a token the listing DID hand back continues the order, exactly once per layer.
#[tokio::test]
async fn list_layers_continues_only_from_a_marker_it_issued() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // `layer-1` is a prefix of `layer-10`, so the names do not come back in publish order:
    // the listing is ordered by name, which is what a marker continues after.
    let names = ["layer-1", "layer-10", "layer-2", "layer-21", "layer-3"];
    for name in names {
        publish(&router, name).await;
    }
    let mut expected: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    expected.sort();

    // One layer per page: every marker on the way is one this listing issued, and the walk
    // reports every layer exactly once in name order.
    let mut seen: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    let mut issued: Vec<String> = Vec::new();
    for _ in 0..=expected.len() {
        let query = match &marker {
            Some(marker) => format!("?MaxItems=1&Marker={marker}"),
            None => "?MaxItems=1".to_string(),
        };
        let answer = json_call(
            &router,
            "GET",
            &format!("/2018-10-31/layers{query}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case the paged walk ({query}): expected 200, got {} {}",
            answer.status,
            text(&answer)
        );
        let body = answer.json();
        let page: Vec<Value> = body
            .get("Layers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in &page {
            let name = entry
                .get("LayerName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case the paged walk: a listed layer carries a LayerName, got {entry}")
                });
            seen.push(name.to_string());
        }
        match body.get("NextMarker").and_then(Value::as_str) {
            Some(next) => {
                // A marker names a PLACE, not a resource: it is never the layer name the
                // page ended on, which a caller could otherwise assemble itself.
                assert!(
                    !names.contains(&next),
                    "case the paged walk: a marker must not be the position it continues \
                     after, got {next:?}"
                );
                issued.push(next.to_string());
                marker = Some(next.to_string());
            }
            None => break,
        }
    }
    assert_eq!(
        seen, expected,
        "the paged walk must report every layer once in name order, got {seen:?}"
    );
    assert!(
        !issued.is_empty(),
        "the walk must have been handed at least one marker to continue from"
    );

    // Every way a value can fail to be a marker this listing issued.
    let minted = issued
        .first()
        .cloned()
        .expect("the walk was handed a marker");
    let tampered = tampered(&minted);
    let rows: Vec<(&str, String)> = vec![
        ("the position itself", "layer-1".to_string()),
        ("a layer name past every layer", "zzz".to_string()),
        ("a value in no token alphabet", "not-a-marker".to_string()),
        ("the empty string", String::new()),
        (
            "the right alphabet, never minted",
            "deadbeef".repeat(9),
        ),
        ("a minted token with one character changed", tampered),
    ];
    for (case, marker) in rows {
        let answer = json_call(
            &router,
            "GET",
            &format!("/2018-10-31/layers?Marker={marker}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            answer.status,
            REFUSAL_STATUS,
            "case {case:?} ({marker:?}): expected {REFUSAL_STATUS}, got {} {}",
            answer.status,
            text(&answer)
        );
        assert!(
            answer.error_code().contains(INVALID_PARAMETER),
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            answer.error_code()
        );
        let worded = message(&answer);
        assert_eq!(
            worded.as_deref(),
            Some(INVALID_PAGINATION_KEY),
            "case {case:?}: expected {INVALID_PAGINATION_KEY:?}, got {worded:?}"
        );
        assert_eq!(
            answer.string("Type").as_deref(),
            Some(USER_FAULT),
            "case {case:?}: a refusal of the request names the {USER_FAULT} fault class, \
             got {}",
            text(&answer)
        );
    }

    // And the token it DID issue still continues the order, so the refusals above are
    // about the value rather than about markers as such.
    let continued = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers?MaxItems=1&Marker={minted}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        continued.status,
        200,
        "case the issued token: expected 200, got {} {}",
        continued.status,
        text(&continued)
    );
}

/// RULE: the value set an enumerated member's refusal ADVERTISES is the set that member
/// accepts — every value the clause lists is taken by the same operation, and the value it
/// refused is not among them — so a client that reads the set to pick a usable value, or
/// validates against it before retrying, is never told about a value the service would
/// refuse.
///
/// The set itself is not written out here: it is read back off the refusal and each value in
/// it is then DRIVEN through the operation, which is what makes the assertion about the
/// service's own set rather than about a list copied into the test. The rows span both
/// operations that bind the member (`ListLayers` and `ListLayerVersions`, whose
/// `CompatibleRuntime` targets the `Runtime` enum:
/// `smithy_query(query="operation", operation="ListLayers,ListLayerVersions")`) and the ways
/// a value can be outside the set: the wrong case of a value that IS in it, a runtime that
/// does not exist, and the empty value.
#[tokio::test]
async fn an_enum_refusal_advertises_the_value_set_the_member_accepts() {
    let (_engine, router) = emulator_holding_a_layer().await;

    // (the operation, its URI)
    let listings: [(&str, String); 2] = [
        ("ListLayers", "/2018-10-31/layers".to_string()),
        (
            "ListLayerVersions",
            format!("/2018-10-31/layers/{HELD}/versions"),
        ),
    ];
    // The values outside the set, which the member names in its clause.
    let outside = ["Nodejs20.x", "nodejs0.1", "", "python"];

    for (operation, uri) in &listings {
        for refused in outside {
            let label = format!("{operation} with CompatibleRuntime {refused:?}");
            let answer = json_call(
                &router,
                "GET",
                &format!("{uri}?CompatibleRuntime={refused}"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                answer.status,
                REFUSAL_STATUS,
                "case {label:?}: expected {REFUSAL_STATUS}, got {} {}",
                answer.status,
                text(&answer)
            );
            let worded = message(&answer).unwrap_or_default();
            let frame = format!(
                "1 validation error detected: Value '{refused}' at 'compatibleRuntime' failed \
                 to satisfy constraint: Member must satisfy enum value set: ["
            );
            assert!(
                worded.starts_with(&frame) && worded.ends_with(']'),
                "case {label:?}: expected the clause {frame:?}…], got {worded:?}"
            );
            let advertised = advertised_set(&worded);
            assert!(
                !advertised.contains(&refused.to_string()),
                "case {label:?}: the set must not advertise the value it refused, got \
                 {advertised:?}"
            );
            // Non-vacuity, against runtimes the model declares
            // (`smithy_query(query="shape", shape="Runtime")`): a set that advertised none
            // of them would satisfy the containment above for free.
            for declared in ["nodejs20.x", "provided.al2023", "java21", "ruby3.4"] {
                assert!(
                    advertised.contains(&declared.to_string()),
                    "case {label:?}: expected the advertised set to carry {declared:?}, got \
                     {advertised:?}"
                );
            }
            // And the set is the set the member ACCEPTS: every value it advertises is taken
            // by this same operation.
            for admitted in &advertised {
                let accepted = json_call(
                    &router,
                    "GET",
                    &format!("{uri}?CompatibleRuntime={admitted}"),
                    &Value::Null,
                )
                .await;
                assert_eq!(
                    accepted.status,
                    200,
                    "case {label:?}: the advertised value {admitted:?} must be accepted, got \
                     {} {}",
                    accepted.status,
                    text(&accepted)
                );
            }
        }
    }
}

/// The values a refusal's bracketed clause advertises, as the clause lists them.
fn advertised_set(message: &str) -> Vec<String> {
    let Some((_, bracketed)) = message.split_once('[') else {
        return Vec::new();
    };
    bracketed
        .trim_end_matches(']')
        .split(", ")
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// `marker` with one character changed, so the value is still in the token alphabet but is
/// no token this store minted. The first character is replaced by another hexadecimal
/// digit, which cannot leave the value unchanged.
fn tampered(marker: &str) -> String {
    let mut characters: Vec<char> = marker.chars().collect();
    characters[0] = if characters[0] == '0' { '1' } else { '0' };
    characters.into_iter().collect()
}

/// A marker names a position in the listing that ISSUED it: the token `ListLayers` handed
/// back continues ITS order, and names no position in another listing of this service.
///
/// The other half of the marker rule — the position is authenticated for one order, so a
/// token cannot be carried from one listing to another — stated over the two layer listings
/// this service binds.
#[tokio::test]
async fn a_marker_names_a_position_in_the_listing_that_issued_it() {
    let (_engine, router) = emulator_holding_a_layer().await;
    publish(&router, "uri-value-layer-2").await;

    let first = json_call(
        &router,
        "GET",
        "/2018-10-31/layers?MaxItems=1",
        &Value::Null,
    )
    .await;
    let marker = first
        .string("NextMarker")
        .unwrap_or_else(|| panic!("expected a NextMarker from the first page, got {}", text(&first)));

    // The same token, handed to the listing of a layer's VERSIONS, names no position
    // there: that listing pages by version number and never issued this value.
    let other = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{HELD}/versions?Marker={marker}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        other.status,
        REFUSAL_STATUS,
        "case another listing's marker: expected {REFUSAL_STATUS} from ListLayerVersions, \
         got {} {}",
        other.status,
        text(&other)
    );

    // And in the listing that issued it, it continues the order: the second page reports
    // the layer ordered after the first page's.
    let second = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers?MaxItems=1&Marker={marker}"),
        &Value::Null,
    )
    .await;
    let names: Vec<String> = second
        .json()
        .get("Layers")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("LayerName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec!["uri-value-layer-2".to_string()],
        "case the issued token in {REGION}: expected the layer after the first page, got {}",
        text(&second)
    );
}
