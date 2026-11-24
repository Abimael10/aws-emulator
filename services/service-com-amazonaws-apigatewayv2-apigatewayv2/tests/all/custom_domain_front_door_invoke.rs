// SPDX-License-Identifier: CC0-1.0

//! Invoking an api through a CUSTOM DOMAIN NAME at this emulator's OWN endpoint — the address
//! a client configured with one endpoint URL can reach — with the domain name as the `Host`.
//!
//! The rule these cases assert, one level above the recorded case (one CloudFormation-deployed
//! domain name with one mapping, fetched at the domain's root):
//!
//! > A custom domain name is a host of the CALLER's own choosing, and what answers under it is
//! > the `{ApiId, Stage}` of the api mapping whose `ApiMappingKey` the request's path begins
//! > with — so which api serves a request made there is read out of the account's records and
//! > not out of the request. API Gateway resolves that from the `Host`/SNI a request carried,
//! > whichever of its addresses the request arrived at, so a request addressed to this
//! > emulator's own endpoint carrying the domain name as its `Host` is served by the mapped
//! > stage exactly as one made at the address the record published
//! > (`DomainNameConfiguration$ApiGatewayDomainName`) is: the mapping's base path is stripped,
//! > the stage the mapping names is applied, and the stage's own routes, authorization and
//! > integration decide the answer. A path no mapping of the domain name answers to reaches no
//! > api of the account; a `Host` naming no domain name of it reaches no api of this plane at
//! > all.
//!
//! Nothing here is a value this emulator fills in: every served row is answered by a FUNCTION
//! that ran, and the handler reports which api, which stage and which path it was given — so a
//! row proves WHICH stage served it rather than merely that something answered. The rows and
//! their expectations are the ones `custom_domain_mapping_invoke` drives over the published
//! ADDRESS, because the two front doors of one domain name must answer one request the same
//! way.
//!
//! The rows span what the rule generalises over: the mapping key (none, one segment, two
//! segments), which of two apis and which stage the matched mapping names (a named stage and
//! the one that serves an api's root), the path under the key (a path, the key itself, the
//! domain root, a segment that merely starts with a key), a query string, a path no mapping
//! answers, a `Host` that names no domain name of the account, and an api whose own default
//! `execute-api` endpoint is turned OFF — which is what a custom domain name exists for.

use serde_json::json;

use crate::custom_domain_mapping_invoke::{control, given_domain_name, given_mapping, ECHO};
use crate::http_api_backend_dispatch::{
    emulator_with_lambda, given_api, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The emulator's own endpoint: the address a client configured with one endpoint URL dials,
/// and the one this suite sends every request to. The custom domain name travels in the `Host`.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The custom domain name the account creates, and the wildcard record beside it, whose single
/// label `DomainName$DomainName` admits.
const DOMAIN: &str = "shop.api.example.com";
const WILDCARD_DOMAIN: &str = "*.stores.example.com";

/// The two names a record is written over to show that NO record stands for them: this
/// emulator's own address, which is how a client reaches the front door at all, and a wildcard
/// over a zone AWS publishes its own endpoints in — with the host each is addressed by
/// ([`EMULATOR_ENDPOINT`] itself, and another service's own endpoint host).
const EMULATOR_OWN_HOST: &str = "127.0.0.1";
const AWS_ZONE_DOMAIN: &str = "*.us-east-1.amazonaws.com";
const OTHER_SERVICE_HOST: &str = "s3.us-east-1.amazonaws.com";

/// The status a request routed to a stage is answered with here: the function's own.
const SERVED: u16 = 200;

/// The status and sentence a request that reaches no api of the account is answered with: the
/// refusal API Gateway's front door turns such a request away with.
const NO_MAPPING: (u16, &str) = (403, "Forbidden");

/// The error code the front door answers when NO service claims a request — which is what a
/// `Host` naming no domain name and no api of this emulator gets.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// One request made at this emulator's own endpoint, carrying `host` as its `Host`.
async fn fetch(
    emulator: &::smithy_server_runtime::MultiServiceRouter,
    host: &str,
    path: &str,
) -> Answer {
    // The `Host` is what names the custom domain name; the request is addressed at the
    // emulator's own endpoint, which is where it really arrives.
    send(emulator, host, "GET", path, &[("host", host)]).await
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
        // The query string is the client's and is carried on unchanged: the mapping chooses
        // the api and the stage, and nothing of the request's own target is rewritten beyond
        // the base path it was addressed under.
        case: "a query string under a mapping key",
        path: "/shop/items?colour=red&size=2",
        served: (Api::Keyed, "prod", "/prod/items"),
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn a_request_carrying_a_custom_domain_names_host_is_served_by_its_mapped_stage() {
    let case = "two apis mapped onto one custom domain name";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(&emulator, &engine, case, "front-door-echo", ECHO).await;
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
    let root_mapping = given_mapping(&emulator, case, DOMAIN, &root_api, "$default", None).await;
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
        let answer = fetch(&emulator, DOMAIN, row.path).await;
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
    }

    // A WILDCARD record stands for one label under its own suffix, so a client that resolved
    // such a name reaches the mapping of that record — and a name with two labels under the
    // suffix is not under it.
    given_domain_name(&emulator, case, WILDCARD_DOMAIN).await;
    given_mapping(&emulator, case, WILDCARD_DOMAIN, &keyed_api, "prod", None).await;
    let answer = fetch(&emulator, "berlin.stores.example.com", "/items").await;
    assert_eq!(
        (answer.status, answer.body.pointer("/stage").cloned()),
        (SERVED, Some(json!("prod"))),
        "case \"one label under a wildcard record\": expected the mapped stage to serve it, \
         got {}",
        answer.summary()
    );
    let answer = fetch(&emulator, "eu.berlin.stores.example.com", "/items").await;
    assert_eq!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case \"two labels under a wildcard record\": the record stands for one label, so no \
         plane of this service answers, got {}",
        answer.summary()
    );

    // A `Host` naming no domain name of the account — and no api of it — reaches no plane of
    // this service at all, which is what keeps this reading from claiming another service's
    // traffic.
    let answer = fetch(&emulator, "elsewhere.example.com", "/items").await;
    assert_eq!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case \"a host naming no domain name of the account\": expected no claim, got {}",
        answer.summary()
    );

    // And the two kinds of host no record stands for whatever it is NAMED: an ADDRESS of this
    // emulator, which is how a client reaches the front door at all, and a host in a zone AWS
    // publishes its own endpoints in, which is a name no customer can be issued. A record
    // written over either of them takes neither this emulator's own endpoint nor another
    // service's traffic.
    given_domain_name(&emulator, case, EMULATOR_OWN_HOST).await;
    given_mapping(&emulator, case, EMULATOR_OWN_HOST, &keyed_api, "prod", None).await;
    given_domain_name(&emulator, case, AWS_ZONE_DOMAIN).await;
    given_mapping(&emulator, case, AWS_ZONE_DOMAIN, &keyed_api, "prod", None).await;
    for (label, host) in [
        ("a record written over this emulator's own address", EMULATOR_ENDPOINT),
        ("a record written over a zone AWS publishes endpoints in", OTHER_SERVICE_HOST),
    ] {
        let answer = fetch(&emulator, host, "/items").await;
        assert_ne!(
            answer.status, SERVED,
            "case {label:?}: {host:?} must not be served by a mapped stage, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.body.pointer("/stage"),
            None,
            "case {label:?}: no stage of this service may answer for {host:?}, got {}",
            answer.summary()
        );
    }

    // A custom domain name exists so that an api can be reached WITHOUT its default endpoint:
    // "to require that clients use a custom domain name to invoke your API, disable the
    // default endpoint" (`Api$DisableExecuteApiEndpoint`). So the mapped stage keeps serving
    // the domain name while that member is set, and the api's own endpoint stops.
    let (status, updated) = control(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{keyed_api}"),
        Some(json!({ "disableExecuteApiEndpoint": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: UpdateApi answered {status} {updated}"
    );
    let answer = fetch(&emulator, DOMAIN, "/shop/items").await;
    assert_eq!(
        (answer.status, answer.body.pointer("/stage").cloned()),
        (SERVED, Some(json!("prod"))),
        "case \"a disabled default endpoint\": the custom domain name must keep serving the \
         mapped stage, got {}",
        answer.summary()
    );
    let own_endpoint = fetch(
        &emulator,
        EMULATOR_ENDPOINT,
        &format!("/_aws/execute-api/{keyed_api}/prod/items"),
    )
    .await;
    assert_eq!(
        (own_endpoint.status, message(&own_endpoint).as_str()),
        (404, "Not Found"),
        "case \"a disabled default endpoint\": the api's own endpoint must serve nothing, got \
         {}",
        own_endpoint.summary()
    );

    // And the mapping is read PER REQUEST: deleting it leaves the path it answered reaching no
    // api of the account, while the mappings beside it serve as before.
    let (status, deleted) = control(
        &emulator,
        "DELETE",
        &format!("/v2/domainnames/{DOMAIN}/apimappings/{root_mapping}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteApiMapping answered {status} {deleted}"
    );
    let answer = fetch(&emulator, DOMAIN, "/items").await;
    assert_eq!(
        (answer.status, message(&answer).as_str()),
        NO_MAPPING,
        "case \"a path whose mapping was deleted\": expected the gateway's own refusal, got {}",
        answer.summary()
    );
    let answer = fetch(&emulator, DOMAIN, "/shop/items").await;
    assert_eq!(
        (answer.status, answer.body.pointer("/stage").cloned()),
        (SERVED, Some(json!("prod"))),
        "case \"a mapping beside a deleted one\": expected it to keep serving, got {}",
        answer.summary()
    );
}
