// SPDX-License-Identifier: CC0-1.0

//! `ListAttachPoints` offers BOTH documented forms of attach point, and offers exactly
//! what the two writers of a connection's attach point accept.
//!
//! RULE under test: the operation "lists all Attach Points the caller has access to that
//! are valid for the specified Environment", and `AttachPointDescriptor$type` — "the type
//! of this AttachPoint, which will dictate the syntax of the identifier" — documents the
//! current types as "ARN" and "DirectConnect Gateway", the two members `AttachPoint`
//! models. So the page is the union of the two: every Direct Connect gateway of the
//! account that is ready for use, and every Connection of the called endpoint that is
//! providing connectivity, named by its full ARN. The set the listing offers for an
//! Environment is therefore the same set a `CreateConnection` (or an
//! `AcceptConnectionProposal`) onto that Environment accepts — neither side offers what
//! the other refuses.
//!
//! The rows differ along what the rule generalises over: the FORM of the attach point
//! (gateway id, connection ARN), the `description` the connection behind an ARN carries
//! (absent, a spaced one, the longest `ConnectionDescription` allows), the bandwidth, the
//! Region the request addresses, how many attach points the account holds, and the state
//! the connection behind the ARN is in — all of it read from the LIVE Environment
//! catalog, the LIVE gateways and the LIVE records rather than from a copy of any of
//! them.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use service_com_amazonaws_interconnect_interconnect::state::attach_point_not_valid_message;

use crate::emulator::{
    attach_point_of, available_bandwidths, code, creatable_environments, identifiers, message,
    of_type, remote_account_for, text, Emulator, ATTACH_POINT_TYPE, ATTACH_POINT_TYPE_ARN,
    CONNECTION_READY_STATE, DEFAULT_SIGNING_REGION,
};

/// The gateway rows the account is stocked with: the name's shape and the `amazonSideAsn`
/// differ per row (absent, and one from the private range
/// `CreateDirectConnectGatewayRequest$amazonSideAsn` documents), so no assertion is keyed
/// to one form of gateway.
const GATEWAY_ROWS: [(&str, Option<i64>); 2] = [
    ("ic-arn-listing-default-asn", None),
    ("ic arn listing spaced name", Some(64_512)),
];

/// A second Region, so the rows span the endpoint a connection belongs to as well as the
/// form of the attach point.
const OTHER_REGION: &str = "eu-west-1";

/// A well-formed `DirectConnectGatewayAttachPoint` (the 36-character UUID the member's
/// pattern spells out) that names no gateway of the account.
const UNOWNED_GATEWAY_UUID: &str = "00000000-0000-4000-8000-000000000000";

/// How long a wait for a state the clock owes runs before the case fails.
const POLL_BUDGET: Duration = Duration::from_secs(5);

/// How often such a wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// `CreateConnection` on `environment` through `region`'s endpoint, as a `(status, body)`
/// pair so a refusal is readable.
///
/// The `remoteAccount` comes from the Environment's own record, so a row about the attach
/// point is not also a row about the identifying information.
async fn create_in(
    emulator: &Emulator,
    region: &str,
    environment: &Value,
    bandwidth: &str,
    description: Option<&str>,
    attach_point: &Value,
) -> (u16, Value) {
    let mut request = Map::new();
    request.insert(
        "environmentId".to_string(),
        json!(text(environment, "environmentId")),
    );
    request.insert("bandwidth".to_string(), json!(bandwidth));
    request.insert("attachPoint".to_string(), attach_point.clone());
    if let Some(description) = description {
        request.insert("description".to_string(), json!(description));
    }
    if let Some(remote_account) = remote_account_for(environment) {
        request.insert("remoteAccount".to_string(), remote_account);
    }
    emulator
        .ic_in(region, "CreateConnection", Value::Object(request))
        .await
}

/// A fully provisioned connection of `region`, created on `gateway` — the form of attach
/// point that needs no connection behind it, so every ARN row starts there.
async fn provisioned_on_gateway(
    emulator: &Emulator,
    region: &str,
    environment: &Value,
    bandwidth: &str,
    gateway: &str,
) -> Value {
    let (status, body) = create_in(
        emulator,
        region,
        environment,
        bandwidth,
        Some("arn attach point subject"),
        &json!({ "directConnectGateway": gateway }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {region:?}: expected the connection the ARN will name to be created, got {status} with {body:?}"
    );
    emulator
        .provisioned_connection(region, &text(&body["connection"], "id"))
        .await
}

/// The `ConnectionState` `region`'s endpoint reports for `identifier` now.
async fn state_now(emulator: &Emulator, region: &str, identifier: &str) -> String {
    let body = emulator
        .ic_ok_in(region, "GetConnection", json!({ "identifier": identifier }))
        .await;
    text(&body["connection"], "state")
}

/// The `arn`s of the connections `region`'s endpoint reports in the state the model
/// documents as fully provisioned ("available: The connection has been fully provisioned
/// between AWS and the Partner") — the set an ARN attach point names one of, read through
/// a DIFFERENT operation than the one under test.
async fn provisioned_arns(emulator: &Emulator, region: &str) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = Map::new();
        request.insert("state".to_string(), json!(CONNECTION_READY_STATE));
        request.insert("maxResults".to_string(), json!(100));
        if let Some(token) = &token {
            request.insert("nextToken".to_string(), json!(token));
        }
        let listed = emulator
            .ic_ok_in(region, "ListConnections", Value::Object(request))
            .await;
        all.extend(
            listed["connections"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|summary| text(summary, "arn")),
        );
        token = listed["nextToken"].as_str().map(str::to_string);
        if token.is_none() {
            return all;
        }
    }
}

/// Assert `(status, body)` is the modelled absent-resource refusal this service answers
/// an attach point that
/// resolves to nothing with, worded for `identifier` on `environment_id`.
fn assert_not_an_attach_point(
    case: &str,
    identifier: &str,
    environment_id: &str,
    status: u16,
    body: &Value,
) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for an attach point the listing does not offer, got {status} with {body:?}"
    );
    assert!(
        code(body).contains("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        code(body)
    );
    assert_eq!(
        message(body),
        attach_point_not_valid_message(identifier, environment_id),
        "case {case:?}: expected this service's own wording for an attach point that resolves to nothing, got {:?}",
        message(body)
    );
}

/// The descriptors of `listed` whose `identifier` is `identifier`.
fn offered(listed: &[Value], identifier: &str) -> Vec<Value> {
    listed
        .iter()
        .filter(|descriptor| text(descriptor, "identifier") == identifier)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// The ARN form of attach point
// ---------------------------------------------------------------------------

/// RULE: a Connection of the called endpoint is one of the Environment's attach points
/// exactly while it is providing connectivity — reported as a descriptor whose `type` is
/// the documented ARN form, whose `identifier` is the connection's full ARN and whose
/// `name` is the connection's own description — and a create onto that descriptor is
/// accepted and reports it back, while one onto a connection the listing no longer offers
/// is refused.
#[tokio::test]
async fn a_connection_is_an_arn_attach_point_exactly_while_it_provides_connectivity() {
    let emulator = Emulator::new();
    let gateway = emulator.attach_point().await;
    let catalog = emulator.environments().await;
    let environment = creatable_environments(&catalog)
        .first()
        .cloned()
        .expect("the seeded catalog offers an environment that carries connections");
    let environment_id = text(&environment, "environmentId");
    let bandwidths = available_bandwidths(&environment);
    assert!(
        !bandwidths.is_empty(),
        "setup: expected the environment to offer a bandwidth, got {environment:?}"
    );

    // Rows differ in the `description` the create carried — absent, a spaced one, and the
    // longest `ConnectionDescription` allows (`@length(min: 1, max: 255)`, pattern
    // `^[-a-zA-Z0-9_ ]+$`) — and in the bandwidth the connection was provisioned at.
    let descriptions: [Option<String>; 3] = [
        None,
        Some("an arn attach point".to_string()),
        Some("d".repeat(255)),
    ];
    for (index, description) in descriptions.iter().enumerate() {
        let bandwidth = &bandwidths[index % bandwidths.len()];
        let case = format!("{bandwidth} / description {:?}", description.as_deref());
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            description.as_deref(),
            &json!({ "directConnectGateway": gateway }),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {case:?}: expected the connection the ARN will name to be created, got {status} with {body:?}"
        );
        let created = body["connection"].clone();
        let arn = text(&created, "arn");
        let identifier = text(&created, "id");

        // A connection that is not providing connectivity yet is no attach point, so the
        // listing does not offer it. The state is read on BOTH sides of the listing, so a
        // row the clock carried across the call is not mistaken for one that was still
        // being brought up; the clock only ever moves a state forward.
        let before = state_now(&emulator, DEFAULT_SIGNING_REGION, &identifier).await;
        let listed = emulator.attach_points(&environment_id).await;
        let after = state_now(&emulator, DEFAULT_SIGNING_REGION, &identifier).await;
        if before == after && before != CONNECTION_READY_STATE {
            assert!(
                !identifiers(&listed).contains(&arn),
                "case {case:?}: expected a {before} connection not to be an attach point yet, got {listed:?}"
            );
        }

        // Once the clock has carried it to the fully provisioned state, it IS one.
        let provisioned = emulator
            .provisioned_connection(DEFAULT_SIGNING_REGION, &identifier)
            .await;
        let listed = emulator.attach_points(&environment_id).await;
        let descriptors = offered(&listed, &arn);
        assert_eq!(
            descriptors.len(),
            1,
            "case {case:?}: expected exactly one descriptor for the provisioned connection, got {listed:?}"
        );
        let descriptor = &descriptors[0];
        assert_eq!(
            text(descriptor, "type"),
            ATTACH_POINT_TYPE_ARN,
            "case {case:?}: expected the documented ARN attach-point type, got {:?}",
            descriptor["type"]
        );
        assert_eq!(
            text(descriptor, "name"),
            text(&provisioned, "description"),
            "case {case:?}: expected the connection's own description as the descriptive name, got {:?}",
            descriptor["name"]
        );
        match description {
            // A create that carried no description leaves the record describing the
            // connection by its id, so the required `name` is never empty.
            None => assert_eq!(
                text(descriptor, "name"),
                identifier,
                "case {case:?}: expected a connection with no description of its own to be named by its id, got {:?}",
                descriptor["name"]
            ),
            Some(sent) => assert_eq!(
                text(descriptor, "name"),
                *sent,
                "case {case:?}: expected the description the create carried, got {:?}",
                descriptor["name"]
            ),
        }
        // The gateway form is still offered beside it: the page is the union of the two.
        assert!(
            identifiers(&listed).contains(&gateway),
            "case {case:?}: expected the account's gateway to be offered beside the ARN, got {listed:?}"
        );

        // The descriptor is offered back to a create in the member its `type` names, and
        // the connection reports that attach point through its own reader.
        let attach_point = attach_point_of(descriptor);
        assert_eq!(
            attach_point,
            json!({ "arn": arn }),
            "case {case:?}: expected the ARN member for an ARN descriptor, got {attach_point:?}"
        );
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            Some("onto a listed arn"),
            &attach_point,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a listed ARN attach point to be accepted, got {status} with {body:?}"
        );
        let fetched = emulator
            .ic_ok(
                "GetConnection",
                json!({ "identifier": text(&body["connection"], "id") }),
            )
            .await;
        assert_eq!(
            fetched["connection"]["attachPoint"], attach_point,
            "case {case:?}: expected GetConnection to report the listed attach point, got {:?}",
            fetched["connection"]["attachPoint"]
        );

        // And the handle stops naming an attach point when the connectivity behind it
        // goes: a connection a `DeleteConnection` has been made on is offered no more,
        // and a create onto it is refused in the one wording this service words such a
        // refusal in.
        let deleted = emulator
            .ic_ok("DeleteConnection", json!({ "identifier": identifier }))
            .await;
        assert_ne!(
            text(&deleted["connection"], "state"),
            CONNECTION_READY_STATE,
            "setup {case:?}: expected the delete to take the connection out of the provisioned state, got {:?}",
            deleted["connection"]["state"]
        );
        let listed = emulator.attach_points(&environment_id).await;
        assert!(
            !identifiers(&listed).contains(&arn),
            "case {case:?}: expected a connection being torn down to be offered no more, got {listed:?}"
        );
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            None,
            &json!({ "arn": arn }),
        )
        .await;
        assert_not_an_attach_point(&case, &arn, &environment_id, status, &body);
    }
}

// ---------------------------------------------------------------------------
// The listing and the refusals are one judgement
// ---------------------------------------------------------------------------

/// RULE: the attach points `ListAttachPoints` offers an Environment are exactly the ones
/// a create onto it accepts — every descriptor on the page is accepted in the member its
/// `type` names, and a well-formed identifier of either form that the page does not offer
/// is refused with the modelled `ResourceNotFoundException`.
#[tokio::test]
async fn the_listing_offers_exactly_the_attach_points_a_create_accepts() {
    let emulator = Emulator::new();
    let gateways: BTreeSet<String> = emulator
        .ready_gateways(&GATEWAY_ROWS)
        .await
        .into_iter()
        .collect();
    let environment = creatable_environments(&emulator.environments().await)
        .first()
        .cloned()
        .expect("the seeded catalog offers an environment that carries connections");
    let environment_id = text(&environment, "environmentId");
    let bandwidths = available_bandwidths(&environment);

    // One provisioned connection, so the page spans BOTH forms of attach point.
    let subject = provisioned_on_gateway(
        &emulator,
        DEFAULT_SIGNING_REGION,
        &environment,
        &bandwidths[0],
        gateways.iter().next().expect("a ready gateway"),
    )
    .await;
    let subject_arn = text(&subject, "arn");

    // The page is the union of the two account-side sets, each read through an operation
    // OTHER than the one under test: the gateways through Direct Connect (above) and the
    // provisioned connections through `ListConnections`. That set is read on both sides
    // of the listing, because the clock carries a connection into the provisioned state
    // on its own, and the comparison is made on a reading the clock did not move across —
    // looked for again until it holds rather than waited out for a fixed time.
    let started = Instant::now();
    let (listed, ready) = loop {
        let ready_before = provisioned_arns(&emulator, DEFAULT_SIGNING_REGION).await;
        let listed = emulator.attach_points(&environment_id).await;
        let ready_after = provisioned_arns(&emulator, DEFAULT_SIGNING_REGION).await;
        if ready_before == ready_after {
            break (listed, ready_before);
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "setup: expected a reading the clock did not move across within {POLL_BUDGET:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        identifiers(&of_type(&listed, ATTACH_POINT_TYPE_ARN)),
        ready,
        "case \"arn form\": expected the ARN attach points to be the account's provisioned connections, got {listed:?}"
    );
    assert_eq!(
        identifiers(&of_type(&listed, ATTACH_POINT_TYPE)),
        gateways,
        "case \"gateway form\": expected the gateway attach points to be the account's ready gateways, got {listed:?}"
    );
    assert!(
        identifiers(&listed).contains(&subject_arn),
        "case \"subject\": expected the provisioned connection among the attach points, got {listed:?}"
    );

    // Every descriptor the page offers is accepted, in the member its own `type` names.
    for (index, descriptor) in listed.iter().enumerate() {
        let attach_point = attach_point_of(descriptor);
        let case = format!(
            "{} / {}",
            text(descriptor, "type"),
            text(descriptor, "identifier")
        );
        let bandwidth = &bandwidths[index % bandwidths.len()];
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            Some("onto a listed attach point"),
            &attach_point,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a listed attach point to be accepted, got {status} with {body:?}"
        );
        assert_eq!(
            body["connection"]["attachPoint"], attach_point,
            "case {case:?}: expected the created connection to carry the listed attach point, got {:?}",
            body["connection"]["attachPoint"]
        );
    }

    // And nothing the page does not offer is: the rows differ in the FORM of the
    // identifier and in why it is not an attach point — a gateway the account does not
    // hold, a connection ARN this endpoint never allocated, the ARN of a connection a
    // delete has been made on, and the ARN of a provisioned connection that belongs to
    // another Region's endpoint.
    let torn_down = provisioned_on_gateway(
        &emulator,
        DEFAULT_SIGNING_REGION,
        &environment,
        &bandwidths[0],
        gateways.iter().next().expect("a ready gateway"),
    )
    .await;
    emulator
        .ic_ok(
            "DeleteConnection",
            json!({ "identifier": text(&torn_down, "id") }),
        )
        .await;

    let other_environment = creatable_environments(&emulator.environments_in(OTHER_REGION).await)
        .first()
        .cloned()
        .expect("the other Region's catalog offers an environment that carries connections");
    let other_bandwidths = available_bandwidths(&other_environment);
    let elsewhere = provisioned_on_gateway(
        &emulator,
        OTHER_REGION,
        &other_environment,
        &other_bandwidths[0],
        gateways.iter().next().expect("a ready gateway"),
    )
    .await;
    let elsewhere_arn = text(&elsewhere, "arn");

    let refused: Vec<(&str, Value, String)> = vec![
        (
            "a gateway the account does not hold",
            json!({ "directConnectGateway": UNOWNED_GATEWAY_UUID }),
            UNOWNED_GATEWAY_UUID.to_string(),
        ),
        (
            "a connection ARN this endpoint never allocated",
            json!({ "arn": format!("arn:aws:interconnect:{DEFAULT_SIGNING_REGION}:{}:connection/mcc-00000000", engine::EMULATOR_ACCOUNT_ID) }),
            format!(
                "arn:aws:interconnect:{DEFAULT_SIGNING_REGION}:{}:connection/mcc-00000000",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
        (
            "a connection a delete has been made on",
            json!({ "arn": text(&torn_down, "arn") }),
            text(&torn_down, "arn"),
        ),
        (
            "a provisioned connection of another Region's endpoint",
            json!({ "arn": elsewhere_arn.clone() }),
            elsewhere_arn.clone(),
        ),
    ];
    for (case, attach_point, identifier) in refused {
        let listed = emulator.attach_points(&environment_id).await;
        assert!(
            !identifiers(&listed).contains(&identifier),
            "case {case:?}: expected the listing not to offer this attach point, got {listed:?}"
        );
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            &bandwidths[0],
            None,
            &attach_point,
        )
        .await;
        assert_not_an_attach_point(case, &identifier, &environment_id, status, &body);
    }

    // The other Region's own endpoint offers — and accepts — that same connection, so an
    // ARN attach point is scoped to the endpoint the connection belongs to rather than
    // withheld everywhere.
    let other_environment_id = text(&other_environment, "environmentId");
    let listed_elsewhere = emulator
        .attach_points_in(OTHER_REGION, &other_environment_id)
        .await;
    assert!(
        identifiers(&listed_elsewhere).contains(&elsewhere_arn),
        "case \"other region\": expected the connection to be an attach point on its own endpoint, got {listed_elsewhere:?}"
    );
    let (status, body) = create_in(
        &emulator,
        OTHER_REGION,
        &other_environment,
        &other_bandwidths[0],
        None,
        &json!({ "arn": elsewhere_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"other region\": expected the connection's own endpoint to accept its ARN, got {status} with {body:?}"
    );
}

// ---------------------------------------------------------------------------
// An acceptance is held to the same listing
// ---------------------------------------------------------------------------

/// RULE: the attach point an `AcceptConnectionProposal` selects is judged against the
/// same listing a create is, so an ARN the Environment's page offers is accepted there
/// too — the two writers of a connection's attach point cannot disagree with the reader.
#[tokio::test]
async fn an_acceptance_accepts_the_arn_attach_points_the_listing_offers() {
    let emulator = Emulator::new();
    let gateway = emulator.attach_point().await;
    let environment = creatable_environments(&emulator.environments().await)
        .first()
        .cloned()
        .expect("the seeded catalog offers an environment that carries connections");
    let environment_id = text(&environment, "environmentId");
    let bandwidths = available_bandwidths(&environment);

    // Rows differ in the bandwidth the proposal was raised at, so the acceptance is
    // judged on more than one otherwise-valid proposal.
    for bandwidth in [
        bandwidths.first().expect("a bandwidth"),
        bandwidths.last().expect("a bandwidth"),
    ] {
        let subject = provisioned_on_gateway(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            &gateway,
        )
        .await;
        let arn = text(&subject, "arn");
        let listed = emulator.attach_points(&environment_id).await;
        let descriptors = offered(&listed, &arn);
        assert_eq!(
            descriptors.len(),
            1,
            "setup {bandwidth:?}: expected the provisioned connection to be offered, got {listed:?}"
        );

        // One proposal per acceptance, raised right before it: a proposal is awaiting
        // acceptance only until somebody accepts it.
        let (status, body) = create_in(
            &emulator,
            DEFAULT_SIGNING_REGION,
            &environment,
            bandwidth,
            Some("a proposal to accept"),
            &json!({ "directConnectGateway": gateway }),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {bandwidth:?}: expected the proposal to be raised, got {status} with {body:?}"
        );
        let proposal = body["connection"].clone();
        let attach_point = attach_point_of(&descriptors[0]);
        let accepted = emulator
            .ic_ok(
                "AcceptConnectionProposal",
                json!({
                    "activationKey": text(&proposal, "activationKey"),
                    "attachPoint": attach_point,
                }),
            )
            .await;
        assert_eq!(
            accepted["connection"]["attachPoint"], attach_point,
            "case {bandwidth:?}: expected the acceptance to associate the listed attach point, got {:?}",
            accepted["connection"]["attachPoint"]
        );

        // The listing is unchanged by the acceptance having read it: the attach point is
        // still offered, and the proposal is not one itself until it is provisioned.
        let started = Instant::now();
        loop {
            let again = emulator.attach_points(&environment_id).await;
            assert!(
                identifiers(&again).contains(&arn),
                "case {bandwidth:?}: expected the attach point still to be offered, got {again:?}"
            );
            if state_now(
                &emulator,
                DEFAULT_SIGNING_REGION,
                &text(&proposal, "id"),
            )
            .await
                == CONNECTION_READY_STATE
            {
                break;
            }
            assert!(
                started.elapsed() < POLL_BUDGET,
                "case {bandwidth:?}: expected the accepted proposal to be provisioned within {POLL_BUDGET:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // And once the accepted connection is provisioned itself, it is an attach point
        // too: the page follows the records rather than a catalog of its own.
        let after = emulator.attach_points(&environment_id).await;
        assert!(
            identifiers(&after).contains(&text(&proposal, "arn")),
            "case {bandwidth:?}: expected the provisioned proposal to be offered as an attach point, got {after:?}"
        );
    }
}
