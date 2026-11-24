// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests that the address an HTTP api PUBLISHES is the address its
//! THROTTLED stage is served on — the two halves of one invoke, driven together.
//!
//! The rule, one level above the recorded case (an HTTP api whose `$default` stage states a
//! rate of one request a second and a burst of one, invoked at the value `CreateApi`
//! announced):
//!
//! > A value this control plane publishes as an api's invoke address is an address THIS
//! > emulator serves the api's data plane at ("the stage name is typically appended to this
//! > URI to form a complete path to a deployed API stage", `Api$ApiEndpoint`), so every
//! > request a client builds from it — that value, the serving stage where the stage is
//! > named, then the route path — is answered BY THAT API: by the route's integration for a
//! > request the serving stage's route settings admit, and by the gateway's own `429`
//! > `{"message":"Too Many Requests"}` for one those settings shed — never by something that
//! > does not serve the address.
//!
//! `http_api_published_invoke_address` holds the ADDRESS half of that rule (what is
//! announced, and that an unthrottled route answers on it) and `http_api_throttle_burst`
//! holds the THROTTLE's arithmetic for requests in flight together. Neither drives the
//! gateway's own refusal through the PUBLISHED address, which is what the recorded case
//! does: a client reads `ApiEndpoint`, builds the invoke URL of the `$default` stage from it,
//! and must be answered by the api whichever of the two outcomes it gets. A published address
//! nothing serves answers neither, so both outcomes arriving is what says the endpoint is
//! real.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane request was
//! addressed at (an IPv4 loopback literal, a loopback NAME, a bracketed IPv6 literal and one
//! of AWS's own service hosts), the Region it was signed for, the stage the api is deployed
//! to (the reserved `$default` stage that serves the api's root, and a named one), the route
//! key and the path within the stage that matches it (the api's root, a named path, a greedy
//! proxy route), the rate-and-burst pair the stage states and where it states it
//! (`DefaultRouteSettings` for every route, `RouteSettings` for this route key), and the form
//! the client builds the invoke URL in ([`Form`]: the endpoint on its own for the stage that
//! serves the api's root, and the stage name appended). The recorded case is ONE row.
//!
//! Nothing here asserts on elapsed time. A row whose rate refills a request's capacity in
//! fractions of a request a second ([`REFILLS_NOTHING_HERE`]) cannot have repaid what the
//! case drew while the case runs, so every request beyond its burst is shed; the row that
//! states the recorded pair repays one request a second and asks back to back in process, so
//! one of them is ([`AT_LEAST_ONE`]) and the clock keeps its room for the rest. Either way the
//! stage's settings and not the host's speed decide what the assertions see.
//!
//! What this suite does NOT restate: that `GetApi`, `GetApis`, `UpdateApi` and `ReimportApi`
//! all report the address `CreateApi` announced, which is
//! `http_api_published_endpoint_reporters`' and `http_api_published_invoke_address`' subject.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_at_host, emulator};
use crate::http_api_published_invoke_address::{announced_parts, expected_announced_parts};
use crate::websocket_handshake_data_plane::{send, Answer};

/// The status an `HTTP_PROXY` route answers a request its throttle ADMITTED with: the
/// upstream is a URI the caller can reach, so the caller is sent there — `307`, carrying that
/// integration's own `IntegrationUri`. Reaching it is what says the request was resolved all
/// the way to the route's integration.
const SERVED: u16 = 307;

/// The upstream every row's `HTTP_PROXY` integration fronts.
const UPSTREAM: &str = "https://upstream.example.com/orders";

/// The status and the sentence the gateway answers a request the serving stage's route
/// settings SHED with, and the content type that body is framed as.
const THROTTLED: u16 = 429;
const THROTTLED_MESSAGE: &str = "Too Many Requests";
const JSON: &str = "application/json";

/// The front door's envelope for a request no service claimed — the answer an address nothing
/// serves produces, which is the defect this suite pins and never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The reserved stage "that serves an api's root" (`$default`), reached with no stage segment
/// in the path at all as well as with its name spelled out.
const DEFAULT_STAGE: &str = "$default";

/// A `ThrottlingRateLimit` written in fractions of a request a second: it refills one
/// request's capacity in minutes, so a row metered at it has repaid nothing a case drew by the
/// time that case ends however slowly the host runs it, and what such a row is shed is the
/// stage's settings rather than the clock.
const REFILLS_NOTHING_HERE: f64 = 0.01;

/// How many requests each case sends beyond the burst the row states, so every case covers
/// both outcomes: the burst is served and what follows it meets a spent bucket.
const BEYOND_THE_BURST: usize = 2;

/// How many of the requests beyond the burst a row whose rate COULD refill while the case runs
/// is held to the refusal: one of them. The bucket such a row states repays one request a
/// second and the case asks back to back in process, so the refusal is what the stage's
/// settings produce — and holding only one of them to it leaves the clock room to have repaid
/// the others, whatever the host was doing.
const AT_LEAST_ONE: usize = 1;

/// One row: how the control-plane client is configured, what is deployed, and how the stage's
/// route settings meter the route.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane requests carry — the endpoint the client is configured
    /// with, which is the address the api is then announced on.
    host: &'static str,
    region: &'static str,
    /// Whether that client resolves nothing but the endpoint it reached.
    reaches_only_the_emulator: bool,
    stage_name: &'static str,
    route_key: &'static str,
    /// The path within the stage that matches that route key.
    route_path: &'static str,
    /// `ThrottlingRateLimit` and `ThrottlingBurstLimit`, as the stage states them.
    rate: f64,
    burst: i32,
    /// Whether the stage states the pair for THIS route key (`RouteSettings`) rather than for
    /// every route of the stage (`DefaultRouteSettings`) — both are the settings the route is
    /// metered by.
    stated_for_the_route: bool,
    /// The forms a client builds the invoke URL in for this row — every one of them an
    /// address of this stage's route.
    forms: &'static [Form],
}

/// How a client composes the invoke URL out of the announced endpoint, the serving stage and
/// the route path.
///
/// Both spellings are what `Api$ApiEndpoint` carries in one word — "the stage name is
/// TYPICALLY appended to this URI to form a complete path to a deployed API stage": the stage
/// that serves the api's ROOT is reached with no stage segment at all, and a named stage with
/// its name as the first segment. Which forms address a given route is the row's to say,
/// because the segments the target carries are what a route key is matched against: appending
/// the root stage's name ahead of the api's own root names a path of one more (empty) segment,
/// which is not the root route.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// The announced endpoint on its own, with the route path after it.
    EndpointItself,
    /// The announced endpoint, the serving stage's name, then the route path.
    StageAppended,
}

impl Form {
    /// What a failing assertion calls this form.
    fn named(self) -> &'static str {
        match self {
            Self::EndpointItself => "the endpoint on its own",
            Self::StageAppended => "the endpoint with the serving stage appended",
        }
    }

    /// The request path this form builds from the announced endpoint's `base` path.
    fn path(self, base: &str, row: &Row) -> String {
        match self {
            Self::EndpointItself => format!("{base}{}", row.route_path),
            Self::StageAppended => format!("{base}/{}{}", row.stage_name, row.route_path),
        }
    }
}

impl Row {
    /// How many of the requests beyond the burst are held to the gateway's own refusal.
    ///
    /// A row metered at [`REFILLS_NOTHING_HERE`] cannot have repaid anything the case drew, so
    /// EVERY request beyond its burst is shed; a row metered at the recorded rate repays one
    /// request a second, so one of them is ([`AT_LEAST_ONE`]) — either way the stage's
    /// settings, and not the host's speed, decide what the assertion sees.
    fn shed_beyond_the_burst(&self) -> usize {
        if self.rate <= REFILLS_NOTHING_HERE {
            BEYOND_THE_BURST
        } else {
            AT_LEAST_ONE
        }
    }

    /// How many requests the burst admits before the bucket the rate refills is spent.
    fn burst_admits(&self) -> usize {
        usize::try_from(self.burst).expect("the row's burst is a count")
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint, the root route of a $default stage metered at one request a \
                   second with a burst of one",
            host: "127.0.0.1:46433",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            stage_name: DEFAULT_STAGE,
            route_key: "GET /",
            route_path: "/",
            rate: 1.0,
            burst: 1,
            stated_for_the_route: false,
            forms: &[Form::EndpointItself],
        },
        Row {
            case: "the same endpoint named by its loopback NAME, a named path on a named \
                   stage whose settings state the pair for the route itself",
            host: "localhost:9297",
            region: "eu-west-1",
            reaches_only_the_emulator: true,
            stage_name: "prod",
            route_key: "GET /orders",
            route_path: "/orders",
            rate: REFILLS_NOTHING_HERE,
            burst: 1,
            stated_for_the_route: true,
            forms: &[Form::StageAppended],
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal, a greedy proxy route \
                   and a burst wider than one",
            host: "[::1]:9297",
            region: "ap-southeast-2",
            reaches_only_the_emulator: true,
            stage_name: DEFAULT_STAGE,
            route_key: "ANY /{proxy+}",
            route_path: "/orders/latest",
            rate: REFILLS_NOTHING_HERE,
            burst: 3,
            stated_for_the_route: false,
            forms: &[Form::EndpointItself, Form::StageAppended],
        },
        Row {
            case: "a client that resolves AWS's own hosts: the api's own documented endpoint, \
                   the root route of a $default stage",
            host: "apigateway.us-east-1.amazonaws.com",
            region: "us-east-1",
            reaches_only_the_emulator: false,
            stage_name: DEFAULT_STAGE,
            route_key: "GET /",
            route_path: "/",
            rate: REFILLS_NOTHING_HERE,
            burst: 1,
            stated_for_the_route: false,
            forms: &[Form::EndpointItself],
        },
    ]
}

/// RULE: every request a client builds from the published `ApiEndpoint` reaches the api's own
/// data plane — the burst the stage's route settings admit are served by the route's
/// integration, and what follows them is the gateway's own `429 {"message":"Too Many
/// Requests"}`.
#[tokio::test]
async fn the_published_invoke_address_serves_the_stage_its_settings_throttle() {
    for (index, row) in rows().into_iter().enumerate() {
        for form in row.forms {
            // One emulator per form, so each form meets a stage whose bucket is full.
            let emulator = emulator();
            let case = row.case;
            let (api_id, announced) =
                deploy(&emulator, &row, &format!("throttled-published-{index}")).await;
            let (authority, base_path) = assert_announced(&row, &api_id, &announced);
            let path = form.path(&base_path, &row);
            let form = form.named();

            let mut answers = Vec::new();
            for _ in 0..(row.burst_admits() + BEYOND_THE_BURST) {
                answers.push(send(&emulator, &authority, "GET", &path, &[]).await);
            }

            // Whatever each request was answered, it was answered BY THE API: the published
            // address is served, so neither the front door's "nothing claims this" envelope
            // nor any other status appears.
            for (sent, answer) in answers.iter().enumerate() {
                assert_ne!(
                    answer.error_type.as_deref(),
                    Some(UNKNOWN_SERVICE),
                    "case {case:?}: {form} ({authority}{path}) must be an address this \
                     emulator serves, but request {sent} was claimed by no service: {}",
                    answer.summary()
                );
                assert!(
                    answer.status == SERVED || answer.status == THROTTLED,
                    "case {case:?}: request {sent} to {form} ({authority}{path}) must be \
                     answered by the route's integration ({SERVED}) or shed by the stage's \
                     throttle ({THROTTLED}), got {}",
                    answer.summary()
                );
            }

            // The burst the settings state is served, and a served request is the
            // INTEGRATION's answer: the route the published address resolved to was reached.
            for (sent, answer) in answers.iter().take(row.burst_admits()).enumerate() {
                assert_eq!(
                    answer.status,
                    SERVED,
                    "case {case:?}: request {sent} of the burst to {form} \
                     ({authority}{path}) must be served, got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.location.as_deref(),
                    Some(UPSTREAM),
                    "case {case:?}: request {sent} to {form} must be sent to the upstream the \
                     integration names, got {}",
                    answer.summary()
                );
            }

            // And the requests beyond it meet the gateway's OWN refusal, in the envelope a
            // shed request carries, with the integration not reached for them.
            let beyond: Vec<&Answer> = answers.iter().skip(row.burst_admits()).collect();
            let shed: Vec<&&Answer> = beyond
                .iter()
                .filter(|answer| answer.status == THROTTLED)
                .collect();
            let statuses: Vec<u16> = beyond.iter().map(|answer| answer.status).collect();
            assert!(
                shed.len() >= row.shed_beyond_the_burst(),
                "case {case:?}: expected at least {} of the {} requests beyond the burst to \
                 {form} ({authority}{path}) shed by the stage's throttle, got {statuses:?}",
                row.shed_beyond_the_burst(),
                beyond.len()
            );
            for answer in shed {
                assert_eq!(
                    answer.body,
                    json!({ "message": THROTTLED_MESSAGE }),
                    "case {case:?}: a shed request to {form} must carry the throttling body, \
                     got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.content_type, JSON,
                    "case {case:?}: a shed request to {form} must be framed as {JSON}, got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.location.as_deref(),
                    None,
                    "case {case:?}: a shed request to {form} must not reach the route's \
                     integration, got {}",
                    answer.summary()
                );
            }
        }
    }
}

/// Assert `announced` is the address this emulator serves the api at — the reading
/// `http_api_published_invoke_address` owns, so the two suites cannot drift apart — and answer
/// the authority and base path a client builds its invoke URL from.
fn assert_announced(row: &Row, api_id: &str, announced: &str) -> (String, String) {
    assert!(
        announced.starts_with("https://"),
        "case {:?}: CreateApi must announce an https endpoint for an HTTP api, got \
         {announced:?}",
        row.case
    );
    let (authority, base_path) = announced_parts(announced);
    let (expected_authority, expected_base_path) =
        expected_announced_parts(row.reaches_only_the_emulator, row.host, row.region, api_id);
    assert_eq!(
        (authority.as_str(), base_path.as_str()),
        (expected_authority.as_str(), expected_base_path.as_str()),
        "case {:?}: CreateApi must announce the api on \
         {expected_authority}{expected_base_path}, got {announced:?}",
        row.case
    );
    (authority, base_path)
}

/// Deploy the row's api: an `HTTP_PROXY` integration, the route that fronts it, and an
/// auto-deploying stage whose route settings state the row's rate-and-burst pair. Answers the
/// `ApiId` and the `ApiEndpoint` `CreateApi` announced.
async fn deploy(emulator: &MultiServiceRouter, row: &Row, name: &str) -> (String, String) {
    let case = row.case;
    let call = |method: &'static str, uri: String, body: Option<Value>| async move {
        call_at_host(emulator, row.host, row.region, method, &uri, body).await
    };

    let (status, api) = call(
        "POST",
        "/v2/apis".to_string(),
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let announced = as_str(&api, "apiEndpoint");

    let (status, integration) = call(
        "POST",
        format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationMethod": "GET",
            "integrationUri": UPSTREAM,
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");

    let (status, route) = call(
        "POST",
        format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": row.route_key,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {status} {route}"
    );

    let settings = json!({
        "throttlingRateLimit": row.rate,
        "throttlingBurstLimit": row.burst
    });
    let mut stage = json!({ "stageName": row.stage_name, "autoDeploy": true });
    if row.stated_for_the_route {
        stage["routeSettings"] = json!({ row.route_key: settings });
    } else {
        stage["defaultRouteSettings"] = settings;
    }
    let (status, created) = call("POST", format!("/v2/apis/{api_id}/stages"), Some(stage)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {created}"
    );
    assert!(
        created.get("deploymentId").and_then(Value::as_str).is_some(),
        "case {case:?}: an auto-deploying stage must carry a deployment, got {created}"
    );

    (api_id, announced)
}
