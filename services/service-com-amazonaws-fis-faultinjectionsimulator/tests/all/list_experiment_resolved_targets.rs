// SPDX-License-Identifier: CC0-1.0

//! AWS FIS `ListExperimentResolvedTargets`, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test is the one FIS states for this operation ("Lists the
//! resolved targets information of the specified experiment"), read together with what it
//! documents for `selectionMode` ("ALL - Run the action on all identified targets. This
//! is the default. COUNT(n) - … the specified number of targets … PERCENT(n) - … the
//! specified percentage of targets"):
//!
//! * the resolution of an experiment reports one entry per resource one of that
//!   experiment's targets identified, scoped by that target's `selectionMode`, so a
//!   target that identified nothing contributes nothing and a resource a target named
//!   twice resolves once;
//! * every entry carries the name of the target it came from, that target's
//!   `resourceType`, and the resolved resource's ARN in `targetInformation`;
//! * a page is at most `maxResults` long, a `nextToken` arrives only with a full page,
//!   and following that token enumerates the resolution exactly once at any page size —
//!   in one and the same order, whatever the page size and however often it is read;
//! * `targetName` narrows the page to that one target and nothing else;
//! * a read taken while the experiment is still RESOLVING its targets — the window it
//!   reports `initiating` for — is refused with `ValidationException` (HTTP 400) and the
//!   sentence FIS words that refusal in, and the same read once the resolution has ended is
//!   answered with the resolution;
//! * a `nextToken` the operation never issued is refused with `ValidationException`
//!   (HTTP 400), and a `maxResults` outside `@range(min: 1, max: 100)` with HTTP 400;
//! * an `experimentId` naming no experiment answers `ResourceNotFoundException`
//!   (HTTP 404);
//! * listing changes nothing: the same request answers the same page afterwards.
//!
//! The HTTP harness below is local to this suite, as in the sibling suites: a test
//! harness is not shared state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_fis_faultinjectionsimulator::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{poll_until_resolution_ends, RESOLUTION_BUDGET};

/// `ListExperimentResolvedTargetsMaxResults`'s published bounds
/// (`@range(min: 1, max: 100)`), spelled out here rather than imported from the crate so a
/// wrong bound in the model or the handler fails this suite.
const MAX_RESULTS_FLOOR: i32 = 1;
/// See [`MAX_RESULTS_FLOOR`].
const MAX_RESULTS_CEILING: i32 = 100;

/// The sentence FIS refuses a read of a resolution that has not ENDED with. Spelled out
/// here rather than imported from the crate so a change to the wording fails this suite.
const TARGET_RESOLUTION_ONGOING: &str =
    "Target resolution is ongoing. Retry after target resolution ends.";

/// How far out a case that reads a resolution IN FLIGHT pushes the transition the clock
/// owes the experiment, so the window the resolution is in flight for stays open across the
/// reads the case takes inside it however slowly a loaded host gets to them.
const RESOLUTION_HELD_OPEN: Duration = Duration::from_secs(3_600);

/// The region an ARN names when the request's credential scope offers none usable.
const FALLBACK_REGION: &str = "us-east-1";

/// The account id every ARN this suite builds carries (`::engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The `targetInformation` key a resolved resource's ARN is reported under.
///
/// `TargetInformationKey` is modelled as a free string, so this is the key this emulator
/// reports the one thing it knows about a resolved resource under. Spelled out here so a
/// change to it fails this suite.
const TARGET_INFORMATION_ARN_KEY: &str = "arn";

/// `ResourceArnList`'s published ceiling (`@length(min: 0, max: 5)`) — the most resources
/// ONE target may name. Spelled out here rather than imported from the crate so a wrong
/// bound in the model or the handler fails this suite.
const TARGET_RESOURCE_ARNS_CEILING: usize = 5;

/// A SigV4 credential scope naming this service, for `region`. The service field is what
/// routes the request to fis.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/fis/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    emulator_and_engine().0
}

/// [`emulator`], with the shared engine it was built over — the engine whose FIS store the
/// cases that drive an experiment's CLOCK reach ([`hold_resolution_open`]).
fn emulator_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Hold the experiment `experiment_id` in the window its targets are being RESOLVED in, by
/// pushing the transition the clock owes it out by `RESOLUTION_HELD_OPEN`.
///
/// This is how a case reads a resolution that is still in flight without racing the clock:
/// a resolution is in flight for as long as the experiment has not finished starting up, so
/// moving that instant is what decides the window rather than how quickly the host gets
/// from the start to the read. [`end_resolution`] is the other half.
fn hold_resolution_open(engine: &Arc<engine::InMemoryEngine>, experiment_id: &str) {
    set_transition_due(
        engine,
        experiment_id,
        SystemTime::now() + RESOLUTION_HELD_OPEN,
    );
}

/// End the resolution of the experiment `experiment_id`: the transition the clock owes it
/// is due as of now, so the experiment has finished starting up and its resolution is
/// readable. See [`hold_resolution_open`].
fn end_resolution(engine: &Arc<engine::InMemoryEngine>, experiment_id: &str) {
    set_transition_due(engine, experiment_id, SystemTime::now());
}

/// Put the instant the transition this experiment owes the clock falls due at `due`.
fn set_transition_due(engine: &Arc<engine::InMemoryEngine>, experiment_id: &str, due: SystemTime) {
    let state = service_com_amazonaws_fis_faultinjectionsimulator::state::service_state(&**engine)
        .expect("the engine carries this service's store");
    let mut store = state.lock();
    let record = store
        .experiments
        .get_mut(experiment_id)
        .unwrap_or_else(|| panic!("the store holds the experiment {experiment_id}"));
    record.transition_due = Some(due);
}

/// What the emulator answered: the HTTP status and the JSON body.
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

    /// The `nextToken` the answer carries, if any.
    fn next_token(&self) -> Option<String> {
        self.body["nextToken"].as_str().map(str::to_string)
    }

    /// The `resolvedTargets` list this answer carries.
    fn resolved(&self, case: &str) -> Vec<Value> {
        self.member("resolvedTargets")
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no resolvedTargets list in {:?}", self.body))
            .clone()
    }
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "fis.us-east-1.amazonaws.com")
        .header("authorization", signed_for(FALLBACK_REGION))
        .header("content-type", "application/json")
        .body(payload)
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// The ARN of one resource an experiment target names, built the way AWS writes one.
fn resource_arn(service: &str, resource: &str, index: usize) -> String {
    format!("arn:aws:{service}:{FALLBACK_REGION}:{ACCOUNT_ID}:{resource}/{resource}-{index:04}")
}

/// One row: a target of the experiment template, and how many resources it resolves to.
struct Row {
    case: &'static str,
    /// The target's name — the `targetName` every entry resolved from it carries.
    name: &'static str,
    /// `resourceType`: one of the target resource types the service publishes, so the rows
    /// vary it to show an entry reports the type of the target it came from rather than one
    /// value for all of them.
    resource_type: &'static str,
    /// The target's `parameters`, for a resource type whose published contract requires
    /// some (an ECS task is identified through its cluster and service).
    parameters: Option<Value>,
    /// The target's `selectionMode`, in one of the three forms FIS documents.
    selection_mode: &'static str,
    /// The ARNs the target NAMES (`resourceArns`) — the resources it identifies, in the
    /// order it names them, deliberately repeating one in a row below.
    named: Vec<String>,
    /// The target's `resourceTags`, for a row that identifies its resources by tag
    /// instead of by ARN.
    resource_tags: Option<Value>,
    /// How many resources this target resolves to: what `selectionMode` keeps of the
    /// DISTINCT resources it identified.
    expected: usize,
}

impl Row {
    /// The distinct resources this target identified — a resource it names twice is one
    /// resource.
    fn identified(&self) -> BTreeSet<String> {
        self.named.iter().cloned().collect()
    }
}

/// The targets the resolution rule is asserted over.
///
/// The rows differ along everything that rule generalises over: which of the three
/// documented `selectionMode` forms the target uses and with which argument (every
/// resource, a count under what it identified, a count past it, a percentage that divides
/// exactly, one that does not, one that does not reach a whole resource), how many
/// resources it names (one, two, three, four, five, none), whether it names a resource
/// twice, whether it identifies its resources by ARN or by tag, and which resource type
/// it targets.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "all-of-three-instances",
            name: "allInstances",
            resource_type: "aws:ec2:instance",
            parameters: None,
            selection_mode: "ALL",
            named: (1..=3).map(|n| resource_arn("ec2", "instance", n)).collect(),
            resource_tags: None,
            expected: 3,
        },
        Row {
            case: "count-one-of-five",
            name: "countOneInstance",
            resource_type: "aws:ec2:instance",
            parameters: None,
            selection_mode: "COUNT(1)",
            named: (11..=15)
                .map(|n| resource_arn("ec2", "instance", n))
                .collect(),
            resource_tags: None,
            expected: 1,
        },
        Row {
            case: "count-three-of-five",
            name: "countThreeTasks",
            resource_type: "aws:ecs:task",
            // `aws:ecs:task` publishes `cluster` and `service` as required parameters, so a
            // target of that type carries them.
            parameters: Some(json!({ "cluster": "fis-resolved", "service": "fis-service" })),
            selection_mode: "COUNT(3)",
            named: (21..=25).map(|n| resource_arn("ecs", "task", n)).collect(),
            resource_tags: None,
            expected: 3,
        },
        Row {
            case: "count-past-what-the-target-identified",
            name: "countNineClusters",
            resource_type: "aws:eks:nodegroup",
            parameters: None,
            selection_mode: "COUNT(9)",
            named: (31..=32)
                .map(|n| resource_arn("eks", "nodegroup", n))
                .collect(),
            resource_tags: None,
            expected: 2,
        },
        Row {
            case: "percent-100-of-four",
            name: "percentAllFunctions",
            resource_type: "aws:lambda:function",
            parameters: None,
            selection_mode: "PERCENT(100)",
            named: (41..=44)
                .map(|n| resource_arn("lambda", "function", n))
                .collect(),
            resource_tags: None,
            expected: 4,
        },
        Row {
            case: "percent-50-of-four",
            name: "percentHalfFunctions",
            resource_type: "aws:lambda:function",
            parameters: None,
            selection_mode: "PERCENT(50)",
            named: (51..=54)
                .map(|n| resource_arn("lambda", "function", n))
                .collect(),
            resource_tags: None,
            expected: 2,
        },
        Row {
            case: "percent-25-of-two-does-not-reach-a-whole-resource",
            name: "percentQuarterOfTwo",
            resource_type: "aws:rds:db",
            parameters: None,
            selection_mode: "PERCENT(25)",
            named: (61..=62).map(|n| resource_arn("rds", "db", n)).collect(),
            resource_tags: None,
            expected: 0,
        },
        Row {
            case: "a-resource-named-twice-resolves-once",
            name: "repeatedInstance",
            resource_type: "aws:ec2:instance",
            parameters: None,
            selection_mode: "ALL",
            named: vec![
                resource_arn("ec2", "instance", 71),
                resource_arn("ec2", "instance", 71),
                resource_arn("ec2", "instance", 72),
            ],
            resource_tags: None,
            expected: 2,
        },
        Row {
            case: "identified-by-tag-not-by-arn",
            name: "taggedInstances",
            resource_type: "aws:ec2:instance",
            parameters: None,
            selection_mode: "ALL",
            named: Vec::new(),
            resource_tags: Some(json!({ "Stage": "test", "Team": "chaos" })),
            // This emulator holds no EC2 instance carrying those tags — the suite mounts
            // FIS alone, so the store such a target is resolved against is empty — so the
            // target identifies nothing. The suite that seeds tagged resources beside FIS
            // is `targets_resolve_against_tagged_resources`.
            expected: 0,
        },
        Row {
            case: "count-one-of-one",
            name: "theOneBucket",
            resource_type: "aws:s3:bucket",
            parameters: None,
            selection_mode: "COUNT(1)",
            named: vec![format!("arn:aws:s3:::fis-resolved-targets-{ACCOUNT_ID}")],
            resource_tags: None,
            expected: 1,
        },
    ]
}

/// The template body holding `rows` as its targets.
fn template_body(label: &str, rows: &[Row]) -> Value {
    let mut targets = Map::new();
    for row in rows {
        let mut target = Map::new();
        target.insert("resourceType".to_string(), json!(row.resource_type));
        target.insert("selectionMode".to_string(), json!(row.selection_mode));
        if let Some(parameters) = &row.parameters {
            target.insert("parameters".to_string(), parameters.clone());
        }
        if !row.named.is_empty() {
            target.insert("resourceArns".to_string(), json!(row.named));
        }
        if let Some(tags) = &row.resource_tags {
            target.insert("resourceTags".to_string(), tags.clone());
        }
        targets.insert(row.name.to_string(), Value::Object(target));
    }
    json!({
        "clientToken": format!("resolved-targets-{label}"),
        "description": format!("list experiment resolved targets suite: {label}"),
        "roleArn": "arn:aws:iam::000000000000:role/fis-list-experiment-resolved-targets",
        "stopConditions": [{ "source": "none" }],
        "targets": Value::Object(targets),
        // `aws:fis:wait` takes a required `duration`, an ISO 8601 duration within the
        // documented 1 to 720 minutes.
        "actions": {
            "wait": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT5M" } }
        }
    })
}

/// Create a template holding `rows`, start an experiment from it and wait until that
/// experiment has finished RESOLVING its targets, reporting the experiment's minted id.
async fn experiment(router: &MultiServiceRouter, label: &str, rows: &[Row]) -> String {
    let experiment_id = started_experiment(router, label, rows).await;
    // A started experiment resolves its targets while it is starting up, and a read of a
    // resolution that has not ended is refused, so the cases that read one do what a client
    // does: they read it once the resolution has ended. The wait is the shared poll of that
    // CONDITION.
    poll_until_resolution_ends(router, &experiment_id, RESOLUTION_BUDGET).await;
    experiment_id
}

/// Create a template holding `rows` and start an experiment from it, reporting the
/// experiment's minted id WITHOUT waiting for its targets to be resolved.
async fn started_experiment(router: &MultiServiceRouter, label: &str, rows: &[Row]) -> String {
    let created = send(
        router,
        "POST",
        "/experimentTemplates",
        Some(template_body(label, rows)),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: expected HTTP 200 from CreateExperimentTemplate, got {} with {:?}",
        created.status, created.body
    );
    let template_id = created.member("experimentTemplate")["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {label:?}: no experimentTemplate.id in {:?}",
                created.body
            )
        })
        .to_string();

    let started = send(
        router,
        "POST",
        "/experiments",
        Some(json!({
            "clientToken": format!("resolved-targets-start-{label}"),
            "experimentTemplateId": template_id,
        })),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {label:?}: expected HTTP 200 from StartExperiment, got {} with {:?}",
        started.status, started.body
    );
    started.member("experiment")["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: no experiment.id in {:?}", started.body))
        .to_string()
}

/// One `ListExperimentResolvedTargets` call.
async fn list(
    router: &MultiServiceRouter,
    experiment_id: &str,
    max_results: Option<i32>,
    next_token: Option<&str>,
    target_name: Option<&str>,
) -> Answer {
    let mut query: Vec<String> = Vec::new();
    if let Some(max_results) = max_results {
        query.push(format!("maxResults={max_results}"));
    }
    if let Some(next_token) = next_token {
        query.push(format!("nextToken={next_token}"));
    }
    if let Some(target_name) = target_name {
        query.push(format!("targetName={target_name}"));
    }
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    send(
        router,
        "GET",
        &format!("/experiments/{experiment_id}/resolvedTargets{query}"),
        None,
    )
    .await
}

/// Walk every page at `page_size`, returning the concatenated resolved targets.
///
/// Asserts the token rule as it goes: a page is never longer than the requested size, and
/// a `nextToken` only ever arrives with a full page.
async fn walk(
    router: &MultiServiceRouter,
    experiment_id: &str,
    page_size: Option<i32>,
    target_name: Option<&str>,
    bound: usize,
) -> Vec<Value> {
    let case = format!("maxResults={page_size:?} targetName={target_name:?}");
    let allowed = page_size
        .map(|size| usize::try_from(size).expect("a positive page size"))
        .unwrap_or(usize::try_from(MAX_RESULTS_CEILING).expect("a positive ceiling"));
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..=bound {
        let answer = list(
            router,
            experiment_id,
            page_size,
            token.as_deref(),
            target_name,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} page {page}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let items = answer.resolved(&case);
        assert!(
            items.len() <= allowed,
            "case {case:?} page {page}: a page holds at most {allowed} resolved targets, got {}",
            items.len()
        );
        collected.extend(items.iter().cloned());
        match answer.next_token() {
            None => return collected,
            Some(next) => {
                assert_eq!(
                    items.len(),
                    allowed,
                    "case {case:?} page {page}: a nextToken must only arrive with a full page, \
                     got {} resolved targets",
                    items.len()
                );
                token = Some(next);
            }
        }
    }
    panic!("case {case:?}: pagination did not terminate over {bound} resolved targets");
}

/// The ARN one resolved target reports, with the entry's shape asserted.
fn resolved_arn(case: &str, entry: &Value) -> String {
    let information = entry["targetInformation"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: no targetInformation on {entry:?}"));
    information
        .get(TARGET_INFORMATION_ARN_KEY)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected targetInformation.{TARGET_INFORMATION_ARN_KEY} to name \
                 the resolved resource, got {entry:?}"
            )
        })
        .to_string()
}

/// Assert the resolution rule for ONE row against everything the pages reported.
fn assert_row(row: &Row, listed: &[Value]) {
    let case = row.case;
    let mine: Vec<&Value> = listed
        .iter()
        .filter(|entry| entry["targetName"] == json!(row.name))
        .collect();
    let identified = row.identified();
    assert_eq!(
        mine.len(),
        row.expected,
        "case {case:?}: expected selectionMode {:?} over {} identified resources to resolve \
         {} target(s), got {mine:?}",
        row.selection_mode,
        identified.len(),
        row.expected
    );

    let mut resolved = BTreeSet::new();
    for entry in &mine {
        assert_eq!(
            entry["resourceType"],
            json!(row.resource_type),
            "case {case:?}: expected the resolved target to report its target's resourceType \
             {:?}, got {entry:?}",
            row.resource_type
        );
        let arn = resolved_arn(case, entry);
        assert!(
            identified.contains(&arn),
            "case {case:?}: resolved {arn:?}, which the target never identified ({identified:?})"
        );
        assert!(
            resolved.insert(arn.clone()),
            "case {case:?}: resolved {arn:?} twice in {mine:?}"
        );
    }

    // When the selection keeps everything the target identified, the resolution IS what it
    // identified — not merely the right number of entries.
    if row.expected == identified.len() {
        assert_eq!(
            resolved, identified,
            "case {case:?}: expected every identified resource to resolve, got {resolved:?}"
        );
    }
}

#[tokio::test]
async fn every_target_resolves_what_it_identified_scoped_by_its_selection_mode() {
    let router = emulator();
    let rows = rows();
    let experiment_id = experiment(&router, "resolution", &rows).await;
    let total: usize = rows.iter().map(|row| row.expected).sum();

    let listed = walk(&router, &experiment_id, None, None, total + 1).await;
    assert_eq!(
        listed.len(),
        total,
        "case all-targets: expected {total} resolved targets over {} targets, got {listed:?}",
        rows.len()
    );
    for row in &rows {
        assert_row(row, &listed);
    }

    // Every reported target name is a target the experiment actually holds, and reports
    // that target's own resource type: the round trip against `GetExperiment`.
    let read = send(&router, "GET", &format!("/experiments/{experiment_id}"), None).await;
    assert_eq!(
        read.status, 200,
        "case round-trip: expected HTTP 200 from GetExperiment, got {} with {:?}",
        read.status, read.body
    );
    let targets = read.member("experiment")["targets"]
        .as_object()
        .unwrap_or_else(|| panic!("case round-trip: no experiment.targets in {:?}", read.body))
        .clone();
    for entry in &listed {
        let name = entry["targetName"]
            .as_str()
            .unwrap_or_else(|| panic!("case round-trip: no targetName on {entry:?}"));
        let target = targets.get(name).unwrap_or_else(|| {
            panic!("case round-trip: resolved target names no target of the experiment: {entry:?}")
        });
        assert_eq!(
            target["resourceType"], entry["resourceType"],
            "case round-trip: expected {name:?} to report the experiment target's resourceType \
             {:?}, got {entry:?}",
            target["resourceType"]
        );
    }
}

#[tokio::test]
async fn every_page_size_enumerates_the_resolution_exactly_once_and_changes_nothing() {
    let router = emulator();
    let rows = rows();
    let experiment_id = experiment(&router, "pagination", &rows).await;
    let total: usize = rows.iter().map(|row| row.expected).sum();

    // The rows here are page sizes: the published floor, every size up to one past the
    // resolution, the published ceiling, and no `maxResults` at all.
    let mut sizes: Vec<Option<i32>> = (MAX_RESULTS_FLOOR
        ..=i32::try_from(total + 1).expect("the resolution size fits an i32"))
        .map(Some)
        .collect();
    sizes.push(Some(MAX_RESULTS_CEILING));
    sizes.push(None);

    let mut first_order: Option<Vec<Value>> = None;
    for page_size in sizes {
        let case = format!("maxResults={page_size:?}");
        let walked = walk(&router, &experiment_id, page_size, None, total + 1).await;
        assert_eq!(
            walked.len(),
            total,
            "case {case:?}: expected {total} resolved targets, got {walked:?}"
        );
        let keys: Vec<(String, String)> = walked
            .iter()
            .map(|entry| {
                (
                    entry["targetName"].as_str().unwrap_or_default().to_string(),
                    resolved_arn(&case, entry),
                )
            })
            .collect();
        let unique: BTreeSet<(String, String)> = keys.iter().cloned().collect();
        assert_eq!(
            keys.len(),
            unique.len(),
            "case {case:?}: a resolved target was served twice: {keys:?}"
        );
        match &first_order {
            None => first_order = Some(walked),
            Some(order) => assert_eq!(
                &walked, order,
                "case {case:?}: expected the same order at every page size, got {walked:?} \
                 after {order:?}"
            ),
        }
    }

    // A read must not change what a later caller sees.
    let again = walk(&router, &experiment_id, Some(3), None, total + 1).await;
    let before = first_order.expect("at least one page size was walked");
    assert_eq!(
        again, before,
        "case re-read: expected listing again to answer the same resolution, got {again:?} \
         after {before:?}"
    );
}

#[tokio::test]
async fn a_target_name_narrows_the_page_to_that_target_only() {
    let router = emulator();
    let rows = rows();
    let experiment_id = experiment(&router, "filter", &rows).await;
    let total: usize = rows.iter().map(|row| row.expected).sum();

    for row in &rows {
        let case = row.case;
        let filtered = walk(
            &router,
            &experiment_id,
            Some(1),
            Some(row.name),
            total + 1,
        )
        .await;
        for entry in &filtered {
            assert_eq!(
                entry["targetName"],
                json!(row.name),
                "case {case:?}: expected only {:?} entries, got {entry:?}",
                row.name
            );
        }
        // The filtered page satisfies the same rule the unfiltered one does.
        assert_row(row, &filtered);
    }

    // A target name no target of this experiment carries resolves to nothing.
    for (case, target_name) in [
        ("an-unknown-target-name", "noSuchTarget"),
        ("a-target-name-of-another-casing", "ALLINSTANCES"),
    ] {
        let answer = list(&router, &experiment_id, None, None, Some(target_name)).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert!(
            answer.resolved(case).is_empty(),
            "case {case:?}: expected no resolved targets, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.next_token(),
            None,
            "case {case:?}: expected no nextToken, got {:?}",
            answer.body
        );
    }
}

#[tokio::test]
async fn a_resolution_reports_only_the_named_experiments_targets() {
    let router = emulator();
    let rows = rows();

    // Each experiment gets a DIFFERENT subset of the rows, so a handler that ignored the
    // experiment it was asked about would report a target the other experiment owns.
    let (mine, theirs) = rows.split_at(4);
    let first = experiment(&router, "scope-first", mine).await;
    let second = experiment(&router, "scope-second", theirs).await;

    for (case, experiment_id, owned) in [
        ("first-experiment", &first, mine),
        ("second-experiment", &second, theirs),
    ] {
        let total: usize = owned.iter().map(|row| row.expected).sum();
        let listed = walk(&router, experiment_id, Some(2), None, total + 1).await;
        let names: BTreeSet<String> = listed
            .iter()
            .map(|entry| entry["targetName"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: BTreeSet<String> = owned
            .iter()
            .filter(|row| row.expected > 0)
            .map(|row| row.name.to_string())
            .collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected only that experiment's targets, got {names:?}"
        );
        for row in owned {
            assert_row(row, &listed);
        }
    }

    // An experiment whose template named no target at all resolves to nothing.
    let bare = experiment(&router, "scope-empty", &[]).await;
    let empty = list(&router, &bare, None, None, None).await;
    assert_eq!(
        empty.status, 200,
        "case no-targets: expected HTTP 200, got {} with {:?}",
        empty.status, empty.body
    );
    assert!(
        empty.resolved("no-targets").is_empty(),
        "case no-targets: expected no resolved targets, got {:?}",
        empty.body
    );
    assert_eq!(
        empty.next_token(),
        None,
        "case no-targets: expected no nextToken, got {:?}",
        empty.body
    );
}

#[tokio::test]
async fn a_list_of_an_experiment_the_store_does_not_hold_is_not_found() {
    let router = emulator();
    let rows = rows();
    let existing = experiment(&router, "not-found", &rows).await;

    // Each row names no experiment this store holds: a well-formed id of the right shape
    // (deliberately far past the ids this store has minted), one of the experiment-TEMPLATE
    // collection, a value that is no minted id at all, and the existing id with a
    // character appended.
    for (case, experiment_id) in [
        ("unknown-experiment-id", "EXP999999999999".to_string()),
        ("an-experiment-template-id", "EXT999999999999".to_string()),
        ("not-an-id", "nonsense".to_string()),
        ("the-existing-id-with-a-suffix", format!("{existing}-x")),
    ] {
        let answer = list(&router, &experiment_id, None, None, None).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            answer.body
        );
    }
}

#[tokio::test]
async fn a_token_this_operation_never_issued_and_a_max_results_out_of_range_are_refused() {
    let router = emulator();
    let rows = rows();
    let experiment_id = experiment(&router, "validation", &rows).await;

    // A token the operation did issue is accepted — the rows below are refused for being
    // tokens it never issued, not for being tokens at all.
    let first_page = list(&router, &experiment_id, Some(1), None, None).await;
    let issued = first_page
        .next_token()
        .unwrap_or_else(|| panic!("a full page over this resolution carries a nextToken"));
    let resumed = list(&router, &experiment_id, Some(1), Some(&issued), None).await;
    assert_eq!(
        resumed.status, 200,
        "case issued-token: expected HTTP 200, got {} with {:?}",
        resumed.status, resumed.body
    );

    let marker = &issued[..=issued
        .rfind('.')
        .expect("the issued token carries a separator")];
    for (case, bad_token) in [
        ("a-plain-word", "nonsense".to_string()),
        ("a-bare-position", "1".to_string()),
        ("an-experiment-id", experiment_id.clone()),
        ("an-experiment-template-id", "EXT000000000001".to_string()),
        ("the-marker-alone", marker.to_string()),
        ("the-marker-with-a-word", format!("{marker}first")),
        ("the-marker-with-a-negative-position", format!("{marker}-1")),
        (
            "the-marker-with-a-fractional-position",
            format!("{marker}1.5"),
        ),
        (
            "the-issued-token-without-its-marker",
            issued[marker.len()..].to_string(),
        ),
        (
            "the-issued-token-with-a-mangled-marker",
            format!("x{issued}"),
        ),
    ] {
        let answer = list(&router, &experiment_id, None, Some(&bad_token), None).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} ({bad_token:?}): expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?} ({bad_token:?}): expected ValidationException, got {:?}",
            answer.body
        );
    }

    // `maxResults` on each published bound, and one past each of them.
    for (case, max_results, expected_status) in [
        ("at-the-floor", MAX_RESULTS_FLOOR, 200),
        ("at-the-ceiling", MAX_RESULTS_CEILING, 200),
        ("below-the-floor", MAX_RESULTS_FLOOR - 1, 400),
        ("above-the-ceiling", MAX_RESULTS_CEILING + 1, 400),
    ] {
        let answer = list(&router, &experiment_id, Some(max_results), None, None).await;
        assert_eq!(
            answer.status, expected_status,
            "case maxResults-{case}: expected HTTP {expected_status}, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

#[tokio::test]
async fn a_target_on_the_published_resource_bound_resolves_and_one_past_it_is_refused() {
    let router = emulator();

    // Two rows: a target naming exactly as many resources as `ResourceArnList` admits,
    // and one naming a single resource more. The first is accepted and every named
    // resource resolves; the second is refused at the boundary, so no request can leave a
    // resolution the model does not admit behind.
    for (case, named, expected_status) in [
        ("at-the-ceiling", TARGET_RESOURCE_ARNS_CEILING, 200),
        ("one-past-the-ceiling", TARGET_RESOURCE_ARNS_CEILING + 1, 400),
    ] {
        let row = Row {
            case: "bounded",
            name: "manyInstances",
            resource_type: "aws:ec2:instance",
            parameters: None,
            selection_mode: "ALL",
            named: (1..=named)
                .map(|n| resource_arn("ec2", "instance", n))
                .collect(),
            resource_tags: None,
            expected: named,
        };
        let created = send(
            &router,
            "POST",
            "/experimentTemplates",
            Some(template_body(case, std::slice::from_ref(&row))),
        )
        .await;
        assert_eq!(
            created.status, expected_status,
            "case {case:?}: expected HTTP {expected_status} from CreateExperimentTemplate over \
             {named} resourceArns, got {} with {:?}",
            created.status, created.body
        );
        if expected_status != 200 {
            assert_eq!(
                created.error_code(),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {:?}",
                created.body
            );
            continue;
        }

        let experiment_id = experiment(&router, case, std::slice::from_ref(&row)).await;
        let listed = walk(&router, &experiment_id, None, None, named + 1).await;
        assert_eq!(
            listed.len(),
            named,
            "case {case:?}: expected {named} resolved targets, got {}",
            listed.len()
        );
        assert_row(&row, &listed);
    }
}

#[tokio::test]
async fn a_token_resumes_where_the_previous_page_ended_whatever_the_filter() {
    let router = emulator();
    let rows = rows();
    let experiment_id = experiment(&router, "resume", &rows).await;
    let total: usize = rows.iter().map(|row| row.expected).sum();

    // The whole resolution, as the reference order.
    let whole = walk(&router, &experiment_id, None, None, total + 1).await;

    // A token names a POSITION in that resolution, so resuming from the token a page of
    // the unfiltered walk handed back, with a `targetName` added, reports exactly the
    // entries of that target from that position on.
    let mut per_target: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for entry in &whole {
        per_target
            .entry(entry["targetName"].as_str().unwrap_or_default().to_string())
            .or_default()
            .push(entry.clone());
    }

    let first = list(&router, &experiment_id, Some(1), None, None).await;
    let token = first
        .next_token()
        .expect("a full page over this resolution carries a nextToken");
    let served = &whole[..1];
    for (name, entries) in &per_target {
        let case = format!("resume-{name}");
        let remaining: Vec<Value> = entries
            .iter()
            .filter(|entry| !served.contains(entry))
            .cloned()
            .collect();
        let answer = list(
            &router,
            &experiment_id,
            Some(MAX_RESULTS_CEILING),
            Some(&token),
            Some(name),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.resolved(&case),
            remaining,
            "case {case:?}: expected the entries of {name:?} from the resumed position on, \
             got {:?}",
            answer.body
        );
    }
}

/// One case of the in-flight rule: which targets the experiment carries, and what the read
/// taken while its resolution is still in flight asks for.
struct InFlightCase {
    case: &'static str,
    /// The names of the [`rows`] the experiment's template carries as its targets — empty
    /// for a template that carries no target at all.
    targets: &'static [&'static str],
    max_results: Option<i32>,
    target_name: Option<&'static str>,
}

/// RULE: an experiment RESOLVES its targets while it is starting up, so a read of that
/// resolution taken before the resolution has ENDED is refused with `ValidationException`
/// (HTTP 400) and the sentence FIS words that refusal in — whatever the read asks for and
/// whatever the resolution turns out to hold — while the same read, once the resolution has
/// ended, is answered with the resolution itself.
#[tokio::test]
async fn a_resolution_read_is_refused_until_the_resolution_has_ended() {
    // The rows differ along what the read asks for (no query at all, a page size at the
    // published floor, a page size at the published ceiling, one target by name, a
    // `targetName` no target of the experiment carries) and along what the experiment will
    // resolve to (several resources, one, none at all, and an experiment carrying no target
    // whatsoever): the refusal is the same for every one of them, because it is about the
    // resolution not having ended rather than about what it holds.
    let cases = [
        InFlightCase {
            case: "no-query-over-three-resolved-resources",
            targets: &["allInstances"],
            max_results: None,
            target_name: None,
        },
        InFlightCase {
            case: "a-page-size-at-the-published-floor",
            targets: &["allInstances"],
            max_results: Some(MAX_RESULTS_FLOOR),
            target_name: None,
        },
        InFlightCase {
            case: "a-page-size-at-the-published-ceiling",
            targets: &["allInstances", "countOneInstance"],
            max_results: Some(MAX_RESULTS_CEILING),
            target_name: None,
        },
        InFlightCase {
            case: "one-target-by-name",
            targets: &["allInstances", "countOneInstance"],
            max_results: None,
            target_name: Some("countOneInstance"),
        },
        InFlightCase {
            case: "a-target-name-no-target-of-the-experiment-carries",
            targets: &["allInstances"],
            max_results: None,
            target_name: Some("absentTarget"),
        },
        InFlightCase {
            case: "a-target-that-resolves-to-nothing",
            targets: &["taggedInstances"],
            max_results: None,
            target_name: None,
        },
        InFlightCase {
            case: "an-experiment-carrying-no-target-at-all",
            targets: &[],
            max_results: None,
            target_name: None,
        },
    ];

    for case in cases {
        // One emulator per case: a case holds its experiment in the window its targets are
        // being resolved in, and an account holds only so many experiments at once.
        let (router, engine) = emulator_and_engine();
        let targets: Vec<Row> = rows()
            .into_iter()
            .filter(|row| case.targets.contains(&row.name))
            .collect();
        assert_eq!(
            targets.len(),
            case.targets.len(),
            "case {:?}: the case names targets this suite's rows do not hold: {:?}",
            case.case,
            case.targets
        );
        // What the answered read must carry: what the targets the read asks about resolve
        // to, cut to the page size the read asked for.
        let resolvable: usize = targets
            .iter()
            .filter(|row| case.target_name.is_none_or(|name| row.name == name))
            .map(|row| row.expected)
            .sum();
        let page_size = case.max_results.map_or(usize::MAX, |size| {
            usize::try_from(size).expect("a published page size is positive")
        });
        let expected = resolvable.min(page_size);

        let experiment_id = started_experiment(&router, case.case, &targets).await;
        // The experiment is HELD in the window its targets are being resolved in, so what
        // the reads below are taken inside is decided by the experiment's clock rather than
        // by how quickly this host gets from the start to the read.
        hold_resolution_open(&engine, &experiment_id);

        // Two reads taken while the resolution is in flight: the refusal is what the state
        // of the resolution says, so it is not spent by being reported once.
        for read in ["the-first-read", "the-read-after-it"] {
            let refused = list(
                &router,
                &experiment_id,
                case.max_results,
                None,
                case.target_name,
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {:?}/{read}: expected HTTP 400 while the resolution is in flight, \
                 got {} with {:?}",
                case.case, refused.status, refused.body
            );
            assert_eq!(
                refused.error_code(),
                "ValidationException",
                "case {:?}/{read}: expected ValidationException while the resolution is in \
                 flight, got {:?}",
                case.case,
                refused.body
            );
            assert_eq!(
                refused.member("message").as_str(),
                Some(TARGET_RESOLUTION_ONGOING),
                "case {:?}/{read}: expected the message {TARGET_RESOLUTION_ONGOING:?}, \
                 got {:?}",
                case.case,
                refused.body
            );
        }

        // An `experimentId` that names no record is still reported MISSING rather than as a
        // resolution in flight: the experiment the path names is resolved before anything
        // about its resolution is.
        let missing = list(
            &router,
            "EXP000000000099",
            case.max_results,
            None,
            case.target_name,
        )
        .await;
        assert_eq!(
            missing.status, 404,
            "case {:?}: expected HTTP 404 for an experiment that does not exist, \
             got {} with {:?}",
            case.case, missing.status, missing.body
        );
        assert_eq!(
            missing.error_code(),
            "ResourceNotFoundException",
            "case {:?}: expected ResourceNotFoundException for an experiment that does not \
             exist, got {:?}",
            case.case,
            missing.body
        );

        // Once the resolution has ENDED — the transition the clock owes the experiment is
        // due, and the experiment has moved off `initiating` — the SAME read is answered
        // with the resolution.
        end_resolution(&engine, &experiment_id);
        poll_until_resolution_ends(&router, &experiment_id, RESOLUTION_BUDGET).await;
        let answered = list(
            &router,
            &experiment_id,
            case.max_results,
            None,
            case.target_name,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected HTTP 200 once the resolution has ended, got {} with {:?}",
            case.case, answered.status, answered.body
        );
        let entries = answered.resolved(case.case);
        assert_eq!(
            entries.len(),
            expected,
            "case {:?}: expected {expected} resolved target(s) once the resolution has \
             ended, got {entries:?}",
            case.case
        );
        for entry in &entries {
            let name = entry["targetName"].as_str().unwrap_or_default();
            let row = targets
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: an entry names a target the experiment does not carry: \
                         {entry:?}",
                        case.case
                    )
                });
            assert_eq!(
                entry["resourceType"].as_str(),
                Some(row.resource_type),
                "case {:?}: every entry carries its target's resourceType, got {entry:?}",
                case.case
            );
        }

        // The refusal consumed nothing and the answered read moved nothing: the same read
        // reports the same page again.
        let again = list(
            &router,
            &experiment_id,
            case.max_results,
            None,
            case.target_name,
        )
        .await;
        assert_eq!(
            again.status, 200,
            "case {:?}: expected HTTP 200 from the repeated read, got {} with {:?}",
            case.case, again.status, again.body
        );
        assert_eq!(
            again.resolved(case.case),
            entries,
            "case {:?}: expected the repeated read to report the same page, got {:?}",
            case.case,
            again.body
        );
    }
}
