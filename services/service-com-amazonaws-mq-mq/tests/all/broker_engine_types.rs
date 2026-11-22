// SPDX-License-Identifier: CC0-1.0

//! `DescribeBrokerEngineTypes`, driven over the wire.
//!
//! RULE: the operation reports Amazon MQ's published engine-type catalog — one entry per
//! engine type the service offers, each carrying the engine versions currently offered
//! for creation on that engine, newest first — narrowed by the `engineType` filter the
//! request set and paged by `maxResults` with a round-trippable `nextToken`. Because the
//! catalog describes the SERVICE and not the account, the answer does not depend on
//! anything a caller created, it does not differ by Region, and a call changes nothing a
//! later caller sees.
//!
//! The tables span what the rule generalises over rather than one recorded request: the
//! filter crossed with every value the model enumerates for it and with a value outside
//! that set, the published bounds of `maxResults` and one past each, several Regions, and
//! every version the catalog reports fed back into `CreateBroker`. The expected answer of
//! a filtered or paged call is always derived from the UNFILTERED response read over the
//! same wire, so no entry is written out here.
//!
//! The value catalog the tables span is the model's own enumeration —
//! `smithy_query(query="shape", shape="EngineType")`, which the crate re-exports as
//! `ENGINE_TYPE_VALUES`.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{
    encode_page_token, invalid_page_token_message, published_engine_versions, DEFAULT_MAX_RESULTS,
    MAX_MAX_RESULTS, MIN_MAX_RESULTS,
};

use crate::broker_lifecycle::{string_at, Mq};

/// The operation's modelled path (`smithy_query(query="operation",
/// operation="DescribeBrokerEngineTypes")`: `GET /v1/broker-engine-types`).
const PATH: &str = "/v1/broker-engine-types";

/// One `DescribeBrokerEngineTypes` call, with `query` as its query string.
async fn describe(mq: &Mq, query: &str) -> (u16, Value) {
    let path = if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{query}")
    };
    mq.call("GET", &path, None).await
}

/// The same call, also reporting the error code a restJson1 client branches on.
async fn describe_with_code(mq: &Mq, query: &str) -> (u16, String, Value) {
    let path = if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{query}")
    };
    mq.call_with_code("GET", &path, None).await
}

/// The entries of a successful answer.
fn entries(case: &str, body: &Value) -> Vec<Value> {
    let Some(entries) = body.get("brokerEngineTypes").and_then(Value::as_array) else {
        panic!("case {case:?}: expected a brokerEngineTypes array, got {body:?}");
    };
    entries.clone()
}

/// One entry's `engineType`.
fn engine_type_of(case: &str, entry: &Value) -> String {
    let Some(value) = entry.get("engineType").and_then(Value::as_str) else {
        panic!("case {case:?}: expected a string engineType, got {entry:?}");
    };
    value.to_string()
}

/// The version names one entry reports, in the order it reports them.
fn version_names(case: &str, entry: &Value) -> Vec<String> {
    let Some(versions) = entry.get("engineVersions").and_then(Value::as_array) else {
        panic!("case {case:?}: expected an engineVersions array, got {entry:?}");
    };
    versions
        .iter()
        .map(|version| {
            let Some(name) = version.get("name").and_then(Value::as_str) else {
                panic!("case {case:?}: expected each engine version to carry a name, got {version:?}");
            };
            name.to_string()
        })
        .collect()
}

/// The whole catalog, read over the wire in one page.
async fn whole_catalog(case: &str, mq: &Mq) -> Vec<Value> {
    let (status, body) = describe(mq, &format!("maxResults={MAX_MAX_RESULTS}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for the unfiltered catalog, got {status} {body:?}"
    );
    assert!(
        body.get("nextToken").is_none(),
        "case {case:?}: expected the whole catalog to fit one page of {MAX_MAX_RESULTS}, \
         got a nextToken in {body:?}"
    );
    entries(case, &body)
}

#[tokio::test]
async fn the_catalog_reports_every_modelled_engine_type_with_its_offered_versions() {
    let mq = Mq::new("us-east-1");
    let case = "unfiltered";
    let catalog = whole_catalog(case, &mq).await;

    // One entry per engine type the model enumerates, in the order the model enumerates
    // them — the order a `nextToken` offset names the same entry by on a later call.
    let reported: Vec<String> = catalog
        .iter()
        .map(|entry| engine_type_of(case, entry))
        .collect();
    let expected: Vec<String> = ENGINE_TYPE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected one entry per modelled engine type in model order, got \
         {reported:?}"
    );

    for entry in &catalog {
        let engine_type = engine_type_of(case, entry);
        let case = &engine_type;
        let versions = version_names(case, entry);
        // The versions a create may name on that engine, newest first.
        let mut offered: Vec<String> = published_engine_versions(&engine_type)
            .iter()
            .map(|version| (*version).to_string())
            .collect();
        offered.reverse();
        assert_eq!(
            versions, offered,
            "case {case:?}: expected the offered versions newest first, got {versions:?}"
        );
        assert!(
            !versions.is_empty(),
            "case {case:?}: every engine type the service offers has at least one version"
        );
        for version in &versions {
            assert_eq!(
                version.split('.').count(),
                2,
                "case {case:?}: a version a broker can be created with is MAJOR.MINOR, got \
                 {version:?}"
            );
        }
        let mut distinct = versions.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            versions.len(),
            "case {case:?}: a version is reported once, got {versions:?}"
        );
    }
}

#[tokio::test]
async fn the_engine_type_filter_selects_exactly_the_matching_entries() {
    let mq = Mq::new("us-west-2");
    let catalog = whole_catalog("unfiltered", &mq).await;

    // Each row: the filter the request sets — omitted, sent empty (which narrows nothing,
    // exactly like one omitted, the member being `__string` on the wire), and each value
    // the model enumerates, which is also the spelling this catalog serves, so a client
    // can feed back what it read.
    let mut rows: Vec<Option<String>> = vec![None, Some(String::new())];
    rows.extend(ENGINE_TYPE_VALUES.iter().map(|value| Some((*value).to_string())));

    for filter in &rows {
        let mut query = vec![format!("maxResults={MAX_MAX_RESULTS}")];
        if let Some(filter) = filter {
            query.push(format!("engineType={filter}"));
        }
        let query = query.join("&");
        let case = &query;

        let expected: Vec<Value> = catalog
            .iter()
            .filter(|entry| match filter {
                None => true,
                Some(filter) if filter.is_empty() => true,
                Some(filter) => &engine_type_of(case, entry) == filter,
            })
            .cloned()
            .collect();
        assert!(
            !expected.is_empty(),
            "case {case:?}: the catalog must report an entry for every modelled engine type"
        );

        let (status, body) = describe(&mq, &query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        let got = entries(case, &body);
        assert_eq!(
            got,
            expected,
            "case {case:?}: expected the {} matching entries, got {}",
            expected.len(),
            got.len()
        );
    }
}

#[tokio::test]
async fn pagination_walks_every_entry_exactly_once() {
    let mq = Mq::new("eu-west-1");
    let catalog = whole_catalog("unfiltered", &mq).await;

    // The published bounds of `maxResults` (1 and 100) and a size between them, so the
    // walk is tested where a page is one entry, several entries and the whole catalog.
    for size in [MIN_MAX_RESULTS, 2, MAX_MAX_RESULTS] {
        let case = &format!("maxResults={size}");
        let mut seen: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                None => format!("maxResults={size}"),
                Some(token) => format!("maxResults={size}&nextToken={token}"),
            };
            let (status, body) = describe(&mq, &query).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 on page {pages}, got {status} {body:?}"
            );
            assert_eq!(
                body.get("maxResults").and_then(Value::as_i64),
                Some(i64::from(size)),
                "case {case:?}: the answer reports the page size it used, got {body:?}"
            );
            let page = entries(case, &body);
            assert!(
                page.len() <= usize::try_from(size).unwrap_or_default(),
                "case {case:?}: a page holds at most {size} entries, got {}",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= catalog.len() + 1,
                "case {case:?}: the walk must terminate, got {pages} pages for {} entries",
                catalog.len()
            );
            match body.get("nextToken").and_then(Value::as_str) {
                None => break,
                Some(next) => token = Some(next.to_string()),
            }
        }
        assert_eq!(
            seen, catalog,
            "case {case:?}: paging must report every entry exactly once, in the same order"
        );
    }

    // The default page size, from the output member's own documentation ("20 by
    // default").
    let case = "no maxResults";
    let (status, body) = describe(&mq, "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {body:?}"
    );
    assert_eq!(
        body.get("maxResults").and_then(Value::as_i64),
        Some(i64::try_from(DEFAULT_MAX_RESULTS).unwrap_or_default()),
        "case {case:?}: the answer reports the documented default page size, got {body:?}"
    );
    assert_eq!(
        entries(case, &body),
        catalog[..catalog.len().min(DEFAULT_MAX_RESULTS)].to_vec(),
        "case {case:?}: the first page is the catalog's first {DEFAULT_MAX_RESULTS} entries"
    );

    // A token that names an offset past the end answers the EMPTY page rather than an
    // error, so a client that paginates one call too far still terminates.
    let case = "a token past the end";
    let past_the_end = encode_page_token(catalog.len() + 5);
    let (status, body) = describe(&mq, &format!("nextToken={past_the_end}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {body:?}"
    );
    assert_eq!(
        entries(case, &body),
        Vec::<Value>::new(),
        "case {case:?}: expected no entries past the end of the catalog, got {body:?}"
    );
    assert!(
        body.get("nextToken").is_none(),
        "case {case:?}: the page past the end carries no token, got {body:?}"
    );
}

#[tokio::test]
async fn values_outside_the_published_bounds_and_sets_are_refused() {
    let mq = Mq::new("us-east-1");

    // Each row: the query, the member the refusal must name, and the sentence it must
    // carry. The enum value is built FROM the model's set rather than copied from a
    // request, and the bounds are the published ones and one past each.
    let engine_outside = format!("{}_NOT_AN_ENGINE", ENGINE_TYPE_VALUES[0]);
    let bogus_token = "not-a-token-this-service-issued";
    let rows: Vec<(String, &str, Option<String>)> = vec![
        (
            format!("maxResults={}", MIN_MAX_RESULTS - 1),
            "maxResults",
            None,
        ),
        (
            format!("maxResults={}", MAX_MAX_RESULTS + 1),
            "maxResults",
            None,
        ),
        (
            format!("engineType={engine_outside}"),
            "engineType",
            Some(format!("Broker engine [{engine_outside}] is invalid.")),
        ),
        (
            format!("nextToken={bogus_token}"),
            "nextToken",
            Some(invalid_page_token_message(bogus_token)),
        ),
    ];

    for (query, attribute, message) in &rows {
        let case = query;
        let (status, code, body) = describe_with_code(&mq, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body:?}"
        );
        assert_eq!(
            code, "BadRequestException",
            "case {case:?}: expected the modelled error code, got {code:?} {body:?}"
        );
        assert_eq!(
            body.get("errorAttribute").and_then(Value::as_str),
            Some(*attribute),
            "case {case:?}: the refusal must name the member at fault, got {body:?}"
        );
        if let Some(message) = message {
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(message.as_str()),
                "case {case:?}: unexpected sentence in {body:?}"
            );
        }
    }

    // The published bounds themselves are accepted.
    for size in [MIN_MAX_RESULTS, MAX_MAX_RESULTS] {
        let query = format!("maxResults={size}");
        let (status, body) = describe(&mq, &query).await;
        assert_eq!(
            status, 200,
            "case {query:?}: a value on the published bound is accepted, got {status} {body:?}"
        );
    }
}

#[tokio::test]
async fn the_catalog_follows_the_service_and_not_the_account() {
    // The catalog describes the service, so it does not differ by the Region the caller
    // signed for — nothing in it is named for a Region.
    let mut first: Option<Vec<Value>> = None;
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
        let mq = Mq::new(region);
        let catalog = whole_catalog(region, &mq).await;
        match &first {
            None => first = Some(catalog),
            Some(first) => assert_eq!(
                &catalog, first,
                "case {region:?}: the offered engine types do not differ by Region"
            ),
        }
    }

    // What the account holds cannot change it, and reading it changes nothing a later
    // caller sees.
    let mq = Mq::new("us-east-1");
    let before = whole_catalog("before a broker exists", &mq).await;
    let (status, created) = mq.create_broker(create_broker_body("catalog-witness", "ACTIVEMQ", None)).await;
    assert_eq!(
        status, 200,
        "case \"create a broker\": expected 200, got {status} {created:?}"
    );
    let after = whole_catalog("after a broker exists", &mq).await;
    assert_eq!(
        after, before,
        "case \"after a broker exists\": the catalog must not depend on the account's brokers"
    );
    let again = whole_catalog("read twice", &mq).await;
    assert_eq!(
        again, after,
        "case \"read twice\": a read must not change what a later caller sees"
    );
}

#[tokio::test]
async fn every_reported_version_is_one_create_broker_accepts_for_its_engine() {
    let mq = Mq::new("us-east-1");
    let catalog = whole_catalog("unfiltered", &mq).await;

    // Each case: one (engine type, reported version) pair of the catalog — the whole
    // catalog crossed, not one row of it. The catalog is what a client consults BEFORE
    // creating a broker, so every version it reports has to be one the create takes, and
    // the broker that comes back has to run that version line.
    for entry in &catalog {
        let engine_type = engine_type_of("unfiltered", entry);
        for (index, version) in version_names(&engine_type, entry).iter().enumerate() {
            let case = &format!("{engine_type} {version}");
            let name = format!("engine-{}-{index}", engine_type.to_ascii_lowercase());
            let (status, created) = mq
                .create_broker(create_broker_body(&name, &engine_type, Some(version)))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a version this catalog reports must be one CreateBroker \
                 accepts, got {status} {created:?}"
            );
            let broker_id = string_at(&created, "/brokerId");
            let (status, broker) = mq.describe_broker(&broker_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected to read the broker back, got {status} {broker:?}"
            );
            let reported = string_at(&broker, "/engineVersion");
            assert!(
                reported == *version || reported.starts_with(&format!("{version}.")),
                "case {case:?}: expected the broker to run the version line it was created \
                 on, got {reported:?}"
            );
            // A broker READ answers the engine's product spelling (`ActiveMQ`), while the
            // catalog answers the modelled token a request sends (`ACTIVEMQ`) — the
            // spelling its own `engineType` filter takes — so the engines are compared
            // without regard to case.
            assert_eq!(
                string_at(&broker, "/engineType").to_ascii_uppercase(),
                engine_type,
                "case {case:?}: expected the broker to run the engine it was created with, \
                 got {broker:?}"
            );
        }
    }
}

/// A minimal valid `CreateBroker` body for `engine_type`, optionally naming
/// `engine_version`: the members the model requires plus the one initial user Amazon MQ
/// creates a broker with (`Users` — "an administrative user is required", see
/// `state::MIN_USERS_PER_BROKER`). A RabbitMQ broker is asked for on a non-burstable
/// instance type, the sizes the engine is offered on.
fn create_broker_body(name: &str, engine_type: &str, engine_version: Option<&str>) -> Value {
    let host_instance_type = if engine_type == "RABBITMQ" {
        "mq.m5.large"
    } else {
        "mq.t3.micro"
    };
    let mut body = json!({
        "brokerName": name,
        "engineType": engine_type,
        "deploymentMode": "SINGLE_INSTANCE",
        "hostInstanceType": host_instance_type,
        "publiclyAccessible": false,
        "users": [{"username": "admin", "password": "SuperSecret123"}],
    });
    if let Some(engine_version) = engine_version {
        body["engineVersion"] = json!(engine_version);
    }
    body
}
