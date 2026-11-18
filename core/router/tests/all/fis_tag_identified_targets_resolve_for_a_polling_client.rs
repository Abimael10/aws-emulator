// SPDX-License-Identifier: CC0-1.0

//! What a client that drives AWS FIS the documented way SEES when it asks which resources an
//! experiment's targets resolved to: `ListExperimentResolvedTargets`, polled from the start of
//! an experiment whose only targets identify their resources by `resourceTags`.
//!
//! RULE: a target that DESCRIBES its resources (`resourceTags`, optionally narrowed by
//! `filters` and scoped by `selectionMode`) resolves at experiment start to the resources of
//! its `resourceType` that the owning service holds for the account in the Region the
//! experiment was started in and that carry every one of those tags, and the experiment
//! reports one `ResolvedTarget` per resolved resource — `targetName` the template's target
//! key, `resourceType` that target's type, `targetInformation` naming the resource. While that
//! resolution has not ENDED the read is refused with HTTP 400, `ValidationException` and the
//! sentence FIS words that refusal in; once it has ended the read is answered with the
//! resolution. So a client polling a tag-identified resolution is answered either the refusal
//! or the finished resolution, and never an empty list for a target that describes resources
//! the account holds — which is what makes such a poll terminate.
//!
//! This suite lives beside the router because the resources belong to ANOTHER service: every
//! instance below is launched through Amazon EC2's own `RunInstances` on the SAME engine that
//! serves FIS, with its tags in the launch's `TagSpecification`, so what the resolution reads
//! is what EC2 persisted and each ARN in an assertion is one EC2 handed out.
//!
//! The rows differ along what the rule generalises over: how many tag pairs the target names
//! (one, two), how many resources carry what it describes (two, one, none), whether a filter
//! narrows them (on an attribute one of the two holds, on one both hold) and which
//! `selectionMode` scopes them (`ALL`, `COUNT(n)`). Two decoys are launched beside the matching
//! instances — one carrying the same tag KEY with another value, one carrying no tag at all —
//! and no row may resolve either of them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode};

/// The Region every request below is signed for: a tag-identified target resolves against the
/// resources of the Region the experiment was started in, and the launches use the same one.
const REGION: &str = "us-east-1";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN below carries.
const ACCOUNT: &str = "000000000000";

/// The execution role the templates name. The account's IAM store is not mounted here, so
/// nothing can be shown about this role and the start goes ahead: what these cases are about
/// is the RESOLUTION, not the role gate.
const EXECUTION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-resolution-poll";

/// The sentence FIS refuses a read of a resolution that has not ENDED with. Spelled out here
/// rather than imported from the service crate so a change to the wording fails this suite.
const TARGET_RESOLUTION_ONGOING: &str =
    "Target resolution is ongoing. Retry after target resolution ends.";

/// The `targetInformation` key the resolved resource's ARN is reported under.
/// `TargetInformationMap` is modelled as a free string map, so this is the key this emulator
/// reports the resource it resolved under; spelled out so a change to it fails this suite.
const TARGET_INFORMATION_ARN_KEY: &str = "arn";

/// The tag key the matching instances and one decoy carry.
const SQUAD_KEY: &str = "Squad";

/// The tag value the matching instances carry.
const SQUAD_MATCH: &str = "chaos";

/// The same key's value on the decoy that must not resolve.
const SQUAD_OTHER: &str = "ops";

/// How long a poll waits for a condition the clock owes — an instance reaching `running`, an
/// experiment finishing its target resolution. Generous by design: it is a deadline, not a
/// measurement, and nothing below asserts how long anything took.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How far out the case that reads a resolution IN FLIGHT pushes the transition the clock owes
/// the experiment, so the window stays open across the reads it takes inside it however slowly
/// a loaded host gets to them.
const RESOLUTION_HELD_OPEN: Duration = Duration::from_secs(3_600);

/// One emulator carrying Amazon EC2 and FIS over ONE shared engine, as the assembled router
/// mounts them, plus that engine: the resolution reads the EC2 store that lives in it.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_fis_faultinjectionsimulator::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and which Region it is served in; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form field of an ec2Query body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// POST one ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {answered}"
    );
    answered
}

/// What FIS answered: the HTTP status and the parsed JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }

    /// The member `name` of this answer's body.
    fn member(&self, name: &str) -> &Value {
        &self.body[name]
    }
}

/// One restJson1 call to FIS.
async fn fis(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("fis.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("fis"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&raw)
            )
        })
    };
    Answer { status, body }
}

/// Launch ONE instance of `instance_type` carrying `tags`, through EC2's own `RunInstances`
/// with the tags in the launch's `TagSpecification`, and answer `(instance id, its ARN)`.
async fn launch(
    router: &MultiServiceRouter,
    instance_type: &str,
    tags: &[(&str, &str)],
) -> (String, String) {
    let mut params = vec![
        param("ImageId", AMI),
        param("InstanceType", instance_type),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    if !tags.is_empty() {
        params.push(param("TagSpecification.1.ResourceType", "instance"));
        for (position, (key, value)) in tags.iter().enumerate() {
            let index = position + 1;
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Value"), value));
        }
    }
    let launched = ec2(router, "RunInstances", &params).await;
    let instance_id = element(&launched, "instanceId");
    let arn = format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/{instance_id}");
    (instance_id, arn)
}

/// Poll EC2 until `instance_id` reports `State.Name == "running"`, the state the rows that
/// filter on it describe. The CONDITION is what the wait ends on.
async fn poll_until_running(router: &MultiServiceRouter, instance_id: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let described = ec2(
            router,
            "DescribeInstances",
            &[param("InstanceId.1", instance_id)],
        )
        .await;
        let state_block = block_of(&described, "instanceState").unwrap_or_default();
        let names = elements(state_block, "name");
        if names.iter().any(|name| name == "running") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: EC2 must report {instance_id} running, got {names:?} in {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: a target of the experiment template, and the resources it has to resolve to.
struct Row {
    case: &'static str,
    /// The target's key in the template's `targets` map — the `targetName` every entry
    /// resolved from it carries.
    name: &'static str,
    /// The `resourceTags` the target identifies its resources by.
    resource_tags: Vec<(&'static str, String)>,
    /// The target's `filters`, as `(path, values)` pairs.
    filters: Vec<(&'static str, Vec<String>)>,
    /// The target's `selectionMode`, in one of the forms FIS documents.
    selection_mode: &'static str,
    /// The ARNs the target has to resolve to, in the order the resolution reports them.
    expected: Vec<String>,
}

/// The target resource type every row below names, as `ListTargetResourceTypes` publishes it.
const RESOURCE_TYPE: &str = "aws:ec2:instance";

impl Row {
    /// This row's target, as a template's `targets` map carries it.
    fn target(&self) -> Value {
        let tags: Map<String, Value> = self
            .resource_tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect();
        let mut target = Map::new();
        target.insert("resourceType".to_string(), json!(RESOURCE_TYPE));
        target.insert("selectionMode".to_string(), json!(self.selection_mode));
        target.insert("resourceTags".to_string(), Value::Object(tags));
        if !self.filters.is_empty() {
            let filters: Vec<Value> = self
                .filters
                .iter()
                .map(|(path, values)| json!({ "path": path, "values": values }))
                .collect();
            target.insert("filters".to_string(), json!(filters));
        }
        Value::Object(target)
    }
}

/// The instances the account holds while the rows below are resolved.
struct Launched {
    /// `Squad=chaos`, `Stage=test`, a `t3.micro`.
    chaos_micro: String,
    /// `Squad=chaos`, `Stage=prod`, an `m5.large`: the same `Squad` tag under another
    /// instance type, which is what a filter tells apart.
    chaos_large: String,
    /// `Squad=ops`: the same tag KEY with another value, which describes it for no row.
    other_value: String,
    /// No tag at all: described by no tag-identified target.
    untagged: String,
}

impl Launched {
    /// The instances carrying `Squad=chaos`, in the ARN order the resolution reports them in.
    fn matching(&self) -> Vec<String> {
        let mut arns = vec![self.chaos_micro.clone(), self.chaos_large.clone()];
        arns.sort();
        arns
    }

    /// The instances no row may resolve.
    fn decoys(&self) -> Vec<&String> {
        vec![&self.other_value, &self.untagged]
    }
}

/// Launch the account's instances and wait until each one the `State.Name` row describes
/// reports `running`.
async fn launch_all(router: &MultiServiceRouter) -> Launched {
    let (chaos_micro_id, chaos_micro) = launch(
        router,
        "t3.micro",
        &[(SQUAD_KEY, SQUAD_MATCH), ("Stage", "test")],
    )
    .await;
    let (chaos_large_id, chaos_large) = launch(
        router,
        "m5.large",
        &[(SQUAD_KEY, SQUAD_MATCH), ("Stage", "prod")],
    )
    .await;
    let (_, other_value) = launch(router, "t3.micro", &[(SQUAD_KEY, SQUAD_OTHER)]).await;
    let (_, untagged) = launch(router, "t3.micro", &[]).await;
    poll_until_running(router, &chaos_micro_id).await;
    poll_until_running(router, &chaos_large_id).await;
    Launched {
        chaos_micro,
        chaos_large,
        other_value,
        untagged,
    }
}

/// The targets the rule is asserted over.
fn rows(launched: &Launched) -> Vec<Row> {
    let matching = launched.matching();
    vec![
        Row {
            case: "one tag pair identifies every instance that carries it",
            name: "squadChaos",
            resource_tags: vec![(SQUAD_KEY, SQUAD_MATCH.to_string())],
            filters: Vec::new(),
            selection_mode: "ALL",
            expected: matching.clone(),
        },
        Row {
            case: "two tag pairs identify only the instance carrying both",
            name: "squadChaosStageTest",
            resource_tags: vec![
                (SQUAD_KEY, SQUAD_MATCH.to_string()),
                ("Stage", "test".to_string()),
            ],
            filters: Vec::new(),
            selection_mode: "ALL",
            expected: vec![launched.chaos_micro.clone()],
        },
        Row {
            case: "a filter narrows the tagged instances to one attribute value",
            name: "squadChaosMicro",
            resource_tags: vec![(SQUAD_KEY, SQUAD_MATCH.to_string())],
            // `Instance$InstanceType` is a member of the instance `DescribeInstances`
            // reports, which is where a filter path names an attribute.
            filters: vec![("InstanceType", vec!["t3.micro".to_string()])],
            selection_mode: "ALL",
            expected: vec![launched.chaos_micro.clone()],
        },
        Row {
            case: "a filter every tagged instance satisfies narrows nothing away",
            name: "squadChaosRunning",
            resource_tags: vec![(SQUAD_KEY, SQUAD_MATCH.to_string())],
            // `Instance$State$Name`, the state a client reads off the same describe.
            filters: vec![("State.Name", vec!["running".to_string()])],
            selection_mode: "ALL",
            expected: matching.clone(),
        },
        Row {
            case: "COUNT scopes the tag-identified instances to that many",
            name: "squadChaosOne",
            resource_tags: vec![(SQUAD_KEY, SQUAD_MATCH.to_string())],
            filters: Vec::new(),
            selection_mode: "COUNT(1)",
            expected: matching.iter().take(1).cloned().collect(),
        },
        Row {
            case: "a tag value no instance of the account carries identifies nothing",
            name: "squadGhost",
            resource_tags: vec![(SQUAD_KEY, "ghost".to_string())],
            filters: Vec::new(),
            selection_mode: "ALL",
            expected: Vec::new(),
        },
    ]
}

/// The whole resolution the rows add up to: the rows in target-name order, each one's
/// resources in the order it resolves them.
fn whole_resolution(rows: &[Row]) -> Vec<String> {
    let mut ordered: Vec<&Row> = rows.iter().collect();
    ordered.sort_by_key(|row| row.name);
    ordered
        .iter()
        .flat_map(|row| row.expected.clone())
        .collect()
}

/// Create a template whose targets are `rows` and start an experiment from it, answering the
/// experiment's minted id.
async fn start_experiment(router: &MultiServiceRouter, label: &str, rows: &[Row]) -> String {
    let mut targets = Map::new();
    for row in rows {
        targets.insert(row.name.to_string(), row.target());
    }
    let created = fis(
        router,
        "POST",
        "/experimentTemplates",
        Some(json!({
            "clientToken": format!("tag-resolution-template-{label}"),
            "description": format!("tag-identified targets resolve: {label}"),
            "roleArn": EXECUTION_ROLE_ARN,
            "stopConditions": [{ "source": "none" }],
            "targets": Value::Object(targets),
            // `aws:fis:wait` takes a required `duration`, an ISO 8601 span inside the
            // documented 1 to 720 minutes, so the run is still going while a case reads it.
            "actions": {
                "wait": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT5M" } }
            },
            // A target that describes resources the account holds none of resolves to
            // nothing, which is one of the rows: `skip` leaves the rest of the run alone.
            "experimentOptions": { "emptyTargetResolutionMode": "skip" }
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: CreateExperimentTemplate must succeed, got {} with {:?}",
        created.status, created.body
    );
    let template_id = created.member("experimentTemplate")["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: no template id in {:?}", created.body))
        .to_string();

    let started = fis(
        router,
        "POST",
        "/experiments",
        Some(json!({
            "clientToken": format!("tag-resolution-start-{label}"),
            "experimentTemplateId": template_id
        })),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {label:?}: StartExperiment must succeed, got {} with {:?}",
        started.status, started.body
    );
    started.member("experiment")["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: no experiment id in {:?}", started.body))
        .to_string()
}

/// One read of an experiment's resolution, optionally narrowed to one `targetName`.
async fn read_resolution(
    router: &MultiServiceRouter,
    experiment_id: &str,
    target_name: Option<&str>,
) -> Answer {
    let uri = match target_name {
        Some(name) => format!("/experiments/{experiment_id}/resolvedTargets?targetName={name}"),
        None => format!("/experiments/{experiment_id}/resolvedTargets"),
    };
    fis(router, "GET", &uri, None).await
}

/// The ARNs an answered resolution reports, in the order it reports them, with each entry's
/// `targetName` and `resourceType` asserted when the read was narrowed to one target.
fn reported_arns(case: &str, answer: &Answer, target_name: Option<&str>) -> Vec<String> {
    let entries = answer
        .member("resolvedTargets")
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a resolvedTargets list, got {:?}",
                answer.body
            )
        })
        .clone();
    entries
        .iter()
        .map(|entry| {
            if let Some(name) = target_name {
                assert_eq!(
                    entry["targetName"].as_str(),
                    Some(name),
                    "case {case:?}: every entry must name the target it came from, got {entry:?}"
                );
            }
            assert_eq!(
                entry["resourceType"].as_str(),
                Some(RESOURCE_TYPE),
                "case {case:?}: every entry must carry its target's resourceType, got {entry:?}"
            );
            entry["targetInformation"][TARGET_INFORMATION_ARN_KEY]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no resolved resource in {entry:?}")
                })
                .to_string()
        })
        .collect()
}

/// Put the instant the transition this experiment owes the clock falls due at `due` — how a
/// case decides whether the resolution is in flight by the experiment's own clock rather than
/// by how quickly this host got from the start to the read.
fn set_transition_due(engine: &Arc<engine::InMemoryEngine>, experiment_id: &str, due: SystemTime) {
    let state = service_com_amazonaws_fis_faultinjectionsimulator::state::service_state(&**engine)
        .expect("the engine carries the FIS store");
    let mut store = state.lock();
    let record = store
        .experiments
        .get_mut(experiment_id)
        .unwrap_or_else(|| panic!("the store holds the experiment {experiment_id}"));
    record.transition_due = Some(due);
}

/// Assert every row's resolution, and that no decoy appears in any of them.
async fn assert_rows(
    router: &MultiServiceRouter,
    experiment_id: &str,
    rows: &[Row],
    launched: &Launched,
) {
    for row in rows {
        let answer = read_resolution(router, experiment_id, Some(row.name)).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected HTTP 200 once the resolution has ended, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        let arns = reported_arns(row.case, &answer, Some(row.name));
        assert_eq!(
            arns, row.expected,
            "case {:?}: expected the target {:?} to resolve to {:?}, got {arns:?}",
            row.case, row.name, row.expected
        );
        for decoy in launched.decoys() {
            assert!(
                !arns.contains(decoy),
                "case {:?}: {decoy} carries no tag this target describes and must not \
                 resolve, got {arns:?}",
                row.case
            );
        }
    }
}

/// RULE: a read of a tag-identified resolution is refused while that resolution has not ENDED
/// — HTTP 400, `ValidationException`, the sentence FIS words it in — and answered with the
/// resolved resources once it has: the instances of the target's `resourceType` the account
/// holds in the experiment's Region carrying every tag the target names, narrowed by its
/// `filters` and scoped by its `selectionMode`.
#[tokio::test]
async fn a_tag_identified_resolution_is_refused_in_flight_and_then_reports_the_tagged_resources() {
    let (router, engine) = emulator();
    let launched = launch_all(&router).await;
    let rows = rows(&launched);
    let experiment_id = start_experiment(&router, "in-flight", &rows).await;

    // The experiment is HELD in the window its targets are being resolved in, so what the
    // reads below are taken inside is decided by the experiment's clock.
    set_transition_due(
        &engine,
        &experiment_id,
        SystemTime::now() + RESOLUTION_HELD_OPEN,
    );
    for narrowing in [None, Some("squadChaos")] {
        let case = narrowing.unwrap_or("the whole resolution");
        let refused = read_resolution(&router, &experiment_id, narrowing).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 while the resolution is in flight, got {} \
             with {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException while the resolution is in flight, \
             got {:?}",
            refused.body
        );
        assert_eq!(
            refused.member("message").as_str(),
            Some(TARGET_RESOLUTION_ONGOING),
            "case {case:?}: expected the message {TARGET_RESOLUTION_ONGOING:?}, got {:?}",
            refused.body
        );
        assert!(
            refused.member("resolvedTargets").is_null(),
            "case {case:?}: a refused read reports no resolution, got {:?}",
            refused.body
        );
    }

    // The transition the clock owes the experiment is due, so the resolution has ended and the
    // same reads are answered with it.
    set_transition_due(&engine, &experiment_id, SystemTime::now());
    assert_rows(&router, &experiment_id, &rows, &launched).await;

    // The resolution is a snapshot the START computed, so reading it does not change it.
    for pass in ["first read", "second read"] {
        let answer = read_resolution(&router, &experiment_id, None).await;
        assert_eq!(
            answer.status, 200,
            "case {pass:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let arns = reported_arns(pass, &answer, None);
        let expected = whole_resolution(&rows);
        assert_eq!(
            arns, expected,
            "case {pass:?}: expected the whole resolution {expected:?}, got {arns:?}"
        );
    }
}

/// RULE (the same rule, as the client that polls for the resolution sees it): every answer to a
/// read of a tag-identified resolution is either the in-flight refusal or the FINISHED
/// resolution, so a poll for the resolved targets of a target that describes resources the
/// account holds terminates with those resources rather than being answered an empty list
/// forever.
#[tokio::test]
async fn a_client_polling_a_tag_identified_resolution_is_never_answered_an_empty_resolution() {
    let (router, _engine) = emulator();
    let launched = launch_all(&router).await;
    let rows = rows(&launched);
    let expected = whole_resolution(&rows);
    assert!(
        !expected.is_empty(),
        "the rows must describe resources the account holds, got {expected:?}"
    );
    let experiment_id = start_experiment(&router, "polling", &rows).await;

    // The experiment's own clock decides when the resolution ends: the poll ends on that
    // CONDITION, and every answer it sees on the way is one of the two the rule allows.
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    let case = "polling from the start";
    loop {
        let answer = read_resolution(&router, &experiment_id, None).await;
        if answer.status == 400 {
            assert_eq!(
                answer.error_code(),
                "ValidationException",
                "case {case:?}: a read refused before the resolution ends is a \
                 ValidationException, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.member("message").as_str(),
                Some(TARGET_RESOLUTION_ONGOING),
                "case {case:?}: expected the message {TARGET_RESOLUTION_ONGOING:?}, got {:?}",
                answer.body
            );
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the resolution must end, and the read was still refused \
                 with {:?}",
                answer.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a read is either refused in flight or answered, got {} with {:?}",
            answer.status, answer.body
        );
        // The FIRST answered read already carries the finished resolution: an empty list would
        // be indistinguishable from "nothing matched" and is what a client polling for the
        // resolution would wait on forever.
        let arns = reported_arns(case, &answer, None);
        assert_eq!(
            arns, expected,
            "case {case:?}: the first answered read must carry the whole resolution \
             {expected:?}, got {arns:?}"
        );
        break;
    }

    assert_rows(&router, &experiment_id, &rows, &launched).await;
}
