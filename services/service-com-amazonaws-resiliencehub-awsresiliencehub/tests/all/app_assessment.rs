// SPDX-License-Identifier: CC0-1.0

//! Assessing an application version against its resiliency policy, end to end over
//! HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a
//! restJson1 request — and asserts the response BODY, not just the status.
//!
//! The rule under test: `StartAppAssessment` assesses the application's CURRENT
//! RELEASE against the resiliency policy that application names — the draft, the
//! one other version an application has, is refused with the modelled 400, and a
//! label that addresses no version of it with the modelled 404 — and records an
//! assessment that
//! PINS the published snapshot it graded (its publish sequence number, not the
//! moving alias). Assessing takes time, so the call answers the non-terminal
//! `Pending` and none of the verdict; one `ASSESSMENT_SPAN` later the assessment
//! reports the terminal status, one compliance entry per disruption type the policy
//! states an objective for, the roll-up of those entries and the score they earn.
//! The verdict is frozen at the call, `DescribeAppAssessment` and
//! `ListAppAssessments` report it unchanged however much the draft moves on, and
//! the application's own compliance members follow its newest FINISHED assessment.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

// The sentence an app-version refusal of an assessment states, from the module that
// owns this crate's shared test harness, so there is ONE literal of it in the tests.
use super::application_lifecycle::invalid_app_version_sentence as invalid_app_version;
use service_com_amazonaws_resiliencehub_awsresiliencehub::ops::types::{
    COST_FREQUENCY_VALUES, DISRUPTION_TYPE_VALUES,
};
use service_com_amazonaws_resiliencehub_awsresiliencehub::state::{
    default_published_version_name, recovers_disruption,
    ComAmazonawsResiliencehubAwsresiliencehubState, ASSESSMENT_SPAN, ASSESSMENT_STATUS_PENDING,
    ASSESSMENT_STATUS_SUCCESS, DRAFT_APP_VERSION, DRIFT_STATUS_DETECTED, DRIFT_STATUS_NOT_CHECKED,
    DRIFT_STATUS_NOT_DETECTED, RELEASE_APP_VERSION, SUPPORTED_RESOURCE_TYPES,
};
use service_com_amazonaws_resiliencehub_awsresiliencehub::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below signs for.
const REGION: &str = "us-east-1";

/// The account id `engine::mock_account_id` mints every ARN under.
const ACCOUNT: &str = "000000000000";

/// The `policy` map a case that is not about the disruption set itself submits: an
/// objective for each failure type a resiliency policy has to state — the
/// application (`Software`), infrastructure (`Hardware`) and Availability Zone
/// objectives — each with `seconds` as its RTO and its RPO.
fn required_objectives(seconds: i64) -> Value {
    json!({
        "Software": {"rtoInSecs": seconds, "rpoInSecs": seconds},
        "Hardware": {"rtoInSecs": seconds, "rpoInSecs": seconds},
        "AZ": {"rtoInSecs": seconds, "rpoInSecs": seconds},
    })
}

/// The `ComplianceStatus` values this suite asserts, from the enum the model gives
/// `AppAssessment$complianceStatus` (`smithy_query query="shape"
/// shape="ComplianceStatus"`: PolicyBreached | PolicyMet | NotApplicable |
/// MissingPolicy).
const POLICY_MET: &str = "PolicyMet";
const POLICY_BREACHED: &str = "PolicyBreached";
const MISSING_POLICY: &str = "MissingPolicy";

/// The score one disruption type's met objective earns, and so the score of an
/// application that meets every objective its policy states: a resiliency score is
/// reported as the FRACTION of its maximum that was earned, so meeting everything
/// scores 1.
const FULL_SCORE: f64 = 1.0;

/// An emulator holding only this service, over one shared engine — and that engine,
/// so a case can move the CLOCK an assessment is waiting on instead of sleeping
/// through its span.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These requests carry an unverifiable placeholder signature: this suite tests
    // the operations, not SigV4.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What a dispatched request answered.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype`; empty on success.
    error_code: String,
    body: Value,
}

impl Reply {
    /// The response body, asserting the call succeeded.
    fn ok(&self, case: &str) -> &Value {
        assert_eq!(
            self.status, 200,
            "case {case:?}: expected HTTP 200, got {} {:?} {:?}",
            self.status, self.error_code, self.body
        );
        &self.body
    }

    /// Assert the call was refused with `status` and `code`.
    fn refused(&self, case: &str, status: u16, code: &str) {
        assert_eq!(
            (self.status, self.error_code.as_str()),
            (status, code),
            "case {case:?}: expected HTTP {status} {code}, got {} {:?} {:?}",
            self.status,
            self.error_code,
            self.body
        );
    }
}

/// Dispatch one request through the router, exactly as the server front door does.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    target: &str,
    body: Option<&Value>,
) -> Reply {
    let payload = body.map_or_else(Vec::new, |b| serde_json::to_vec(b).expect("serializable body"));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://resiliencehub.{REGION}.amazonaws.com{target}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/resiliencehub/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        );
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&raw)}))
    };
    Reply {
        status,
        error_code,
        body,
    }
}

async fn post(router: &MultiServiceRouter, target: &str, body: &Value) -> Reply {
    call(router, "POST", target, Some(body)).await
}

async fn get(router: &MultiServiceRouter, target: &str) -> Reply {
    call(router, "GET", target, None).await
}

/// A resiliency policy stating `objectives` (a disruption type to its RTO/RPO
/// target), and its ARN.
async fn create_a_policy(
    router: &MultiServiceRouter,
    policy_name: &str,
    objectives: &Value,
) -> String {
    let reply = post(
        router,
        "/create-resiliency-policy",
        &json!({
            "policyName": policy_name,
            "tier": "Important",
            "policy": objectives,
        }),
    )
    .await;
    reply.ok(policy_name)["policy"]["policyArn"]
        .as_str()
        .expect("a minted policyArn")
        .to_string()
}

/// An application named `name`, pointing at `policy_arn` when the case gives one.
async fn create_an_app(
    router: &MultiServiceRouter,
    name: &str,
    policy_arn: Option<&str>,
) -> String {
    let body = match policy_arn {
        Some(policy_arn) => json!({"name": name, "policyArn": policy_arn}),
        None => json!({"name": name}),
    };
    let reply = post(router, "/create-app", &body).await;
    reply.ok(name)["app"]["appArn"]
        .as_str()
        .expect("a minted appArn")
        .to_string()
}

/// Add one Application Component to `app_arn`'s draft version.
async fn add_a_component(router: &MultiServiceRouter, app_arn: &str, id: &str) {
    let reply = post(
        router,
        "/create-app-version-app-component",
        &json!({
            "appArn": app_arn,
            "id": id,
            "name": id,
            "type": "AWS::ResilienceHub::ComputeAppComponent",
        }),
    )
    .await;
    reply.ok(id);
}

/// The `resourceType` every case that is not about the recovery derivation itself
/// maps: a DNS record, which holds no application data and whose service holds it
/// in more than one Region, so it recovers every disruption a resiliency policy can
/// state an objective for. A case about WHICH resource recovers WHICH disruption
/// picks its own types; see [`recovery_table`].
const FULLY_RECOVERED_RESOURCE_TYPE: &str = "AWS::Route53::RecordSet";

/// Add one physical resource to `app_arn`'s draft version, mapped to `component`.
async fn add_a_resource(
    router: &MultiServiceRouter,
    app_arn: &str,
    identifier: &str,
    component: &str,
) {
    add_a_resource_of_type(
        router,
        app_arn,
        identifier,
        component,
        FULLY_RECOVERED_RESOURCE_TYPE,
    )
    .await;
}

/// Add one physical resource of `resource_type` to `app_arn`'s draft version,
/// mapped to `component`.
async fn add_a_resource_of_type(
    router: &MultiServiceRouter,
    app_arn: &str,
    identifier: &str,
    component: &str,
    resource_type: &str,
) {
    let reply = post(
        router,
        "/create-app-version-resource",
        &json!({
            "appArn": app_arn,
            "resourceName": identifier,
            "logicalResourceId": {"identifier": identifier},
            "physicalResourceId": identifier,
            "resourceType": resource_type,
            "appComponents": [component],
        }),
    )
    .await;
    reply.ok(identifier);
}

/// Publish `app_arn`'s draft, answering the publish sequence number it was minted
/// with.
async fn publish(router: &MultiServiceRouter, app_arn: &str, version_name: Option<&str>) -> i64 {
    let mut body = json!({"appArn": app_arn});
    if let Some(version_name) = version_name {
        body["versionName"] = json!(version_name);
    }
    let reply = post(router, "/publish-app-version", &body).await;
    let published = reply.ok("publish");
    assert_eq!(
        published["appVersion"],
        json!(RELEASE_APP_VERSION),
        "case \"publish\": the publish must answer the release alias, got {published:?}"
    );
    published["identifier"]
        .as_i64()
        .unwrap_or_else(|| panic!("a publish sequence number, got {published:?}"))
}

/// `StartAppAssessment` for one version of one application.
async fn start_assessment(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
    assessment_name: &str,
) -> Reply {
    post(
        router,
        "/start-app-assessment",
        &json!({
            "appArn": app_arn,
            "appVersion": app_version,
            "assessmentName": assessment_name,
        }),
    )
    .await
}

/// `DescribeAppAssessment` for one assessment.
async fn describe_assessment(router: &MultiServiceRouter, assessment_arn: &str) -> Reply {
    post(
        router,
        "/describe-app-assessment",
        &json!({"assessmentArn": assessment_arn}),
    )
    .await
}

/// `DescribeApp` for one application.
async fn describe_app(router: &MultiServiceRouter, app_arn: &str) -> Reply {
    post(router, "/describe-app", &json!({"appArn": app_arn})).await
}

/// Move the clock an assessment is waiting on into the PAST, so a case can read
/// what the finished assessment reports without sleeping through its span.
fn finish_the_assessment(engine: &Arc<engine::InMemoryEngine>, assessment_arn: &str) {
    engine
        .state::<ComAmazonawsResiliencehubAwsresiliencehubState>()
        .lock()
        .assessment_mut(assessment_arn)
        .expect("the assessment the call recorded")
        .completes_at = SystemTime::now() - Duration::from_secs(1);
}

/// The instant the stored assessment finishes.
fn completion_instant(engine: &Arc<engine::InMemoryEngine>, assessment_arn: &str) -> SystemTime {
    engine
        .state::<ComAmazonawsResiliencehubAwsresiliencehubState>()
        .lock()
        .assessment(assessment_arn)
        .expect("the assessment the call recorded")
        .completes_at
}

/// Start an assessment, report it finished, and answer the finished assessment as
/// `DescribeAppAssessment` reports it.
async fn assess(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    app_arn: &str,
    assessment_name: &str,
) -> Value {
    let started = start_assessment(router, app_arn, RELEASE_APP_VERSION, assessment_name).await;
    let assessment_arn = started.ok(assessment_name)["assessment"]["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();
    finish_the_assessment(engine, &assessment_arn);
    describe_assessment(router, &assessment_arn).await.ok(assessment_name)["assessment"].clone()
}

/// The compliance entries an assessment reports, keyed by disruption type.
fn compliance_of(assessment: &Value) -> BTreeMap<String, Value> {
    assessment["compliance"]
        .as_object()
        .unwrap_or_else(|| panic!("a finished assessment carries compliance, got {assessment:?}"))
        .iter()
        .map(|(disruption_type, entry)| (disruption_type.clone(), entry.clone()))
        .collect()
}

/// A row of the verdict table: the objectives the policy states, whether every
/// Application Component of the published version has a resource mapped to it, and
/// the verdict that earns.
struct Verdict {
    label: &'static str,
    /// The policy's disruption objectives, or `None` for an application that names
    /// no policy at all.
    objectives: Option<Value>,
    /// Component ids each holding at least one mapped resource.
    mapped: &'static [&'static str],
    /// Component ids with nothing mapped to them.
    unmapped: &'static [&'static str],
}

/// The table. The rows differ along what the rule generalises over: which disruption
/// types the policy states objectives for beyond the three every policy has to state,
/// the RTO/RPO targets those objectives carry, how much the published version maps,
/// and whether every component of it has something to recover.
fn verdict_table() -> Vec<Verdict> {
    vec![
        Verdict {
            label: "the required objectives, every component mapped",
            objectives: Some(json!({
                "Software": {"rtoInSecs": 3600, "rpoInSecs": 3600},
                "Hardware": {"rtoInSecs": 3600, "rpoInSecs": 3600},
                "AZ": {"rtoInSecs": 3600, "rpoInSecs": 3600},
            })),
            mapped: &["compute-tier"],
            unmapped: &[],
        },
        Verdict {
            label: "three objectives with different targets, every component mapped",
            objectives: Some(json!({
                "Software": {"rtoInSecs": 60, "rpoInSecs": 30},
                "Hardware": {"rtoInSecs": 600, "rpoInSecs": 300},
                "AZ": {"rtoInSecs": 7200, "rpoInSecs": 3600},
            })),
            mapped: &["compute-tier", "storage-tier"],
            unmapped: &[],
        },
        Verdict {
            label: "every disruption type, zero-second targets, every component mapped",
            objectives: Some(json!({
                "Software": {"rtoInSecs": 0, "rpoInSecs": 0},
                "Hardware": {"rtoInSecs": 0, "rpoInSecs": 0},
                "AZ": {"rtoInSecs": 0, "rpoInSecs": 0},
                "Region": {"rtoInSecs": 0, "rpoInSecs": 0},
            })),
            mapped: &["compute-tier"],
            unmapped: &[],
        },
        Verdict {
            label: "the multi-Region objective beside the required three, one component \
                    with nothing mapped to it",
            objectives: Some(json!({
                "Software": {"rtoInSecs": 86400, "rpoInSecs": 3600},
                "Hardware": {"rtoInSecs": 86400, "rpoInSecs": 3600},
                "AZ": {"rtoInSecs": 86400, "rpoInSecs": 3600},
                "Region": {"rtoInSecs": 86400, "rpoInSecs": 3600},
            })),
            mapped: &["compute-tier"],
            unmapped: &["empty-tier"],
        },
        Verdict {
            label: "the required objectives, two components with nothing mapped to them",
            objectives: Some(json!({
                "Software": {"rtoInSecs": 120, "rpoInSecs": 60},
                "Hardware": {"rtoInSecs": 600, "rpoInSecs": 300},
                "AZ": {"rtoInSecs": 86400, "rpoInSecs": 3600},
            })),
            mapped: &["compute-tier"],
            unmapped: &["empty-tier", "other-empty-tier"],
        },
        Verdict {
            label: "no policy to judge against",
            objectives: None,
            mapped: &["compute-tier"],
            unmapped: &[],
        },
    ]
}

/// The whole table: each row's assessment reports one compliance entry per
/// objective the policy states, each entry comparing the objective against what the
/// published version can recover, the roll-up of those entries as the assessment's
/// `complianceStatus`, and the score they earn.
#[tokio::test]
async fn the_verdict_follows_the_policy_and_what_the_published_version_maps() {
    for (index, row) in verdict_table().into_iter().enumerate() {
        let (router, engine) = emulator();
        let case = row.label;

        let policy_arn = match row.objectives.as_ref() {
            Some(objectives) => {
                Some(create_a_policy(&router, &format!("policy-{index}"), objectives).await)
            }
            None => None,
        };
        let app_arn = create_an_app(&router, &format!("app-{index}"), policy_arn.as_deref()).await;

        for component in row.mapped {
            add_a_component(&router, &app_arn, component).await;
            add_a_resource(
                &router,
                &app_arn,
                &format!("{component}-bucket"),
                component,
            )
            .await;
        }
        for component in row.unmapped {
            add_a_component(&router, &app_arn, component).await;
        }
        publish(&router, &app_arn, Some("v1")).await;

        let assessed = assess(&router, &engine, &app_arn, &format!("assessment-{index}")).await;

        // One entry per objective the policy states, and nothing else: an
        // application is judged against the objectives it was given.
        let objectives: BTreeMap<String, Value> = row
            .objectives
            .as_ref()
            .and_then(|objectives| objectives.as_object())
            .map(|objectives| {
                objectives
                    .iter()
                    .map(|(disruption_type, target)| (disruption_type.clone(), target.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let compliance = compliance_of(&assessed);
        assert_eq!(
            compliance.keys().collect::<Vec<&String>>(),
            objectives.keys().collect::<Vec<&String>>(),
            "case {case:?}: expected one compliance entry per stated objective, got {assessed:?}"
        );

        let met = row.unmapped.is_empty();
        for (disruption_type, target) in &objectives {
            let entry = &compliance[disruption_type];
            let expected_status = if met { POLICY_MET } else { POLICY_BREACHED };
            assert_eq!(
                entry["complianceStatus"],
                json!(expected_status),
                "case {case:?}: {disruption_type} must be {expected_status}, got {entry:?}"
            );
            if met {
                // The recovery times are what the assessment MEASURED on the
                // version's resources, not the objectives they were judged
                // against: every resource of these rows recovers every disruption
                // where it stands, which takes no time at all.
                assert_eq!(
                    (
                        entry["currentRtoInSecs"].clone(),
                        entry["currentRpoInSecs"].clone(),
                        entry["achievableRtoInSecs"].clone(),
                        entry["achievableRpoInSecs"].clone()
                    ),
                    (json!(0), json!(0), json!(0), json!(0)),
                    "case {case:?}: an absorbed {disruption_type} disruption is recovered in \
                     no time, got {entry:?}"
                );
            } else {
                // The breach says which component has nothing to recover — the
                // first of them, so the same state earns the same sentence.
                let message = entry["message"].as_str().unwrap_or_default();
                assert!(
                    message.contains(row.unmapped[0]),
                    "case {case:?}: the breach of {disruption_type} must name the component \
                     {:?} that has nothing mapped to it, got {entry:?}",
                    row.unmapped[0]
                );
                // Nothing recovers a component with nothing mapped to it, and
                // nothing recommended for it would until a resource is: so
                // neither the current nor the achievable recovery reaches the
                // objective, and each is reported one second past the objective it
                // was judged by.
                let past_rto = target["rtoInSecs"].as_i64().unwrap_or_default() + 1;
                let past_rpo = target["rpoInSecs"].as_i64().unwrap_or_default() + 1;
                assert_eq!(
                    (
                        entry["currentRtoInSecs"].clone(),
                        entry["currentRpoInSecs"].clone(),
                        entry["achievableRtoInSecs"].clone(),
                        entry["achievableRpoInSecs"].clone()
                    ),
                    (
                        json!(past_rto),
                        json!(past_rpo),
                        json!(past_rto),
                        json!(past_rpo)
                    ),
                    "case {case:?}: an unreached {disruption_type} recovery is reported one \
                     second past the objective, got {entry:?}"
                );
                assert!(
                    entry["currentRtoInSecs"].as_i64().unwrap_or_default()
                        > target["rtoInSecs"].as_i64().unwrap_or_default(),
                    "case {case:?}: a breached {disruption_type} objective must report a \
                     current RTO past the target, got {entry:?}"
                );
            }
        }

        let expected_rollup = match (row.objectives.is_some(), met) {
            (false, _) => MISSING_POLICY,
            (true, true) => POLICY_MET,
            (true, false) => POLICY_BREACHED,
        };
        assert_eq!(
            assessed["complianceStatus"],
            json!(expected_rollup),
            "case {case:?}: the assessment's roll-up must be {expected_rollup}, got {assessed:?}"
        );

        let expected_score = if objectives.is_empty() || !met {
            0.0
        } else {
            FULL_SCORE
        };
        assert_eq!(
            assessed["resiliencyScore"]["score"].as_f64(),
            Some(expected_score),
            "case {case:?}: the score must be {expected_score}, got {assessed:?}"
        );
        for disruption_type in objectives.keys() {
            assert_eq!(
                assessed["resiliencyScore"]["disruptionScore"][disruption_type].as_f64(),
                Some(if met { FULL_SCORE } else { 0.0 }),
                "case {case:?}: {disruption_type} must score what its verdict earns, got \
                 {assessed:?}"
            );
        }

        // The policy the verdict was reached against is the one the application
        // names, carried with the assessment; an application with no policy carries
        // none.
        assert_eq!(
            assessed["policy"]["policyArn"].as_str(),
            policy_arn.as_deref(),
            "case {case:?}: the assessment must carry the policy it judged against, got \
             {assessed:?}"
        );

        // A read changes nothing a later caller sees.
        let again = describe_assessment(
            &router,
            assessed["assessmentArn"].as_str().expect("an assessmentArn"),
        )
        .await;
        assert_eq!(
            again.ok(case)["assessment"], assessed,
            "case {case:?}: a repeated read must answer identically"
        );
    }
}

/// An assessment starts non-terminal and carries no verdict until its span has
/// elapsed, and the span is the one the service records — never zero.
#[tokio::test]
async fn an_assessment_reports_no_verdict_until_it_has_finished() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "pending-policy",
        &required_objectives(3600),
    )
    .await;
    let app_arn = create_an_app(&router, "pending-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "bucket", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    let before = SystemTime::now();
    let started = start_assessment(&router, &app_arn, RELEASE_APP_VERSION, "pending-run").await;
    let after = SystemTime::now();
    let started = started.ok("the started assessment")["assessment"].clone();
    let assessment_arn = started["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();

    assert!(
        assessment_arn.starts_with(&format!(
            "arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app-assessment/"
        )),
        "case \"the started assessment\": expected an app-assessment ARN in this region, \
         got {assessment_arn:?}"
    );

    for (case, reported) in [
        ("the start", started.clone()),
        (
            "a read while it runs",
            describe_assessment(&router, &assessment_arn)
                .await
                .ok("a read while it runs")["assessment"]
                .clone(),
        ),
    ] {
        assert_eq!(
            reported["assessmentStatus"],
            json!(ASSESSMENT_STATUS_PENDING),
            "case {case:?}: an assessment that has not finished is {ASSESSMENT_STATUS_PENDING}, \
             got {reported:?}"
        );
        for member in [
            "compliance",
            "complianceStatus",
            "resiliencyScore",
            "endTime",
            "cost",
        ] {
            assert_eq!(
                reported[member],
                Value::Null,
                "case {case:?}: an unfinished assessment reports no {member}, got {reported:?}"
            );
        }
        // Evaluating drift is part of assessing, so an assessment that has not
        // finished has not asked the drift question yet.
        assert_eq!(
            reported["driftStatus"],
            json!(DRIFT_STATUS_NOT_CHECKED),
            "case {case:?}: an unfinished assessment has checked no drift, got {reported:?}"
        );
        assert_eq!(
            reported["appArn"].as_str(),
            Some(app_arn.as_str()),
            "case {case:?}: the assessment names the application it is of, got {reported:?}"
        );
        assert_eq!(
            reported["invoker"],
            json!("User"),
            "case {case:?}: an assessment a caller started was invoked by the user, got \
             {reported:?}"
        );
    }

    // The span is the recorded one, measured from the call.
    let completes_at = completion_instant(&engine, &assessment_arn);
    assert!(
        completes_at >= before + ASSESSMENT_SPAN && completes_at <= after + ASSESSMENT_SPAN,
        "case \"the span\": the assessment must finish one {ASSESSMENT_SPAN:?} after the call, \
         which ran between {before:?} and {after:?}, got {completes_at:?}"
    );

    finish_the_assessment(&engine, &assessment_arn);
    let finished = describe_assessment(&router, &assessment_arn).await;
    let finished = finished.ok("the finished assessment")["assessment"].clone();
    assert_eq!(
        finished["assessmentStatus"],
        json!(ASSESSMENT_STATUS_SUCCESS),
        "case \"the finished assessment\": expected {ASSESSMENT_STATUS_SUCCESS}, got {finished:?}"
    );
    assert_eq!(
        finished["complianceStatus"],
        json!(POLICY_MET),
        "case \"the finished assessment\": the verdict arrives with the terminal status, got \
         {finished:?}"
    );
    assert!(
        finished["endTime"].as_f64().is_some(),
        "case \"the finished assessment\": a finished assessment reports when it ended, got \
         {finished:?}"
    );
}

/// Each assessment pins the published snapshot it graded — the publish sequence
/// number the release alias resolved to at the call, never the alias itself — and
/// keeps reporting that snapshot's verdict however much the draft moves on
/// afterwards.
#[tokio::test]
async fn an_assessment_pins_the_published_version_it_graded() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "pinning-policy",
        &required_objectives(3600),
    )
    .await;
    let app_arn = create_an_app(&router, "pinning-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;

    // The rows differ along the publish they assess: the first, the second and the
    // third release of one application, each with its own `versionName`.
    let rows: &[(&str, Option<&str>)] = &[
        ("the first release", None),
        ("the second release", Some("v2")),
        ("the third release", Some("v3.0.0")),
    ];

    let mut graded: Vec<(&str, String, i64, String)> = Vec::new();
    for (index, (case, version_name)) in rows.iter().enumerate() {
        add_a_resource(
            &router,
            &app_arn,
            &format!("record-{index}"),
            "compute-tier",
        )
        .await;
        let sequence = publish(&router, &app_arn, *version_name).await;
        // A publish that named no version still froze a NAMED snapshot: the
        // service names it after the publish sequence number it was minted with.
        let expected_name = version_name
            .map(str::to_string)
            .unwrap_or_else(|| default_published_version_name(sequence));

        let assessed = assess(&router, &engine, &app_arn, &format!("run-{index}")).await;
        assert_eq!(
            assessed["appVersion"],
            json!(sequence.to_string()),
            "case {case:?}: the assessment must pin the published version {sequence}, got \
             {assessed:?}"
        );
        assert_eq!(
            assessed["versionName"].as_str(),
            Some(expected_name.as_str()),
            "case {case:?}: the assessment carries the published version's own name, got \
             {assessed:?}"
        );
        graded.push((
            case,
            assessed["assessmentArn"]
                .as_str()
                .expect("an assessmentArn")
                .to_string(),
            sequence,
            expected_name,
        ));

        // Every earlier assessment still reports the snapshot IT graded.
        for (earlier, assessment_arn, earlier_sequence, earlier_name) in &graded {
            let reread = describe_assessment(&router, assessment_arn).await;
            let reread = reread.ok(earlier)["assessment"].clone();
            assert_eq!(
                (reread["appVersion"].clone(), reread["versionName"].as_str()),
                (
                    json!(earlier_sequence.to_string()),
                    Some(earlier_name.as_str())
                ),
                "case {case:?}: {earlier} must still report version {earlier_sequence}, got \
                 {reread:?}"
            );
        }
    }
}

/// Only the release alias is assessable: the draft is a version the application
/// HAS, so it is refused with the modelled 400 naming the one version this
/// operation assesses, while a label that addresses no version of the application
/// at all — a superseded number, a number nothing minted, a label of another shape,
/// the alias in another case, or the alias before any publish — is the modelled
/// 404, as is an application that does not exist.
#[tokio::test]
async fn only_the_current_release_is_assessable() {
    let (router, _engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "refusal-policy",
        &required_objectives(3600),
    )
    .await;

    // An application with nothing published has no release to assess.
    let unpublished_name = "unpublished-app";
    let unpublished = create_an_app(&router, unpublished_name, Some(&policy_arn)).await;
    let refused = start_assessment(&router, &unpublished, RELEASE_APP_VERSION, "no-release").await;
    refused.refused(
        "the release alias before any publish",
        404,
        "ResourceNotFoundException",
    );
    assert_eq!(
        refused.body["message"].as_str(),
        Some(
            invalid_app_version(RELEASE_APP_VERSION, unpublished_name, &unpublished).as_str()
        ),
        "case \"the release alias before any publish\": the refusal must state the \
         precondition and name the application, got {:?}",
        refused.body
    );

    let app_name = "refusal-app";
    let app_arn = create_an_app(&router, app_name, Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "bucket", "compute-tier").await;
    publish(&router, &app_arn, None).await;
    add_a_resource(&router, &app_arn, "second-bucket", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    // The draft is a version this application HAS — it is just not the one an
    // assessment grades.
    let refused = start_assessment(&router, &app_arn, DRAFT_APP_VERSION, "refused-run").await;
    refused.refused("the draft", 400, "ValidationException");
    assert_eq!(
        refused.body["message"].as_str(),
        Some(
            format!(
                "AppAssessment version {DRAFT_APP_VERSION} is invalid, must be \
                 {RELEASE_APP_VERSION}"
            )
            .as_str()
        ),
        "case \"the draft\": the refusal must name the version asked for and the one \
         version that is assessable, got {:?}",
        refused.body
    );

    // The rows differ along the form of a label that addresses no version: a
    // superseded publish number, a number no publish minted, a label of another
    // shape, and the alias in another case.
    let rows: &[(&str, &str)] = &[
        ("a superseded publish number", "1"),
        ("a publish number nothing minted", "99"),
        ("a label the application never had", "v1"),
        ("the alias in another case", "RELEASE"),
    ];

    for (case, app_version) in rows {
        let refused = start_assessment(&router, &app_arn, app_version, "refused-run").await;
        refused.refused(case, 404, "ResourceNotFoundException");
        assert_eq!(
            refused.body["message"].as_str(),
            Some(invalid_app_version(app_version, app_name, &app_arn).as_str()),
            "case {case:?}: the refusal must state the precondition and name the \
             application, got {:?}",
            refused.body
        );
    }

    // The release alias of the same application is assessable, so the refusals above
    // are about the label and not about the application.
    start_assessment(&router, &app_arn, RELEASE_APP_VERSION, "accepted-run")
        .await
        .ok("the release alias");

    // An application that does not exist, and an assessment nothing minted.
    let unknown_app =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app/00000000-0000-0000-0000-000000000000");
    start_assessment(&router, &unknown_app, RELEASE_APP_VERSION, "unknown-app")
        .await
        .refused(
            "an application that was never created",
            404,
            "ResourceNotFoundException",
        );
    describe_assessment(
        &router,
        &format!(
            "arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app-assessment/\
             00000000-0000-0000-0000-000000000000"
        ),
    )
    .await
    .refused(
        "an assessment that was never started",
        404,
        "ResourceNotFoundException",
    );
}

/// What an application reports about its compliance follows its newest FINISHED
/// assessment — in `DescribeApp` and in `ListApps` alike — and an application
/// nothing has assessed yet reports that it has not been assessed.
#[tokio::test]
async fn the_application_reports_its_newest_finished_assessment() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "rollup-policy",
        &required_objectives(3600),
    )
    .await;
    let app_arn = create_an_app(&router, "rollup-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "bucket", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    // Before any assessment: not assessed, no score, no evaluation time.
    let fresh = describe_app(&router, &app_arn).await;
    let fresh = fresh.ok("before any assessment")["app"].clone();
    assert_eq!(
        (
            fresh["complianceStatus"].clone(),
            fresh["resiliencyScore"].as_f64(),
            fresh["lastAppComplianceEvaluationTime"].clone()
        ),
        (json!("NotAssessed"), Some(0.0), Value::Null),
        "case \"before any assessment\": an unassessed application reports no verdict, got \
         {fresh:?}"
    );

    // An assessment that has not finished is no verdict either.
    let started = start_assessment(&router, &app_arn, RELEASE_APP_VERSION, "met-run").await;
    let first_arn = started.ok("a running assessment")["assessment"]["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();
    let while_running = describe_app(&router, &app_arn).await;
    assert_eq!(
        while_running.ok("a running assessment")["app"]["complianceStatus"],
        json!("NotAssessed"),
        "case \"a running assessment\": a verdict nothing has reached must not reach the \
         application, got {:?}",
        while_running.body
    );

    // Once it finishes, the application reports that verdict, its score and when it
    // was evaluated — through every reader of an application.
    finish_the_assessment(&engine, &first_arn);
    let finished = describe_assessment(&router, &first_arn).await;
    let finished = finished.ok("the finished assessment")["assessment"].clone();
    for (case, app) in [
        (
            "DescribeApp after the assessment",
            describe_app(&router, &app_arn).await.ok("describe")["app"].clone(),
        ),
        (
            "ListApps after the assessment",
            get(&router, "/list-apps").await.ok("list")["appSummaries"][0].clone(),
        ),
    ] {
        assert_eq!(
            (
                app["complianceStatus"].clone(),
                app["resiliencyScore"].as_f64(),
                app["lastAppComplianceEvaluationTime"].clone()
            ),
            (
                json!(POLICY_MET),
                Some(FULL_SCORE),
                finished["endTime"].clone()
            ),
            "case {case:?}: the application must report the finished assessment's verdict, \
             score and evaluation time, got {app:?}"
        );
        // The application's own recovery times are the worst the assessment
        // measured, and running that assessment is what evaluated drift.
        assert_eq!(
            (
                app["rtoInSecs"].as_i64(),
                app["rpoInSecs"].as_i64(),
                app["driftStatus"].clone()
            ),
            (Some(0), Some(0), json!(DRIFT_STATUS_NOT_DETECTED)),
            "case {case:?}: the application must report the recovery it was measured at and \
             the drift the assessment evaluated, got {app:?}"
        );
    }

    // `lastDriftEvaluationTime` is the application record's own member — the
    // instant the assessment that evaluated drift finished.
    let described = describe_app(&router, &app_arn).await.ok("describe")["app"].clone();
    assert_eq!(
        described["lastDriftEvaluationTime"],
        finished["endTime"].clone(),
        "case \"DescribeApp after the assessment\": the application must report when drift \
         was last evaluated, got {described:?}"
    );

    // A NEWER assessment of a version that breaches the policy replaces the verdict.
    add_a_component(&router, &app_arn, "empty-tier").await;
    publish(&router, &app_arn, None).await;
    let breached = assess(&router, &engine, &app_arn, "breached-run").await;
    assert_eq!(
        breached["complianceStatus"],
        json!(POLICY_BREACHED),
        "case \"the breaching assessment\": a component with nothing mapped to it breaches \
         the policy, got {breached:?}"
    );
    let after = describe_app(&router, &app_arn).await;
    let after = after.ok("after the breaching assessment")["app"].clone();
    assert_eq!(
        (
            after["complianceStatus"].clone(),
            after["resiliencyScore"].as_f64(),
            after["lastAppComplianceEvaluationTime"].clone()
        ),
        (json!(POLICY_BREACHED), Some(0.0), breached["endTime"].clone()),
        "case \"after the breaching assessment\": the application reports its NEWEST verdict, \
         got {after:?}"
    );
    // The worst recovery the newer assessment measured is one second past the
    // objective it judged, and the compliance it found DOES deviate from the
    // previous assessment's — which is a drift.
    assert_eq!(
        (
            after["rtoInSecs"].as_i64(),
            after["rpoInSecs"].as_i64(),
            after["driftStatus"].clone()
        ),
        (Some(3601), Some(3601), json!(DRIFT_STATUS_DETECTED)),
        "case \"after the breaching assessment\": the application reports the recovery the \
         newest assessment measured and the drift it detected, got {after:?}"
    );
}

/// `ListAppAssessments` answers the assessments of the application asked for,
/// oldest start first, narrowed by each filter the request carries and paged with a
/// `nextToken`; the assessments of another application are no part of the answer,
/// and deleting an application forgets its assessments.
#[tokio::test]
async fn assessments_are_listed_filtered_and_paged() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "listing-policy",
        &required_objectives(3600),
    )
    .await;
    let app_arn = create_an_app(&router, "listing-app", Some(&policy_arn)).await;
    let other_arn = create_an_app(&router, "other-app", Some(&policy_arn)).await;
    for arn in [&app_arn, &other_arn] {
        add_a_component(&router, arn, "compute-tier").await;
        add_a_resource(&router, arn, "bucket", "compute-tier").await;
        publish(&router, arn, None).await;
    }

    // Three assessments of one application: two finished, one still running — so a
    // status filter has both sides to choose between.
    let mut finished: Vec<String> = Vec::new();
    for name in ["first-run", "second-run"] {
        let assessed = assess(&router, &engine, &app_arn, name).await;
        finished.push(
            assessed["assessmentArn"]
                .as_str()
                .expect("an assessmentArn")
                .to_string(),
        );
    }
    let running = start_assessment(&router, &app_arn, RELEASE_APP_VERSION, "running-run").await;
    let running = running.ok("a running assessment")["assessment"]["assessmentArn"]
        .as_str()
        .expect("an assessmentArn")
        .to_string();
    // And one of the other application, which must never appear below.
    let elsewhere = assess(&router, &engine, &other_arn, "elsewhere-run").await;

    let all: Vec<String> = finished
        .iter()
        .cloned()
        .chain(std::iter::once(running.clone()))
        .collect();

    // The rows differ along the filter: none, each filtering member in turn, and the
    // ordering flag.
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "no filter but the application",
            format!("/list-app-assessments?appArn={app_arn}"),
            all.clone(),
        ),
        (
            "reverseOrder",
            format!("/list-app-assessments?appArn={app_arn}&reverseOrder=true"),
            all.iter().rev().cloned().collect(),
        ),
        (
            "assessmentName",
            format!("/list-app-assessments?appArn={app_arn}&assessmentName=second-run"),
            vec![finished[1].clone()],
        ),
        (
            "assessmentStatus Success",
            format!(
                "/list-app-assessments?appArn={app_arn}&assessmentStatus={ASSESSMENT_STATUS_SUCCESS}"
            ),
            finished.clone(),
        ),
        (
            "assessmentStatus Pending",
            format!(
                "/list-app-assessments?appArn={app_arn}&assessmentStatus={ASSESSMENT_STATUS_PENDING}"
            ),
            vec![running.clone()],
        ),
        (
            "complianceStatus PolicyMet",
            format!("/list-app-assessments?appArn={app_arn}&complianceStatus={POLICY_MET}"),
            finished.clone(),
        ),
        (
            "complianceStatus PolicyBreached",
            format!("/list-app-assessments?appArn={app_arn}&complianceStatus={POLICY_BREACHED}"),
            Vec::new(),
        ),
        (
            "invoker User",
            format!("/list-app-assessments?appArn={app_arn}&invoker=User"),
            all.clone(),
        ),
        (
            "invoker System",
            format!("/list-app-assessments?appArn={app_arn}&invoker=System"),
            Vec::new(),
        ),
        (
            "another application",
            format!("/list-app-assessments?appArn={other_arn}"),
            vec![elsewhere["assessmentArn"]
                .as_str()
                .expect("an assessmentArn")
                .to_string()],
        ),
    ];

    for (case, target, expected) in rows {
        let listed = get(&router, &target).await;
        let answered = listed.ok(case)["assessmentSummaries"].clone();
        let arns: Vec<String> = answered
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: a summaries array, got {answered:?}"))
            .iter()
            .map(|summary| {
                summary["assessmentArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            arns, expected,
            "case {case:?}: expected exactly {expected:?} in that order, got {answered:?}"
        );
    }

    // Paging: one summary per page walks the whole list exactly once.
    let mut walked: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let target = match next_token.as_deref() {
            None => format!("/list-app-assessments?appArn={app_arn}&maxResults=1"),
            Some(token) => {
                format!("/list-app-assessments?appArn={app_arn}&maxResults=1&nextToken={token}")
            }
        };
        let page = get(&router, &target).await;
        let body = page.ok("one summary per page").clone();
        let summaries = body["assessmentSummaries"]
            .as_array()
            .expect("a summaries array")
            .clone();
        assert!(
            summaries.len() <= 1,
            "case \"one summary per page\": maxResults=1 must answer at most one summary, got \
             {summaries:?}"
        );
        walked.extend(
            summaries
                .iter()
                .map(|summary| summary["assessmentArn"].as_str().unwrap_or_default().to_string()),
        );
        next_token = body["nextToken"].as_str().map(str::to_string);
        if next_token.is_none() {
            break;
        }
    }
    assert_eq!(
        walked, all,
        "case \"one summary per page\": paging must walk every assessment once, in order, got \
         {walked:?}"
    );

    // A token this operation did not issue is refused rather than guessed at.
    get(
        &router,
        &format!("/list-app-assessments?appArn={app_arn}&nextToken=not-a-token"),
    )
    .await
    .refused("a token nothing issued", 400, "ValidationException");

    // An application that does not exist has no assessments to list — and says so
    // as a 404, not as an empty page.
    let unknown_app =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app/00000000-0000-0000-0000-000000000000");
    get(
        &router,
        &format!("/list-app-assessments?appArn={unknown_app}"),
    )
    .await
    .refused(
        "an application that was never created",
        404,
        "ResourceNotFoundException",
    );

    // Deleting an application forgets the assessments OF it, and only those.
    post(&router, "/delete-app", &json!({"appArn": app_arn}))
        .await
        .ok("delete the assessed application");
    for assessment_arn in &all {
        describe_assessment(&router, assessment_arn)
            .await
            .refused(
                "an assessment of a deleted application",
                404,
                "ResourceNotFoundException",
            );
    }
    let elsewhere_arn = elsewhere["assessmentArn"].as_str().expect("an assessmentArn");
    describe_assessment(&router, elsewhere_arn)
        .await
        .ok("another application's assessment survives");
}

/// A replayed `clientToken` answers the assessment its first call started; the same
/// token with other parameters is a conflict.
#[tokio::test]
async fn a_replayed_client_token_answers_the_first_assessment() {
    let (router, _engine) = emulator();
    let policy_arn = create_a_policy(
        &router,
        "token-policy",
        &required_objectives(3600),
    )
    .await;
    let app_arn = create_an_app(&router, "token-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "bucket", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    let request = json!({
        "appArn": app_arn,
        "appVersion": RELEASE_APP_VERSION,
        "assessmentName": "tokened-run",
        "clientToken": "token-for-one-assessment",
    });
    let first = post(&router, "/start-app-assessment", &request).await;
    let first_arn = first.ok("the first call")["assessment"]["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();

    let replayed = post(&router, "/start-app-assessment", &request).await;
    assert_eq!(
        replayed.ok("the replayed token")["assessment"]["assessmentArn"].as_str(),
        Some(first_arn.as_str()),
        "case \"the replayed token\": the first assessment must be answered again, got {:?}",
        replayed.body
    );

    let mut other = request.clone();
    other["assessmentName"] = json!("another-run");
    post(&router, "/start-app-assessment", &other)
        .await
        .refused(
            "the same token with other parameters",
            409,
            "ConflictException",
        );
}

/// The objectives every row of [`recovery_table`] is judged against: one per
/// disruption type a resiliency policy can state an objective for, each a different
/// number, so a reported recovery time can be told apart from the objective it was
/// compared with.
fn graded_objectives() -> Value {
    json!({
        "Software": {"rtoInSecs": 60, "rpoInSecs": 30},
        "Hardware": {"rtoInSecs": 600, "rpoInSecs": 300},
        "AZ": {"rtoInSecs": 7200, "rpoInSecs": 3600},
        "Region": {"rtoInSecs": 86400, "rpoInSecs": 43200},
    })
}

/// A row of the recovery table: the resource types the assessed version maps, and
/// the disruption types those resources recover from.
struct Recovery {
    label: &'static str,
    resource_types: &'static [&'static str],
    recovers: &'static [&'static str],
}

/// The table. Each row's expectation comes from the service's own recovery
/// catalogue (`crate::state`'s `RESOURCE_RECOVERY`, one row per entry of
/// `SUPPORTED_RESOURCE_TYPES`): a resource recovers a software fault only where it
/// holds no application data to lose to one, and any broader disruption only where
/// its own service's redundancy spans that scope. The rows differ along what the
/// rule generalises over: how wide that redundancy reaches (a multi-Region record, a
/// multi-Region table, a regional store, a single-Zone database, a single host),
/// whether the resource holds data, and how many resources the version maps — none
/// of them the `AWS::S3::Bucket` the issue cited alone.
fn recovery_table() -> Vec<Recovery> {
    vec![
        Recovery {
            label: "a DNS record: no data, held in every Region",
            resource_types: &["AWS::Route53::RecordSet"],
            recovers: &["Software", "Hardware", "AZ", "Region"],
        },
        Recovery {
            label: "a global table: data, replicated across Regions",
            resource_types: &["AWS::DynamoDB::GlobalTable"],
            recovers: &["Hardware", "AZ", "Region"],
        },
        Recovery {
            label: "a serverless function: no data, regional service",
            resource_types: &["AWS::Lambda::Function"],
            recovers: &["Software", "Hardware", "AZ"],
        },
        Recovery {
            label: "a bucket: data, regional service",
            resource_types: &["AWS::S3::Bucket"],
            recovers: &["Hardware", "AZ"],
        },
        Recovery {
            label: "a single database instance: data, one Availability Zone",
            resource_types: &["AWS::RDS::DBInstance"],
            recovers: &["Hardware"],
        },
        Recovery {
            label: "a single compute instance: no data, lost with its host",
            resource_types: &["AWS::EC2::Instance"],
            recovers: &["Software"],
        },
        Recovery {
            label: "a function beside a bucket: the application recovers only what \
                    BOTH of them do",
            resource_types: &["AWS::Lambda::Function", "AWS::S3::Bucket"],
            recovers: &["Hardware", "AZ"],
        },
    ]
}

/// The whole table: a compliance entry's recovery times are MEASURED on the
/// resources the assessed version maps — a disruption those resources absorb where
/// they stand is recovered in no time at all, one they do not recover from is
/// reported one second past the objective it was judged by — and the entry's verdict
/// is that comparison. No reported recovery time is a copy of the objective, so an
/// application whose every component is mapped can still breach its policy.
#[tokio::test]
async fn the_recovery_an_assessment_measures_follows_the_version_resources() {
    let objectives = graded_objectives();
    for (index, row) in recovery_table().into_iter().enumerate() {
        let (router, engine) = emulator();
        let case = row.label;

        let policy_arn = create_a_policy(&router, &format!("recovery-policy-{index}"), &objectives)
            .await;
        let app_arn = create_an_app(&router, &format!("recovery-app-{index}"), Some(&policy_arn))
            .await;
        add_a_component(&router, &app_arn, "the-tier").await;
        for (position, resource_type) in row.resource_types.iter().enumerate() {
            add_a_resource_of_type(
                &router,
                &app_arn,
                &format!("resource-{position}"),
                "the-tier",
                resource_type,
            )
            .await;
        }
        publish(&router, &app_arn, None).await;

        let assessed = assess(&router, &engine, &app_arn, &format!("recovery-run-{index}")).await;
        let compliance = compliance_of(&assessed);
        let objectives = objectives.as_object().expect("a JSON object");
        for (disruption_type, target) in objectives {
            let entry = &compliance[disruption_type];
            let recovered = row.recovers.contains(&disruption_type.as_str());
            let expected_status = if recovered { POLICY_MET } else { POLICY_BREACHED };
            assert_eq!(
                entry["complianceStatus"],
                json!(expected_status),
                "case {case:?}: {disruption_type} must be {expected_status}, got {entry:?}"
            );
            let expected_current = if recovered {
                (json!(0), json!(0))
            } else {
                (
                    json!(target["rtoInSecs"].as_i64().unwrap_or_default() + 1),
                    json!(target["rpoInSecs"].as_i64().unwrap_or_default() + 1),
                )
            };
            assert_eq!(
                (
                    entry["currentRtoInSecs"].clone(),
                    entry["currentRpoInSecs"].clone()
                ),
                expected_current,
                "case {case:?}: the {disruption_type} recovery measured on the version must \
                 be {expected_current:?}, got {entry:?}"
            );
            // Whatever the version recovers today, the recovery its resources
            // COULD be given is the absorbed one: every mapped type can be given
            // the recovery its class needs.
            assert_eq!(
                (
                    entry["achievableRtoInSecs"].clone(),
                    entry["achievableRpoInSecs"].clone()
                ),
                (json!(0), json!(0)),
                "case {case:?}: the achievable {disruption_type} recovery of a mapped \
                 component is the absorbed one, got {entry:?}"
            );
            if !recovered {
                let message = entry["message"].as_str().unwrap_or_default();
                assert!(
                    row.resource_types
                        .iter()
                        .any(|resource_type| message.contains(resource_type)),
                    "case {case:?}: the breached {disruption_type} entry must name a resource \
                     that does not recover it, got {entry:?}"
                );
            }
        }

        let every_disruption_recovered = objectives
            .keys()
            .all(|disruption_type| row.recovers.contains(&disruption_type.as_str()));
        let expected_rollup = if every_disruption_recovered {
            POLICY_MET
        } else {
            POLICY_BREACHED
        };
        assert_eq!(
            assessed["complianceStatus"],
            json!(expected_rollup),
            "case {case:?}: the roll-up must be {expected_rollup}, got {assessed:?}"
        );

        // The application reports the WORST recovery the assessment measured.
        let worst = objectives
            .iter()
            .filter(|(disruption_type, _)| !row.recovers.contains(&disruption_type.as_str()))
            .map(|(_, target)| {
                (
                    target["rtoInSecs"].as_i64().unwrap_or_default() + 1,
                    target["rpoInSecs"].as_i64().unwrap_or_default() + 1,
                )
            })
            .fold((0, 0), |worst, measured| {
                (worst.0.max(measured.0), worst.1.max(measured.1))
            });
        let app = describe_app(&router, &app_arn).await.ok(case)["app"].clone();
        assert_eq!(
            (app["rtoInSecs"].as_i64(), app["rpoInSecs"].as_i64()),
            (Some(worst.0), Some(worst.1)),
            "case {case:?}: the application must report the worst measured recovery \
             {worst:?}, got {app:?}"
        );
    }
}

/// Every resource type this service supports is in the recovery catalogue, so an
/// assessment can say what each one recovers: a type the catalogue does not carry
/// recovers nothing at all, which no supported type does — each recovers at least
/// the disruption class its own service absorbs or its lack of data makes
/// irrelevant.
#[test]
fn every_supported_resource_type_recovers_something() {
    for resource_type in SUPPORTED_RESOURCE_TYPES {
        let recovered: Vec<&&str> = DISRUPTION_TYPE_VALUES
            .iter()
            .filter(|disruption_type| recovers_disruption(resource_type, disruption_type))
            .collect();
        assert!(
            !recovered.is_empty(),
            "case {resource_type:?}: a supported resource type must be in the recovery \
             catalogue, so it recovers at least one disruption type, got {recovered:?}"
        );
    }
    // Not vacuous: a type the catalogue does not carry recovers nothing.
    for disruption_type in DISRUPTION_TYPE_VALUES {
        assert!(
            !recovers_disruption("AWS::Nothing::Known", disruption_type),
            "case \"an uncatalogued resource type\": nothing is known about what it survives, \
             so it must recover no {disruption_type} disruption"
        );
    }
}

/// A finished assessment states what the application costs as a COMPLETE cost
/// object — an amount in a currency at a frequency, the three members the model
/// makes required — however many resources the assessed version maps. This service
/// holds no price for any resource it stores, so the amount they add up to is 0.
#[tokio::test]
async fn a_finished_assessment_states_what_the_application_costs() {
    // The rows differ along how much the assessed version maps, which is what the
    // amount adds up over.
    for resources in 1..=3usize {
        let (router, engine) = emulator();
        let case = format!("{resources} mapped resource(s)");
        let policy_arn =
            create_a_policy(&router, &format!("cost-policy-{resources}"), &required_objectives(3600))
                .await;
        let app_arn =
            create_an_app(&router, &format!("cost-app-{resources}"), Some(&policy_arn)).await;
        add_a_component(&router, &app_arn, "compute-tier").await;
        for index in 0..resources {
            add_a_resource(&router, &app_arn, &format!("record-{index}"), "compute-tier").await;
        }
        publish(&router, &app_arn, None).await;

        let assessed = assess(&router, &engine, &app_arn, &format!("cost-run-{resources}")).await;
        assert_eq!(
            assessed["cost"],
            json!({"amount": 0.0, "currency": "USD", "frequency": "Monthly"}),
            "case {case:?}: a finished assessment states a complete cost, got {assessed:?}"
        );
        assert!(
            COST_FREQUENCY_VALUES.contains(&assessed["cost"]["frequency"].as_str().unwrap_or("")),
            "case {case:?}: the frequency must be one the model admits, got {assessed:?}"
        );
    }
}

/// Running an assessment IS the drift evaluation, so a finished one answers the
/// drift question: `NotDetected` while the compliance it computed matches the
/// compliance of the application's previous assessment — the first assessment of an
/// application included, which has nothing to deviate from — and `Detected` when it
/// does not.
#[tokio::test]
async fn the_drift_verdict_compares_the_previous_assessment_of_the_application() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(&router, "drift-policy", &required_objectives(3600)).await;
    let app_arn = create_an_app(&router, "drift-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "record", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    // The rows differ along what happened to the application between assessments:
    // nothing at all, a change that moves its compliance, and a change that does
    // not.
    let assessed = assess(&router, &engine, &app_arn, "first-run").await;
    assert_eq!(
        assessed["driftStatus"],
        json!(DRIFT_STATUS_NOT_DETECTED),
        "case \"the first assessment\": the first assessment of an application deviates from \
         nothing, got {assessed:?}"
    );

    let unchanged = assess(&router, &engine, &app_arn, "unchanged-run").await;
    assert_eq!(
        unchanged["driftStatus"],
        json!(DRIFT_STATUS_NOT_DETECTED),
        "case \"an unchanged application\": the same compliance twice is no deviation, got \
         {unchanged:?}"
    );

    // A component with nothing mapped to it moves the compliance, so the next
    // assessment finds a deviation.
    add_a_component(&router, &app_arn, "empty-tier").await;
    publish(&router, &app_arn, None).await;
    let moved = assess(&router, &engine, &app_arn, "breached-run").await;
    assert_eq!(
        (moved["driftStatus"].clone(), moved["complianceStatus"].clone()),
        (json!(DRIFT_STATUS_DETECTED), json!(POLICY_BREACHED)),
        "case \"compliance that moved\": a compliance that differs from the previous \
         assessment's is a drift, got {moved:?}"
    );

    // And a further assessment of that same breached application deviates from
    // nothing again.
    let settled = assess(&router, &engine, &app_arn, "settled-run").await;
    assert_eq!(
        settled["driftStatus"],
        json!(DRIFT_STATUS_NOT_DETECTED),
        "case \"a settled breach\": the breach is now the baseline, so there is no further \
         deviation, got {settled:?}"
    );
}

/// How many digits of a second a `TimeStamp` member may carry on the wire: AWS
/// carries restJson1 timestamps as epoch seconds with at most a millisecond
/// fraction, so a reported instant has at most three.
const MAX_FRACTION_DIGITS: usize = 3;

/// Assert `reported` is an epoch-seconds number in that shape.
fn assert_timestamp_shape(case: &str, member: &str, reported: &Value) {
    let number = reported
        .as_number()
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be an epoch-seconds number, got {reported:?}"));
    // The serialized form, so the precision asserted on is the one on the wire and
    // not a re-rounding of it.
    let rendered = number.to_string();
    let fraction = rendered.split_once('.').map_or("", |(_, fraction)| fraction);
    assert!(
        fraction.len() <= MAX_FRACTION_DIGITS,
        "case {case:?}: {member} must carry at most {MAX_FRACTION_DIGITS} fractional digits, \
         got {rendered}"
    );
}

/// Every `TimeStamp` member this service reports — the assessment's start and end,
/// the application's creation, compliance, score and drift evaluation times, and a
/// published version's creation time — is carried in the epoch-seconds shape AWS
/// uses, whichever operation reports it.
#[tokio::test]
async fn every_reported_timestamp_carries_at_most_millisecond_precision() {
    let (router, engine) = emulator();
    let policy_arn = create_a_policy(&router, "shape-policy", &required_objectives(3600)).await;
    let app_arn = create_an_app(&router, "shape-app", Some(&policy_arn)).await;
    add_a_component(&router, &app_arn, "compute-tier").await;
    add_a_resource(&router, &app_arn, "record", "compute-tier").await;
    publish(&router, &app_arn, None).await;

    // The started assessment: it reports when it began, and nothing of its end yet.
    let started = start_assessment(&router, &app_arn, RELEASE_APP_VERSION, "shape-run").await;
    let started = started.ok("the started assessment")["assessment"].clone();
    assert_timestamp_shape("the started assessment", "startTime", &started["startTime"]);
    let assessment_arn = started["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();

    finish_the_assessment(&engine, &assessment_arn);
    let finished = describe_assessment(&router, &assessment_arn).await;
    let finished = finished.ok("the finished assessment")["assessment"].clone();
    for member in ["startTime", "endTime"] {
        assert_timestamp_shape("the finished assessment", member, &finished[member]);
    }
    // The start a read reports is the one the starter answered: the instant is
    // truncated where the record is built, not at each read.
    assert_eq!(
        finished["startTime"], started["startTime"],
        "case \"the finished assessment\": a read must report the start the call answered, got \
         {finished:?}"
    );

    let app = describe_app(&router, &app_arn).await.ok("the application")["app"].clone();
    for member in [
        "creationTime",
        "lastAppComplianceEvaluationTime",
        "lastResiliencyScoreEvaluationTime",
        "lastDriftEvaluationTime",
    ] {
        assert_timestamp_shape("the application", member, &app[member]);
    }

    let versions = post(&router, "/list-app-versions", &json!({"appArn": app_arn})).await;
    for summary in versions.ok("the listed versions")["appVersions"]
        .as_array()
        .expect("appVersions is @required")
    {
        assert_timestamp_shape("a listed app version", "creationTime", &summary["creationTime"]);
    }
}
