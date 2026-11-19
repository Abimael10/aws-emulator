// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a SUSTAINED stream of invocations through an alias: `Invoke` called
//! over and over on one alias, including one whose `RoutingConfig` splits its traffic.
//!
//! The rule under test: every `RequestResponse` invocation Lambda admits to a function is
//! answered with what the resolved version's handler produced — `200`, no `FunctionError`,
//! `ExecutedVersion` naming the version that ran and the payload that version's own code
//! returned — and that holds for each invocation of a whole sequence and not only for the
//! first, because "after a Lambda function is invoked, Lambda maintains the execution
//! environment for some time in anticipation of another function invocation", which the next
//! invocation is then served in. So repeating a call never turns a handler that completes into
//! a failure the platform reports of its sandbox, and the share each version of a split alias
//! receives is "the percentage of invocation requests that it receives" the configuration
//! gives it (`CreateAlias`).
//!
//! The rows differ along what the rule generalises over: the weight the second version carries
//! — including each published bound of the member's own range (`smithy_query(query="shape",
//! shape="AdditionalVersionWeights")`, whose `Weight` target is `@range(min: 0.0, max: 1.0)`)
//! and values between them the issue this suite pins did not cite — and an alias that shifts
//! nothing at all. Each row makes MANY invocations rather than one, which is what a round of a
//! canary deployment does and what a client measuring a split has to make.
//!
//! The suites that merely resolve a split alias are `alias_weighted_routing`; the reuse of an
//! environment across two invocations of one version is `invocation_environment_reuse`. This
//! one is about a LONG round of runs through the split staying clean throughout.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
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

/// How long past a due instant this suite waits before calling. Every transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function this suite creates carries, in seconds.
///
/// It bounds the wall clock one run may spend, and these handlers return at once, so what it
/// has to cover is not their work but this host's own scheduling: the suites of this target
/// run in parallel over the emulator's run slots, so a run can sit mid-flight while another
/// suite holds the CPU. Generous for that reason, and well inside the host ceiling a caller
/// waits out.
const FUNCTION_TIMEOUT_SECONDS: u32 = 25;

/// How many invocations one row of the RUN table makes through its alias.
///
/// A round rather than a call: the rule is about each invocation of a sequence, so the row has
/// to make enough of them that an environment is reused many times over — and enough that the
/// half-weighted row reaches both versions (the chance a correct emulator sends all of them to
/// one version is under one in a million).
const RUNS_PER_ROW: usize = 24;

/// How many invocations the DISTRIBUTION table draws per row.
///
/// Large enough that the share observed is a share and not an accident: at this many samples a
/// correctly drawn split is within [`SHARE_TOLERANCE`] of its weight with overwhelming
/// probability, while a split drawn once and reused, or never drawn at all, is far outside it.
const DRAWS_PER_ROW: usize = 300;

/// How far the observed share of a version may sit from the weight configured for it.
///
/// Wide enough that no correct draw of [`DRAWS_PER_ROW`] samples fails a row — several
/// standard deviations of the sample — and narrow enough that the rows below, which are a
/// quarter apart, can each only be satisfied by its own weight.
const SHARE_TOLERANCE: f64 = 0.12;

/// "For the DryRun invocation type, the status code is 204" (`Invoke$StatusCode`).
const DRY_RUN_STATUS: u16 = 204;

/// How many times an invocation is re-driven while the answer reports one of this PROCESS's
/// own host bounds instead of a run of the handler.
const HOST_STOP_ATTEMPTS: usize = 6;

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

    /// How many invocations the execution environment that served this one has served, as the
    /// handler's own module-level state counted them.
    fn served(&self) -> u64 {
        self.json()
            .get("served")
            .and_then(Value::as_u64)
            .unwrap_or_default()
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

/// The handler a version reports itself from: each published version carries its own number,
/// so the marker an answer returns says which version's code ran, and the module-level counter
/// says how many invocations the environment that ran it has served.
fn source_reporting(version: &str) -> String {
    format!(
        "SERVED = []\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   SERVED.append(context.aws_request_id)\n\
         \x20   return {{'ran': {version:?}, 'served': len(SERVED)}}\n"
    )
}

/// Create a function with two published versions, each running its own code, and wait out the
/// provisioning window the create and the update document.
async fn create_two_versions(router: &MultiServiceRouter, name: &str) {
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
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting("1"))),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/code"),
        &serde_json::to_vec(&json!({
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&source_reporting("2"))),
            "Publish": true,
        }))
        .expect("the update body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionCode on {name:?} must succeed, got {} {}",
        updated.status,
        updated.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// Point the alias `alias` at version 1, shifting `weight` of its invocations to version 2
/// when a weight is given at all.
async fn create_alias(
    router: &MultiServiceRouter,
    name: &str,
    alias: &str,
    weight: Option<f64>,
) {
    let mut body = json!({ "Name": alias, "FunctionVersion": "1" });
    if let Some(weight) = weight {
        body["RoutingConfig"] = json!({ "AdditionalVersionWeights": { "2": weight } });
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
        "CreateAlias {alias:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// `Invoke` the alias `reference` names, re-driven while the answer reports one of this
/// PROCESS's own host bounds instead of a run of the handler.
///
/// The handler of every row returns at once and allocates nothing, so no row of this suite can
/// ask for such an answer: what the bound is, is the run slots an invocation waits for, whose
/// refusal says to retry the request — the suites of this target run in parallel over the
/// emulator's own run envelope (`crate::invoke_request_id::is_host_bound` is the one reading of
/// such an answer this target has). Such an answer is no answer to what the row asked, so the
/// request is made again — a bounded number of times, and the last answer is returned whatever
/// it is, so a run that really failed still fails the row.
async fn invoke_run(router: &MultiServiceRouter, reference: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/invocations");
    let mut answered = call(router, "POST", &uri, b"{}", &[]).await;
    for _ in 1..HOST_STOP_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(SLACK).await;
        answered = call(router, "POST", &uri, b"{}", &[]).await;
    }
    answered
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

/// One row of the RUN table: the case, the weight the second version carries (none at all for
/// an alias that shifts nothing), the versions an invocation may reach, and the versions a
/// whole round of them must reach.
struct RunRow {
    case: &'static str,
    weight: Option<f64>,
    allowed: &'static [&'static str],
    reached: &'static [&'static str],
}

/// Every invocation of a long round through an alias is answered by the version it ran: a
/// clean `200` carrying no `FunctionError`, with the payload of the version the answer reports.
///
/// The round is what the rule is about — an execution environment serves one invocation after
/// another, so the twentieth call is answered as the first was — and the rows span the weights
/// the split can carry, so the version each invocation runs is drawn afresh for most of them.
#[tokio::test]
async fn every_invocation_of_a_round_through_an_alias_answers_the_version_that_ran() {
    let _stream = one_invocation_stream_at_a_time().await;
    let rows: &[RunRow] = &[
        RunRow {
            case: "an alias with no routing configuration",
            weight: None,
            allowed: &["1"],
            reached: &["1"],
        },
        RunRow {
            case: "an alias shifting half of its invocations",
            weight: Some(0.5),
            allowed: &["1", "2"],
            reached: &["1", "2"],
        },
        RunRow {
            case: "an alias shifting a tenth of its invocations",
            weight: Some(0.1),
            allowed: &["1", "2"],
            reached: &["1"],
        },
        RunRow {
            case: "an alias shifting every invocation, the top of the weight's range",
            weight: Some(1.0),
            allowed: &["2"],
            reached: &["2"],
        },
        RunRow {
            case: "an alias shifting none of them, the bottom of the weight's range",
            weight: Some(0.0),
            allowed: &["1"],
            reached: &["1"],
        },
    ];
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-weighted-round";
    create_two_versions(&router, name).await;
    for (index, row) in rows.iter().enumerate() {
        let alias = format!("round{index}");
        create_alias(&router, name, &alias, row.weight).await;
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut longest_served = 0_u64;
        for attempt in 0..RUNS_PER_ROW {
            let answered = invoke_run(&router, &format!("{name}:{alias}")).await;
            assert_eq!(
                answered.status,
                200,
                "case {} invocation {attempt}: expected 200, got {} {}",
                row.case,
                answered.status,
                answered.text()
            );
            let failure = answered.header(FUNCTION_ERROR_HEADER);
            assert_eq!(
                failure, None,
                "case {} invocation {attempt}: a handler that completes reports no function \
                 error, got {failure:?} with {}",
                row.case,
                answered.text()
            );
            let executed = answered.executed_version();
            assert!(
                row.allowed.contains(&executed.as_str()),
                "case {} invocation {attempt}: an invocation may only reach {:?}, got \
                 {executed:?}",
                row.case,
                row.allowed
            );
            let marker = answered.marker();
            assert_eq!(
                marker,
                executed,
                "case {} invocation {attempt}: the answer reports version {executed:?}, so that \
                 version's own code must be what ran, got {marker:?} from {}",
                row.case,
                answered.text()
            );
            longest_served = longest_served.max(answered.served());
            *seen.entry(executed).or_default() += 1;
        }
        for expected in row.reached {
            assert!(
                seen.contains_key(*expected),
                "case {}: {RUNS_PER_ROW} invocations must reach version {expected}, got {seen:?}",
                row.case
            );
        }
        // The round is served by environments that STAND: an environment Lambda maintains in
        // anticipation of another invocation serves that invocation too, so a round of this
        // many calls is not a round of that many initializations.
        assert!(
            longest_served > 1,
            "case {}: an execution environment must serve a later invocation of the round as \
             well as the one it was built for, got a longest run of {longest_served} \
             invocation(s) over {RUNS_PER_ROW} calls to {seen:?}",
            row.case
        );
    }
}

/// One row of the DISTRIBUTION table: the case, the weight the second version carries, and the
/// share of the invocations each version is expected to receive.
struct ShareRow {
    case: &'static str,
    weight: f64,
    shifted: f64,
}

/// The share of invocations a version receives is the share the alias configures for it: over
/// many invocations the observed split tracks the weight, within the span a draw of this many
/// samples can wander.
///
/// The invocations are DRY RUNS — "Validate parameter values and verify that the user or role
/// has permission to invoke the function" — because the resolution is what a share is measured
/// from: a dry run reports the version the qualifier resolved to and runs nothing, so a row can
/// take enough samples to measure a share rather than observe an accident. That the version
/// reported is the version whose code ran is the run table above.
#[tokio::test]
async fn the_share_each_version_receives_tracks_the_weight_the_alias_configures() {
    let rows: &[ShareRow] = &[
        ShareRow {
            case: "a quarter of the invocations shifted",
            weight: 0.25,
            shifted: 0.25,
        },
        ShareRow {
            case: "half of them shifted",
            weight: 0.5,
            shifted: 0.5,
        },
        ShareRow {
            case: "three quarters of them shifted",
            weight: 0.75,
            shifted: 0.75,
        },
        ShareRow {
            case: "every one of them shifted, the top of the weight's range",
            weight: 1.0,
            shifted: 1.0,
        },
        ShareRow {
            case: "none of them shifted, the bottom of the weight's range",
            weight: 0.0,
            shifted: 0.0,
        },
    ];
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-weighted-share";
    create_two_versions(&router, name).await;
    for (index, row) in rows.iter().enumerate() {
        let alias = format!("share{index}");
        create_alias(&router, name, &alias, Some(row.weight)).await;
        let mut shifted = 0_usize;
        for attempt in 0..DRAWS_PER_ROW {
            let answered = dry_run(&router, &format!("{name}:{alias}")).await;
            assert_eq!(
                answered.status,
                DRY_RUN_STATUS,
                "case {} invocation {attempt}: expected the dry run to be served, got {} {}",
                row.case,
                answered.status,
                answered.text()
            );
            let executed = answered.executed_version();
            assert!(
                executed == "1" || executed == "2",
                "case {} invocation {attempt}: an invocation may only reach one of the two \
                 configured versions, got {executed:?}",
                row.case
            );
            if executed == "2" {
                shifted += 1;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let observed = shifted as f64 / DRAWS_PER_ROW as f64;
        assert!(
            (observed - row.shifted).abs() <= SHARE_TOLERANCE,
            "case {}: version 2 receives the share its weight states, so over {DRAWS_PER_ROW} \
             invocations its observed share must be within {SHARE_TOLERANCE} of {}, got \
             {observed} ({shifted} of {DRAWS_PER_ROW})",
            row.case,
            row.shifted
        );
    }
}
