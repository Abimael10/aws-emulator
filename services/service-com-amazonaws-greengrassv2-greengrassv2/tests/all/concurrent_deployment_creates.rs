// SPDX-License-Identifier: CC0-1.0

//! What several callers creating a deployment for ONE target at the same instant are
//! answered, and what the target is left holding.
//!
//! The rules under test:
//!
//! * a `CreateDeployment` is answered by this service's own handler — the request names
//!   the route the 2020-11-30 model binds for it (`POST /greengrass/v2/deployments`), and
//!   however many callers address it at once, each answer is either that operation's
//!   modelled success (201 with a `deploymentId`, an `iotJobId` and an `iotJobArn`) or one
//!   of the error shapes its own model declares for it; no other code may reach the wire,
//!   and in particular no refusal that stands for "the request could not be placed";
//! * the lookup and the insert happen as ONE step, so the revisions concurrent callers
//!   create are distinct — distinct deployment ids, distinct revision ids — and the target
//!   is left with exactly ONE latest revision, which is the one its readers report as
//!   latest and the one `historyFilter=LATEST_ONLY` selects, while every earlier revision
//!   stays in the history and is no longer latest;
//! * a `clientToken` is an idempotency token, so N callers presenting ONE token for one
//!   target are answered with ONE deployment — the same identity to every caller, or that
//!   operation's modelled "already in progress" refusal — and the target holds exactly one
//!   revision afterwards.
//!
//! Every assertion drives the `restJson1` front door: requests are built the way an SDK
//! builds them, dispatched through [`MultiServiceRouter`], and judged on the HTTP status
//! and the JSON body, so nothing here can be satisfied by state a reader does not report.
//!
//! The rows differ along what the rules generalise over — how many callers race (one, a
//! pair, and more than a pair), which target FORM they race on (a thing, a thing group, a
//! subdeployment under a parent thing group), and whether they present a shared
//! idempotency token — not along the names, which the rules say nothing about.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_greengrassv2_greengrassv2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these requests are signed for.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The status `CreateDeployment` models for success
/// (`smithy_query(query="operation",operation="CreateDeployment")` -> `code 201`).
const CREATED: u16 = 201;

/// Every error shape `CreateDeployment` declares, with the status it carries, from the
/// same model citation: `AccessDeniedException` 403, `ConflictException` 409,
/// `InternalServerException` 500, `RequestAlreadyInProgressException` 400,
/// `ResourceNotFoundException` 404, `ThrottlingException` 429, `ValidationException` 400.
/// An answer naming anything else is a code no client of this API can handle.
const MODELLED_CREATE_ERRORS: &[(&str, u16)] = &[
    ("AccessDeniedException", 403),
    ("ConflictException", 409),
    ("InternalServerException", 500),
    ("RequestAlreadyInProgressException", 400),
    ("ResourceNotFoundException", 404),
    ("ThrottlingException", 429),
    ("ValidationException", 400),
];

/// The refusal AWS models for a racer that lost a create to another operation on the same
/// resource, and the one a caller re-presenting a token still being processed is answered.
const CONFLICT: &str = "ConflictException";
const ALREADY_IN_PROGRESS: &str = "RequestAlreadyInProgressException";

/// The `DeploymentStatus` the deployment that is current for its target carries.
const STATUS_ACTIVE: &str = "ACTIVE";

/// The `DeploymentHistoryFilter` values: the whole history, and the latest revision of
/// each target alone.
const HISTORY_ALL: &str = "ALL";
const HISTORY_LATEST_ONLY: &str = "LATEST_ONLY";

/// The emulator front door with this service mounted over ONE shared engine — exactly how
/// `core/router` assembles it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One answer from the front door: its status and its parsed body.
#[derive(Debug, Clone)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The `restJson1` error code this answer names, or the empty string when it names
    /// none.
    fn error_code(&self) -> String {
        let raw = self
            .body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        raw.rsplit(['#', '.']).next().unwrap_or(raw).to_string()
    }

    /// A string member of the body, or the empty string when it is absent.
    fn text(&self, key: &str) -> String {
        self.body
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Dispatch one signed request through the router.
async fn send(router: &MultiServiceRouter, method: &str, path: &str, body: Vec<u8>) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://greengrass.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/greengrass/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Answer { status, body }
}

/// `CreateDeployment` with `request` as its body.
async fn create_deployment(router: &MultiServiceRouter, request: &Value) -> Answer {
    let body = serde_json::to_vec(request).expect("a serializable request body");
    send(router, "POST", "/greengrass/v2/deployments", body).await
}

/// `GetDeployment` on `deployment_id`.
async fn get_deployment(router: &MultiServiceRouter, deployment_id: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/greengrass/v2/deployments/{deployment_id}"),
        Vec::new(),
    )
    .await
}

/// `ListDeployments` for one target under one history filter.
///
/// The `targetArn` goes into the query as written: `:` and `/` stand for themselves in a
/// query component (RFC 3986 `query = *( pchar / "/" / "?" )`), which is how an SDK sends
/// an ARN-valued `@httpQuery` member.
async fn list_deployments(router: &MultiServiceRouter, target: &str, filter: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/greengrass/v2/deployments?targetArn={target}&historyFilter={filter}"),
        Vec::new(),
    )
    .await
}

/// The entries a listing reported.
fn listed(answer: &Answer) -> Vec<Value> {
    answer
        .body
        .get("deployments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A string member of a listed entry.
fn entry_text(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An IoT thing ARN: one of the two target forms a deployment may name
/// (`TargetARN`: `^arn:[^:]*:iot:[^:]*:[0-9]+:(thing|thinggroup)/.+$`).
fn thing_target(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{name}")
}

/// An IoT thing-group ARN: the other target form, and the only one a subdeployment's
/// parent may be (`ThingGroupARN`).
fn thing_group_target(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:thinggroup/{name}")
}

/// Judge one racer's answer: it is this operation's modelled success, or one of the error
/// shapes its own model declares, at that shape's own status. Answers the new deployment's
/// identity for a success, `None` for a modelled refusal.
fn judge(case: &str, racer: usize, answer: &Answer) -> Option<(String, String, String)> {
    if answer.status == CREATED {
        let deployment_id = answer.text("deploymentId");
        let iot_job_id = answer.text("iotJobId");
        let iot_job_arn = answer.text("iotJobArn");
        assert!(
            !deployment_id.is_empty() && !iot_job_id.is_empty() && !iot_job_arn.is_empty(),
            "case {case:?} racer {racer}: a served create reports a deploymentId, an \
             iotJobId and an iotJobArn, got {:?}",
            answer.body
        );
        return Some((deployment_id, iot_job_id, iot_job_arn));
    }
    let code = answer.error_code();
    let modelled = MODELLED_CREATE_ERRORS
        .iter()
        .find(|(shape, _)| shape == &code);
    let Some((shape, status)) = modelled else {
        panic!(
            "case {case:?} racer {racer}: expected {CREATED} or an error shape \
             CreateDeployment models, got {} naming {code:?} with {:?}",
            answer.status, answer.body
        );
    };
    assert_eq!(
        answer.status, *status,
        "case {case:?} racer {racer}: {shape} carries HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    None
}

/// One row of the racing-create table: the case name, how many callers race, the target
/// they race on, and the parent thing group the deployment is a subdeployment of.
struct RaceCase {
    case: &'static str,
    racers: usize,
    target: String,
    parent: Option<String>,
}

/// The rows the racing rule is checked over: one caller (the degenerate race), a pair, and
/// more than a pair, over both target forms and over a subdeployment under a parent.
fn race_rows() -> Vec<RaceCase> {
    vec![
        RaceCase {
            case: "one caller, a thing target",
            racers: 1,
            target: thing_target("solo-core"),
            parent: None,
        },
        RaceCase {
            case: "two callers, a thing target",
            racers: 2,
            target: thing_target("paired-core"),
            parent: None,
        },
        RaceCase {
            case: "five callers, a thing target",
            racers: 5,
            target: thing_target("contended-core"),
            parent: None,
        },
        RaceCase {
            case: "five callers, a thing-group target",
            racers: 5,
            target: thing_group_target("contended-fleet"),
            parent: None,
        },
        RaceCase {
            case: "nine callers, a subdeployment under a parent group",
            racers: 9,
            target: thing_group_target("sub-fleet"),
            parent: Some(thing_group_target("parent-fleet")),
        },
    ]
}

/// RULE: however many callers create a deployment for one target at the same instant, each
/// is answered either that operation's modelled success or an error shape its own model
/// declares; the revisions they create are distinct; and the target is left with exactly
/// one latest revision, which is what its readers report.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_creates_for_one_target_leave_exactly_one_latest_revision() {
    let router = Arc::new(emulator());
    for RaceCase {
        case,
        racers,
        target,
        parent,
    } in race_rows()
    {
        let mut running = Vec::new();
        for racer in 0..racers {
            let router = Arc::clone(&router);
            let mut request = json!({
                "targetArn": target,
                // A per-racer name, so each answer identifies WHICH create it served
                // rather than being the same value either way.
                "deploymentName": format!("{case} revision {racer}"),
            });
            if let Some(parent) = parent.clone() {
                request["parentTargetArn"] = Value::String(parent);
            }
            running.push(tokio::spawn(async move {
                (racer, create_deployment(&router, &request).await)
            }));
        }

        let mut served: Vec<(usize, String)> = Vec::new();
        let mut refused: Vec<(usize, String)> = Vec::new();
        for task in running {
            let (racer, answer) = task.await.expect("the racing create task ran");
            match judge(case, racer, &answer) {
                Some((deployment_id, _, _)) => served.push((racer, deployment_id)),
                None => refused.push((racer, answer.error_code())),
            }
        }

        for (racer, code) in &refused {
            assert_eq!(
                code, CONFLICT,
                "case {case:?} racer {racer}: a racer that loses a create to another \
                 operation on the same target is refused {CONFLICT}, got {code:?}"
            );
        }
        // "You can define one deployment for each target. When you create a new deployment
        // for a target that has an existing deployment, you replace the previous
        // deployment" — a create is never in contention with the deployment it replaces, so
        // every racer gets a revision of its own and none has to be refused. A refusal that
        // reaches a caller has to be one of the shapes judged above, but none is owed here.
        assert_eq!(
            served.len(),
            racers,
            "case {case:?}: each of {racers} racing creates records its own revision of the \
             target, got {} served and {refused:?} refused",
            served.len()
        );

        // Distinct revisions: no two served creates may report one deployment identity.
        let ids: BTreeSet<String> = served.iter().map(|(_, id)| id.clone()).collect();
        assert_eq!(
            ids.len(),
            served.len(),
            "case {case:?}: each served create mints its own deploymentId, got {served:?}"
        );

        // The whole history for this target is exactly what the served creates recorded,
        // and each entry carries its own revision id.
        let all = list_deployments(&router, &target, HISTORY_ALL).await;
        assert_eq!(
            all.status, 200,
            "case {case:?}: listing the target's history must answer 200, got {} with {:?}",
            all.status, all.body
        );
        let history = listed(&all);
        let listed_ids: BTreeSet<String> = history
            .iter()
            .map(|entry| entry_text(entry, "deploymentId"))
            .collect();
        assert_eq!(
            listed_ids, ids,
            "case {case:?}: the target's history is exactly the served creates, got {:?}",
            all.body
        );
        let revisions: BTreeSet<String> = history
            .iter()
            .map(|entry| entry_text(entry, "revisionId"))
            .collect();
        assert_eq!(
            revisions.len(),
            history.len(),
            "case {case:?}: each revision of one target carries its own revisionId, got \
             {:?}",
            all.body
        );

        // Exactly one of them is the latest for the target, in the history and under the
        // filter that selects it.
        let latest_in_history: Vec<String> = history
            .iter()
            .filter(|entry| entry.get("isLatestForTarget").and_then(Value::as_bool) == Some(true))
            .map(|entry| entry_text(entry, "deploymentId"))
            .collect();
        assert_eq!(
            latest_in_history.len(),
            1,
            "case {case:?}: exactly one revision of a target is the latest, got \
             {latest_in_history:?} in {:?}",
            all.body
        );
        let latest_only = list_deployments(&router, &target, HISTORY_LATEST_ONLY).await;
        let selected: Vec<String> = listed(&latest_only)
            .iter()
            .map(|entry| entry_text(entry, "deploymentId"))
            .collect();
        assert_eq!(
            selected, latest_in_history,
            "case {case:?}: {HISTORY_LATEST_ONLY} selects the target's latest revision \
             alone, got {:?}",
            latest_only.body
        );

        // And that revision reads back as the target's current deployment.
        let latest_id = &latest_in_history[0];
        let read = get_deployment(&router, latest_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the latest revision must read back, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            (
                read.text("deploymentId"),
                read.text("targetArn"),
                read.text("deploymentStatus"),
                read.body.get("isLatestForTarget").and_then(Value::as_bool),
            ),
            (
                latest_id.clone(),
                target.clone(),
                STATUS_ACTIVE.to_string(),
                Some(true),
            ),
            "case {case:?}: the latest revision is the ACTIVE, latest deployment of its \
             target, got {:?}",
            read.body
        );
        assert_eq!(
            read.text("parentTargetArn"),
            parent.clone().unwrap_or_default(),
            "case {case:?}: the parent a subdeployment named is the one it reports, got \
             {:?}",
            read.body
        );

        // Every earlier revision survives the supersede and is no longer the latest.
        for entry in &history {
            let id = entry_text(entry, "deploymentId");
            if &id == latest_id {
                continue;
            }
            let superseded = get_deployment(&router, &id).await;
            assert_eq!(
                (
                    superseded.status,
                    superseded
                        .body
                        .get("isLatestForTarget")
                        .and_then(Value::as_bool),
                ),
                (200, Some(false)),
                "case {case:?}: a superseded revision stays readable and is not the \
                 latest, got {:?}",
                superseded.body
            );
        }
    }
}

/// How many callers present one idempotency token in the row below: more than a pair, so
/// the row is about ANY number of concurrent repeats.
const TOKEN_REPEATS: usize = 5;

/// RULE: a `clientToken` is an idempotency token, so N callers presenting ONE token for
/// one target are answered with ONE deployment — the identity the first create minted, or
/// the modelled refusal of a token still being processed — and the target holds exactly
/// one revision afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_repeats_of_one_client_token_create_one_deployment() {
    let router = Arc::new(emulator());
    let target = thing_group_target("idempotent-fleet");
    // `ClientTokenString`: len 1..64, pattern `^[a-zA-Z0-9-]+$`.
    let token = "one-token-many-callers";

    let mut running = Vec::new();
    for racer in 0..TOKEN_REPEATS {
        let router = Arc::clone(&router);
        let request = json!({
            "targetArn": target,
            "clientToken": token,
            "deploymentName": format!("repeat {racer}"),
        });
        running.push(tokio::spawn(async move {
            (racer, create_deployment(&router, &request).await)
        }));
    }

    let case = "five callers, one clientToken";
    let mut served: BTreeSet<String> = BTreeSet::new();
    let mut refused: Vec<(usize, String)> = Vec::new();
    for task in running {
        let (racer, answer) = task.await.expect("the racing create task ran");
        match judge(case, racer, &answer) {
            Some((deployment_id, _, _)) => {
                served.insert(deployment_id);
            }
            None => refused.push((racer, answer.error_code())),
        }
    }

    for (racer, code) in &refused {
        assert_eq!(
            code, ALREADY_IN_PROGRESS,
            "case {case:?} racer {racer}: a repeat of a token still being processed is \
             refused {ALREADY_IN_PROGRESS}, got {code:?}"
        );
    }
    assert_eq!(
        served.len(),
        1,
        "case {case:?}: every caller presenting one token is answered with the one \
         deployment it created, got {served:?} (refused: {refused:?})"
    );

    let all = list_deployments(&router, &target, HISTORY_ALL).await;
    let ids: Vec<String> = listed(&all)
        .iter()
        .map(|entry| entry_text(entry, "deploymentId"))
        .collect();
    assert_eq!(
        ids,
        served.iter().cloned().collect::<Vec<String>>(),
        "case {case:?}: one token leaves one revision on the target, got {:?}",
        all.body
    );
}
