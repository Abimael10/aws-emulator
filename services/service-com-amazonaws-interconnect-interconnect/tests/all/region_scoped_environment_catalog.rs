// SPDX-License-Identifier: CC0-1.0

//! The Environment catalog belongs to the Region the request was signed for.
//!
//! `ListEnvironments` lists "all of the environments that can produce connections that
//! will land in the called AWS region"
//! (`smithy_query(query="doc",shape="ListEnvironments")`), and this service's endpoints
//! are regional (`smithy_query(query="service_info")` ->
//! `endpointResolution.urlTemplates = https://interconnect.{Region}.{dnsSuffix}`), so an
//! Environment belongs to ONE Region: the Region whose endpoint offers it, and the
//! Region every connection placed on it lands in. The rule under test is therefore
//! that every operation resolving an `environmentId` resolves it in the catalog of the
//! called Region — the listing, the per-id read, the attach-point listing and the
//! create — so no two regional endpoints offer the same Environment and no connection
//! is ever placed on an Environment that produces connections somewhere else.
//!
//! Every assertion is checked over a TABLE of Regions — the one the rest of this
//! crate's suites sign for plus four more on four other continents, so no row's Region
//! is the harness default by accident — and, inside each Region, over every Environment
//! its own catalog offers.
//!
//! The last suite here pins the other half of that rule: the three operations whose
//! SUBJECT is an Environment (the per-id read, the attach-point listing and the create)
//! resolve an id through ONE judgement, so they answer an unresolvable one with ONE
//! answer — the same status, the same code and the same sentence, compared against each
//! other rather than each against a constant — and serve a resolvable one.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use service_com_amazonaws_interconnect_interconnect::state::environment_not_found_message;

use crate::emulator::{
    available_bandwidths, code, creatable_environments, message, text, Emulator,
    DEFAULT_SIGNING_REGION,
};

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The signing name the model gives this service; also its ARN namespace.
const SIGNING_NAME: &str = "interconnect";

/// The Regions the rows address.
const REGIONS: [&str; 5] = [
    DEFAULT_SIGNING_REGION,
    "eu-west-1",
    "ap-southeast-2",
    "sa-east-1",
    "me-south-1",
];

/// The `@length` the model gives `EnvironmentId`
/// (`smithy_query(query="shape",shape="EnvironmentId")`), which every id a Region's
/// catalog publishes has to fit.
const ENVIRONMENT_ID_LENGTH: (usize, usize) = (1, 64);

/// Every Region's catalog, read through that Region's own endpoint.
async fn catalogs(emulator: &Emulator) -> BTreeMap<&'static str, Vec<Value>> {
    let mut by_region = BTreeMap::new();
    for region in REGIONS {
        let catalog = emulator.environments_in(region).await;
        assert!(
            !catalog.is_empty(),
            "case {region:?}: expected the Region's endpoint to offer a catalog, got {catalog:?}"
        );
        by_region.insert(region, catalog);
    }
    by_region
}

/// The `environmentId`s of a catalog, as a set.
fn ids_of(catalog: &[Value]) -> BTreeSet<String> {
    catalog
        .iter()
        .map(|environment| text(environment, "environmentId"))
        .collect()
}

/// Assert `(status, body)` is the refusal this service gives an `environmentId` that
/// resolves to no Environment of the called Region: the 400
/// `InterconnectClientException`, worded from the requested id.
fn assert_environment_not_found(case: &str, environment_id: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for an environmentId this endpoint does not offer, got {status} with {body:?}"
    );
    assert!(
        code(body).contains("InterconnectClientException"),
        "case {case:?}: expected InterconnectClientException, got {:?}",
        code(body)
    );
    let reported = message(body);
    assert_eq!(
        reported,
        environment_not_found_message(environment_id),
        "case {case:?}: expected this service's own unresolvable-environment wording, got {reported:?}"
    );
}

/// RULE: the catalog is a function of the called Region — each Region's endpoint offers
/// its own Environments, every one of them readable by id THERE and byte-identical to
/// the listed entry, and no two Regions offer the same Environment.
#[tokio::test]
async fn each_regional_endpoint_offers_its_own_environment_catalog() {
    let emulator = Emulator::new();
    let by_region = catalogs(&emulator).await;

    for (region, catalog) in &by_region {
        for environment in catalog {
            let id = text(environment, "environmentId");
            let length = id.chars().count();
            let (min, max) = ENVIRONMENT_ID_LENGTH;
            assert!(
                (min..=max).contains(&length),
                "case {region:?}/{id:?}: expected an EnvironmentId of length {min}..{max}, got {length}"
            );

            // The per-id read answers from the same catalog the listing did.
            let fetched = emulator.environment_in(region, &id).await;
            assert_eq!(
                &fetched, environment,
                "case {region:?}/{id:?}: expected GetEnvironment to report the listed record, got {fetched:?}"
            );
        }

        // A listing changes nothing a later caller sees: the same endpoint answers the
        // same catalog, and so does every other endpoint.
        let again = emulator.environments_in(region).await;
        assert_eq!(
            &again, catalog,
            "case {region:?}: expected a repeated listing to answer the same catalog, got {again:?}"
        );
    }

    // No Environment is offered by two Regions: the Region is part of what identifies
    // one, so two regional endpoints can always be told apart by their catalogs.
    for (region, catalog) in &by_region {
        let mine = ids_of(catalog);
        for (other, other_catalog) in &by_region {
            if other == region {
                continue;
            }
            let theirs = ids_of(other_catalog);
            let shared: BTreeSet<&String> = mine.intersection(&theirs).collect();
            assert!(
                shared.is_empty(),
                "case {region:?} against {other:?}: expected the two catalogs to share no environmentId, got {shared:?}"
            );
        }
    }
}

/// RULE: an `environmentId` of another Region's catalog resolves to nothing here —
/// the read, the attach-point listing and the create all answer exactly as they do for
/// an id no catalog holds at all.
#[tokio::test]
async fn an_environment_of_another_region_resolves_nowhere_else() {
    let emulator = Emulator::new();
    let by_region = catalogs(&emulator).await;
    // An attach point the account really holds, so a refusal below is about the
    // Environment and not about the attach point.
    let attach_point = emulator.attach_point().await;

    // Ids no catalog of any Region holds: the baseline every out-of-Region answer is
    // compared against. They differ in shape — a plausible one of an unknown partner, a
    // plausible one of an unknown Region, and one that is not of this emulator's form.
    let never_offered = [
        "env-csp-unknownpartner-us-east-1".to_string(),
        // The shape of an id of a Region nobody addresses here.
        "env-csp-azure-xx-nowhere-9".to_string(),
        "not-an-environment".to_string(),
    ];

    for region in by_region.keys() {
        let foreign: Vec<String> = by_region
            .iter()
            .filter(|(other, _)| *other != region)
            .flat_map(|(_, catalog)| ids_of(catalog))
            .collect();
        assert!(
            !foreign.is_empty(),
            "case {region:?}: expected other Regions to offer Environments, got none"
        );

        for environment_id in foreign.iter().chain(never_offered.iter()) {
            let case = format!("{environment_id} addressed in {region}");

            let (status, body) = emulator
                .ic_in(region, "GetEnvironment", json!({ "id": environment_id }))
                .await;
            assert_environment_not_found(&format!("{case} on GetEnvironment"), environment_id, status, &body);

            let (status, body) = emulator
                .ic_in(
                    region,
                    "ListAttachPoints",
                    json!({ "environmentId": environment_id }),
                )
                .await;
            assert_environment_not_found(
                &format!("{case} on ListAttachPoints"),
                environment_id,
                status,
                &body,
            );

            // The create carries a bandwidth and an attach point the Region's own
            // Environments do take, so what refuses it is the Environment alone.
            let (status, body) = emulator
                .ic_in(
                    region,
                    "CreateConnection",
                    json!({
                        "environmentId": environment_id,
                        "bandwidth": "1Gbps",
                        "attachPoint": { "directConnectGateway": attach_point },
                    }),
                )
                .await;
            assert_environment_not_found(
                &format!("{case} on CreateConnection"),
                environment_id,
                status,
                &body,
            );
        }

        // And the refused calls placed nothing: this Region's endpoint holds no
        // connection at all yet.
        let listed = emulator
            .ic_ok_in(region, "ListConnections", json!({ "maxResults": 100 }))
            .await;
        assert_eq!(
            listed["connections"].as_array().map(Vec::len).unwrap_or(0),
            0,
            "case {region:?}: expected the refused creates to have placed nothing, got {:?}",
            listed["connections"]
        );
    }
}

/// Every `environmentId` a row is refused for, as `(shape, id)`.
///
/// Each fits the `@length(min: 1, max: 64)` the model gives `EnvironmentId`
/// (`smithy_query(query="shape",shape="EnvironmentId")`, which puts no pattern on it),
/// and the rows differ in what makes them unresolvable: the shortest and the longest id
/// the member may carry, a published id of this Region with its case changed, a
/// published id with one more character on it, a plausible id of a partner no catalog
/// publishes, and text that is not of this service's id form at all. `offered` is an id
/// the called Region's catalog DOES hold, so the near-miss rows are near misses of a
/// real one.
fn unresolvable_id_shapes(offered: &str) -> Vec<(String, String)> {
    let (min, max) = ENVIRONMENT_ID_LENGTH;
    vec![
        (
            "the shortest id the member may carry".to_string(),
            "x".repeat(min),
        ),
        (
            "the longest id the member may carry".to_string(),
            "e".repeat(max),
        ),
        (
            "a published id of this Region in upper case".to_string(),
            offered.to_ascii_uppercase(),
        ),
        (
            "a published id of this Region with one character appended".to_string(),
            format!("{offered}0"),
        ),
        (
            "a plausible id of a partner no catalog publishes".to_string(),
            "env-csp-unknownpartner-us-east-1".to_string(),
        ),
        (
            "text that is not an id of this service at all".to_string(),
            "not-an-environment".to_string(),
        ),
    ]
}

/// The `(status, code, message)` each operation whose SUBJECT is an Environment answers
/// `environment_id` with, through the endpoint of `region`.
///
/// `attach_point` and `bandwidth` are ones the Region's own catalog takes, so what any
/// of these answers is about the `environmentId` alone.
async fn environment_subject_answers(
    emulator: &Emulator,
    region: &str,
    environment_id: &str,
    attach_point: &str,
    bandwidth: &str,
) -> Vec<(&'static str, (u16, String, String))> {
    let requests = [
        ("GetEnvironment", json!({ "id": environment_id })),
        (
            "ListAttachPoints",
            json!({ "environmentId": environment_id }),
        ),
        (
            "CreateConnection",
            json!({
                "environmentId": environment_id,
                "bandwidth": bandwidth,
                "attachPoint": { "directConnectGateway": attach_point },
            }),
        ),
    ];
    let mut answers = Vec::new();
    for (operation, request) in requests {
        let (status, body) = emulator.ic_in(region, operation, request).await;
        answers.push((operation, (status, code(&body), message(&body))));
    }
    answers
}

/// RULE: an `environmentId` is resolved in the called Region's catalog by ONE judgement
/// shared by every operation whose SUBJECT is an Environment, so the per-id read, the
/// attach-point listing and the create cannot disagree about whether an id resolves, nor
/// about the class and the wording of the refusal when it does not.
#[tokio::test]
async fn every_environment_subject_operation_answers_one_verdict() {
    let emulator = Emulator::new();
    let by_region = catalogs(&emulator).await;
    // An attach point the account really holds, so nothing below is refused for the
    // attach point instead of the Environment.
    let attach_point = emulator.attach_point().await;

    for (region, catalog) in &by_region {
        let offered = ids_of(catalog);
        let creatable = creatable_environments(catalog);
        let environment = creatable
            .first()
            .expect("every Region's catalog offers an Environment that takes a connection")
            .clone();
        let environment_id = text(&environment, "environmentId");
        let bandwidth = available_bandwidths(&environment)
            .into_iter()
            .next()
            .expect("a creatable Environment offers a bandwidth");

        for (shape, id) in unresolvable_id_shapes(&environment_id) {
            let case = format!("{region} / {shape} ({id})");
            let (min, max) = ENVIRONMENT_ID_LENGTH;
            let length = id.chars().count();
            assert!(
                (min..=max).contains(&length),
                "case {case:?}: expected the row to fit an EnvironmentId of length {min}..{max}, got {length}"
            );
            assert!(
                !offered.contains(&id),
                "case {case:?}: expected the row to be outside this Region's catalog, got one it offers"
            );

            let answers = environment_subject_answers(
                &emulator,
                region,
                &id,
                &attach_point,
                &bandwidth,
            )
            .await;
            let (first_operation, first) = answers
                .first()
                .cloned()
                .expect("three operations were asked");
            for (operation, answer) in &answers {
                // ONE verdict: every operation's answer is the same answer, compared
                // against its neighbour's and not only against a constant.
                assert_eq!(
                    answer, &first,
                    "case {case:?}: expected {operation} to answer exactly what {first_operation} answers ({first:?}), got {answer:?}"
                );
            }
            // And that one answer is the refusal this service gives an unresolvable
            // environment: HTTP 400, `InterconnectClientException`, worded from the id
            // the request carried.
            let (status, reported_code, reported_message) = first;
            assert_environment_not_found(
                &case,
                &id,
                status,
                &json!({ "__type": reported_code, "message": reported_message }),
            );
        }

        // The same seam SERVES a resolvable id, through each of the three operations: the
        // read reports the catalog record, the attach-point listing is served for it, and
        // a connection is placed on it.
        let fetched = emulator.environment_in(region, &environment_id).await;
        assert_eq!(
            fetched, environment,
            "case {region:?}/{environment_id:?}: expected GetEnvironment to report the listed record, got {fetched:?}"
        );
        let listed = emulator
            .ic_ok_in(
                region,
                "ListAttachPoints",
                json!({ "environmentId": environment_id }),
            )
            .await;
        assert!(
            listed["attachPoints"].is_array(),
            "case {region:?}/{environment_id:?}: expected a served attach-point page, got {listed:?}"
        );
        let mut request = Map::new();
        request.insert("environmentId".to_string(), json!(environment_id));
        request.insert("bandwidth".to_string(), json!(bandwidth));
        request.insert(
            "attachPoint".to_string(),
            json!({ "directConnectGateway": attach_point }),
        );
        if let Some(remote_account) = crate::emulator::remote_account_for(&environment) {
            request.insert("remoteAccount".to_string(), remote_account);
        }
        let created = emulator
            .ic_ok_in(region, "CreateConnection", Value::Object(request))
            .await;
        assert_eq!(
            text(&created["connection"], "environmentId"),
            environment_id,
            "case {region:?}/{environment_id:?}: expected the connection to be placed on the resolved Environment, got {:?}",
            created["connection"]["environmentId"]
        );
    }
}

/// RULE: a connection is placed only on an Environment of the Region the create
/// addresses, and it lands THERE — the record carries that Environment's partner,
/// location and product type, and its ARN carries the called Region.
#[tokio::test]
async fn a_connection_is_placed_only_on_an_environment_of_the_called_region() {
    let emulator = Emulator::new();
    let by_region = catalogs(&emulator).await;
    let attach_point = emulator.attach_point().await;

    for (region, catalog) in &by_region {
        // Rows differ in the Environment and in the bandwidth: every Environment of
        // this Region that takes a connection, on the first bandwidth it offers.
        for environment in creatable_environments(catalog) {
            let environment_id = text(&environment, "environmentId");
            let bandwidth = available_bandwidths(&environment)
                .into_iter()
                .next()
                .expect("a creatable Environment offers a bandwidth");
            let case = format!("{environment_id} / {bandwidth} in {region}");

            let mut request = Map::new();
            request.insert("environmentId".to_string(), json!(environment_id));
            request.insert("bandwidth".to_string(), json!(bandwidth));
            request.insert(
                "attachPoint".to_string(),
                json!({ "directConnectGateway": attach_point }),
            );
            if let Some(remote_account) = crate::emulator::remote_account_for(&environment) {
                request.insert("remoteAccount".to_string(), remote_account);
            }
            let created = emulator
                .ic_ok_in(region, "CreateConnection", Value::Object(request))
                .await;
            let connection = created["connection"].clone();

            let arn = text(&connection, "arn");
            let id = text(&connection, "id");
            assert_eq!(
                arn,
                format!("arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:connection/{id}"),
                "case {case:?}: expected the connection to land in the called Region, got {arn:?}"
            );
            for member in ["provider", "location", "type"] {
                assert_eq!(
                    connection[member], environment[member],
                    "case {case:?}: expected the Environment's {member}, got {:?}",
                    connection[member]
                );
            }
            assert_eq!(
                text(&connection, "environmentId"),
                environment_id,
                "case {case:?}: expected the connection to carry the requested environmentId, got {:?}",
                connection["environmentId"]
            );
        }
    }
}

/// RULE: the two modelled filters select INSIDE the called Region's catalog, and a
/// pagination token is one that Region's endpoint issued — so neither reaches an
/// Environment of another Region.
#[tokio::test]
async fn the_filters_and_cursors_stay_inside_the_called_regions_catalog() {
    let emulator = Emulator::new();
    let by_region = catalogs(&emulator).await;

    for (region, catalog) in &by_region {
        // Rows: every provider the Region's catalog holds, every location it holds, and
        // both together — each expectation computed from that Region's catalog.
        let mut filters: Vec<(String, Value)> = Vec::new();
        for environment in catalog {
            filters.push((
                format!("provider {:?}", environment["provider"]),
                json!({ "provider": environment["provider"] }),
            ));
            filters.push((
                format!("location {:?}", environment["location"]),
                json!({ "location": environment["location"] }),
            ));
        }
        filters.push((
            "provider and location together".to_string(),
            json!({
                "provider": catalog[0]["provider"],
                "location": catalog[0]["location"],
            }),
        ));

        for (label, filter) in filters {
            let expected: Vec<Value> = catalog
                .iter()
                .filter(|environment| {
                    ["provider", "location"].iter().all(|member| {
                        filter
                            .get(*member)
                            .is_none_or(|wanted| environment[*member] == *wanted)
                    })
                })
                .cloned()
                .collect();
            let listed = emulator
                .ic_ok_in(region, "ListEnvironments", filter)
                .await;
            let actual = listed["environments"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {region:?}/{label}: expected the {} matching Environments of this Region, got {actual:?}",
                expected.len()
            );
        }

        // A paged walk at a page size of one visits this Region's catalog exactly once,
        // and every cursor it hands out is served by the SAME endpoint.
        let mut walked: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = Map::new();
            request.insert("maxResults".to_string(), json!(1));
            if let Some(token) = &token {
                request.insert("nextToken".to_string(), json!(token));
            }
            let listed = emulator
                .ic_ok_in(region, "ListEnvironments", Value::Object(request))
                .await;
            walked.extend(listed["environments"].as_array().cloned().unwrap_or_default());
            token = listed["nextToken"].as_str().map(str::to_string);
            let Some(cursor) = token.clone() else {
                break;
            };
            assert!(
                walked.len() <= catalog.len(),
                "case {region:?}: expected the paged walk to terminate, got {} entries so far",
                walked.len()
            );

            // That very cursor, offered to every OTHER Region's endpoint, is not a
            // token THAT endpoint issued: it names an Environment that Region's catalog
            // does not hold.
            for other in REGIONS.iter().filter(|other| *other != region) {
                let (status, body) = emulator
                    .ic_in(other, "ListEnvironments", json!({ "nextToken": cursor }))
                    .await;
                assert_eq!(
                    status, 400,
                    "case {region:?} cursor {cursor:?} offered in {other}: expected HTTP 400 for a token this endpoint never issued, got {status} with {body:?}"
                );
                assert!(
                    code(&body).contains("InterconnectValidationException"),
                    "case {region:?} cursor {cursor:?} offered in {other}: expected InterconnectValidationException, got {:?}",
                    code(&body)
                );
            }
        }
        assert_eq!(
            &walked, catalog,
            "case {region:?}: expected the paged walk to equal the unpaged catalog, got {walked:?}"
        );
    }
}
