// SPDX-License-Identifier: CC0-1.0

//! Invoking an api through a CUSTOM DOMAIN NAME a client CARRIES IN THE PATH — the addressing
//! of a client that resolves no such name at all, which reaches this emulator's own endpoint
//! and names the host it was handed in the reserved data-plane path namespace.
//!
//! The rule these cases assert, one level above the recorded case:
//!
//! > A custom domain name is a host of the CALLER's own choosing, and what answers under it is
//! > the `{ApiId, Stage}` of the api mapping whose `ApiMappingKey` the request's path begins
//! > with. A client on the machine this emulator runs on resolves none of those names, so it
//! > reaches the emulator's own endpoint and carries in the PATH what it cannot carry in the
//! > host — `/{namespace}/{domain-name}/{path}`, the same rewriting an api's own endpoint is
//! > reached through. The name carried that way addresses exactly the record it spells, and the
//! > path left under it is the path a mapping answers, so ONE request reaches one api and one
//! > stage whichever way the client stated the host: the two spellings answer alike, down to
//! > the refusal a path no mapping of the record answers earns.
//!
//! Nothing here is a value this emulator fills in: every served row is answered by a FUNCTION
//! that ran, and the handler reports which api, which stage and which path it was given — so a
//! row proves WHICH stage served it rather than merely that something answered. The rows are
//! the ones [`crate::custom_domain_front_door_invoke`] drives as the `Host` of a request made
//! at the same endpoint, because the two spellings of one host must answer one request the same
//! way.
//!
//! The rows span what the rule generalises over: the mapping key (none, one segment, two
//! segments), which of two apis and which stage the matched mapping names (a named stage and
//! the one that serves an api's root), the path under the key (a path, the key itself, a
//! segment that merely starts with a key), a query string, a path no mapping answers, and a
//! carried name that names no record of the account.

use serde_json::{json, Value};

use crate::custom_domain_mapping_invoke::{control, given_domain_name, given_mapping, ECHO};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_api, given_function, lambda_integration};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The emulator's own endpoint: the address a client configured with one endpoint URL dials,
/// and the one every request of this suite is made at. The custom domain name travels in the
/// PATH.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved path namespace a client carries a host it cannot resolve in — the marker this
/// emulator's data planes are addressed through, read here as a client writes it.
const NAMESPACE: &str = "_aws";

/// The custom domain name the account creates.
const DOMAIN: &str = "carried.api.example.com";

/// The status a request routed to a stage is answered with here: the function's own.
const SERVED: u16 = 200;

/// The status and sentence a request that reaches no api of the account is answered with: the
/// refusal API Gateway's front door turns such a request away with.
const NO_MAPPING: (u16, &str) = (403, "Forbidden");

/// The error code the front door answers when NO service claims a request — which is what a
/// carried name naming no domain name and no api of this emulator gets.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// One request made at this emulator's own endpoint with `path`.
async fn fetch(
    emulator: &::smithy_server_runtime::MultiServiceRouter,
    path: &str,
) -> Answer {
    send(
        emulator,
        EMULATOR_ENDPOINT,
        "GET",
        path,
        &[("host", EMULATOR_ENDPOINT)],
    )
    .await
}

/// The path a client that resolves `host` nowhere writes for `path`: the reserved namespace,
/// the host it was handed, then the path it would have sent to that host.
fn carried(host: &str, path: &str) -> String {
    format!("/{NAMESPACE}/{host}{path}")
}

/// Which of the two apis a row's answer must come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Api {
    /// The api the ROOT mapping names (no mapping key), on the stage that serves its own root.
    Root,
    /// The api a KEYED mapping names, on a named stage.
    Keyed,
}

/// One row: the path a client asks the domain name for, and the api, stage and path the answer
/// must report.
struct Row {
    case: &'static str,
    path: &'static str,
    served: (Api, &'static str, &'static str),
}

/// The paths the mappings below answer, and what each must be served by.
const ROWS: &[Row] = &[
    Row {
        case: "the root mapping serves a path no key claims",
        path: "/items",
        served: (Api::Root, "$default", "/items"),
    },
    Row {
        case: "the domain's own root",
        path: "/",
        served: (Api::Root, "$default", "/"),
    },
    Row {
        case: "a one-segment key, which is stripped from the path the stage is given",
        path: "/shop/items",
        served: (Api::Keyed, "prod", "/prod/items"),
    },
    Row {
        case: "the key itself, with nothing under it",
        path: "/shop",
        served: (Api::Keyed, "prod", "/prod"),
    },
    Row {
        case: "the longer of two matching keys",
        path: "/shop/eu/items",
        served: (Api::Keyed, "prod", "/prod/items"),
    },
    Row {
        case: "a segment that merely starts with a key falls to the root mapping",
        path: "/shopping/items",
        served: (Api::Root, "$default", "/shopping/items"),
    },
    Row {
        case: "a query string under a mapping key",
        path: "/shop/items?colour=red&size=2",
        served: (Api::Keyed, "prod", "/prod/items"),
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn a_request_carrying_a_custom_domain_name_in_its_path_is_served_by_its_mapped_stage() {
    let case = "two apis mapped onto one custom domain name";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(&emulator, &engine, case, "carried-host-echo", ECHO).await;
    // The api the ROOT mapping names, on the stage that serves an api's root.
    let (root_api, _) = given_api(
        &emulator,
        case,
        lambda_integration(&function, "2.0"),
        "$default",
        json!({ "stageName": "$default", "autoDeploy": true }),
    )
    .await;
    // The api a KEYED mapping names, on a named stage, so a row proves the mapping chose the
    // api AND the stage.
    let (keyed_api, _) = given_api(
        &emulator,
        case,
        lambda_integration(&function, "2.0"),
        "$default",
        json!({ "stageName": "prod", "autoDeploy": true }),
    )
    .await;

    given_domain_name(&emulator, case, DOMAIN).await;
    given_mapping(&emulator, case, DOMAIN, &root_api, "$default", None).await;
    given_mapping(&emulator, case, DOMAIN, &keyed_api, "prod", Some("shop")).await;
    given_mapping(&emulator, case, DOMAIN, &keyed_api, "prod", Some("shop/eu")).await;

    for row in ROWS {
        let (api, stage, path) = row.served;
        let query = row
            .path
            .split_once('?')
            .map(|(_path, query)| query)
            .unwrap_or_default();
        let expected = json!({
            "api": match api {
                Api::Root => root_api.clone(),
                Api::Keyed => keyed_api.clone(),
            },
            "stage": stage,
            "path": path,
            "query": query,
        });
        let answer = fetch(&emulator, &carried(DOMAIN, row.path)).await;
        assert_eq!(
            answer.status, SERVED,
            "case {:?}: expected {SERVED} from the mapped stage, got {}",
            row.case,
            answer.summary()
        );
        assert_eq!(
            answer.body,
            expected,
            "case {:?}: expected the answer of {expected}, got {}",
            row.case,
            answer.summary()
        );
        // And the two spellings of one host answer alike: the same request stated as the
        // `Host` of a request made at the same endpoint is served by the same stage.
        let stated = send(&emulator, DOMAIN, "GET", row.path, &[("host", DOMAIN)]).await;
        assert_eq!(
            (answer.status, &answer.body),
            (stated.status, &stated.body),
            "case {:?}: the carried spelling must answer as the stated one does, which said \
             {}",
            row.case,
            stated.summary()
        );
    }

    // A path no mapping of the record answers reaches no api of the account, whichever way the
    // host was stated: the mapping that answered the root is deleted and the path it served
    // earns the front door's own refusal.
    let (status, deleted) = control(
        &emulator,
        "GET",
        &format!("/v2/domainnames/{DOMAIN}/apimappings"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetApiMappings answered {status} {deleted}"
    );
    let root_mapping = deleted
        .pointer("/items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("apiMappingKey").is_none())
                .or_else(|| items.first())
        })
        .and_then(|item| item.get("apiMappingId"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the listing must carry the root mapping: {deleted}"))
        .to_string();
    let (status, removed) = control(
        &emulator,
        "DELETE",
        &format!("/v2/domainnames/{DOMAIN}/apimappings/{root_mapping}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteApiMapping answered {status} {removed}"
    );
    let answer = fetch(&emulator, &carried(DOMAIN, "/items")).await;
    assert_eq!(
        (answer.status, message(&answer).as_str()),
        NO_MAPPING,
        "case \"a carried name whose mapping was deleted\": expected the gateway's own \
         refusal, got {}",
        answer.summary()
    );

    // A name CARRIED that names no domain name of the account — and no api of it — reaches no
    // plane of this service at all, which is what keeps this reading from claiming another
    // service's traffic.
    let answer = fetch(&emulator, &carried("elsewhere.example.com", "/items")).await;
    assert_eq!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case \"a carried name naming no domain name of the account\": expected no claim, got \
         {}",
        answer.summary()
    );
}
