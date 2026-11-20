// SPDX-License-Identifier: CC0-1.0

//! The feed MEDIA data plane: the endpoint `dataEndpoints` advertises is one this
//! responder answers on, and the media path a client concatenates onto it is served.
//!
//! The rule under test: a URL this service hands a caller to send later requests to is
//! rooted at the endpoint this emulator is reached on, and that path space is mounted —
//! so a client that follows the documented flow (create the feed, read
//! `dataEndpoints[0]`, sign a metadata query to it) reaches this emulator and gets a
//! metadata document or a modelled client error, never a host nothing here answers on
//! and never an unimplemented-operation answer for the one address the control plane
//! told it to use.
//!
//! Every case drives the real request path, and every target is built from the endpoint
//! the service ITSELF advertised rather than from a path written here, so a change to the
//! advertised value that stopped naming the served path space would fail these tests.

use crate::harness::{call, call_in_region, message_of, output, router, REGION};
use serde_json::{json, Value};
use service_com_amazonaws_elementalinference_elementalinference::state::{
    cannot_find_output_message, NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter, UNSERVED_URL_BASE};

/// The error code the service's 404 refusal carries
/// (`smithy_query(query="shape",shape="ResourceNotFoundException")`: client error, HTTP
/// 404).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The error code its 400 refusal carries
/// (`smithy_query(query="shape",shape="ValidationException")`: client error, HTTP 400).
const VALIDATION: &str = "ValidationException";

/// The one media input a feed created with no explicit input exposes, addressed by its
/// POSITION in the feed's input set — the position the documented metadata path carries
/// (`/v1/feed/{id}/input/{n}/metadata`).
const FIRST_INPUT: &str = "0";

/// A feed with one output of each feature the `OutputConfig` union models
/// (`smithy_query(query="shape",shape="OutputConfig")`), so a rule about an OUTPUT is
/// asserted on every kind of output a feed can hold.
fn feed_body(name: &str) -> Value {
    json!({
        "name": name,
        "outputs": [
            output("crop1", "cropping", "ENABLED"),
            output("clip1", "clipping", "ENABLED"),
            output("sub1", "subtitling", "ENABLED"),
            output("meta1", "contextualMetadata", "DISABLED"),
        ]
    })
}

/// The names of the outputs [`feed_body`] gives a feed.
const OUTPUT_NAMES: [&str; 4] = ["crop1", "clip1", "sub1", "meta1"];

/// Create one feed and return its id together with the data endpoint it advertises.
async fn feed_with_endpoint(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, created) = call(router, "POST", "/v1/feed", Some(feed_body(name))).await;
    assert_eq!(
        status, 202,
        "CreateFeed({name:?}) is accepted, got {status} with {created}"
    );
    let id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the created feed carries an id, got {created}"))
        .to_string();
    let endpoint = created["dataEndpoints"][0]
        .as_str()
        .unwrap_or_else(|| panic!("the created feed advertises an endpoint, got {created}"))
        .to_string();
    (id, endpoint)
}

/// The documented metadata path of one input of `feed_id`.
fn metadata_path(feed_id: &str, input: &str) -> String {
    format!("/v1/feed/{feed_id}/input/{input}/metadata")
}

/// The request target a client that CONCATENATES the metadata path onto the advertised
/// endpoint sends.
///
/// The endpoint ends in the slash the documented value ends in, so the concatenation
/// DOUBLES the separator — the spelling a client following the flow really produces.
/// Only the endpoint's own path reaches the wire here, because a suite drives the service
/// in process rather than over a socket; what matters is that the path the client builds
/// from the advertised value is the path this plane serves.
fn concatenated_target(endpoint: &str, metadata_path: &str) -> String {
    let endpoint_path = endpoint
        .split_once("://")
        .map_or(endpoint, |(_scheme, authority)| {
            authority
                .find('/')
                .map_or("/", |start| &authority[start..])
        });
    format!("{endpoint_path}{metadata_path}")
}

/// A metadata query for `output_name` over one window of the media timeline.
fn query(output_name: &str, start_pts: i64, end_pts: i64, timescale: i64) -> Value {
    json!({
        "outputName": output_name,
        "timeSpecification": {
            "ptsBased": {"startPts": start_pts, "endPts": end_pts, "timescale": timescale}
        }
    })
}

/// A query the recorded flow sends: the first second of the timeline at a 1 kHz
/// timescale.
fn default_query(output_name: &str) -> Value {
    query(output_name, 0, 1001, 1000)
}

/// RULE: the endpoint a feed advertises is DERIVED from the base this responder is reached
/// on, so whatever address this emulator answers on is the authority the create, the read
/// and the update all publish — and the media path a client concatenates onto it is served
/// under every one of them.
///
/// The rows differ along what the rule generalises over: the FORM the base takes (a named
/// host with and without a port, a loopback address and port as a served data port reports
/// it, an IPv6 literal, and the base an unserved router reports), which is the only thing
/// the advertised value may depend on. A host written into this crate would answer every
/// row alike, which is what the final assertion refuses.
#[tokio::test]
async fn the_advertised_endpoint_follows_the_base_this_emulator_is_reached_on() {
    // Each base is a bare `https` origin, the only form an endpoint base takes
    // (`smithy_server_runtime::EndpointUrl::parse`).
    let bases: [(&str, &str); 5] = [
        ("the base an unserved router reports", UNSERVED_URL_BASE),
        ("a loopback address and a data port", "https://127.0.0.1:9297"),
        ("an IPv6 loopback literal and a port", "https://[::1]:9297"),
        ("a named host with no port", "https://emulator.test"),
        (
            "a named host and a non-default port",
            "https://my-emulator.example:8443",
        ),
    ];

    let mut advertised: Vec<String> = Vec::new();
    for (label, base) in bases {
        let mut router = router();
        router.set_url_base(
            EndpointUrl::parse(base)
                .unwrap_or_else(|| panic!("case {label:?}: {base:?} is an endpoint base")),
        );
        let name = "endpoint-base-feed";
        let (id, endpoint) = feed_with_endpoint(&router, name).await;
        // The root path of THIS responder, ending in the slash a client concatenates a
        // media path onto.
        let expected = format!("{base}/");
        assert_eq!(
            endpoint, expected,
            "case {label:?}: CreateFeed advertises the base this emulator is reached on, \
             expected {expected:?}, got {endpoint:?}"
        );
        assert!(
            !endpoint.contains("amazonaws.com") && !endpoint.contains("api.aws"),
            "case {label:?}: the endpoint names no host of AWS's own, got {endpoint:?}"
        );

        let (status, fetched) = call(&router, "GET", &format!("/v1/feed/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {label:?}: GetFeed answers 200, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["dataEndpoints"],
            json!([expected]),
            "case {label:?}: GetFeed reports the same endpoint, got {:?}",
            fetched["dataEndpoints"]
        );

        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/v1/feed/{id}"),
            Some(feed_body(name)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: UpdateFeed answers 200, got {status} with {updated}"
        );
        assert_eq!(
            updated["dataEndpoints"],
            json!([expected]),
            "case {label:?}: UpdateFeed echoes the same endpoint, got {:?}",
            updated["dataEndpoints"]
        );

        // Reachable, not merely well-formed: the path a client builds from the value it was
        // just handed is served by this very responder under this base too.
        let target = concatenated_target(&endpoint, &metadata_path(&id, FIRST_INPUT));
        let (status, answered) = call(&router, "POST", &target, Some(default_query("sub1"))).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the advertised endpoint serves the query at {target:?}, got \
             {status} with {answered}"
        );
        assert!(
            answered["items"].is_array(),
            "case {label:?}: the metadata document carries an items list, got {answered}"
        );

        advertised.push(endpoint);
    }

    // Derived from where this process answers, not written into the crate: no two bases
    // are advertised as the same endpoint.
    let distinct: std::collections::BTreeSet<&String> = advertised.iter().collect();
    assert_eq!(
        distinct.len(),
        advertised.len(),
        "each base is advertised as its own endpoint, got {advertised:?}"
    );
}

/// RULE: the endpoint is reported from the base in force WHEN the feed is read, not
/// remembered from the create — the address a feed's media plane answers at is a property of
/// where this process serves, which is why a feed created while this emulator was reached on
/// one base is read back on the base it is reached on now.
///
/// The rows differ along the move the base makes while the feed stands: from the unserved base
/// to a served one, from a served one to another, and back again.
#[tokio::test]
async fn a_read_reports_the_base_in_force_when_it_is_read() {
    let mut router = router();
    let name = "endpoint-base-moves";
    let (id, created) = {
        let (status, created) = call(&router, "POST", "/v1/feed", Some(feed_body(name))).await;
        assert_eq!(
            status, 202,
            "CreateFeed is accepted, got {status} with {created}"
        );
        let id = created["id"]
            .as_str()
            .unwrap_or_else(|| panic!("the created feed carries an id, got {created}"))
            .to_string();
        (id, created)
    };
    assert_eq!(
        created["dataEndpoints"],
        json!([format!("{UNSERVED_URL_BASE}/")]),
        "the create advertises the base in force at the create, got {:?}",
        created["dataEndpoints"]
    );

    for (label, base) in [
        ("served on a data port", "https://127.0.0.1:9297"),
        ("served on another address", "https://emulator.test:8443"),
        ("not served again", UNSERVED_URL_BASE),
    ] {
        router.set_url_base(
            EndpointUrl::parse(base)
                .unwrap_or_else(|| panic!("case {label:?}: {base:?} is an endpoint base")),
        );
        let expected = json!([format!("{base}/")]);
        let (status, fetched) = call(&router, "GET", &format!("/v1/feed/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {label:?}: GetFeed answers 200, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["dataEndpoints"], expected,
            "case {label:?}: GetFeed reports the base in force now ({base}), got {:?}",
            fetched["dataEndpoints"]
        );
        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/v1/feed/{id}"),
            Some(feed_body(name)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: UpdateFeed answers 200, got {status} with {updated}"
        );
        assert_eq!(
            updated["dataEndpoints"], expected,
            "case {label:?}: UpdateFeed reports the base in force now ({base}), got {:?}",
            updated["dataEndpoints"]
        );
    }
}

/// RULE: the endpoint the feed advertises is SERVED — a metadata query sent to the path a
/// client builds from it answers a metadata document, for every output of the feed and
/// every window of the timeline.
#[tokio::test]
async fn the_advertised_endpoint_serves_a_metadata_query() {
    let router = router();
    let (id, endpoint) = feed_with_endpoint(&router, "media-plane-feed").await;
    let path = metadata_path(&id, FIRST_INPUT);

    // The rows span what the rule generalises over: which output of the feed the query is
    // about (one of each feature), the window asked for (an empty span, the recorded
    // one-second span, a span far down the timeline, and another timescale), and how the
    // client spelled the target — concatenated onto the advertised endpoint, which
    // doubles the separator, or written with a single one.
    let mut rows: Vec<(String, String, Value)> = Vec::new();
    for name in OUTPUT_NAMES {
        rows.push((
            format!("the {name} output, the recorded window, concatenated"),
            concatenated_target(&endpoint, &path),
            default_query(name),
        ));
    }
    let windows: [(&str, i64, i64, i64); 4] = [
        ("an empty span", 0, 0, 1000),
        ("the recorded one-second span", 0, 1001, 1000),
        ("a span far down the timeline", 90_000_000, 90_090_000, 90_000),
        ("a 48 kHz timescale", 480, 48_000, 48_000),
    ];
    for (label, start, end, timescale) in windows {
        rows.push((
            format!("{label}, single separator"),
            path.clone(),
            query("sub1", start, end, timescale),
        ));
    }

    for (label, target, body) in rows {
        let (status, answered) = call(&router, "POST", &target, Some(body)).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the advertised endpoint serves the query at {target:?}, got \
             {status} with {answered}"
        );
        assert!(
            answered["items"].is_array(),
            "case {label:?}: the metadata document carries an items list, got {answered}"
        );
    }
}

/// One row of the refusal table: what the client sent, and what the service answers.
struct RefusalCase {
    label: &'static str,
    /// The metadata path the query is sent to.
    target: String,
    /// The query body, or `None` for a request that carries no body at all.
    body: Option<Value>,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE: the metadata query is validated against the request and against STORED feed
/// state, and what it cannot answer is a modelled CLIENT error of this service — never a
/// 5xx and never an unimplemented-operation answer.
#[tokio::test]
async fn a_metadata_query_the_feed_cannot_answer_is_a_modelled_client_error() {
    let router = router();
    let (id, endpoint) = feed_with_endpoint(&router, "media-plane-refusals").await;
    let path = metadata_path(&id, FIRST_INPUT);
    let refusal = |label: &'static str,
                   target: String,
                   body: Option<Value>,
                   status: u16,
                   code: &'static str,
                   message: String| RefusalCase {
        label,
        target,
        body,
        status,
        code,
        message,
    };

    // The rows span what the rule generalises over: which half of the address resolves to
    // nothing (the feed id, the input position), which member of the query is unusable
    // (the output name, the time specification, each member of the window), and the form
    // the body itself takes.
    let rows: Vec<RefusalCase> = vec![
        refusal(
            "a feed id this Region never minted",
            metadata_path("aaaaaaaaaaaaaaaaaaa", FIRST_INPUT),
            Some(default_query("sub1")),
            404,
            RESOURCE_NOT_FOUND,
            NOT_FOUND_MESSAGE.to_string(),
        ),
        refusal(
            "an input position past the feed's input set",
            metadata_path(&id, "1"),
            Some(default_query("sub1")),
            404,
            RESOURCE_NOT_FOUND,
            NOT_FOUND_MESSAGE.to_string(),
        ),
        refusal(
            "an input the path does not spell as a position",
            metadata_path(&id, "default-audio"),
            Some(default_query("sub1")),
            404,
            RESOURCE_NOT_FOUND,
            NOT_FOUND_MESSAGE.to_string(),
        ),
        refusal(
            "an output name the feed holds no output under",
            path.clone(),
            Some(default_query("absent-output")),
            400,
            VALIDATION,
            cannot_find_output_message("absent-output"),
        ),
        refusal(
            "no output name at all",
            path.clone(),
            Some(json!({
                "timeSpecification": {
                    "ptsBased": {"startPts": 0, "endPts": 1001, "timescale": 1000}
                }
            })),
            400,
            VALIDATION,
            "outputName must be the name of an output of the feed.".to_string(),
        ),
        refusal(
            "an empty output name",
            path.clone(),
            Some(default_query("")),
            400,
            VALIDATION,
            "outputName must be the name of an output of the feed.".to_string(),
        ),
        refusal(
            // The published bound itself: a name of exactly the modelled maximum is a
            // name a feed COULD hold, so it is refused by the lookup and not by the
            // member's own shape.
            "an output name at the modelled maximum length that the feed does not hold",
            path.clone(),
            Some(default_query(&"a".repeat(128))),
            400,
            VALIDATION,
            cannot_find_output_message(&"a".repeat(128)),
        ),
        refusal(
            // `GetOutput$name` targets the modelled `ResourceName`
            // (`smithy_query(query="shape",shape="GetOutput")`), so a name no output can
            // be held under is refused on the member rather than echoed back.
            "an output name longer than an output's name may be",
            path.clone(),
            Some(default_query(&"a".repeat(129))),
            400,
            VALIDATION,
            "outputName must be the name of an output of the feed.".to_string(),
        ),
        refusal(
            "an output name carrying a character the modelled name admits nowhere",
            path.clone(),
            Some(default_query("sub 1")),
            400,
            VALIDATION,
            "outputName must be the name of an output of the feed.".to_string(),
        ),
        refusal(
            "an output name that does not end in an alphanumeric character",
            path.clone(),
            Some(default_query("sub1-")),
            400,
            VALIDATION,
            "outputName must be the name of an output of the feed.".to_string(),
        ),
        refusal(
            "a body that is not a JSON object",
            path.clone(),
            Some(json!(["sub1"])),
            400,
            VALIDATION,
            "The request body must be a JSON object.".to_string(),
        ),
        refusal(
            "no time specification",
            path.clone(),
            Some(json!({"outputName": "sub1"})),
            400,
            VALIDATION,
            "timeSpecification must name ptsBased: this endpoint reads only \
             presentation-timestamp windows."
                .to_string(),
        ),
        refusal(
            "a time specification naming a window this endpoint does not read",
            path.clone(),
            Some(json!({
                "outputName": "sub1",
                "timeSpecification": {"wallClockBased": {"start": 0, "end": 1}}
            })),
            400,
            VALIDATION,
            "timeSpecification must name ptsBased: this endpoint reads only \
             presentation-timestamp windows."
                .to_string(),
        ),
        refusal(
            "a time specification naming two kinds of window",
            path.clone(),
            Some(json!({
                "outputName": "sub1",
                "timeSpecification": {
                    "ptsBased": {"startPts": 0, "endPts": 1001, "timescale": 1000},
                    "wallClockBased": {"start": 0, "end": 1}
                }
            })),
            400,
            VALIDATION,
            "timeSpecification names one kind of window, so it must carry ptsBased alone."
                .to_string(),
        ),
        refusal(
            "a start past the end",
            path.clone(),
            Some(query("sub1", 1002, 1001, 1000)),
            400,
            VALIDATION,
            "ptsBased.startPts must not be greater than ptsBased.endPts.".to_string(),
        ),
        refusal(
            "a timescale of zero",
            path.clone(),
            Some(query("sub1", 0, 1001, 0)),
            400,
            VALIDATION,
            "ptsBased.timescale must be greater than 0.".to_string(),
        ),
        refusal(
            "a negative timescale",
            path.clone(),
            Some(query("sub1", 0, 1001, -1000)),
            400,
            VALIDATION,
            "ptsBased.timescale must be greater than 0.".to_string(),
        ),
        refusal(
            "a start that is not a whole number",
            path.clone(),
            Some(json!({
                "outputName": "sub1",
                "timeSpecification": {
                    "ptsBased": {"startPts": "0", "endPts": 1001, "timescale": 1000}
                }
            })),
            400,
            VALIDATION,
            "ptsBased.startPts must be a whole number.".to_string(),
        ),
        refusal(
            "an absent end",
            path.clone(),
            Some(json!({
                "outputName": "sub1",
                "timeSpecification": {"ptsBased": {"startPts": 0, "timescale": 1000}}
            })),
            400,
            VALIDATION,
            "ptsBased.endPts must be a whole number.".to_string(),
        ),
        refusal(
            "no body at all",
            path.clone(),
            None,
            400,
            VALIDATION,
            "The request body must be a JSON object.".to_string(),
        ),
    ];

    for case in rows {
        let label = case.label;
        // Sent through the concatenated spelling as well, so a refusal is the same
        // whichever way the client built the target from the advertised endpoint.
        for target in [
            case.target.clone(),
            concatenated_target(&endpoint, &case.target),
        ] {
            let (status, answered) = call(&router, "POST", &target, case.body.clone()).await;
            assert_eq!(
                status, case.status,
                "case {label:?}: expected HTTP {} at {target:?}, got {status} with {answered}",
                case.status
            );
            assert_eq!(
                answered["__type"],
                json!(case.code),
                "case {label:?}: expected the {} error code, got {answered}",
                case.code
            );
            assert_eq!(
                message_of(&answered),
                case.message,
                "case {label:?}: expected the message {:?}, got {answered}",
                case.message
            );
        }
    }
}

/// RULE: the metadata query resolves the feed id WITHIN the Region the request was signed
/// for, exactly as every control-plane read of a feed does — this service is regional, so
/// another Region's endpoint knows nothing of the feed.
#[tokio::test]
async fn a_metadata_query_resolves_the_feed_in_the_requests_own_region() {
    let router = router();
    let (id, _endpoint) = feed_with_endpoint(&router, "media-plane-region").await;
    let path = metadata_path(&id, FIRST_INPUT);

    // The rows span the Region dimension: the endpoint that minted the feed, and other
    // Regions' endpoints, which hold no feed under that id.
    let rows: [(&str, &str, u16); 3] = [
        ("the Region that minted the feed", REGION, 200),
        ("another Region in the same partition", "eu-west-1", 404),
        ("a Region on the other side of the world", "ap-south-1", 404),
    ];
    for (label, region, expected_status) in rows {
        let (status, answered) = call_in_region(
            &router,
            region,
            "POST",
            &path,
            Some(default_query("sub1")),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {label:?}: expected HTTP {expected_status} through {region:?}, got {status} \
             with {answered}"
        );
    }
}

/// RULE: the metadata query is a READ — it reports the metadata the feed holds and
/// changes nothing a later caller sees.
#[tokio::test]
async fn a_metadata_query_leaves_the_feed_as_it_found_it() {
    let router = router();
    let (id, _endpoint) = feed_with_endpoint(&router, "media-plane-read-only").await;
    let (status, before) = call(&router, "GET", &format!("/v1/feed/{id}"), None).await;
    assert_eq!(
        status, 200,
        "GetFeed answers 200 before the query, got {status} with {before}"
    );

    // The rows span the query's own outcomes: one the feed answers, and one it refuses —
    // neither may move the record.
    let rows: [(&str, Value); 2] = [
        ("a query the feed answers", default_query("sub1")),
        ("a query the feed refuses", default_query("absent-output")),
    ];
    for (label, body) in rows {
        let (_status, _answered) = call(
            &router,
            "POST",
            &metadata_path(&id, FIRST_INPUT),
            Some(body),
        )
        .await;
        let (status, after) = call(&router, "GET", &format!("/v1/feed/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {label:?}: GetFeed still answers 200, got {status} with {after}"
        );
        assert_eq!(
            after, before,
            "case {label:?}: the query reported metadata and changed nothing, got {after}"
        );
    }
    // The outputs the feed was created with are still every one of them, read back
    // through the implemented reader rather than inferred from the comparison above.
    let names = reported_output_names(&before);
    assert_eq!(
        names,
        OUTPUT_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<String>>(),
        "the feed still holds every output it was created with, got {names:?}"
    );
}

/// The names of the outputs a feed response reports, in the order it reports them.
fn reported_output_names(feed: &Value) -> Vec<String> {
    feed["outputs"]
        .as_array()
        .unwrap_or_else(|| panic!("a feed response carries an outputs list, got {feed}"))
        .iter()
        .map(|output| {
            output["name"]
                .as_str()
                .unwrap_or_else(|| panic!("an output carries a name, got {output}"))
                .to_string()
        })
        .collect()
}
