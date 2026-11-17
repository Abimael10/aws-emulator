// SPDX-License-Identifier: CC0-1.0

//! The `AWS/CodeBuild` series a project's builds publish about themselves — read in
//! CloudWatch, where a client reads them.
//!
//! The rule under test: **every build that ENDS publishes, into the `AWS/CodeBuild` namespace
//! dimensioned on exactly the `ProjectName` it belongs to, one `Builds` datapoint of 1 and one
//! `Duration` datapoint in `Seconds`, plus the one outcome counter its status belongs to —
//! `SucceededBuilds` for a build that succeeded, `FailedBuilds` for one that failed because of
//! a client error or because of a timeout, and neither for a build that ended in some other
//! way — with the counters in `Count`, so a client summing a window gets the builds of that
//! window and an alarm on failing builds has something to grade.** Two crates have to agree
//! for that to hold (CodeBuild runs the builds, CloudWatch answers for the series), so the
//! suite lives here: a CodeBuild-only router has no metric store to publish into, and a
//! CloudWatch-only one has nothing publishing.
//!
//! The rows differ along what the rule generalises over: how many builds one project runs
//! (one, two, three, four), which outcomes they reach, in which ORDER they reach them, and
//! WHERE a failure comes from (a non-zero exit of the build phase, a failing command of an
//! earlier phase). A project nothing has built is its own test: it publishes no datapoint and
//! indexes no series at all, so "no build ran" stays distinguishable from "a build ran and
//! failed".
//!
//! Every namespace, metric, dimension and unit name here is written out as a literal, taken
//! from the CodeBuild metric reference rather than from the service crate's constants, so a
//! series published under a name no AWS reader would ask for fails here instead of agreeing
//! with itself.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "eu-west-1";

/// CodeBuild's `X-Amz-Target` prefix and signing name.
const CODEBUILD_TARGET_PREFIX: &str = "CodeBuild_20161006";
/// See [`CODEBUILD_TARGET_PREFIX`].
const CODEBUILD_SIGNING_NAME: &str = "codebuild";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
/// See [`CLOUDWATCH_TARGET_PREFIX`].
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace CodeBuild publishes its own metrics into, and the dimension it reports them
/// per (the CodeBuild metric reference).
const NAMESPACE: &str = "AWS/CodeBuild";
/// See [`NAMESPACE`].
const PROJECT_DIMENSION: &str = "ProjectName";

/// The metrics under test, as that reference names them.
const BUILDS_METRIC: &str = "Builds";
/// See [`BUILDS_METRIC`].
const SUCCEEDED_BUILDS_METRIC: &str = "SucceededBuilds";
/// See [`BUILDS_METRIC`].
const FAILED_BUILDS_METRIC: &str = "FailedBuilds";
/// See [`BUILDS_METRIC`].
const DURATION_METRIC: &str = "Duration";

/// The units those metrics carry: a count of builds is a plain tally, and a duration is a
/// span of time.
const COUNT_UNIT: &str = "Count";
/// See [`COUNT_UNIT`].
const SECONDS_UNIT: &str = "Seconds";

/// The two build outcomes the rows drive, as `StatusType` spells them.
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";

/// The `Period`, in seconds, every read below asks for — the five-minute granularity a client
/// watching build health reads at.
const PERIOD_SECS: i64 = 300;

/// How far back and how far forward of now a read's window reaches, in seconds: the
/// forty-minute window a client polling these series uses, and one period past now so a
/// datapoint stamped in the period that is still open is inside the window.
const WINDOW_BACK_SECS: f64 = 2_400.0;
/// See [`WINDOW_BACK_SECS`].
const WINDOW_FORWARD_SECS: f64 = 300.0;

/// How long a row waits for the transition the clock owes a build, and how long one poll
/// pauses before asking again.
///
/// Generous on purpose: what is waited for is the CONDITION (the build reporting itself
/// complete), never an elapsed time, and the deadline is only here so a build that never
/// completes fails the row instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them, with request authentication off so each call carries
/// a credential scope (naming the Region and the service) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Epoch seconds, the unit a CloudWatch timestamp is read in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs_f64()
}

/// Dispatch one RPC call at `signing_name` and answer its status and decoded body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=not-verified"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodeBuild call, over its awsJson1_1 wire, required to succeed.
async fn codebuild(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        CODEBUILD_SIGNING_NAME,
        CODEBUILD_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CodeBuild {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch call, over its awsJson wire, required to succeed.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A buildspec declaring `commands` for each phase named, in buildspec order.
fn buildspec(phases: &[(&str, &str)]) -> String {
    let mut spec = String::from("version: 0.2\nphases:\n");
    for (phase, command) in phases {
        spec.push_str(&format!("  {phase}:\n    commands:\n      - {command}\n"));
    }
    spec
}

/// What one build of a row runs, and so the outcome it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Every command succeeds.
    Succeeds,
    /// The BUILD phase's command exits non-zero, which fails the build.
    BuildExitsNonZero,
    /// An EARLIER phase's command fails, so the build is aborted before its own build
    /// commands run — a failure of a different phase, still a failed build.
    InstallFails,
}

impl Shape {
    /// The buildspec a build of this shape runs.
    fn buildspec(self) -> String {
        match self {
            Self::Succeeds => buildspec(&[("build", "echo building")]),
            Self::BuildExitsNonZero => buildspec(&[("build", "exit 1")]),
            Self::InstallFails => buildspec(&[("install", "false"), ("build", "echo never")]),
        }
    }
}

/// Create a `NO_SOURCE` / `NO_ARTIFACTS` project whose own source carries `spec` — which a
/// build of it can override with its own.
async fn create_project(router: &MultiServiceRouter, case: &str, name: &str, spec: &str) {
    let created = codebuild(
        router,
        case,
        "CreateProject",
        &json!({
            "name": name,
            "source": {"type": "NO_SOURCE", "buildspec": spec},
            "artifacts": {"type": "NO_ARTIFACTS"},
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                "computeType": "BUILD_GENERAL1_SMALL",
            },
            "serviceRole": "arn:aws:iam::000000000000:role/service-role/codebuild-metrics",
        }),
    )
    .await;
    assert_eq!(
        created["project"]["name"],
        json!(name),
        "case {case:?}: CreateProject must answer the project it created, got {created}"
    );
}

/// Start one build of `project` running `shape`'s buildspec, wait until it reports itself
/// complete, and answer the status it ended in.
async fn run_one_build(
    router: &MultiServiceRouter,
    case: &str,
    project: &str,
    shape: Shape,
) -> String {
    let started = codebuild(
        router,
        case,
        "StartBuild",
        &json!({"projectName": project, "buildspecOverride": shape.buildspec()}),
    )
    .await;
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartBuild must answer a build id, got {started}")
        })
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = codebuild(router, case, "BatchGetBuilds", &json!({"ids": [id.clone()]})).await;
        let build = read["builds"][0].clone();
        if build["buildComplete"] == json!(true) {
            return build["buildStatus"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a complete build states its status, got {build}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: build {id} never completed, last read {build}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The datapoints CloudWatch holds for one series of `project`, read the way a client reads
/// them: the metric of [`NAMESPACE`] dimensioned on that project alone.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    project: &str,
    metric: &str,
) -> Vec<Value> {
    let now = now_secs();
    let reported = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": PROJECT_DIMENSION, "Value": project}],
            "StartTime": now - WINDOW_BACK_SECS,
            "EndTime": now + WINDOW_FORWARD_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "Maximum", "SampleCount"],
        }),
    )
    .await;
    reported["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One statistic totalled over every datapoint of a series — what a client reading the whole
/// window gets for it.
fn total(points: &[Value], statistic: &str) -> f64 {
    points
        .iter()
        .map(|point| point[statistic].as_f64().unwrap_or_default())
        .sum()
}

/// The `Unit` the datapoints of a series carry, as a set, so a series that mixes units is
/// visible rather than averaged away.
fn units(points: &[Value]) -> BTreeSet<String> {
    points
        .iter()
        .map(|point| point["Unit"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `AWS/CodeBuild` metric names `ListMetrics` indexes for `project`, each asserted to be
/// dimensioned on that project alone.
async fn indexed_metrics(
    router: &MultiServiceRouter,
    case: &str,
    project: &str,
) -> BTreeSet<String> {
    let listed = cloudwatch(
        router,
        case,
        "ListMetrics",
        &json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{"Name": PROJECT_DIMENSION, "Value": project}],
        }),
    )
    .await;
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            assert_eq!(
                metric["Dimensions"],
                json!([{"Name": PROJECT_DIMENSION, "Value": project}]),
                "case {case:?}: a project's series is dimensioned on that project alone, got \
                 {metric}"
            );
            metric["MetricName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One row: the builds it runs on ONE project, and what each series must then report.
struct Row {
    case: &'static str,
    /// Each build the row runs, in order, with the status it must reach — the status is
    /// written out rather than derived from the shape, so a buildspec that stops behaving as
    /// this row assumes fails here instead of being believed.
    builds: &'static [(Shape, &'static str)],
    /// What `Builds` must sum to over the window.
    builds_sum: f64,
    /// What `SucceededBuilds` must sum to.
    succeeded_sum: f64,
    /// What `FailedBuilds` must sum to.
    failed_sum: f64,
}

/// RULE: a build that ends publishes one `Builds` and one `Duration` datapoint of its
/// project's series plus the one outcome counter its status belongs to, so the two counters
/// SPLIT the builds between them rather than both counting every build.
#[tokio::test]
async fn a_projects_builds_publish_their_outcome_in_the_codebuild_namespace() {
    let router = emulator();
    let rows: Vec<Row> = vec![
        Row {
            case: "one build that succeeds",
            builds: &[(Shape::Succeeds, SUCCEEDED)],
            builds_sum: 1.0,
            succeeded_sum: 1.0,
            failed_sum: 0.0,
        },
        Row {
            case: "one build whose build phase exits non-zero",
            builds: &[(Shape::BuildExitsNonZero, FAILED)],
            builds_sum: 1.0,
            succeeded_sum: 0.0,
            failed_sum: 1.0,
        },
        Row {
            case: "one build that succeeded and one that failed, on the same project",
            builds: &[
                (Shape::Succeeds, SUCCEEDED),
                (Shape::BuildExitsNonZero, FAILED),
            ],
            builds_sum: 2.0,
            succeeded_sum: 1.0,
            failed_sum: 1.0,
        },
        Row {
            case: "the failure first, then the build that succeeds",
            builds: &[
                (Shape::InstallFails, FAILED),
                (Shape::Succeeds, SUCCEEDED),
            ],
            builds_sum: 2.0,
            succeeded_sum: 1.0,
            failed_sum: 1.0,
        },
        Row {
            case: "two builds that fail in different phases and two that succeed",
            builds: &[
                (Shape::InstallFails, FAILED),
                (Shape::Succeeds, SUCCEEDED),
                (Shape::BuildExitsNonZero, FAILED),
                (Shape::Succeeds, SUCCEEDED),
            ],
            builds_sum: 4.0,
            succeeded_sum: 2.0,
            failed_sum: 2.0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let project = format!("metrics-row-{index}");
        create_project(&router, case, &project, &Shape::Succeeds.buildspec()).await;
        for &(shape, expected) in row.builds {
            let status = run_one_build(&router, case, &project, shape).await;
            assert_eq!(
                status, expected,
                "case {case:?}: a {shape:?} build must end {expected}, got {status}"
            );
        }

        // The counters, each summed and counted over the window a client polls.
        for (metric, expected) in [
            (BUILDS_METRIC, row.builds_sum),
            (SUCCEEDED_BUILDS_METRIC, row.succeeded_sum),
            (FAILED_BUILDS_METRIC, row.failed_sum),
        ] {
            let points = datapoints(&router, case, &project, metric).await;
            let sum = total(&points, "Sum");
            let samples = total(&points, "SampleCount");
            assert_eq!(
                sum, expected,
                "case {case:?}: expected {metric} to sum to {expected}, got {sum} over {points:?}"
            );
            assert_eq!(
                samples, expected,
                "case {case:?}: expected {expected} {metric} observations (one per build), got \
                 {samples} over {points:?}"
            );
            if expected > 0.0 {
                assert_eq!(
                    units(&points),
                    BTreeSet::from([COUNT_UNIT.to_string()]),
                    "case {case:?}: expected every {metric} datapoint in {COUNT_UNIT}, got \
                     {points:?}"
                );
            }
        }

        // The duration of each build that ran, which is a span of time rather than a tally.
        let durations = datapoints(&router, case, &project, DURATION_METRIC).await;
        assert_eq!(
            total(&durations, "SampleCount"),
            row.builds_sum,
            "case {case:?}: expected one {DURATION_METRIC} observation per build \
             ({}), got {durations:?}",
            row.builds_sum
        );
        assert_eq!(
            units(&durations),
            BTreeSet::from([SECONDS_UNIT.to_string()]),
            "case {case:?}: expected every {DURATION_METRIC} datapoint in {SECONDS_UNIT}, got \
             {durations:?}"
        );
        let longest = durations
            .iter()
            .map(|point| point["Maximum"].as_f64().unwrap_or_default())
            .fold(0.0_f64, f64::max);
        assert!(
            longest > 0.0,
            "case {case:?}: a build that ran took a positive {DURATION_METRIC}, got {longest} \
             over {durations:?}"
        );

        // And every series a client can FIND for this project, and nothing else: an outcome
        // counter exists because a build reached that outcome.
        let mut expected_indexed =
            BTreeSet::from([BUILDS_METRIC.to_string(), DURATION_METRIC.to_string()]);
        if row.succeeded_sum > 0.0 {
            expected_indexed.insert(SUCCEEDED_BUILDS_METRIC.to_string());
        }
        if row.failed_sum > 0.0 {
            expected_indexed.insert(FAILED_BUILDS_METRIC.to_string());
        }
        assert_eq!(
            indexed_metrics(&router, case, &project).await,
            expected_indexed,
            "case {case:?}: expected exactly {expected_indexed:?} indexed in {NAMESPACE} for \
             {project:?}"
        );
    }
}

/// RULE (the other side of it): a project nothing has built publishes NOTHING — no datapoint
/// in any of the four series and no series indexed at all — so a client cannot read "no build
/// ran" as a healthy build, nor as a failing one.
///
/// The rows differ along what the project's builds WOULD do, which must make no difference at
/// all, since nothing ran them.
#[tokio::test]
async fn a_project_that_has_run_no_build_publishes_nothing() {
    let router = emulator();
    let case = "a project created but never built";
    for (index, shape) in [
        Shape::Succeeds,
        Shape::BuildExitsNonZero,
        Shape::InstallFails,
    ]
    .iter()
    .enumerate()
    {
        let project = format!("unbuilt-project-{index}");
        create_project(&router, case, &project, &shape.buildspec()).await;
        for metric in [
            BUILDS_METRIC,
            SUCCEEDED_BUILDS_METRIC,
            FAILED_BUILDS_METRIC,
            DURATION_METRIC,
        ] {
            let points = datapoints(&router, case, &project, metric).await;
            assert!(
                points.is_empty(),
                "case {case:?}: project {project:?} must publish no {metric} datapoint, got \
                 {points:?}"
            );
        }
        let indexed = indexed_metrics(&router, case, &project).await;
        assert!(
            indexed.is_empty(),
            "case {case:?}: project {project:?} must index no {NAMESPACE} series, got {indexed:?}"
        );
    }
}
