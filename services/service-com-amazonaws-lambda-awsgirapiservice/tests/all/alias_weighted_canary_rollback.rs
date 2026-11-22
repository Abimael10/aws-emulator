// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a whole CANARY cycle through a weighted alias: shifting a share of an
//! alias's invocations to a second version, invoking it, and then rolling that shift back.
//!
//! The rule under test: an alias's `RoutingConfig` is a stored property of the alias that every
//! write replaces and every omission leaves alone ("Use the `RoutingConfig` parameter to specify
//! a second version and the percentage of invocation requests that it receives", `CreateAlias`;
//! "The routing configuration of the alias", `UpdateAlias`), and EVERY invocation of the alias
//! is resolved against the weights the alias holds at the moment that invocation arrives — so a
//! write that leaves the alias no weights pins every later invocation to the version the alias
//! names, one that moves the weights sends them to the version now weighted, and each
//! invocation is answered promptly from the version that ran (`200`, no `FunctionError`,
//! `X-Amz-Executed-Version` naming that version, and that version's own payload).
//!
//! The rows differ along what the rule generalises over: the weights the alias is created with
//! (including the top of `AliasRoutingConfiguration$AdditionalVersionWeights`' own range —
//! `smithy_query(query="shape", shape="AdditionalVersionWeights")`, whose `Weight` target is
//! `@range(min: 0.0, max: 1.0)`), and the FORM of the write applied to them: weights added to an
//! alias that had none, weights cleared with an empty map, weights cleared with a routing
//! configuration stating no map at all, weights moved to a third version, the alias promoted to
//! the version it was shifting to, and a write that states no routing configuration and must
//! therefore leave the stored one standing.
//!
//! The sibling suites pin the neighbouring rules: `alias_weighted_routing` that an invocation of
//! a split alias is drawn per invocation, `alias_weighted_invoke_durability` that a long round of
//! runs through one split stays clean, and `alias_update` the control-plane round trip of the
//! member. This one is about the alias's weights CHANGING under the data plane.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module every package below holds its handler in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The header `Invoke$ExecutedVersion` is bound to: "The version of the function that
/// executed."
const EXECUTED_VERSION_HEADER: &str = "x-amz-executed-version";

/// The header `Invoke$FunctionError` is bound to: "If present, indicates that an error
/// occurred during function execution."
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// "For the DryRun invocation type, the status code is 204" (`Invoke$StatusCode`).
const DRY_RUN_STATUS: u16 = 204;

/// The versions this suite publishes, in the order it publishes them: each one's handler
/// returns its own number, so the payload of an answer says which version's code ran.
const VERSIONS: &[&str] = &["1", "2", "3"];

/// How long a provisioning transition is waited out for before a row gives up.
///
/// Generous on purpose: the wait is for the CONDITION (the function reporting itself ready),
/// polled for rather than slept through, so this bounds only how long a wedged emulator is
/// given before the row fails, and a loaded host only changes how often it looks.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again while it waits for a condition to hold.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many invocations each leg of a TABLE row resolves.
///
/// Dry runs, so the figure can be large enough to make the resolution's own behaviour plain
/// rather than an accident: a split that is really consulted per invocation reaches both of its
/// versions well within this many draws, and a split that was supposed to be cleared and was not
/// shows a shifted draw with overwhelming probability (the chance a half split hides from this
/// many draws is under one in a thousand billion).
const DRAWS_PER_LEG: usize = 40;

/// How many invocations each leg of the RUN test performs.
///
/// Each one is a run of the interpreter rather than a resolution, so it is far smaller than
/// [`DRAWS_PER_LEG`]: what the runs establish is that the version an answer REPORTS is the
/// version whose code produced the payload, which one run of each version settles. That the
/// share is drawn per invocation is the table's business.
const RUNS_PER_LEG: usize = 6;

/// How many times an invocation is re-driven while the answer reports one of this PROCESS's own
/// host bounds instead of a run of the handler.
const HOST_STOP_ATTEMPTS: usize = 6;

/// The `Timeout` the function this suite creates carries, in seconds.
///
/// Its handlers return at once, so what this has to cover is not their work but this host's own
/// scheduling: the suites of this target run in parallel over the emulator's run slots, so a run
/// can sit mid-flight while another suite holds the CPU.
const FUNCTION_TIMEOUT_SECONDS: u32 = 25;

/// How far a reported weight may sit from the weight configured for it: a JSON round trip of a
/// double, not a tolerance on the behaviour.
const WEIGHT_EPSILON: f64 = 1e-9;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The version this answer reports as the one that ran.
    fn executed_version(&self) -> String {
        self.header(EXECUTED_VERSION_HEADER).unwrap_or_default()
    }

    /// The marker the handler returned, which is the version its own code belongs to.
    fn marker(&self) -> String {
        self.json()
            .get("ran")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request, with the headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// The handler a version reports itself from: each published version carries its own number, so
/// the marker an answer returns says which version's code ran.
fn source_reporting(version: &str) -> String {
    format!("def lambda_handler(event, context):\n    return {{'ran': {version:?}}}\n")
}

/// Look at `reference`'s configuration until it reports itself ready to be invoked — `Active`,
/// with no update still in flight — or fail once [`READY_DEADLINE`] has passed.
///
/// Waits for the CONDITION rather than for a span, so a loaded or a fast host only changes how
/// often it looks.
async fn poll_until_ready(router: &MultiServiceRouter, case: &str, reference: &str) {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{reference}/configuration"),
            &[],
            &[],
        )
        .await;
        let reported = read.json();
        let state = reported
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // A published version carries no update of its own, so an answer that reports no
        // `LastUpdateStatus` has none in flight.
        let update = reported
            .get("LastUpdateStatus")
            .and_then(Value::as_str)
            .unwrap_or("Successful")
            .to_string();
        if read.status == 200 && state == "Active" && update == "Successful" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: {reference:?} never reported itself ready within {READY_DEADLINE:?}, \
             last read {} {}",
            read.status,
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create `name` with one published version per entry of [`VERSIONS`], each running its own
/// code, waiting for each to report itself ready before the next is published.
async fn create_published_versions(router: &MultiServiceRouter, case: &str, name: &str) {
    let mut versions = VERSIONS.iter();
    let first = versions.next().expect("at least one version to publish");
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 128,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(first))),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case}: CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    poll_until_ready(router, case, name).await;
    poll_until_ready(router, case, &format!("{name}:{first}")).await;
    for version in versions {
        let updated = call(
            router,
            "PUT",
            &format!("{FUNCTIONS_URI}/{name}/code"),
            &serde_json::to_vec(&json!({
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(version))),
                "Publish": true,
            }))
            .expect("the update body serializes"),
            &[],
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case}: UpdateFunctionCode on {name:?} must succeed, got {} {}",
            updated.status,
            updated.text()
        );
        poll_until_ready(router, case, name).await;
        poll_until_ready(router, case, &format!("{name}:{version}")).await;
    }
}

/// The routing configuration member `weights` states, or no member at all when it is empty.
fn routing_config(weights: &[(&str, f64)]) -> Option<Value> {
    if weights.is_empty() {
        return None;
    }
    let stated: serde_json::Map<String, Value> = weights
        .iter()
        .map(|(version, weight)| ((*version).to_string(), json!(weight)))
        .collect();
    Some(json!({ "AdditionalVersionWeights": stated }))
}

/// Point the alias `alias` at version 1, shifting `weights` of its invocations.
async fn create_alias(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    alias: &str,
    weights: &[(&str, f64)],
) {
    let mut body = json!({ "Name": alias, "FunctionVersion": "1" });
    if let Some(routing) = routing_config(weights) {
        body["RoutingConfig"] = routing;
    }
    let created = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &serde_json::to_vec(&body).expect("the alias body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case}: CreateAlias {alias:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// `UpdateAlias` on `alias` with `body`.
async fn update_alias(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    alias: &str,
    body: &Value,
) -> Answer {
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/aliases/{alias}"),
        &serde_json::to_vec(body).expect("the update body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case}: UpdateAlias {alias:?} with {body} must succeed, got {} {}",
        updated.status,
        updated.text()
    );
    updated
}

/// The weights an answer reports for an alias, as a map: an answer carrying no
/// `RoutingConfig`, or one carrying no weights inside it, reports NO weights, which is what an
/// alias that shifts nothing holds.
fn reported_weights(answer: &Answer) -> BTreeMap<String, f64> {
    answer
        .json()
        .get("RoutingConfig")
        .and_then(|routing| routing.get("AdditionalVersionWeights"))
        .and_then(Value::as_object)
        .map(|weights| {
            weights
                .iter()
                .filter_map(|(version, weight)| {
                    weight.as_f64().map(|weight| (version.clone(), weight))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read the alias back and assert it reports the version it invokes and the weights it holds.
async fn assert_alias_reports(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    name: &str,
    alias: &str,
    version: &str,
    weights: &[(&str, f64)],
) {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/aliases/{alias}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case} ({leg}): GetAlias must succeed, got {} {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.json().get("FunctionVersion").and_then(Value::as_str),
        Some(version),
        "case {case} ({leg}): the alias must report version {version:?} as the one it invokes, \
         got {}",
        read.text()
    );
    let observed = reported_weights(&read);
    let expected: BTreeMap<String, f64> = weights
        .iter()
        .map(|(version, weight)| ((*version).to_string(), *weight))
        .collect();
    assert_eq!(
        observed.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "case {case} ({leg}): the alias must report the weights {expected:?}, got {observed:?} \
         from {}",
        read.text()
    );
    for (version, weight) in &expected {
        let reported = observed.get(version).copied().unwrap_or_default();
        assert!(
            (reported - weight).abs() <= WEIGHT_EPSILON,
            "case {case} ({leg}): version {version} must carry the weight {weight}, got \
             {reported}"
        );
    }
}

/// A DRY RUN of the alias `reference` names: it resolves the qualifier and runs nothing.
async fn dry_run(router: &MultiServiceRouter, reference: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations"),
        b"{}",
        &[("x-amz-invocation-type", "DryRun")],
    )
    .await
}

/// `Invoke` the alias `reference` names, re-driven while the answer reports one of this
/// PROCESS's own host bounds instead of a run of the handler.
///
/// The handlers here return at once and allocate nothing, so no leg can ask for such an answer:
/// what the bound is, is the run slots an invocation waits for, whose refusal says to retry the
/// request — the suites of this target run in parallel over the emulator's own run envelope. The
/// last answer is returned whatever it is, so a run that really failed still fails the leg.
async fn invoke_run(router: &MultiServiceRouter, reference: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/invocations");
    let mut answered = call(router, "POST", &uri, b"{}", &[]).await;
    for _ in 1..HOST_STOP_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        answered = call(router, "POST", &uri, b"{}", &[]).await;
    }
    answered
}

/// Resolve `reference` [`DRAWS_PER_LEG`] times and report which versions the draws reached,
/// asserting every one of them landed inside `allowed`.
async fn draws_over(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    reference: &str,
    allowed: &[&str],
) -> BTreeMap<String, usize> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for attempt in 0..DRAWS_PER_LEG {
        let answered = dry_run(router, reference).await;
        assert_eq!(
            answered.status,
            DRY_RUN_STATUS,
            "case {case} ({leg}) invocation {attempt}: expected the dry run to be served with \
             {DRY_RUN_STATUS}, got {} {}",
            answered.status,
            answered.text()
        );
        let executed = answered.executed_version();
        assert!(
            allowed.contains(&executed.as_str()),
            "case {case} ({leg}) invocation {attempt}: an invocation may only reach {allowed:?}, \
             got {executed:?}"
        );
        *seen.entry(executed).or_default() += 1;
    }
    seen
}

/// One row of the table: the case, the weights the alias is created with, the versions its
/// invocations may and must reach then, the `UpdateAlias` body applied to it, and the version,
/// the weights and the reachable versions that write leaves behind.
struct Row {
    case: &'static str,
    created: &'static [(&'static str, f64)],
    before_allowed: &'static [&'static str],
    before_reached: &'static [&'static str],
    update: Value,
    after_version: &'static str,
    after_weights: &'static [(&'static str, f64)],
    after_allowed: &'static [&'static str],
    after_reached: &'static [&'static str],
}

/// Every invocation of an alias is resolved against the weights the alias holds AT THAT MOMENT:
/// a write that leaves it no weights pins every later invocation to the version it names, one
/// that moves them sends later invocations to the version now weighted, and one that states no
/// routing configuration at all leaves the stored split standing.
///
/// The invocations are DRY RUNS — "Validate parameter values and verify that the user or role has
/// permission to invoke the function" — because the RESOLUTION is what a shift and a rollback are
/// read off: a dry run resolves the qualifier per invocation exactly as a run does and runs
/// nothing, so a leg can take enough samples that a split still in force cannot hide from it.
/// That the version an answer reports is the version whose code ran is
/// [`a_canary_round_and_its_rollback_each_answer_from_the_version_that_ran`].
#[tokio::test]
async fn an_invocation_is_resolved_against_the_weights_the_alias_holds_when_it_arrives() {
    let rows: Vec<Row> = vec![
        Row {
            case: "a split configured on an alias that had none",
            created: &[],
            before_allowed: &["1"],
            before_reached: &["1"],
            update: json!({ "RoutingConfig": { "AdditionalVersionWeights": { "2": 0.5 } } }),
            after_version: "1",
            after_weights: &[("2", 0.5)],
            after_allowed: &["1", "2"],
            after_reached: &["1", "2"],
        },
        Row {
            case: "half the traffic shifted, then every weight cleared",
            created: &[("2", 0.5)],
            before_allowed: &["1", "2"],
            before_reached: &["1", "2"],
            update: json!({ "RoutingConfig": { "AdditionalVersionWeights": {} } }),
            after_version: "1",
            after_weights: &[],
            after_allowed: &["1"],
            after_reached: &["1"],
        },
        Row {
            case: "the whole of the traffic shifted, then rolled back with a routing \
                   configuration stating no weights at all",
            created: &[("2", 1.0)],
            before_allowed: &["2"],
            before_reached: &["2"],
            update: json!({ "RoutingConfig": {} }),
            after_version: "1",
            after_weights: &[],
            after_allowed: &["1"],
            after_reached: &["1"],
        },
        Row {
            case: "the split moved to a third version",
            created: &[("2", 0.5)],
            before_allowed: &["1", "2"],
            before_reached: &["1", "2"],
            update: json!({ "RoutingConfig": { "AdditionalVersionWeights": { "3": 1.0 } } }),
            after_version: "1",
            after_weights: &[("3", 1.0)],
            after_allowed: &["3"],
            after_reached: &["3"],
        },
        Row {
            case: "the canary promoted: the alias moved onto the version it was shifting to and \
                   the split cleared",
            created: &[("2", 0.5)],
            before_allowed: &["1", "2"],
            before_reached: &["1", "2"],
            update: json!({
                "FunctionVersion": "2",
                "RoutingConfig": { "AdditionalVersionWeights": {} },
            }),
            after_version: "2",
            after_weights: &[],
            after_allowed: &["2"],
            after_reached: &["2"],
        },
        Row {
            case: "a write that states no routing configuration leaves the split standing",
            created: &[("2", 1.0)],
            before_allowed: &["2"],
            before_reached: &["2"],
            update: json!({ "Description": "a write about something else entirely" }),
            after_version: "1",
            after_weights: &[("2", 1.0)],
            after_allowed: &["2"],
            after_reached: &["2"],
        },
    ];
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-canary-rollback";
    create_published_versions(&router, "the shared function", name).await;
    for (index, row) in rows.iter().enumerate() {
        let alias = format!("canary{index}");
        let reference = format!("{name}:{alias}");
        create_alias(&router, row.case, name, &alias, row.created).await;
        assert_alias_reports(
            &router,
            row.case,
            "as created",
            name,
            &alias,
            "1",
            row.created,
        )
        .await;
        let before = draws_over(&router, row.case, "as created", &reference, row.before_allowed)
            .await;
        for expected in row.before_reached {
            assert!(
                before.contains_key(*expected),
                "case {} (as created): {DRAWS_PER_LEG} invocations must reach version \
                 {expected}, got {before:?}",
                row.case
            );
        }
        update_alias(&router, row.case, name, &alias, &row.update).await;
        // The write is visible to the control plane IMMEDIATELY: an alias's routing
        // configuration is not provisioned, so the read right after the write reports it.
        assert_alias_reports(
            &router,
            row.case,
            "after the write",
            name,
            &alias,
            row.after_version,
            row.after_weights,
        )
        .await;
        let after = draws_over(
            &router,
            row.case,
            "after the write",
            &reference,
            row.after_allowed,
        )
        .await;
        for expected in row.after_reached {
            assert!(
                after.contains_key(*expected),
                "case {} (after the write): {DRAWS_PER_LEG} invocations must reach version \
                 {expected}, got {after:?}",
                row.case
            );
        }
    }
}

/// Each invocation of the cycle is ANSWERED, promptly and from the version that ran: a clean
/// `200`, no `FunctionError`, a reported version drawn from the pair the alias splits between,
/// and the payload that version's own handler returns — and once the split is cleared, every
/// invocation is answered by the version the alias names.
///
/// This is the leg a canary deployment reads its decision off: the run table above resolves the
/// alias, this one runs it.
#[tokio::test]
async fn a_canary_round_and_its_rollback_each_answer_from_the_version_that_ran() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-canary-runs";
    let case = "a half split rolled back";
    create_published_versions(&router, case, name).await;
    create_alias(&router, case, name, "canary", &[("2", 0.5)]).await;
    // Each leg carries the write that OPENS it, so what the leg does is its own data rather
    // than a comparison against its label.
    let legs: [(&str, &[&str], Option<Value>); 2] = [
        ("the split in force", &["1", "2"], None),
        (
            "rolled back",
            &["1"],
            Some(json!({ "RoutingConfig": { "AdditionalVersionWeights": {} } })),
        ),
    ];
    for (leg, allowed, write) in legs {
        if let Some(write) = write.as_ref() {
            update_alias(&router, case, name, "canary", write).await;
        }
        for attempt in 0..RUNS_PER_LEG {
            let answered = invoke_run(&router, &format!("{name}:canary")).await;
            assert_eq!(
                answered.status,
                200,
                "case {case} ({leg}) invocation {attempt}: expected 200, got {} {}",
                answered.status,
                answered.text()
            );
            let failure = answered.header(FUNCTION_ERROR_HEADER);
            assert_eq!(
                failure, None,
                "case {case} ({leg}) invocation {attempt}: a handler that completes reports no \
                 function error, got {failure:?} with {}",
                answered.text()
            );
            let executed = answered.executed_version();
            assert!(
                allowed.contains(&executed.as_str()),
                "case {case} ({leg}) invocation {attempt}: an invocation may only reach \
                 {allowed:?}, got {executed:?}"
            );
            let marker = answered.marker();
            assert_eq!(
                marker,
                executed,
                "case {case} ({leg}) invocation {attempt}: the answer reports version \
                 {executed:?}, so that version's own code must be what ran, got {marker:?} from {}",
                answered.text()
            );
        }
    }
}
