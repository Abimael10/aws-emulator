// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the IDENTIFIER `CreateAsset` mints.
//!
//! The rule under test: the asset plane names an asset by an opaque identifier of
//! its own — the canonical UUID text, and nothing else — drawn per asset, so what
//! names one asset carries nothing about the other resources the store holds and the
//! same call sequence against two stores does not name one asset twice.
//!
//! The INVARIANTS of that identifier are asserted over drawn creates at the end of
//! this file: whatever a create carries, the identifier it answers stays inside the
//! bounds its member publishes, is the handle the asset is read back by, repeats for
//! an idempotent replay of the same create and for nothing else, and is never the
//! same for two assets.
//!
//! Every case drives the service through the same router the emulator serves, so
//! the routes, status codes, protocol envelope and state are all exercised.

use std::sync::Arc;

use bytes::Bytes;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The characters a minted asset identifier carries, as the service's own state
/// publishes that form.
use service_com_amazonaws_devopsagent_devopsagent::state::ASSET_ID_LEN;

/// A SigV4 credential scope naming this service's signing name (`aidevops`), which
/// is how the router resolves which service claims a request. Signature
/// verification is switched off for these tests, so the signature is a placeholder.
const AUTHORIZATION: &str = concat!(
    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-west-2/aidevops/aws4_request, ",
    "SignedHeaders=host, Signature=0123456789abcdef"
);

/// The published bounds of `ResourceId`, the shape `Asset$assetId` targets:
/// `@length(min: 1, max: 128)` with `@pattern ^[a-zA-Z0-9_.-]+$`, from
/// `smithy_query(query="shape", shape="ResourceId")`.
const RESOURCE_ID_MIN_LEN: usize = 1;
const RESOURCE_ID_MAX_LEN: usize = 128;

/// One emulator: a single shared engine behind the production router.
fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_devopsagent_devopsagent::EmulatedService::new(engine),
    );
    router
}

/// Send one request and return its status and parsed body (`Value::Null` for an
/// empty body).
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .as_ref()
        .map(|b| Bytes::from(serde_json::to_vec(b).expect("serialize request body")))
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "aidevops.us-west-2.amazonaws.com")
        .header("authorization", AUTHORIZATION);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "response body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`) of an error response.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an error response.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Create an agent space and return its id.
async fn create_space(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(router, "POST", "/v1/agentspaces", Some(json!({"name": name}))).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateAgentSpace, got {status}: {body}"
    );
    body["agentSpace"]["agentSpaceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no agentSpaceId in {body}"))
        .to_string()
}

/// Create a backlog task in `space` and return its id — one of the OTHER resource
/// kinds whose creation must leave an asset's identifier alone.
async fn create_task(router: &MultiServiceRouter, case: &str, space: &str, title: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/backlog/agent-space/{space}/tasks"),
        Some(json!({"taskType": "INVESTIGATION", "title": title, "priority": "MEDIUM"})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateBacklogTask, got {status}: {body}"
    );
    body["task"]["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no taskId in {body}"))
        .to_string()
}

/// Create a Trigger in `space` and return its id — another of those other resource
/// kinds.
async fn create_trigger(
    router: &MultiServiceRouter,
    case: &str,
    space: &str,
    expression: &str,
) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/trigger/agent-space/{space}/triggers"),
        Some(json!({
            "type": "SCHEDULE",
            "condition": {"schedule": {"expression": expression}},
            "action": {"agent": "custom", "prompt": "review the oncall backlog"},
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateTrigger, got {status}: {body}"
    );
    body["trigger"]["triggerId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no triggerId in {body}"))
        .to_string()
}

/// `POST .../assets` in `space`, answering the created asset's identifier.
async fn create_asset(
    router: &MultiServiceRouter,
    case: &str,
    space: &str,
    request: Value,
) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/asset/agent-space/{space}/assets"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateAsset, got {status}: {body}"
    );
    body["asset"]["assetId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no assetId in {body}"))
        .to_string()
}

fn get_asset_uri(space: &str, asset: &str) -> String {
    format!("/asset/agent-space/{space}/assets/{asset}")
}

/// A create request for an asset of `asset_type` whose one file is at `path`.
fn asset_request(asset_type: &str, path: &str) -> Value {
    let mut request = json!({
        "assetType": asset_type,
        "content": {"file": {"path": path, "body": {"text": "# content"}}},
    });
    // The types that require a metadata field are given one, so a row's identifier
    // is what the case is about and not a refused create. The required field per
    // type is `crate::state::ASSET_TYPE_RULES`.
    match asset_type {
        "skill" => request["metadata"] = json!({"name": path}),
        "artifact" => request["metadata"] = json!({"title": path}),
        "attachment" => request["metadata"] = json!({"filename": path}),
        _ => {}
    }
    request
}

/// Whether `value` is an opaque asset identifier of the asset plane's form: the
/// canonical UUID text — lowercase hexadecimal in `8-4-4-4-12` groups, carrying the
/// version digit `4` and a variant digit of `8`, `9`, `a` or `b`, which is what 16
/// opaque bytes rendered as a UUID come out as.
fn is_opaque_asset_identifier(value: &str) -> bool {
    /// The hexadecimal digits each hyphen-separated group of the canonical text has.
    const GROUP_LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];

    let mut groups = value.split('-');
    let mut seen: Vec<&str> = Vec::new();
    for length in GROUP_LENGTHS {
        let Some(group) = groups.next() else {
            return false;
        };
        let lowercase_hex = group
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if group.len() != length || !lowercase_hex {
            return false;
        }
        seen.push(group);
    }
    if groups.next().is_some() {
        return false;
    }
    value.chars().count() == ASSET_ID_LEN
        && seen[2].starts_with('4')
        && seen[3]
            .chars()
            .next()
            .is_some_and(|variant| matches!(variant, '8' | '9' | 'a' | 'b'))
}

/// Assert `asset_id` is an identifier of the asset plane's form: the opaque
/// identifier, inside the bounds and charset `ResourceId` publishes.
fn assert_asset_identifier(case: &str, asset_id: &str) {
    assert!(
        is_opaque_asset_identifier(asset_id),
        "case {case:?}: expected an opaque asset identifier of {ASSET_ID_LEN} characters in \
         the canonical UUID form, got {asset_id:?}"
    );
    assert_within_resource_id(case, asset_id);
}

/// Assert `asset_id` satisfies what the MODEL publishes for the member that reports it:
/// `ResourceId`'s length bound and charset, and nothing about the form the service
/// happens to draw inside them.
fn assert_within_resource_id(case: &str, asset_id: &str) {
    let length = asset_id.chars().count();
    assert!(
        (RESOURCE_ID_MIN_LEN..=RESOURCE_ID_MAX_LEN).contains(&length)
            && asset_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')),
        "case {case:?}: expected an identifier satisfying ResourceId \
         (^[a-zA-Z0-9_.-]+$, {RESOURCE_ID_MIN_LEN}..={RESOURCE_ID_MAX_LEN}), got {asset_id:?} \
         of length {length}"
    );
}

/// One store's worth of setup: how many resources of OTHER kinds exist before this
/// row's asset is created, and what the asset itself is.
struct Row {
    case: &'static str,
    /// Agent spaces created before the one the asset is created in.
    spaces_before: usize,
    /// Backlog tasks created in the asset's space before the asset.
    tasks_before: usize,
    /// Triggers created in the asset's space before the asset.
    triggers_before: usize,
    /// Assets created in the space before the one the row asserts on.
    assets_before: usize,
    /// The asset type the row's asset carries.
    asset_type: &'static str,
}

/// RULE: whatever else the store holds, `CreateAsset` answers an identifier of the
/// asset plane's own opaque form, and that identifier is the one a read of the asset
/// resolves.
#[tokio::test]
async fn created_asset_identifier_has_the_asset_plane_form() {
    // The rows differ along what the rule generalises over: how many resources of
    // every other kind this service mints (agent spaces, backlog tasks, Triggers)
    // were created first, how many assets the space already holds, and the asset's
    // own type. Each row runs against a FRESH store, so the counts above are the
    // whole store's creation count — the quantity the identifier must not carry.
    let rows = [
        Row {
            case: "the first resource of a fresh store after its space",
            spaces_before: 0,
            tasks_before: 0,
            triggers_before: 0,
            assets_before: 0,
            asset_type: "skill",
        },
        Row {
            case: "behind one backlog task",
            spaces_before: 0,
            tasks_before: 1,
            triggers_before: 0,
            assets_before: 0,
            asset_type: "artifact",
        },
        Row {
            case: "behind several resources of three other kinds",
            spaces_before: 2,
            tasks_before: 3,
            triggers_before: 2,
            assets_before: 0,
            asset_type: "runbook",
        },
        Row {
            case: "the fourth asset of its space",
            spaces_before: 1,
            tasks_before: 0,
            triggers_before: 1,
            assets_before: 3,
            asset_type: "attachment",
        },
        Row {
            case: "a type with no metadata requirement, behind a Trigger",
            spaces_before: 0,
            tasks_before: 2,
            triggers_before: 1,
            assets_before: 1,
            asset_type: "playbook",
        },
    ];

    let mut seen: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let router = new_router();
        for n in 0..row.spaces_before {
            create_space(&router, case, &format!("unrelated-space-{n}")).await;
        }
        let space = create_space(&router, case, "identifier-form").await;
        for n in 0..row.tasks_before {
            create_task(&router, case, &space, &format!("unrelated task {n}")).await;
        }
        for n in 0..row.triggers_before {
            create_trigger(&router, case, &space, &format!("rate({} hours)", n + 1)).await;
        }
        for n in 0..row.assets_before {
            let earlier = create_asset(
                &router,
                case,
                &space,
                asset_request(row.asset_type, &format!("earlier-{n}.md")),
            )
            .await;
            assert_asset_identifier(case, &earlier);
            assert!(
                !seen.contains(&earlier),
                "case {case:?}: identifier {earlier:?} was handed out twice"
            );
            seen.push(earlier);
        }

        let asset_id = create_asset(
            &router,
            case,
            &space,
            asset_request(row.asset_type, "asserted.md"),
        )
        .await;
        assert_asset_identifier(case, &asset_id);
        assert!(
            !seen.contains(&asset_id),
            "case {case:?}: identifier {asset_id:?} was handed out twice"
        );
        seen.push(asset_id.clone());

        // READ BACK: the identifier the create answered is the one the asset is
        // reachable by, and it reports the asset the create persisted.
        let (status, fetched) = call(&router, "GET", &get_asset_uri(&space, &asset_id), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetAsset on {asset_id:?}, got {status}: {fetched}"
        );
        assert_eq!(
            fetched["asset"]["assetId"],
            json!(asset_id),
            "case {case:?}: GetAsset reported {:?} for the asset created as {asset_id:?}",
            fetched["asset"]["assetId"]
        );
        assert_eq!(
            fetched["asset"]["assetType"],
            json!(row.asset_type),
            "case {case:?}: GetAsset reported {:?} as the asset type",
            fetched["asset"]["assetType"]
        );
    }
}

/// RULE: an identifier this service mints for a resource comes from that kind's own
/// identifying space, so creating resources of OTHER kinds first does not move it.
#[tokio::test]
async fn an_identifier_does_not_count_the_resources_of_other_kinds() {
    // The rows differ along how many resources of other kinds the store holds before
    // the backlog task and the Trigger whose identifiers are asserted: a shared
    // counter would number them differently in every row.
    let mut per_noise: Vec<(usize, String, String, String)> = Vec::new();
    for noise in [0_usize, 1, 3] {
        let case = format!("{noise} resource(s) of other kinds first");
        let router = new_router();
        let space = create_space(&router, &case, "other-kind-independence").await;
        for n in 0..noise {
            create_space(&router, &case, &format!("unrelated-space-{n}")).await;
            create_asset(
                &router,
                &case,
                &space,
                asset_request("skill", &format!("unrelated-{n}.md")),
            )
            .await;
        }
        let task_id = create_task(&router, &case, &space, "asserted task").await;
        let trigger_id = create_trigger(&router, &case, &space, "rate(1 hour)").await;
        per_noise.push((noise, case, task_id, trigger_id));
    }

    let (_, first_case, first_task, first_trigger) = &per_noise[0];
    for (noise, case, task_id, trigger_id) in &per_noise[1..] {
        assert_eq!(
            task_id, first_task,
            "case {case:?}: the first backlog task of a store holding {noise} resource(s) of \
             other kinds got {task_id:?}, but with none it got {first_task:?} (case \
             {first_case:?}): its identifier counts other kinds"
        );
        assert_eq!(
            trigger_id, first_trigger,
            "case {case:?}: the first Trigger of a store holding {noise} resource(s) of other \
             kinds got {trigger_id:?}, but with none it got {first_trigger:?} (case \
             {first_case:?}): its identifier counts other kinds"
        );
    }
}

/// RULE: an asset's identifier is drawn per asset, so it is not a function of the
/// position its create holds in the store's creation order: the same calls against
/// two stores name two different assets.
#[tokio::test]
async fn asset_identifier_is_not_the_store_creation_ordinal() {
    // The rows differ along the call sequence the stores are driven by — what the
    // store holds before the asset, and how many assets are created — because a
    // position-derived identifier would repeat for ANY sequence replayed on a fresh
    // store.
    let sequences: [(&str, usize, usize, usize); 4] = [
        ("one asset, nothing else", 0, 0, 1),
        ("one asset behind two tasks", 2, 0, 1),
        ("two assets behind a Trigger", 0, 1, 2),
        ("three assets behind tasks and Triggers", 2, 2, 3),
    ];

    for (case, tasks, triggers, assets) in sequences {
        let mut runs: Vec<Vec<String>> = Vec::new();
        for _ in 0..2 {
            let router = new_router();
            let space = create_space(&router, case, "creation-ordinal").await;
            for n in 0..tasks {
                create_task(&router, case, &space, &format!("task {n}")).await;
            }
            for n in 0..triggers {
                create_trigger(&router, case, &space, &format!("rate({} hours)", n + 1)).await;
            }
            let mut minted: Vec<String> = Vec::new();
            for n in 0..assets {
                let asset_id = create_asset(
                    &router,
                    case,
                    &space,
                    asset_request("skill", &format!("asset-{n}.md")),
                )
                .await;
                assert_asset_identifier(case, &asset_id);
                minted.push(asset_id);
            }
            runs.push(minted);
        }

        let (first, second) = (&runs[0], &runs[1]);
        for (position, (one, other)) in first.iter().zip(second.iter()).enumerate() {
            assert_ne!(
                one, other,
                "case {case:?}: the asset at position {position} of two stores driven by the \
                 same calls got the same identifier {one:?}, so the identifier is the \
                 creation ordinal rather than the asset's own"
            );
        }
        // Within one store no identifier repeats either, however many assets it holds.
        for (position, asset_id) in first.iter().enumerate() {
            assert_eq!(
                first.iter().filter(|seen| *seen == asset_id).count(),
                1,
                "case {case:?}: identifier {asset_id:?} at position {position} was handed out \
                 more than once in one store"
            );
        }
    }
}

/// RULE: the identifier is the asset's, so it is stable — a replayed idempotent
/// create answers the identifier the first one minted rather than a second asset,
/// and an identifier of the same form that names no asset resolves to nothing.
#[tokio::test]
async fn asset_identifier_is_stable_and_resolves_only_its_own_asset() {
    let router = new_router();
    let space = create_space(&router, "identifier-stability", "identifier-stability").await;

    // Rows: the asset types and content paths differ, so the rule is asserted over
    // more than one asset rather than one recorded create.
    let rows = [
        ("skill", "SKILL.md", "token-skill"),
        ("artifact", "notes.md", "token-artifact"),
        ("runbook", "run.md", "token-runbook"),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (asset_type, path, token) in rows {
        let case = format!("{asset_type} at {path}");
        let mut request = asset_request(asset_type, path);
        request["clientToken"] = json!(token);

        let first = create_asset(&router, &case, &space, request.clone()).await;
        assert_asset_identifier(&case, &first);
        // `@idempotencyToken`: the repeat is the same asset, so it carries the same
        // identifier.
        let replayed = create_asset(&router, &case, &space, request).await;
        assert_eq!(
            replayed, first,
            "case {case:?}: the replayed create answered {replayed:?}, not the identifier \
             {first:?} the first one minted"
        );
        // Two reads of one asset report one identifier.
        for read in 0..2 {
            let (status, fetched) = call(&router, "GET", &get_asset_uri(&space, &first), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: read {read} expected 200, got {status}: {fetched}"
            );
            assert_eq!(
                fetched["asset"]["assetId"],
                json!(first),
                "case {case:?}: read {read} reported {:?}",
                fetched["asset"]["assetId"]
            );
        }
        minted.push(first);
    }

    // An identifier of the asset plane's own form that this store never minted names
    // no asset: the form admits a value, it does not resolve one.
    let absent = "00000000-0000-4000-8000-000000000000";
    assert!(
        is_opaque_asset_identifier(absent) && !minted.iter().any(|id| id == absent),
        "the absent-identifier case must use an identifier of the right form that was \
         never minted, got {absent:?} among {minted:?}"
    );
    let (status, body) = call(&router, "GET", &get_asset_uri(&space, absent), None).await;
    assert_eq!(
        status, 404,
        "case absent identifier: expected 404, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case absent identifier: expected ResourceNotFoundException, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Asset not found: {absent}"),
        "case absent identifier: refusal reported {:?}",
        error_message(&body)
    );
}

// ---------------------------------------------------------------------------
// The identifier's invariants, over DRAWN creates
// ---------------------------------------------------------------------------

/// The seeds the drawn suite below runs from: a fixed set this suite owns, so the case
/// count is bounded and every run draws the same creates, or the single seed
/// [`DRAW_SEED_OVERRIDE`] names for an exploratory run. Every assertion carries its
/// seed and its draw index, so a failure names the create that produced it and the
/// override replays exactly that one.
const DRAW_SEEDS: [u64; 4] = [0xa55e_7001, 0xa55e_7002, 0xa55e_7003, 0xa55e_7004];

/// The environment variable that replaces [`DRAW_SEEDS`] with one named seed.
const DRAW_SEED_OVERRIDE: &str = "DEVOPSAGENT_ASSET_IDENTIFIER_SEED";

/// How many assets one seed creates. Bounded, and more than one per seed so the draws
/// of a seed share their store.
const DRAWS_PER_SEED: usize = 5;

/// The `assetType` values a draw picks from.
///
/// Source: the three types this service enforces a metadata rule for
/// (`crate::state::ASSET_TYPE_RULES` — skill, artifact, attachment) plus types it
/// enforces none for, because `AssetType` is an open string the model documents only
/// by example — "The type of asset (e.g. skill, artifact)",
/// `smithy_query(query="shape", shape="AssetType")`.
const DRAWN_ASSET_TYPES: [&str; 6] = [
    "skill",
    "artifact",
    "attachment",
    "runbook",
    "playbook",
    "evaluation.suite",
];

/// The content paths a draw picks from: a bare file name, nested paths, a path with a
/// space and a one-character one, all inside the modelled `AssetFilePath`
/// (`^[a-zA-Z0-9_./ ()-]+$`, `smithy_query(query="shape", shape="AssetFilePath")`).
const DRAWN_PATHS: [&str; 5] = [
    "SKILL.md",
    "data.json",
    "docs/notes.md",
    "deep/nested/dir/run (1).yaml",
    "a",
];

/// How many resources of OTHER kinds a draw may create before its asset, so the
/// identifier is minted behind a varying number of unrelated creates.
const MOST_UNRELATED_PER_DRAW: usize = 3;

/// RULE: whatever a create carries, the identifier `CreateAsset` answers is the
/// asset's own — inside the bounds its member publishes, the handle the asset is read
/// back by, repeated for an idempotent replay of that create and for no other asset.
#[tokio::test]
async fn a_drawn_create_mints_an_identifier_only_its_own_asset_answers_to() {
    let seeds = match std::env::var(DRAW_SEED_OVERRIDE) {
        Ok(named) => vec![named.trim().parse::<u64>().unwrap_or_else(|error| {
            panic!("{DRAW_SEED_OVERRIDE}={named:?} is not a u64 seed: {error}")
        })],
        Err(_) => DRAW_SEEDS.to_vec(),
    };

    // Every identifier any seed's store minted, so no two assets of any store in this
    // suite share one.
    let mut minted_everywhere: Vec<(String, String)> = Vec::new();

    for seed in seeds {
        let mut generator = StdRng::seed_from_u64(seed);
        let seed_case = format!("seed {seed}");
        let router = new_router();
        // Two spaces, so a draw's asset is created in one of them and an identifier is
        // asserted across the spaces of one store as well as within one.
        let spaces = [
            create_space(&router, &seed_case, "drawn-identifiers-a").await,
            create_space(&router, &seed_case, "drawn-identifiers-b").await,
        ];

        for draw in 0..DRAWS_PER_SEED {
            let asset_type = DRAWN_ASSET_TYPES[generator.random_range(0..DRAWN_ASSET_TYPES.len())];
            let path = DRAWN_PATHS[generator.random_range(0..DRAWN_PATHS.len())];
            let space = &spaces[generator.random_range(0..spaces.len())];
            let body = format!("drawn body {}", generator.random_range(0..1_000_000_u32));
            let case = format!("{seed_case}, draw {draw}: a {asset_type:?} at {path:?}");

            // Unrelated resources of two other kinds first: what names this asset must
            // not depend on how many of them the store holds.
            for n in 0..generator.random_range(0..=MOST_UNRELATED_PER_DRAW) {
                create_task(&router, &case, space, &format!("unrelated task {draw}-{n}")).await;
            }
            for n in 0..generator.random_range(0..=MOST_UNRELATED_PER_DRAW) {
                create_trigger(&router, &case, space, &format!("rate({} hours)", n + 1)).await;
            }

            let mut request = asset_request(asset_type, path);
            request["content"]["file"]["body"] = json!({ "text": body });
            let token = format!("drawn-{seed}-{draw}");
            request["clientToken"] = json!(token);

            let asset_id = create_asset(&router, &case, space, request.clone()).await;
            // What the MODEL publishes for the member that reports it; the form the
            // service draws inside those bounds is the table rows' subject, not this
            // suite's.
            assert_within_resource_id(&case, &asset_id);

            // No other asset of any store answers to it.
            if let Some((other_case, repeated)) = minted_everywhere
                .iter()
                .find(|(_, seen)| *seen == asset_id)
            {
                panic!(
                    "case {case:?}: identifier {repeated:?} was already minted by case \
                     {other_case:?}"
                );
            }
            minted_everywhere.push((case.clone(), asset_id.clone()));

            // READ BACK: the identifier the create answered resolves the asset it
            // persisted, in the space the create was addressed to.
            let uri = get_asset_uri(space, &asset_id);
            let (status, fetched) = call(&router, "GET", &uri, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from GetAsset on {asset_id:?}, got {status}: {fetched}"
            );
            assert_eq!(
                fetched["asset"]["assetId"],
                json!(asset_id),
                "case {case:?}: GetAsset reported {:?} for the asset created as {asset_id:?}",
                fetched["asset"]["assetId"]
            );
            assert_eq!(
                fetched["asset"]["assetType"],
                json!(asset_type),
                "case {case:?}: GetAsset reported {:?} as the asset type",
                fetched["asset"]["assetType"]
            );

            // IDEMPOTENCE: the same create replayed with the same `clientToken` is the
            // same asset, so it answers the same identifier...
            let replayed = create_asset(&router, &case, space, request.clone()).await;
            assert_eq!(
                replayed, asset_id,
                "case {case:?}: the create replayed with clientToken {token:?} answered \
                 {replayed:?}, not the identifier {asset_id:?} the first one minted"
            );
            // ... while the SAME content under a fresh token is another asset, so the
            // identifier is the asset's and not a function of what the request carried.
            let mut twin = request;
            twin["clientToken"] = json!(format!("{token}-twin"));
            let twin_id = create_asset(&router, &case, space, twin).await;
            assert_within_resource_id(&case, &twin_id);
            assert_ne!(
                twin_id, asset_id,
                "case {case:?}: a second create of the same content answered the identifier \
                 {twin_id:?} the first asset already carries"
            );
            minted_everywhere.push((format!("{case} (twin)"), twin_id));
        }
    }
}
