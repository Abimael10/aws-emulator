// SPDX-License-Identifier: CC0-1.0

//! `GetLambdaFunctionRecommendations` over the wire: the recommendation this service derives
//! for the Lambda functions THE ACCOUNT REALLY HOLDS, and how a request narrows, pages and
//! refuses.
//!
//! Compute Optimizer "generates recommendations for Lambda functions that meet a specific set
//! of requirements", so the rule this suite asserts is that a recommendation exists for
//! exactly the function versions the AWS Lambda emulator holds for this account in this
//! Region — cross-checked against Lambda's own `ListFunctions` — and that every member of one
//! is computed from that version's own configured memory, the `AWS/Lambda` series published
//! for it and the memory its run reports stated. A function Lambda does not hold has no
//! recommendation at all, and a function a client creates has one from the moment Lambda
//! holds it.
//!
//! Compute Optimizer, AWS Lambda and CloudWatch are mounted on ONE engine, exactly as
//! `core/router` mounts them, and every call goes through the front door a client uses: the
//! Compute Optimizer calls as awsJson `POST /` with `X-Amz-Target`, the Lambda calls as the
//! `restJson1` requests an SDK sends. Each function is created over Lambda's own wire and read
//! back through `GetFunction` before it is analysed, so what the analysis reads is a resource
//! the owning service itself reports.
//!
//! Every assertion is on the RULE, checked over a set of functions that differ in configured
//! memory, demand shape, invocation rate, version (published and `$LATEST`), Region and log
//! format — the response itself carries the analysed metrics, so an option can be checked
//! against the demand it has to carry without this suite restating the handler's arithmetic.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_in_region, call_lambda, create_lambda_function, emulator_with_engine, error_code,
    make_analysis_due, opt_in, publish_function_runs, publish_function_version, publish_log_line,
    register_analysed_functions, ObservedRuns, LATEST_VERSION, REGION,
};

/// The memory a Lambda function can be configured with, from the AWS Lambda quotas: "128 MB
/// to 10,240 MB, in 1-MB increments". No memory size a recommendation names may fall outside
/// this range.
const MIN_MEMORY_MB: f64 = 128.0;
const MAX_MEMORY_MB: f64 = 10_240.0;

/// The headroom a recommendation option is sized with, which the service documents as the
/// only headroom it names ("`PERCENT_20` for headroom",
/// `PutRecommendationPreferences$utilizationPreferences`).
const HEADROOM: f64 = 0.20;

/// How many invocations the analysed period has to hold for Compute Optimizer to reach a
/// finding for a function (the "Supported resources and requirements" page the
/// `InsufficientData` reason code links: at least 50 invocations over the look-back period).
const MIN_ANALYSED_INVOCATIONS: f64 = 50.0;

/// The look-back period in force for a Lambda function: the service's own default, which no
/// preference can move for this resource type.
const LOOK_BACK_DAYS: f64 = 14.0;

/// The `LambdaFunctionRecommendationFinding` values
/// (`crate::ops::types::LAMBDA_FUNCTION_RECOMMENDATION_FINDING_VALUES`).
const FINDINGS: &[&str] = &["Optimized", "NotOptimized", "Unavailable"];

/// The `LambdaFunctionRecommendationFindingReasonCode` values
/// (`crate::ops::types::LAMBDA_FUNCTION_RECOMMENDATION_FINDING_REASON_CODE_VALUES`).
const FINDING_REASON_CODES: &[&str] = &[
    "MemoryOverprovisioned",
    "MemoryUnderprovisioned",
    "InsufficientData",
    "Inconclusive",
];

/// The functions the account holds while this suite runs ([`register_analysed_functions`]),
/// by name — one per case a function recommendation distinguishes.
const OVER_PROVISIONED: &str = "invoice-renderer";
const UNDER_PROVISIONED: &str = "thumbnail-resizer";
const ALREADY_RIGHT_SIZED: &str = "order-dispatcher";
const TOO_FEW_INVOCATIONS: &str = "log-forwarder";
const NO_MEMORY_OBSERVED: &str = "nightly-reconciler";

/// An enrolled account whose analysis is already due, holding NO function at all: the
/// emulator a case needs to drive the whole path from "the account holds no function" to "it
/// holds this one".
///
/// The analysis becomes readable because its stored instant is already past
/// ([`make_analysis_due`]), not because the suite waited for the clock to reach it, so
/// nothing here depends on how fast the host runs.
async fn enrolled_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_engine();
    opt_in(&router).await;
    make_analysis_due(&engine);
    (router, engine)
}

/// The same emulator, holding the account's own analysed functions.
async fn analysed_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_engine();
    register_analysed_functions(&router, &engine).await;
    opt_in(&router).await;
    make_analysis_due(&engine);
    (router, engine)
}

/// The unqualified ARN of one function of this account, in `region`.
fn function_arn_in(region: &str, name: &str) -> String {
    format!(
        "arn:aws:lambda:{region}:{}:function:{name}",
        ::engine::emulator_account_id()
    )
}

/// The unqualified ARN of one function, at the Region these suites sign for.
fn function_arn(name: &str) -> String {
    function_arn_in(REGION, name)
}

/// The qualified ARN of one version of a function.
fn qualified_arn(name: &str, version: &str) -> String {
    format!("{}:{version}", function_arn(name))
}

/// Read function recommendations, asserting the call was served.
async fn recommendations(router: &MultiServiceRouter, request: Value) -> Value {
    let (status, body) = call(router, "GetLambdaFunctionRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {request}: expected 200, got {status}: {body}"
    );
    body
}

/// Every recommendation of the account, unavailable findings included — the whole analysed
/// population, which the model withholds the unavailable part of unless a request asks for it.
async fn whole_population(router: &MultiServiceRouter) -> Value {
    recommendations(
        router,
        json!({"filters": [{"name": "Finding", "values": FINDINGS}], "maxResults": 1000}),
    )
    .await
}

/// The recommendations of one response, in the order it reports them.
fn reported(body: &Value) -> Vec<Value> {
    body["lambdaFunctionRecommendations"]
        .as_array()
        .unwrap_or_else(|| panic!("lambdaFunctionRecommendations must be a list, got {body}"))
        .clone()
}

/// The ARN of every recommendation in a response, in the order it reports them.
fn reported_arns(body: &Value) -> Vec<String> {
    reported(body)
        .iter()
        .map(|recommendation| {
            recommendation["functionArn"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The recommendation a response carries for one function, by name.
fn recommendation_for(body: &Value, name: &str) -> Value {
    reported(body)
        .into_iter()
        .find(|recommendation| {
            recommendation["functionArn"]
                .as_str()
                .unwrap_or_default()
                .ends_with(&format!(":function:{name}"))
        })
        .unwrap_or_else(|| panic!("case {name:?}: no recommendation for it: {body}"))
}

/// A number the wire carries, or `0` for one the protocol omitted because it equals the
/// member's modelled default.
fn number(value: &Value, member: &str) -> f64 {
    value[member].as_f64().unwrap_or(0.0)
}

/// One reported `utilizationMetrics` entry's value.
fn metric(recommendation: &Value, name: &str, statistic: &str) -> f64 {
    recommendation["utilizationMetrics"]
        .as_array()
        .unwrap_or_else(|| panic!("utilizationMetrics must be a list: {recommendation}"))
        .iter()
        .find(|entry| entry["name"] == json!(name) && entry["statistic"] == json!(statistic))
        .map(|entry| number(entry, "value"))
        .unwrap_or_else(|| panic!("no {name}/{statistic} metric: {recommendation}"))
}

/// The single memory size recommendation option of a recommendation.
fn only_option(recommendation: &Value) -> Value {
    let options = recommendation["memorySizeRecommendationOptions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("memorySizeRecommendationOptions must be a list: {recommendation}")
        });
    assert_eq!(
        options.len(),
        1,
        "case {}: expected one memory size option, got {}: {recommendation}",
        recommendation["functionArn"],
        options.len()
    );
    options[0].clone()
}

/// The finding reason codes a recommendation reports.
fn reason_codes(recommendation: &Value) -> Vec<String> {
    recommendation["findingReasonCodes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|code| code.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The function ARNs AWS LAMBDA holds for the account at the endpoint of `region`, as its own
/// `ListFunctions` reports them — what the recommendations are cross-checked against.
async fn functions_lambda_holds(router: &MultiServiceRouter, region: &str) -> BTreeSet<String> {
    let (status, listed) = call_lambda(router, region, "GET", "/2015-03-31/functions/", None).await;
    assert_eq!(
        status, 200,
        "listing the account's functions must succeed, got {status}: {listed}"
    );
    listed["Functions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|function| {
            function["FunctionArn"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The memory AWS Lambda holds one function configured with, as its own
/// `GetFunctionConfiguration` reports it.
async fn configured_memory(router: &MultiServiceRouter, name: &str) -> f64 {
    let (status, read) = call_lambda(
        router,
        REGION,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: reading the function's configuration must succeed, got {status}: {read}"
    );
    number(&read, "MemorySize")
}

/// Every invariant a function recommendation satisfies, whatever function it is for.
///
/// The rule: a recommendation is the function's OBSERVATION read three ways — the memory
/// size it names is the smallest whole MB inside Lambda's range that carries the reported
/// memory peak with headroom; the finding and its reason code say which side of the
/// configured memory that lands on; and the saving is what the move is worth, never negative
/// and never claimed where the move costs the same or more.
fn assert_recommendation_invariants(recommendation: &Value) {
    let case = recommendation["functionArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    assert_eq!(
        recommendation["accountId"].as_str(),
        Some(::engine::emulator_account_id()),
        "case {case:?}: a recommendation names the account that owns the function, got \
         {recommendation}"
    );
    let version = recommendation["functionVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a recommendation names the function version"));
    // An unqualified ARN is the `$LATEST` version; a published version is qualified with it.
    let qualified = case.ends_with(&format!(":{version}"));
    assert_eq!(
        qualified,
        version != LATEST_VERSION,
        "case {case:?}: a published version is named by a qualified ARN and $LATEST by an \
         unqualified one, got version {version:?}"
    );

    let finding = recommendation["finding"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a recommendation carries a finding"));
    assert!(
        FINDINGS.contains(&finding),
        "case {case:?}: {finding:?} is not a LambdaFunctionRecommendationFinding, got \
         {recommendation}"
    );
    for code in reason_codes(recommendation) {
        assert!(
            FINDING_REASON_CODES.contains(&code.as_str()),
            "case {case:?}: {code:?} is not a LambdaFunctionRecommendationFindingReasonCode, \
             got {recommendation}"
        );
    }

    assert_eq!(
        number(recommendation, "lookbackPeriodInDays"),
        LOOK_BACK_DAYS,
        "case {case:?}: the period analysed is the look-back period in force, got \
         {recommendation}"
    );
    let invocations = number(recommendation, "numberOfInvocations");
    assert!(
        invocations >= 0.0 && invocations.fract() == 0.0,
        "case {case:?}: numberOfInvocations is a whole number of invocations, got {invocations}"
    );

    let current_memory = number(recommendation, "currentMemorySize");
    assert!(
        (MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&current_memory),
        "case {case:?}: a function's memory is within Lambda's {MIN_MEMORY_MB}..={MAX_MEMORY_MB} \
         MB range, got {current_memory}"
    );
    let peak_memory = metric(recommendation, "Memory", "Maximum");
    let average_memory = metric(recommendation, "Memory", "Average");
    assert!(
        average_memory <= peak_memory,
        "case {case:?}: the mean memory an invocation used is at most the peak {peak_memory}, \
         got {average_memory}"
    );
    let average_duration = metric(recommendation, "Duration", "Average");
    assert!(
        average_duration <= metric(recommendation, "Duration", "Maximum"),
        "case {case:?}: the mean duration is at most the longest invocation, got {recommendation}"
    );

    let risk = recommendation["currentPerformanceRisk"]
        .as_str()
        .unwrap_or_default();
    // The model's `CurrentPerformanceRisk` enum.
    assert!(
        ["VeryLow", "Low", "Medium", "High"].contains(&risk),
        "case {case:?}: {risk:?} is not a CurrentPerformanceRisk, got {recommendation}"
    );
    assert_eq!(
        recommendation["effectiveRecommendationPreferences"]["savingsEstimationMode"]["source"],
        json!("PublicPricing"),
        "case {case:?}: this service's money figures come from public pricing, got \
         {recommendation}"
    );

    if finding == "Unavailable" {
        // "Compute Optimizer was unable to generate a recommendation for the function", so
        // there is no memory size to move it to.
        assert!(
            recommendation["memorySizeRecommendationOptions"].is_null(),
            "case {case:?}: an unavailable finding has no memory size option, got \
             {recommendation}"
        );
        let codes = reason_codes(recommendation);
        assert!(
            codes
                .iter()
                .all(|code| code == "InsufficientData" || code == "Inconclusive"),
            "case {case:?}: an unavailable finding is explained by InsufficientData or \
             Inconclusive, got {codes:?}"
        );
        // One of the two inputs the analysis needs was missing: too few invocations in the
        // period, or no memory utilization observed at all.
        assert!(
            invocations < MIN_ANALYSED_INVOCATIONS || peak_memory <= 0.0,
            "case {case:?}: a function with {invocations} invocations peaking at {peak_memory} \
             MB has what the analysis needs, got {recommendation}"
        );
        return;
    }

    // A finding was reached, so the function qualified for a recommendation.
    assert!(
        invocations >= MIN_ANALYSED_INVOCATIONS,
        "case {case:?}: a function is analysed only once it has been invoked at least \
         {MIN_ANALYSED_INVOCATIONS} times in the analysed period, got {invocations}"
    );
    assert!(
        peak_memory > 0.0,
        "case {case:?}: a finding is reached only where memory utilization was observed, got \
         {recommendation}"
    );

    let option = only_option(recommendation);
    assert_eq!(
        number(&option, "rank"),
        1.0,
        "case {case:?}: the one option reported is the first-ranked one, got {option}"
    );
    let memory_size = number(&option, "memorySize");
    // The rule: the smallest whole MB inside Lambda's range that carries the observed peak
    // with HEADROOM to spare.
    let expected_memory = (peak_memory * (1.0 + HEADROOM))
        .ceil()
        .clamp(MIN_MEMORY_MB, MAX_MEMORY_MB);
    assert_eq!(
        memory_size, expected_memory,
        "case {case:?}: a {peak_memory} MB peak with {HEADROOM} headroom asks for \
         {expected_memory} MB, got {memory_size}"
    );

    // The finding is which side of the configured memory that lands on, and its reason code
    // says the same thing in the model's words.
    let codes = reason_codes(recommendation);
    if memory_size == current_memory {
        assert_eq!(
            finding, "Optimized",
            "case {case:?}: a function already at the recommended {memory_size} MB is \
             optimized, got {finding:?}"
        );
        assert!(
            codes.is_empty(),
            "case {case:?}: an optimized function has no finding reason code, got {codes:?}"
        );
    } else {
        assert_eq!(
            finding, "NotOptimized",
            "case {case:?}: a function the analysis would move from {current_memory} MB to \
             {memory_size} MB is not optimized, got {finding:?}"
        );
        let expected_code = if memory_size < current_memory {
            "MemoryOverprovisioned"
        } else {
            "MemoryUnderprovisioned"
        };
        assert_eq!(
            codes,
            vec![expected_code.to_string()],
            "case {case:?}: {current_memory} MB against a recommended {memory_size} MB is \
             {expected_code}, got {codes:?}"
        );
    }

    // The projected duration: one range, on the one metric `LambdaFunctionMemoryMetricName`
    // declares, bounding the value expected inside it. The analysis observed the function at
    // the memory it is configured with and at no other size, so what it projects is the
    // duration it observed.
    let projected = option["projectedUtilizationMetrics"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: an option projects the function's duration"));
    let projected_at = |statistic: &str| -> f64 {
        projected
            .iter()
            .find(|entry| entry["statistic"] == json!(statistic))
            .map(|entry| {
                assert_eq!(
                    entry["name"],
                    json!("Duration"),
                    "case {case:?}: Duration is the only projected metric, got {entry}"
                );
                number(entry, "value")
            })
            .unwrap_or_else(|| panic!("case {case:?}: no projected {statistic}: {option}"))
    };
    let (lower, expected, upper) = (
        projected_at("LowerBound"),
        projected_at("Expected"),
        projected_at("UpperBound"),
    );
    assert!(
        lower <= expected && expected <= upper,
        "case {case:?}: the expected duration lies inside the projected {lower}..={upper} \
         range, got {expected}"
    );
    assert_eq!(
        expected, average_duration,
        "case {case:?}: the projected duration is the duration observed, got {expected}"
    );

    // The saving: never negative, in a currency the model names, a percentage of monthly
    // cost that is zero exactly when the amount is, and the same before and after discounts
    // (this service records no negotiated discount).
    for member in ["savingsOpportunity", "savingsOpportunityAfterDiscounts"] {
        let opportunity = &option[member];
        let value = number(&opportunity["estimatedMonthlySavings"], "value");
        let percentage = number(opportunity, "savingsOpportunityPercentage");
        assert!(
            value >= 0.0,
            "case {case:?}/{member}: a saving is never negative, got {value}"
        );
        assert_eq!(
            opportunity["estimatedMonthlySavings"]["currency"],
            json!("USD"),
            "case {case:?}/{member}: the prices this service holds are USD, got {opportunity}"
        );
        assert!(
            (0.0..=100.0).contains(&percentage),
            "case {case:?}/{member}: a saving is a percentage of monthly cost, got {percentage}"
        );
        assert_eq!(
            percentage == 0.0,
            value == 0.0,
            "case {case:?}/{member}: a percentage is zero exactly when the saving is, got \
             {percentage} for {value}"
        );
        if memory_size >= current_memory {
            // More memory never costs less, and the memory it already runs on costs the
            // same, so neither move saves anything.
            assert_eq!(
                value, 0.0,
                "case {case:?}/{member}: moving from {current_memory} MB to {memory_size} MB \
                 saves nothing, got {value}"
            );
        }
    }
}

/// The recommendations reported are exactly the functions AWS LAMBDA holds for the account:
/// every reported ARN is one `ListFunctions` reports, every function it reports has a
/// recommendation, and an ARN naming a function Lambda does not hold selects nothing.
#[tokio::test]
async fn the_recommendations_reported_are_the_functions_aws_lambda_holds() {
    let (router, _engine) = analysed_emulator().await;

    let held = functions_lambda_holds(&router, REGION).await;
    assert!(
        !held.is_empty(),
        "the account must hold functions for this to prove anything, got {held:?}"
    );
    let body = whole_population(&router).await;
    let reported: BTreeSet<String> = reported_arns(&body).into_iter().collect();
    assert_eq!(
        reported, held,
        "the analysed functions are the ones AWS Lambda holds for the account: expected \
         {held:?}, got {body}"
    );

    // Rows: well-formed ARNs of functions no account holds — a name nothing created, the
    // plural of one that exists, and one differing from a held function by a single
    // character. Each selects nothing rather than a recommendation of a function that does
    // not exist.
    for name in ["my-worker", "invoice-renderers", "invoice-render"] {
        let arn = function_arn(name);
        assert!(
            !held.contains(&arn),
            "case {name:?}: this case needs a function the account does NOT hold, got {held:?}"
        );
        let body = recommendations(&router, json!({"functionArns": [arn]})).await;
        assert_eq!(
            reported_arns(&body),
            Vec::<String>::new(),
            "case {name:?}: a function AWS Lambda does not hold has no recommendation, got \
             {body}"
        );
    }
}

/// A function a CLIENT creates is analysed: from the moment AWS Lambda holds it, Compute
/// Optimizer reports it — with the memory Lambda holds it configured with — and what the
/// analysis finds for it follows from the runs observed on it.
///
/// Rows: the three ways an observed peak can land against a function's configured memory,
/// each a different configured size, plus a function nothing has run yet. A function with no
/// finding is reported only when a request asks for `Unavailable`, as the model states.
#[tokio::test]
async fn a_function_the_client_creates_is_analysed_from_its_own_runs() {
    let (router, engine) = enrolled_emulator().await;

    // (the function, the memory it is created with, the memory peak its runs report, the
    // finding the analysis must reach, its reason code)
    let rows: [(&str, i32, f64, &str, Option<&str>); 4] = [
        ("my-worker", 1_024, 300.0, "NotOptimized", Some("MemoryOverprovisioned")),
        ("thumbnailer", 128, 200.0, "NotOptimized", Some("MemoryUnderprovisioned")),
        // 426 MB with 20% headroom is 511.2 MB, which rounds up to the 512 MB it already
        // runs on: there is no other memory size to move it to.
        ("resizer", 512, 426.0, "Optimized", None),
        ("idler", 256, 0.0, "Unavailable", Some("InsufficientData")),
    ];

    // Before any of them exists, the account has no function recommendation at all.
    assert_eq!(
        reported_arns(&whole_population(&router).await),
        Vec::<String>::new(),
        "an account holding no function reads the empty list"
    );

    for (name, memory_mb, peak_mb, _, _) in rows {
        create_lambda_function(&router, REGION, name, memory_mb, &[("Owner", name)], "Text").await;
        if peak_mb <= 0.0 {
            continue;
        }
        publish_function_runs(
            &engine,
            name,
            LATEST_VERSION,
            "Text",
            &[ObservedRuns {
                days_ago: 1.0,
                invocations: 60.0,
                durations_ms: &[10.0, 20.0, 30.0],
                memory_used_mb: &[peak_mb / 2.0, peak_mb],
            }],
        );
        // What the function itself wrote lands in the same stream as the report that closes
        // the run. None of it is an observation of the memory a run used, so none of it may
        // move what the analysis reads out of the group — whichever form it takes.
        for line in [
            "START RequestId: 00000000-0000-4000-8000-000000000000 Version: $LATEST",
            "hello from the handler",
            "{\"level\":\"INFO\",\"message\":\"Max Memory Used: 9001 MB\"}",
            "{\"type\":\"platform.start\",\"record\":{\"requestId\":\"r\"}}",
            "{ not json after all",
        ] {
            publish_log_line(&engine, name, LATEST_VERSION, 1.0, line);
        }
    }

    let body = whole_population(&router).await;
    for (name, memory_mb, peak_mb, finding, code) in rows {
        let reported = recommendation_for(&body, name);
        assert_recommendation_invariants(&reported);
        assert_eq!(
            reported["functionArn"],
            json!(function_arn(name)),
            "case {name:?}: the recommendation names the function's own ARN, got {reported}"
        );
        assert_eq!(
            number(&reported, "currentMemorySize"),
            configured_memory(&router, name).await,
            "case {name:?}: the memory reported is the memory AWS Lambda holds the function \
             configured with, got {reported}"
        );
        assert_eq!(
            number(&reported, "currentMemorySize"),
            f64::from(memory_mb),
            "case {name:?}: expected the {memory_mb} MB it was created with, got {reported}"
        );
        assert_eq!(
            metric(&reported, "Memory", "Maximum"),
            peak_mb,
            "case {name:?}: the peak analysed is the peak its runs reported, and nothing the \
             function logged beside those reports moves it, got {reported}"
        );
        assert_eq!(
            reported["finding"],
            json!(finding),
            "case {name:?}: a {memory_mb} MB function peaking at {peak_mb} MB is {finding}, \
             got {reported}"
        );
        assert_eq!(
            reason_codes(&reported),
            code.map(|code| vec![code.to_string()]).unwrap_or_default(),
            "case {name:?}: expected the reason code {code:?}, got {reported}"
        );
        assert_eq!(
            reported["tags"],
            json!([{"key": "Owner", "value": name}]),
            "case {name:?}: a recommendation reports the function's own tags, got {reported}"
        );
    }

    // "Functions with a finding of unavailable are not returned unless you specify the filter
    // parameter with a value of Unavailable": the function nothing has run yet is the one the
    // default answer withholds.
    let default_answer = reported_arns(&recommendations(&router, json!({})).await);
    assert_eq!(
        default_answer,
        rows.iter()
            .filter(|(_, _, peak, _, _)| *peak > 0.0)
            .map(|(name, _, _, _, _)| function_arn(name))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect::<Vec<String>>(),
        "a request naming no filter reports the functions the analysis reached a finding for"
    );
}

/// A PUBLISHED version is its own analysed resource: the qualified ARN selects it, it reports
/// the version number and the memory that version is configured with, and the unpublished
/// version stays the unqualified ARN's own.
#[tokio::test]
async fn a_published_version_is_analysed_as_its_own_function_version() {
    let (router, engine) = enrolled_emulator().await;

    let name = "publisher";
    create_lambda_function(&router, REGION, name, 512, &[], "Text").await;
    let version = publish_function_version(&router, REGION, name).await;
    assert_ne!(
        version, LATEST_VERSION,
        "publishing must mint a version number, got {version:?}"
    );
    // Each version has its OWN observed runs, published against the resource an invocation of
    // that version addresses.
    publish_function_runs(
        &engine,
        name,
        LATEST_VERSION,
        "Text",
        &[ObservedRuns {
            days_ago: 1.0,
            invocations: 60.0,
            durations_ms: &[10.0, 20.0, 30.0],
            memory_used_mb: &[100.0, 200.0],
        }],
    );
    publish_function_runs(
        &engine,
        name,
        &version,
        "Text",
        &[ObservedRuns {
            days_ago: 1.0,
            invocations: 90.0,
            durations_ms: &[40.0, 50.0, 60.0],
            memory_used_mb: &[300.0, 400.0],
        }],
    );

    let body = whole_population(&router).await;
    assert_eq!(
        reported_arns(&body),
        vec![function_arn(name), qualified_arn(name, &version)],
        "both versions of the function are analysed resources, got {body}"
    );
    for recommendation in reported(&body) {
        assert_recommendation_invariants(&recommendation);
    }

    // Rows: the unqualified ARN, the qualified one, both, and a version the function has not
    // published.
    let cases: [(&str, Vec<String>, Vec<String>); 4] = [
        (
            "an unqualified ARN selects $LATEST",
            vec![function_arn(name)],
            vec![function_arn(name)],
        ),
        (
            "a qualified ARN selects that version",
            vec![qualified_arn(name, &version)],
            vec![qualified_arn(name, &version)],
        ),
        (
            "both",
            vec![qualified_arn(name, &version), function_arn(name)],
            vec![function_arn(name), qualified_arn(name, &version)],
        ),
        (
            "a version the function has not published",
            vec![qualified_arn(name, "7")],
            Vec::new(),
        ),
    ];
    for (case, arns, expected) in cases {
        let body = recommendations(&router, json!({"functionArns": arns})).await;
        assert_eq!(
            reported_arns(&body),
            expected,
            "case {case:?}: expected exactly {expected:?}, got {body}"
        );
    }

    // Each version's own analysis: the version number it reports, and the peak its own runs
    // observed.
    let latest = recommendation_for(&body, name);
    assert_eq!(
        latest["functionVersion"],
        json!(LATEST_VERSION),
        "the unqualified ARN is the unpublished version, got {latest}"
    );
    assert_eq!(
        metric(&latest, "Memory", "Maximum"),
        200.0,
        "the unpublished version is analysed over ITS runs, got {latest}"
    );
    let published = reported(&body)
        .into_iter()
        .find(|recommendation| recommendation["functionVersion"] == json!(version))
        .unwrap_or_else(|| panic!("no recommendation for version {version}: {body}"));
    assert_eq!(
        metric(&published, "Memory", "Maximum"),
        400.0,
        "the published version is analysed over ITS runs, got {published}"
    );
    assert_eq!(
        number(&published, "numberOfInvocations"),
        90.0,
        "the published version counts ITS invocations, got {published}"
    );
}

/// Every recommendation the account's inventory yields satisfies the rule, across functions
/// that differ in configured memory, demand, invocation rate and log format — and the
/// over-provisioned one is the case that actually saves money.
#[tokio::test]
async fn every_recommendation_is_its_function_s_observation_derived() {
    let (router, _engine) = analysed_emulator().await;

    let body = whole_population(&router).await;
    let all = reported(&body);
    assert!(
        all.len() > 2,
        "the inventory must span several functions for this to prove anything, got {body}"
    );
    for recommendation in &all {
        assert_recommendation_invariants(recommendation);
    }

    // The three cases a finding distinguishes, each reached from a different observation.
    let over = recommendation_for(&body, OVER_PROVISIONED);
    assert_eq!(
        over["finding"],
        json!("NotOptimized"),
        "case {OVER_PROVISIONED:?}: a function peaking far below its memory is not \
         optimized, got {over}"
    );
    let saved = number(
        &only_option(&over)["savingsOpportunity"]["estimatedMonthlySavings"],
        "value",
    );
    assert!(
        saved > 0.0,
        "case {OVER_PROVISIONED:?}: sizing an over-provisioned function down saves money, \
         got {saved}"
    );

    let under = recommendation_for(&body, UNDER_PROVISIONED);
    assert_eq!(
        reason_codes(&under),
        vec!["MemoryUnderprovisioned".to_string()],
        "case {UNDER_PROVISIONED:?}: a function peaking at its memory limit is \
         under-provisioned, got {under}"
    );

    // Its logs are delivered as structured records rather than the runtime's plain-text
    // framing, and the memory a run used is observed in either form.
    let optimized = recommendation_for(&body, ALREADY_RIGHT_SIZED);
    assert_eq!(
        optimized["finding"],
        json!("Optimized"),
        "case {ALREADY_RIGHT_SIZED:?}: a function whose peak plus headroom fits its memory \
         is optimized, got {optimized}"
    );
    assert_eq!(
        metric(&optimized, "Memory", "Maximum"),
        100.0,
        "case {ALREADY_RIGHT_SIZED:?}: a structured run report states the memory it used, \
         got {optimized}"
    );

    // The memory each recommendation reports is the memory AWS Lambda holds for the function.
    for recommendation in &all {
        let arn = recommendation["functionArn"].as_str().unwrap_or_default();
        let name = arn.rsplit(':').next().unwrap_or_default().to_string();
        assert_eq!(
            number(recommendation, "currentMemorySize"),
            configured_memory(&router, &name).await,
            "case {name:?}: the memory reported is the memory AWS Lambda holds, got \
             {recommendation}"
        );
    }

    // Tags are the function's own: reported where it carries any, absent where it does not.
    assert_eq!(
        over["tags"],
        json!([{"key": "Owner", "value": "TeamA"}]),
        "case {OVER_PROVISIONED:?}: a recommendation reports the function's tags, got {over}"
    );
    assert!(
        optimized["tags"].is_null(),
        "case {ALREADY_RIGHT_SIZED:?}: a function carrying no tag reports none, got {optimized}"
    );
}

/// "Functions with a finding of unavailable are not returned unless you specify the filter
/// parameter with a value of `Unavailable`": the two functions the analysis could reach no
/// finding for are withheld from every other request, and each carries the reason code of
/// what the analysis was missing.
#[tokio::test]
async fn unavailable_functions_are_withheld_unless_a_filter_asks_for_them() {
    let (router, _engine) = analysed_emulator().await;

    // Rows: the member absent, an empty filter list, a filter that names other findings, and
    // a reason-code filter that names an unavailable function's own reason code — none of
    // them is "the filter parameter with a value of Unavailable".
    let withholding: [(&str, Value); 4] = [
        ("absent", json!({})),
        ("empty", json!({"filters": []})),
        (
            "another finding",
            json!({"filters": [{"name": "Finding", "values": ["Optimized", "NotOptimized"]}]}),
        ),
        (
            "an unavailable reason code alone",
            json!({"filters": [{"name": "FindingReasonCode", "values": ["InsufficientData"]}]}),
        ),
    ];
    for (case, request) in withholding {
        let body = recommendations(&router, request.clone()).await;
        for recommendation in reported(&body) {
            assert_ne!(
                recommendation["finding"],
                json!("Unavailable"),
                "case {case:?}: an unavailable function is returned only when a Finding filter \
                 names Unavailable, got {recommendation}"
            );
        }
    }

    let body = recommendations(
        &router,
        json!({"filters": [{"name": "Finding", "values": ["Unavailable"]}]}),
    )
    .await;
    let asked = reported(&body);
    assert_eq!(
        asked.len(),
        2,
        "asking for Unavailable must report exactly the functions the analysis reached no \
         finding for, got {body}"
    );
    for recommendation in &asked {
        assert_recommendation_invariants(recommendation);
        assert_eq!(
            recommendation["finding"],
            json!("Unavailable"),
            "a Finding filter reports only the functions at that finding, got {recommendation}"
        );
    }
    // Each unavailable function says WHAT the analysis was missing: too few invocations is
    // insufficient data, no memory utilization at all is inconclusive.
    assert_eq!(
        reason_codes(&recommendation_for(&body, TOO_FEW_INVOCATIONS)),
        vec!["InsufficientData".to_string()],
        "case {TOO_FEW_INVOCATIONS:?}: a function invoked too rarely has insufficient data, \
         got {body}"
    );
    assert_eq!(
        reason_codes(&recommendation_for(&body, NO_MEMORY_OBSERVED)),
        vec!["Inconclusive".to_string()],
        "case {NO_MEMORY_OBSERVED:?}: a function whose memory was never observed is \
         inconclusive, got {body}"
    );
}

/// A filter selects on what the service DERIVED, over the whole documented vocabulary of
/// both filter names: the functions a filter reports are exactly the ones whose reported
/// finding (or reason code) is among its values, and several filters are ANDed.
#[tokio::test]
async fn filters_select_on_the_derived_finding_and_reason_code() {
    let (router, _engine) = analysed_emulator().await;

    // The whole population, unavailable functions included, so each row below can be checked
    // against the set it should pick out of it.
    let population = reported(&whole_population(&router).await);
    assert_eq!(
        population.len(),
        5,
        "a Finding filter naming every finding reports the whole inventory, got {population:?}"
    );

    // Rows: each `Finding` value alone, and a filter naming several values.
    let mut rows: Vec<(String, Value, Vec<String>)> = Vec::new();
    for finding in FINDINGS {
        rows.push((
            format!("Finding={finding}"),
            json!({"name": "Finding", "values": [finding]}),
            vec![(*finding).to_string()],
        ));
    }
    rows.push((
        "Finding=[Optimized, Unavailable]".to_string(),
        json!({"name": "Finding", "values": ["Optimized", "Unavailable"]}),
        vec!["Optimized".to_string(), "Unavailable".to_string()],
    ));
    for (case, filter, wanted) in rows {
        let body = recommendations(&router, json!({"filters": [filter]})).await;
        let expected: Vec<String> = population
            .iter()
            .filter(|recommendation| {
                wanted.contains(
                    &recommendation["finding"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .map(|recommendation| {
                recommendation["functionArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            reported_arns(&body),
            expected,
            "case {case:?}: a filter reports exactly the functions at those findings"
        );
    }

    // The reason codes: a filter on a code reports the functions that report it — and an
    // `Unavailable` code still only reaches them when the same request asks for Unavailable
    // findings as well.
    for code in FINDING_REASON_CODES {
        let case = format!("FindingReasonCode={code}");
        let body = recommendations(
            &router,
            json!({"filters": [
                {"name": "FindingReasonCode", "values": [code]},
                {"name": "Finding", "values": FINDINGS},
            ]}),
        )
        .await;
        let expected: Vec<String> = population
            .iter()
            .filter(|recommendation| reason_codes(recommendation).iter().any(|has| has == code))
            .map(|recommendation| {
                recommendation["functionArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            reported_arns(&body),
            expected,
            "case {case:?}: a reason-code filter reports exactly the functions reporting it"
        );
        assert!(
            !expected.is_empty(),
            "case {case:?}: this inventory spans every documented reason code, so each must \
             select something"
        );
    }

    // ANDed: a finding and a reason code that never occur together select nothing.
    let body = recommendations(
        &router,
        json!({"filters": [
            {"name": "Finding", "values": ["Optimized"]},
            {"name": "FindingReasonCode", "values": ["MemoryOverprovisioned"]},
        ]}),
    )
    .await;
    assert_eq!(
        reported_arns(&body),
        Vec::<String>::new(),
        "an optimized function carries no reason code, so the two filters together select \
         nothing, got {body}"
    );
}

/// A regional endpoint answers for ITS Region: a function created at one Region's endpoint is
/// analysed there and nowhere else, and the ARNs an endpoint reports name its own Region.
#[tokio::test]
async fn the_arns_reported_name_the_region_the_request_addressed() {
    let (router, engine) = enrolled_emulator().await;

    // The Lambda inventory keys a function by its NAME, so each Region's endpoint creates its
    // own function: what tells the two apart is the endpoint the request addressed.
    let functions = [(REGION, "regional-worker"), ("eu-west-1", "eu-worker")];
    for (region, name) in functions {
        create_lambda_function(&router, region, name, 1_024, &[], "Text").await;
        publish_function_runs(
            &engine,
            name,
            LATEST_VERSION,
            "Text",
            &[ObservedRuns {
                days_ago: 1.0,
                invocations: 6_000.0,
                durations_ms: &[10.0, 20.0, 30.0],
                memory_used_mb: &[100.0, 300.0],
            }],
        );
    }

    // Rows: each Region that holds one of them, and a Region that holds no function at all.
    for (region, expected) in [
        (REGION, vec![function_arn_in(REGION, "regional-worker")]),
        ("eu-west-1", vec![function_arn_in("eu-west-1", "eu-worker")]),
        ("ap-southeast-2", Vec::new()),
    ] {
        let (status, body) = call_in_region(
            &router,
            region,
            "GetLambdaFunctionRecommendations",
            json!({"filters": [{"name": "Finding", "values": FINDINGS}]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected 200, got {status}: {body}"
        );
        assert_eq!(
            reported_arns(&body),
            expected,
            "case {region:?}: an endpoint reports the functions of ITS Region, got {body}"
        );
    }

    // The function of the other Region is held by AWS Lambda there, and only that endpoint's
    // analysis reports it.
    let held = functions_lambda_holds(&router, "eu-west-1").await;
    assert!(
        held.contains(&function_arn_in("eu-west-1", "eu-worker")),
        "AWS Lambda must hold the function created at the other endpoint, got {held:?}"
    );
}

/// Only what was observed INSIDE the look-back period is analysed: the heavier day this
/// function saw 40 days back is outside the service's own 14-day period, so neither the
/// invocations counted, nor the peak reported, nor the memory size asked for carries it. No
/// preference can move that period for a Lambda function.
#[tokio::test]
async fn only_the_runs_inside_the_look_back_period_are_analysed() {
    let (router, _engine) = analysed_emulator().await;

    let body = recommendations(&router, json!({})).await;
    let over = recommendation_for(&body, OVER_PROVISIONED);
    assert_eq!(
        number(&over, "lookbackPeriodInDays"),
        LOOK_BACK_DAYS,
        "the period analysed is the service's own default, got {over}"
    );
    assert_eq!(
        number(&over, "numberOfInvocations"),
        100_000.0,
        "only the invocations inside the period are counted, got {over}"
    );
    assert_eq!(
        metric(&over, "Memory", "Maximum"),
        420.0,
        "the analysed peak is the peak of the runs inside the period, got {over}"
    );
    assert_eq!(
        number(&only_option(&over), "memorySize"),
        504.0,
        "the memory size asked for carries the analysed peak with headroom, got {over}"
    );

    // Rows: each `LookBackPeriodPreference` value the model declares. None of them can be
    // set for a Lambda function — "you can only set this preference for the Amazon EC2
    // instance, Auto Scaling group, Amazon EBS volume, Amazon ECS service and Amazon
    // Aurora and RDS database resource types" — so the period in force cannot be moved and
    // the recommendations do not change.
    for preference in ["DAYS_14", "DAYS_32", "DAYS_93"] {
        let (status, refusal) = call(
            &router,
            "PutRecommendationPreferences",
            json!({"resourceType": "LambdaFunction", "lookBackPeriod": preference}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {preference:?}: a look-back preference is not settable for a Lambda \
             function, got {status}: {refusal}"
        );
        assert_eq!(
            error_code(&refusal),
            "InvalidParameterValueException",
            "case {preference:?}: expected InvalidParameterValueException, got {refusal}"
        );
        assert_eq!(
            recommendations(&router, json!({})).await,
            body,
            "case {preference:?}: a refused preference changes no recommendation"
        );
    }
}

/// Paging never changes WHAT is reported: walking the recommendations `maxResults` at a time
/// yields exactly the unpaged list, in the same order, with a `nextToken` on every page but
/// the last.
#[tokio::test]
async fn paging_walks_the_same_recommendations_in_the_same_order() {
    let (router, _engine) = analysed_emulator().await;

    let unpaged = reported_arns(&recommendations(&router, json!({})).await);
    assert!(
        unpaged.len() > 2,
        "this test needs several recommendations to page over, got {unpaged:?}"
    );

    // Rows: the smallest page, a page that divides the set unevenly, one that holds it all,
    // the modelled upper bound of `maxResults`, its lower bound `0` (no size asked for) and
    // the member left out entirely.
    let page_sizes: [Option<i64>; 6] = [
        Some(1),
        Some(2),
        Some(unpaged.len() as i64),
        Some(1000),
        Some(0),
        None,
    ];
    for page_size in page_sizes {
        let case = format!("maxResults={page_size:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({});
            if let Some(size) = page_size {
                request["maxResults"] = json!(size);
            }
            if let Some(token) = &next_token {
                request["nextToken"] = json!(token);
            }
            let body = recommendations(&router, request).await;
            pages += 1;
            assert!(
                pages <= unpaged.len() + 1,
                "case {case:?}: paging must terminate, got {pages} pages"
            );
            let page = reported_arns(&body);
            if let Some(size) = page_size.filter(|size| *size > 0) {
                assert!(
                    page.len() as i64 <= size,
                    "case {case:?}: a page holds at most {size} recommendations, got {}",
                    page.len()
                );
            }
            walked.extend(page);
            next_token = body["nextToken"].as_str().map(ToString::to_string);
            if next_token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, unpaged,
            "case {case:?}: paging must walk exactly the unpaged recommendations, in order"
        );
        let expected_pages = match page_size.filter(|size| *size > 0) {
            Some(size) => unpaged.len().div_ceil(size as usize),
            None => 1,
        };
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} recommendations, got \
             {pages}",
            unpaged.len()
        );
    }
}

/// What this operation refuses, and with which modelled error: a request whose value is out
/// of range or not valid is a 400 `InvalidParameterValueException`, and one the model itself
/// rejects never reaches the handler.
#[tokio::test]
async fn a_request_that_is_not_valid_is_refused_with_the_modelled_error() {
    let (router, _engine) = analysed_emulator().await;

    let own = ::engine::emulator_account_id().to_string();
    let cases: [(&str, Value, &str); 10] = [
        (
            "more than one account id",
            json!({"accountIds": [own.clone(), "111122223333"]}),
            "InvalidParameterValueException",
        ),
        (
            "an account id that is not 12 digits",
            json!({"accountIds": ["12345"]}),
            "InvalidParameterValueException",
        ),
        (
            "another account's functions",
            json!({"accountIds": ["111122223333"]}),
            "InvalidParameterValueException",
        ),
        (
            "a function ARN of another account",
            json!({"functionArns": ["arn:aws:lambda:us-east-1:111122223333:function:f"]}),
            "InvalidParameterValueException",
        ),
        (
            "a function id that is not an ARN",
            json!({"functionArns": [OVER_PROVISIONED]}),
            "InvalidParameterValueException",
        ),
        (
            "an ARN of another service",
            json!({"functionArns": [format!("arn:aws:ec2:us-east-1:{own}:instance/i-0123456789abcdef0")]}),
            "InvalidParameterValueException",
        ),
        (
            "an ARN with no function name",
            json!({"functionArns": [format!("arn:aws:lambda:us-east-1:{own}:function:")]}),
            "InvalidParameterValueException",
        ),
        (
            "a finding value outside the LambdaFunctionRecommendationFinding value set",
            json!({"filters": [{"name": "Finding", "values": ["Oversized"]}]}),
            "InvalidParameterValueException",
        ),
        (
            "a filter carrying no name",
            json!({"filters": [{"values": ["Optimized"]}]}),
            "InvalidParameterValueException",
        ),
        (
            "a forged nextToken",
            json!({"nextToken": "not-a-token"}),
            "InvalidParameterValueException",
        ),
    ];
    for (case, request, expected) in cases {
        let (status, body) = call(&router, "GetLambdaFunctionRecommendations", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {body}"
        );
    }

    // Rows the MODEL refuses before the handler runs: a filter name outside
    // `LambdaFunctionRecommendationFilterName`, and a `maxResults` outside 0..1000.
    for (case, request) in [
        (
            "a filter name the model does not declare",
            json!({"filters": [{"name": "tag:Owner", "values": ["TeamA"]}]}),
        ),
        (
            "a maxResults above the modelled range",
            json!({"maxResults": 1001}),
        ),
    ] {
        let (status, body) = call(&router, "GetLambdaFunctionRecommendations", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case {case:?}: expected the model's own refusal, got {body}"
        );
    }
}

/// The enrollment gate, and that reading is a READ: nothing is reported before the account
/// opts in, nothing until the analysis its enrollment starts is due, two identical calls
/// answer identically, and reading moves neither the enrollment record nor the preferences.
#[tokio::test]
async fn recommendations_need_an_enrolled_account_and_reading_changes_nothing() {
    let (router, engine) = emulator_with_engine();
    register_analysed_functions(&router, &engine).await;

    let (status, body) = call(&router, "GetLambdaFunctionRecommendations", json!({})).await;
    assert_eq!(
        status, 403,
        "before opting in: expected 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "OptInRequiredException",
        "before opting in: expected OptInRequiredException, got {body}"
    );

    // Opted in, but the analysis an enrollment starts is not due yet: the account reads the
    // empty set rather than an analysis nothing has produced.
    opt_in(&router).await;
    let body = recommendations(&router, json!({})).await;
    assert_eq!(
        reported_arns(&body),
        Vec::<String>::new(),
        "straight after opting in there is no analysis to report, got {body}"
    );

    // The analysis is due because its stored instant is already past, not because the suite
    // waited for the clock to reach it.
    make_analysis_due(&engine);
    let before = call(&router, "GetEnrollmentStatus", json!({})).await.1;
    let first = recommendations(&router, json!({})).await;
    let second = recommendations(&router, json!({})).await;
    assert!(
        !reported_arns(&first).is_empty(),
        "once the analysis is due the account's functions are reported, got {first}"
    );
    assert_eq!(
        first, second,
        "two reads of the same observation must answer identically"
    );
    let after = call(&router, "GetEnrollmentStatus", json!({})).await.1;
    assert_eq!(
        before, after,
        "reading recommendations must not move the enrollment record"
    );
    let preferences = call(
        &router,
        "GetRecommendationPreferences",
        json!({"resourceType": "LambdaFunction"}),
    )
    .await
    .1;
    assert_eq!(
        preferences["recommendationPreferencesDetails"],
        json!([]),
        "reading recommendations must not write a preference, got {preferences}"
    );
    // The functions AWS Lambda holds are untouched by the read, too: a reporting operation
    // writes to no store.
    assert_eq!(
        functions_lambda_holds(&router, REGION).await.len(),
        5,
        "reading recommendations must not touch the account's functions"
    );

    // Opting back out takes the recommendations away again: the gate is the stored
    // enrollment, not whether the account ever enrolled.
    let (status, body) = call(
        &router,
        "UpdateEnrollmentStatus",
        json!({"status": "Inactive"}),
    )
    .await;
    assert_eq!(status, 200, "opting out: expected 200, got {status}: {body}");
    let (status, body) = call(&router, "GetLambdaFunctionRecommendations", json!({})).await;
    assert_eq!(
        status, 403,
        "after opting out: expected 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "OptInRequiredException",
        "after opting out: expected OptInRequiredException, got {body}"
    );
}

/// `accountIds` selects WHOSE functions are reported: absent or the caller's own account
/// reads the caller's, and both answer identically.
#[tokio::test]
async fn account_ids_selects_the_caller_s_own_account() {
    let (router, _engine) = analysed_emulator().await;

    let own = ::engine::emulator_account_id().to_string();
    let absent = recommendations(&router, json!({})).await;
    for request in [json!({"accountIds": []}), json!({"accountIds": [own]})] {
        let body = recommendations(&router, request.clone()).await;
        assert_eq!(
            body, absent,
            "case {request}: expected the caller's own recommendations"
        );
    }
}

/// The per-function reader and `GetRecommendationSummaries` answer from ONE analysis: the
/// summary of the account's Lambda functions counts exactly the functions this reader
/// reports at each finding.
#[tokio::test]
async fn the_summary_of_lambda_functions_counts_what_this_reader_reports() {
    let (router, _engine) = analysed_emulator().await;

    let reported_functions = reported(&recommendations(&router, json!({})).await);
    let (status, summaries) = call(&router, "GetRecommendationSummaries", json!({})).await;
    assert_eq!(
        status, 200,
        "reading the summaries: expected 200, got {status}: {summaries}"
    );
    let lambda = summaries["recommendationSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|summary| summary["recommendationResourceType"] == json!("LambdaFunction"))
        .unwrap_or_else(|| panic!("no LambdaFunction summary: {summaries}"));

    // The findings a summary of Lambda functions reports: "the number of Lambda functions in
    // an account that are NotOptimized, or Optimized".
    for finding in ["NotOptimized", "Optimized"] {
        let counted = lambda["summaries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|entry| entry["name"] == json!(finding))
            .map(|entry| entry["value"].as_f64().unwrap_or(0.0))
            .unwrap_or_else(|| panic!("case {finding:?}: no count in {lambda}"));
        let reported_at = reported_functions
            .iter()
            .filter(|recommendation| recommendation["finding"] == json!(finding))
            .count() as f64;
        assert_eq!(
            counted, reported_at,
            "case {finding:?}: the summary counts the {reported_at} function(s) this reader \
             reports at that finding, got {counted}"
        );
    }
}
