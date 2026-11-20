// SPDX-License-Identifier: CC0-1.0

//! CodeBuild publishes one EventBridge event per change of a build's state.
//!
//! The RULE under test: every status a build MOVES INTO is published once onto the account's
//! default EventBridge bus, as `source` `aws.codebuild` with the `detail-type` `CodeBuild
//! Build State Change`, carrying the build's ARN in the envelope's `resources` and, in the
//! event `detail`, the project the build belongs to (`project-name`), the build's own ARN
//! (`build-id`), the status it moved into (`build-status`) and the phase it is in
//! (`current-phase`, `current-phase-context`). EventBridge then routes it: a rule whose
//! `EventPattern` matches delivers one message per event to its SQS queue target, which a
//! client reads with `ReceiveMessage`, and a rule whose pattern selects another project — or
//! another status — receives nothing.
//!
//! The `detail` members are the ones the AWS-managed event schema for this `detail-type`
//! declares (`CodeBuildBuildStateChange`: `build-id`, `build-status`, `project-name`,
//! `version`, plus `current-phase` and `current-phase-context`), spelled out here rather than
//! imported from the implementation, so a wrong constant in the service cannot make a test
//! pass.
//!
//! The rule is asserted over a TABLE of patterns and builds that differ along what it
//! generalises over — WHICH project a pattern names, WHICH status it names, and HOW the build
//! ends (it runs to its own outcome, or a client stops it) — because the rule is about every
//! transition of every build, not about one shape of build. Nothing here reads the events out
//! of the service's own store: they are read from the queue the rule targets, so an event
//! that arrives was published onto the bus and routed by the pattern.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::project_build_lifecycle::{create_project_body, REGION, SETTLE_DEADLINE};

/// The `source` and the `detail-type` CodeBuild publishes a build's state changes under.
const EVENT_SOURCE: &str = "aws.codebuild";
const BUILD_STATE_CHANGE: &str = "CodeBuild Build State Change";

/// The status a build carries while it runs, and the one a build a client stopped ends in
/// (`StatusType`: SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED).
const IN_PROGRESS: &str = "IN_PROGRESS";
const STOPPED: &str = "STOPPED";

/// How long one poll of a rule's queue waits before reading again. The waits here all end on
/// a CONDITION; this is only how often the condition is looked at.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator: CodeBuild, EventBridge and SQS mounted on ONE shared engine, with the router
/// that dispatches to all three.
///
/// EventBridge and SQS are mounted BESIDE CodeBuild because the build plane publishes its
/// state changes onto the default event bus through the in-process callback and a rule on that
/// bus delivers them to its SQS queue target: with neither mounted there is nowhere to
/// publish, so the path under test would not be exercised at all. Each service claims only the
/// requests scoped to its own signing name, so no CodeBuild request below is routed any
/// differently for their presence.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDCODEBUILDTEST/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0f0f"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("authorization", authorization)
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Call one CodeBuild operation, asserting it succeeded.
    async fn codebuild(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc("codebuild", "CodeBuild_20161006", "1.1", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} {body}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Call one EventBridge operation, asserting it succeeded.
    async fn events(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.rpc("events", "AWSEvents", "1.1", operation, body).await;
        assert_eq!(
            status, 200,
            "{operation} {body}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Call one SQS operation, asserting it succeeded.
    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.rpc("sqs", "AmazonSQS", "1.0", operation, body).await;
        assert_eq!(
            status, 200,
            "{operation} {body}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }
}

/// An SQS queue called `name`, carrying the grant a rule's target needs — EventBridge makes
/// the target's `sqs:SendMessage` as its own service principal and "relies on resource-based
/// policies" for a queue (`PutTargets`), so a queue that has granted it nothing receives
/// nothing. Answers its url and the ARN a rule names it by.
async fn given_queue(emulator: &Emulator, name: &str) -> (String, String) {
    let created = emulator.sqs("CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("queue {name}: no QueueUrl in {created}"))
        .to_string();
    let attributes = emulator
        .sqs(
            "GetQueueAttributes",
            &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        )
        .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("queue {name}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "events.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    emulator
        .sqs(
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        )
        .await;
    (url, arn)
}

/// A rule called `name` on the account's default bus whose pattern is `pattern`, targeting a
/// queue of its own. Answers that queue's url, which is where the events the pattern selects
/// are read from.
async fn given_rule(emulator: &Emulator, name: &str, pattern: &Value) -> String {
    let (url, queue_arn) = given_queue(emulator, &format!("{name}-queue")).await;
    emulator
        .events(
            "PutRule",
            &json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
        )
        .await;
    emulator
        .events(
            "PutTargets",
            &json!({ "Rule": name, "Targets": [{ "Id": "t1", "Arn": queue_arn }] }),
        )
        .await;
    url
}

/// Create a project called `name` whose build runs no source and produces no artifacts — the
/// smallest project a build can be started from, built by the harness every suite of this
/// crate creates projects with.
async fn given_project(emulator: &Emulator, name: &str) {
    emulator
        .codebuild("CreateProject", &create_project_body(name, json!({})))
        .await;
}

/// Start a build of `project` and answer the `(id, arn)` the service minted for it.
async fn start_build(emulator: &Emulator, project: &str) -> (String, String) {
    let started = emulator
        .codebuild("StartBuild", &json!({ "projectName": project }))
        .await;
    let build = &started["build"];
    (
        build["id"]
            .as_str()
            .unwrap_or_else(|| panic!("StartBuild {project}: no build id in {started}"))
            .to_string(),
        build["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("StartBuild {project}: no build arn in {started}"))
            .to_string(),
    )
}

/// Read `BatchGetBuilds` until `id` reports itself complete and answer the status it ended
/// in — the transition the clock owes a started build, materialised by the reader that finds
/// it due.
///
/// The wait ends on the CONDITION; the deadline is only there so a build that never settles
/// fails the test instead of hanging it.
async fn settled_status(emulator: &Emulator, id: &str) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator
            .codebuild("BatchGetBuilds", &json!({ "ids": [id] }))
            .await;
        let build = &read["builds"][0];
        if build["buildComplete"] == json!(true) {
            return build["buildStatus"]
                .as_str()
                .unwrap_or_else(|| panic!("build {id}: no buildStatus in {read}"))
                .to_string();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "build {id}: expected the build to complete, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Every message waiting on the queue at `url` right now, as parsed JSON event envelopes.
async fn receive(emulator: &Emulator, url: &str) -> Vec<Value> {
    let answer = emulator
        .sqs(
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
    answer["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Read the queue at `url` until it has delivered at least `wanted` messages, answering every
/// message it delivered.
///
/// The wait ends on the CONDITION, and the deadline is generous: it is only there so a
/// delivery that never happens fails the test instead of hanging it.
async fn delivered(emulator: &Emulator, url: &str, wanted: usize, case: &str) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(receive(emulator, url).await);
        if collected.len() >= wanted {
            return collected;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {wanted} message(s) on the rule's queue, it delivered \
             {collected:#?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The `(build-status, project-name, build-id)` one delivered event reports.
fn reported(event: &Value) -> (String, String, String) {
    let detail = &event["detail"];
    (
        detail["build-status"].as_str().unwrap_or_default().to_string(),
        detail["project-name"].as_str().unwrap_or_default().to_string(),
        detail["build-id"].as_str().unwrap_or_default().to_string(),
    )
}

/// Assert the envelope of one delivered event: the publishing service, the kind of event, the
/// resource it is about and the members the event's schema requires.
fn assert_envelope(case: &str, event: &Value, build_arn: &str, project: &str) {
    assert_eq!(
        event["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: every envelope must carry the CodeBuild source, got {event}"
    );
    assert_eq!(
        event["detail-type"],
        json!(BUILD_STATE_CHANGE),
        "case {case:?}: every envelope must carry the build state-change detail-type, got \
         {event}"
    );
    assert_eq!(
        event["resources"],
        json!([build_arn]),
        "case {case:?}: every event must carry the build's ARN in resources, got {event}"
    );
    assert_eq!(
        event["detail"]["build-id"],
        json!(build_arn),
        "case {case:?}: detail.build-id must be the build's ARN, got {event}"
    );
    assert_eq!(
        event["detail"]["project-name"],
        json!(project),
        "case {case:?}: detail.project-name must name the build's project, got {event}"
    );
    assert_eq!(
        event["detail"]["version"],
        json!("1"),
        "case {case:?}: detail.version must be the event detail's schema version, got {event}"
    );
    assert!(
        event["detail"]["current-phase"].is_string(),
        "case {case:?}: every event must report the phase the build is in, got {event}"
    );
    assert!(
        event["detail"]["current-phase-context"].is_string(),
        "case {case:?}: every event must report the current phase's context, got {event}"
    );
}

/// RULE, positive and negative halves together: a rule whose pattern constrains
/// `detail.project-name` to one project receives that project's build state changes, and a
/// sibling project's builds are withheld from it.
///
/// The rows differ along what the rule generalises over: which of the started projects the
/// pattern names, and a pattern naming a project nothing was built for at all.
#[tokio::test]
async fn a_project_name_pattern_delivers_only_that_projects_build_events() {
    let emulator = Emulator::new();
    // Two projects whose builds are started, and one whose build is never started.
    let built = ["events-project-one", "events-project-two"];
    let unbuilt = "events-project-three";
    for project in built.iter().copied().chain([unbuilt]) {
        given_project(&emulator, project).await;
    }
    // One rule per project, each with its own queue, so a delivery is attributable to the
    // rule whose pattern matched.
    let mut queues: Vec<(&str, String)> = Vec::new();
    for project in built.iter().copied().chain([unbuilt]) {
        let pattern = json!({
            "source": [EVENT_SOURCE],
            "detail-type": [BUILD_STATE_CHANGE],
            "detail": { "project-name": [project] },
        });
        let url = given_rule(&emulator, &format!("{project}-rule"), &pattern).await;
        queues.push((project, url));
    }

    let mut started: Vec<(&str, String, String)> = Vec::new();
    for project in built {
        let (id, arn) = start_build(&emulator, project).await;
        started.push((project, id, arn));
    }

    // Each named project's rule receives the start of its own build and nothing of the
    // other's.
    for (project, _, arn) in &started {
        let messages = delivered(&emulator, queue_of(&queues, project), 1, project).await;
        for event in &messages {
            assert_envelope(project, event, arn, project);
        }
        let reports: Vec<(String, String, String)> = messages.iter().map(reported).collect();
        assert_eq!(
            reports,
            vec![(
                IN_PROGRESS.to_string(),
                (*project).to_string(),
                arn.clone()
            )],
            "case {project:?}: the rule naming this project must receive exactly its own \
             build's start, got {reports:?}"
        );
    }

    // The transition the clock owes each started build, materialised by a read of it: the
    // status each build ended in is published as a second event on that project's rule.
    for (project, id, arn) in &started {
        let outcome = settled_status(&emulator, id).await;
        let messages = delivered(&emulator, queue_of(&queues, project), 1, project).await;
        let reports: Vec<(String, String, String)> = messages.iter().map(reported).collect();
        assert_eq!(
            reports,
            vec![(outcome.clone(), (*project).to_string(), arn.clone())],
            "case {project:?}: the status the build ended in must be published on that \
             project's rule, got {reports:?}"
        );
        for event in &messages {
            assert_envelope(project, event, arn, project);
        }
    }

    // The rule naming the project nothing was built for receives nothing: the pattern
    // selects a project with no builds, so no event of either build reaches it.
    let withheld = receive(&emulator, queue_of(&queues, unbuilt)).await;
    assert!(
        withheld.is_empty(),
        "case {unbuilt:?}: a rule naming a project nothing was built for must receive \
         nothing, got {withheld:#?}"
    );
}

/// The queue of the rule that names `project`.
fn queue_of<'a>(queues: &'a [(&str, String)], project: &str) -> &'a str {
    queues
        .iter()
        .find(|(named, _)| *named == project)
        .map(|(_, url)| url.as_str())
        .unwrap_or_default()
}

/// RULE: every status a build moves into is published, whatever ends the build — so a client
/// watching one build sees its start and then the status it came to rest in, in that order.
///
/// The rows differ along HOW the build ends: it runs to the outcome the clock owes it, or a
/// client stops it. A second rule, whose pattern constrains `detail.build-status` to the
/// statuses a build comes to rest in, receives the terminal event of each and neither start:
/// the same `detail` filtering, on the other member a client gates on.
#[tokio::test]
async fn every_status_a_build_moves_into_is_published_in_order() {
    // (case, project, whether a client stops the build)
    let rows: Vec<(&str, &str, bool)> = vec![
        ("a build that runs to its own outcome", "events-run", false),
        ("a build a client stops", "events-stopped", true),
    ];
    for (case, project, stop) in rows {
        let emulator = Emulator::new();
        given_project(&emulator, project).await;
        let every_change = given_rule(
            &emulator,
            "every-build-change",
            &json!({ "source": [EVENT_SOURCE], "detail-type": [BUILD_STATE_CHANGE] }),
        )
        .await;
        // The statuses a build COMES TO REST in, as the model spells them (`StatusType`:
        // SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED) — every value except
        // the one a build carries while it runs.
        let terminal_only = given_rule(
            &emulator,
            "terminal-build-changes",
            &json!({
                "source": [EVENT_SOURCE],
                "detail": {
                    "build-status": ["SUCCEEDED", "FAILED", "FAULT", "TIMED_OUT", STOPPED],
                },
            }),
        )
        .await;

        let (id, arn) = start_build(&emulator, project).await;
        let outcome = match stop {
            true => {
                let stopped = emulator
                    .codebuild("StopBuild", &json!({ "id": &id }))
                    .await;
                assert_eq!(
                    stopped["build"]["buildStatus"],
                    json!(STOPPED),
                    "case {case:?}: StopBuild must leave the build stopped, got {stopped}"
                );
                STOPPED.to_string()
            }
            false => settled_status(&emulator, &id).await,
        };

        let messages = delivered(&emulator, &every_change, 2, case).await;
        let statuses: Vec<String> = messages
            .iter()
            .map(|event| reported(event).0)
            .collect();
        assert_eq!(
            statuses,
            vec![IN_PROGRESS.to_string(), outcome.clone()],
            "case {case:?}: expected the start and then the status the build came to rest \
             in, got {statuses:?}"
        );
        for event in &messages {
            assert_envelope(case, event, &arn, project);
        }
        // The phase the build is in moves with its status: a build that has come to rest
        // reports the completed phase.
        assert_eq!(
            messages[1]["detail"]["current-phase"],
            json!("COMPLETED"),
            "case {case:?}: a build that came to rest reports the phase it ended in, got {}",
            messages[1]
        );

        let terminal = delivered(&emulator, &terminal_only, 1, case).await;
        let terminal_statuses: Vec<String> =
            terminal.iter().map(|event| reported(event).0).collect();
        assert_eq!(
            terminal_statuses,
            vec![outcome],
            "case {case:?}: the rule naming the statuses a build comes to rest in must \
             receive exactly the terminal event, got {terminal_statuses:?}"
        );
    }
}
