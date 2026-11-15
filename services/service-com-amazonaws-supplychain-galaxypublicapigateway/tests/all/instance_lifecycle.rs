// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the AWS Supply Chain **Instance** lifecycle, driven over the
//! wire (restJson1, signing name `scn`) through the same `MultiServiceRouter` the
//! emulator's front door uses — so every assertion here is about what a real client
//! observes, not about a handler's internals.
//!
//! The five operations under test (`CreateInstance`, `GetInstance`, `ListInstances`,
//! `UpdateInstance`, `DeleteInstance`) share ONE store, so the tests read back through
//! `GetInstance`/`ListInstances` what the mutating operations persisted rather than
//! trusting a create response on its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::INSTANCE_STATE_VALUES;
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    is_modelled_uuid, INSTANCE_ACTIVATION_DELAY, INSTANCE_STATE_ACTIVE, INSTANCE_STATE_DELETING,
    INSTANCE_STATE_INITIALIZING, MAX_INSTANCES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names this service (`scn`), which
/// is how the router picks the service that claims the request. Signature verification is
/// switched off for the test router, so the signature itself is never read.
const SCN_CREDENTIAL: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/scn/aws4_request, SignedHeaders=host, Signature=00";

/// A KMS key ARN satisfying `KmsKeyArn`'s modelled pattern
/// (`^arn:[a-z0-9][-.a-z0-9]{0,62}:kms:...:key/.{0,1019}$`).
const KMS_KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path_and_query: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.us-east-1.amazonaws.com{path_and_query}"))
            .header("host", "scn.us-east-1.amazonaws.com")
            .header("authorization", SCN_CREDENTIAL)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "{method} {path_and_query}: body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, json)
    }

    /// `CreateInstance` with `request` as its body, asserting the 200 and returning the
    /// `instance` member.
    async fn create(&self, case: &str, request: Value) -> Value {
        let (status, body) = self.send("POST", "/api/instance", Some(request)).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateInstance expected 200, got {status} with {body}"
        );
        instance_of(case, &body)
    }

    /// `GetInstance` for `id`, asserting the 200 and returning the `instance` member.
    async fn get(&self, case: &str, id: &str) -> Value {
        let (status, body) = self.send("GET", &format!("/api/instance/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case}: GetInstance expected 200, got {status} with {body}"
        );
        instance_of(case, &body)
    }
}

/// The `instance` member of a response body.
fn instance_of(case: &str, body: &Value) -> Value {
    body.get("instance")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: response carries no `instance` member: {body}"))
}

/// The `instanceId` of an `instance` payload.
fn id_of(case: &str, instance: &Value) -> String {
    instance
        .get("instanceId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: instance carries no `instanceId`: {instance}"))
        .to_string()
}

/// A `ClientToken` built from `seed`: the shape is modelled with length 33..126 only, so
/// the seed is padded to 40 characters.
fn client_token(seed: &str) -> String {
    format!("{seed:0<40}")
}

/// An `instance` member as an `f64`, for the numeric members (`versionNumber`, the
/// epoch-second timestamps restJson1 renders as JSON numbers).
fn number_of(case: &str, instance: &Value, member: &str) -> f64 {
    instance
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: instance carries no numeric `{member}`: {instance}"))
}

/// How long a poll in this suite may keep looking before it gives up. GENEROUS: every poll
/// below waits for a CONDITION — a state the emulator has reached, an instant the wall clock
/// has passed — so the deadline is only there to end a test that can never satisfy it, and
/// never a budget the host has to beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// The wall clock as the epoch seconds this service's restJson1 timestamps carry, so a
/// poll can compare it against a timestamp a response reported.
fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Look again until the wall clock has passed `epoch_seconds`, or until [`POLL_DEADLINE`].
///
/// The CONDITION an assertion that `lastModifiedTime` ADVANCED past an earlier stamp rests
/// on is that the clock has an instant left to stamp that is strictly later than that one.
/// Waiting for that condition rather than for a fixed span is what keeps the assertion
/// independent of how fast the host runs.
async fn wait_for_an_instant_after(epoch_seconds: f64) {
    let deadline = Instant::now() + POLL_DEADLINE;
    while epoch_now() <= epoch_seconds && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
}

/// The whole lifecycle from the issue's reproduction script: create, replay the token,
/// poll to `Active`, list, rename, read the rename back, delete, and observe the 404.
#[tokio::test]
async fn an_instance_round_trips_through_create_poll_list_rename_and_delete() {
    let emulator = Emulator::new();
    let token = client_token("lifecycle-");
    let request = json!({
        "instanceName": "demo-instance",
        "instanceDescription": "bootstrap demo",
        "kmsKeyArn": KMS_KEY_ARN,
        "webAppDnsDomain": "demo",
        "tags": { "team": "supply" },
        "clientToken": token,
    });

    let created = emulator.create("create", request.clone()).await;
    let id = id_of("create", &created);
    assert!(
        is_modelled_uuid(&id),
        "case create: instanceId must be a lowercase-hex UUID, got {id:?}"
    );
    assert_eq!(
        created.get("awsAccountId").and_then(Value::as_str),
        Some(engine::emulator_account_id()),
        "case create: awsAccountId must be the emulator's account, got {created}"
    );
    assert_eq!(
        created.get("state").and_then(Value::as_str),
        Some(INSTANCE_STATE_INITIALIZING),
        "case create: a new instance starts Initializing, got {created}"
    );
    assert_eq!(
        created.get("instanceName").and_then(Value::as_str),
        Some("demo-instance"),
        "case create: instanceName must come back from the stored record, got {created}"
    );
    let created_time = number_of("create", &created, "createdTime");
    let first_modified = number_of("create", &created, "lastModifiedTime");
    // The instance's own version, read from the create and compared against every later
    // answer below: what the service fixes is that the number does not move, so the
    // assertions compare answers with each other rather than with a constant.
    let created_version = number_of("create", &created, "versionNumber");

    // The same clientToken replays onto the SAME instance rather than creating a second.
    let replayed = emulator.create("token replay", request).await;
    assert_eq!(
        id_of("token replay", &replayed),
        id,
        "case token replay: a repeated clientToken must return the first instanceId"
    );

    // Poll GetInstance for readiness, exactly as a client waiting on the asynchronous
    // creation does. The transition is owed by the clock, so the poll terminates.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed = String::new();
    while Instant::now() < deadline {
        let instance = emulator.get("readiness poll", &id).await;
        observed = instance
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if observed == INSTANCE_STATE_ACTIVE {
            assert_eq!(
                number_of("readiness poll", &instance, "createdTime"),
                created_time,
                "case readiness poll: createdTime must not move, got {instance}"
            );
            assert_eq!(
                instance.get("instanceDescription").and_then(Value::as_str),
                Some("bootstrap demo"),
                "case readiness poll: the stored description must survive the transition, got {instance}"
            );
            assert_eq!(
                instance.get("kmsKeyArn").and_then(Value::as_str),
                Some(KMS_KEY_ARN),
                "case readiness poll: the stored kmsKeyArn must be reported, got {instance}"
            );
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        observed, INSTANCE_STATE_ACTIVE,
        "case readiness poll: the instance never became Active (last state {observed:?})"
    );

    // Exactly one instance exists, replay included.
    let (status, body) = emulator.send("GET", "/api/instance", None).await;
    assert_eq!(status, 200, "case list: expected 200, got {status} with {body}");
    let listed: Vec<String> = instances_of("list", &body)
        .iter()
        .map(|instance| id_of("list", instance))
        .collect();
    assert_eq!(
        listed,
        vec![id.clone()],
        "case list: the store holds exactly the one created instance"
    );

    // Rename it. `lastModifiedTime` is compared strictly, so wait for the clock to have an
    // instant left that is strictly later than the one the create stamped.
    wait_for_an_instant_after(first_modified).await;
    let (status, body) = emulator
        .send(
            "PATCH",
            &format!("/api/instance/{id}"),
            Some(json!({ "instanceName": "demo-renamed" })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case rename: UpdateInstance expected 200, got {status} with {body}"
    );
    let renamed = instance_of("rename", &body);
    assert_eq!(
        renamed.get("instanceName").and_then(Value::as_str),
        Some("demo-renamed"),
        "case rename: the new name must be reported, got {renamed}"
    );
    assert_eq!(
        renamed.get("instanceDescription").and_then(Value::as_str),
        Some("bootstrap demo"),
        "case rename: an omitted member must keep its stored value, got {renamed}"
    );
    let renamed_modified = number_of("rename", &renamed, "lastModifiedTime");
    assert!(
        renamed_modified > first_modified,
        "case rename: lastModifiedTime must advance, got {renamed_modified} after {first_modified}"
    );
    assert_eq!(
        number_of("rename", &renamed, "versionNumber"),
        created_version,
        "case rename: a change must not move versionNumber, got {renamed}"
    );

    // The rename is in the store, not just in the response.
    let after_rename = emulator.get("read the rename back", &id).await;
    assert_eq!(
        after_rename.get("instanceName").and_then(Value::as_str),
        Some("demo-renamed"),
        "case read the rename back: GetInstance must show the persisted name, got {after_rename}"
    );
    assert_eq!(
        number_of("read the rename back", &after_rename, "lastModifiedTime"),
        renamed_modified,
        "case read the rename back: a read must not move lastModifiedTime, got {after_rename}"
    );

    // Delete it: the final payload reports `Deleting`, and the id is then gone.
    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case delete: DeleteInstance expected 200, got {status} with {body}"
    );
    let deleted = instance_of("delete", &body);
    assert_eq!(
        deleted.get("state").and_then(Value::as_str),
        Some(INSTANCE_STATE_DELETING),
        "case delete: the final payload reports Deleting, got {deleted}"
    );
    assert_eq!(
        id_of("delete", &deleted),
        id,
        "case delete: the final payload is the instance that was deleted"
    );

    let (status, body) = emulator.send("GET", &format!("/api/instance/{id}"), None).await;
    assert_eq!(
        status, 404,
        "case read after delete: expected 404, got {status} with {body}"
    );
    assert!(
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("ResourceNotFoundException"),
        "case read after delete: expected a ResourceNotFoundException envelope, got {body}"
    );

    let (status, body) = emulator.send("GET", "/api/instance", None).await;
    assert_eq!(
        status, 200,
        "case list after delete: expected 200, got {status} with {body}"
    );
    assert!(
        instances_of("list after delete", &body).is_empty(),
        "case list after delete: the deleted id must not be listed, got {body}"
    );
}

/// The `instances` member of a `ListInstances` response body.
fn instances_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("instances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: response carries no `instances` list: {body}"))
}

/// RULE: every member `CreateInstance` carries is persisted verbatim and reported by
/// `GetInstance`; a member the request omits stays absent. One row per shape of request
/// the class allows — all members, one member, none, an empty string, a long value, and
/// several tags.
#[tokio::test]
async fn create_persists_exactly_the_members_the_request_carried() {
    let emulator = Emulator::new();
    // 63 characters: `InstanceName`'s modelled maximum length.
    let longest_name = "n".repeat(63);
    let rows: Vec<(&str, Value)> = vec![
        (
            "every member",
            json!({
                "instanceName": "full-member-set",
                "instanceDescription": "every member, present.",
                "kmsKeyArn": KMS_KEY_ARN,
                "webAppDnsDomain": "full-member-set",
                "tags": { "team": "supply", "env": "test" },
            }),
        ),
        ("name only", json!({ "instanceName": "name-only" })),
        ("description only", json!({ "instanceDescription": "description only" })),
        ("no members at all", json!({})),
        ("empty description", json!({ "instanceDescription": "" })),
        (
            "longest name the model allows",
            json!({ "instanceName": longest_name }),
        ),
        (
            "three tags",
            json!({ "instanceName": "tagged-three", "tags": { "a": "1", "b": "2", "c": "3" } }),
        ),
    ];

    let mut minted = Vec::new();
    for (case, request) in rows {
        let created = emulator.create(case, request.clone()).await;
        let id = id_of(case, &created);
        assert!(
            is_modelled_uuid(&id),
            "case {case}: instanceId must be a lowercase-hex UUID, got {id:?}"
        );
        minted.push(id.clone());

        let stored = emulator.get(case, &id).await;
        for member in ["instanceName", "instanceDescription", "kmsKeyArn"] {
            let expected = request.get(member).cloned();
            let actual = stored.get(member).cloned().filter(|v| !v.is_null());
            assert_eq!(
                actual, expected,
                "case {case}: GetInstance member {member} expected {expected:?}, got {actual:?}"
            );
        }
        // `webAppDnsDomain` is the one optional member an omitting request does NOT leave
        // absent: every instance has a WebApp domain, so the create echoes the requested
        // one or mints a conforming label. `instance_domain_projection.rs` owns that rule;
        // here it is only asserted present and echoed when asked for.
        let domain = stored
            .get("webAppDnsDomain")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: the stored instance has no webAppDnsDomain: {stored}"));
        if let Some(requested) = request.get("webAppDnsDomain").and_then(Value::as_str) {
            assert_eq!(
                domain, requested,
                "case {case}: the requested webAppDnsDomain must be echoed, got {stored}"
            );
        }
        assert_eq!(
            stored.get("awsAccountId").and_then(Value::as_str),
            Some(engine::emulator_account_id()),
            "case {case}: awsAccountId must be the emulator's account, got {stored}"
        );
        assert_eq!(
            number_of(case, &stored, "versionNumber"),
            number_of(case, &created, "versionNumber"),
            "case {case}: a read must report the version the create reported, got {stored}"
        );
        assert!(
            number_of(case, &stored, "lastModifiedTime")
                >= number_of(case, &stored, "createdTime"),
            "case {case}: lastModifiedTime must not precede createdTime, got {stored}"
        );
    }

    let unique: std::collections::BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        unique.len(),
        minted.len(),
        "every CreateInstance mints a distinct instanceId, got {minted:?}"
    );
}

/// RULE: `clientToken` makes `CreateInstance` idempotent — a token already seen answers
/// with the instance that token created, and a token never seen creates a new one.
#[tokio::test]
async fn a_replayed_client_token_answers_with_the_instance_it_first_created() {
    let emulator = Emulator::new();
    let seeds = ["token-alpha", "1234567890", "Tok_3n-beta", "zz"];
    let mut ids = Vec::new();

    for seed in seeds {
        let token = client_token(seed);
        let first = emulator
            .create(
                seed,
                json!({ "instanceName": "first-call", "clientToken": token }),
            )
            .await;
        let id = id_of(seed, &first);
        // The replay carries a DIFFERENT name: an idempotent replay answers with the
        // stored record, so the first call's name is what comes back.
        let replay = emulator
            .create(
                seed,
                json!({ "instanceName": "second-call", "clientToken": token }),
            )
            .await;
        assert_eq!(
            id_of(seed, &replay),
            id,
            "case {seed}: a replayed clientToken must return the first instanceId"
        );
        assert_eq!(
            replay.get("instanceName").and_then(Value::as_str),
            Some("first-call"),
            "case {seed}: a replay reports the stored record, not the replayed request: {replay}"
        );
        ids.push(id);
    }

    // Two creates with NO token are two instances: idempotency is the token's doing.
    let untokened_a = emulator.create("no token a", json!({})).await;
    let untokened_b = emulator.create("no token b", json!({})).await;
    assert_ne!(
        id_of("no token a", &untokened_a),
        id_of("no token b", &untokened_b),
        "case no token: a create without a clientToken always mints a new instance"
    );

    let (status, body) = emulator.send("GET", "/api/instance?maxResults=20", None).await;
    assert_eq!(status, 200, "case list: expected 200, got {status} with {body}");
    assert_eq!(
        instances_of("list", &body).len(),
        seeds.len() + 2,
        "case list: one instance per distinct token plus the two untokened ones, got {body}"
    );
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        seeds.len(),
        "distinct tokens must map to distinct instances, got {ids:?}"
    );
}

/// RULE: `ListInstances` reports the stored instances a request's filters select, cut
/// into pages of at most `maxResults`, with an opaque `nextToken` resuming exactly where
/// the previous page stopped — so paging at ANY page size yields every matching instance
/// exactly once.
#[tokio::test]
async fn list_instances_pages_at_every_page_size_and_honours_its_filters() {
    let emulator = Emulator::new();
    let names = [
        "alpha-one",
        "beta-two",
        "gamma-three",
        "delta-four",
        "epsilon-five",
    ];
    let mut expected_ids = Vec::new();
    for name in names {
        let created = emulator
            .create(name, json!({ "instanceName": name, "webAppDnsDomain": name }))
            .await;
        expected_ids.push(id_of(name, &created));
    }
    // A page reports the most recently created instance first, so the order every page is
    // cut from is the creation order reversed.
    expected_ids.reverse();

    for page_size in [1usize, 2, 3, 5, 20] {
        let case = format!("maxResults={page_size}");
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                Some(t) => format!("/api/instance?maxResults={page_size}&nextToken={t}"),
                None => format!("/api/instance?maxResults={page_size}"),
            };
            let (status, body) = emulator.send("GET", &query, None).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            let page = instances_of(&case, &body);
            assert!(
                page.len() <= page_size,
                "case {case}: a page holds {} instances, more than maxResults",
                page.len()
            );
            collected.extend(page.iter().map(|instance| id_of(&case, instance)));
            pages += 1;
            assert!(
                pages <= names.len() + 1,
                "case {case}: paging did not terminate after {pages} pages"
            );
            match body.get("nextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            collected, expected_ids,
            "case {case}: paging must yield every instance exactly once, in store order"
        );
    }

    // `maxResults` is modelled `@range(0..20)`; its lower bound returns no instance, and
    // no token either — a token that cannot advance would make a paging client spin.
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=0", None).await;
    assert_eq!(
        status, 200,
        "case maxResults=0: expected 200, got {status} with {body}"
    );
    assert!(
        instances_of("maxResults=0", &body).is_empty(),
        "case maxResults=0: a page of zero holds no instance, got {body}"
    );
    assert!(
        body.get("nextToken").and_then(Value::as_str).is_none(),
        "case maxResults=0: a page that cannot advance must carry no nextToken, got {body}"
    );
    // ...and it consumed nothing: the next list still reports every instance.
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=20", None).await;
    assert_eq!(
        status, 200,
        "case after maxResults=0: expected 200, got {status} with {body}"
    );
    let mut after_zero: Vec<String> = instances_of("after maxResults=0", &body)
        .iter()
        .map(|instance| id_of("after maxResults=0", instance))
        .collect();
    after_zero.sort();
    let mut every_id = expected_ids.clone();
    every_id.sort();
    assert_eq!(
        after_zero, every_id,
        "case after maxResults=0: a zero-sized page must not consume anything"
    );

    // `maxResults` past its modelled maximum is refused before any handler runs.
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=21", None).await;
    assert_eq!(
        status, 400,
        "case maxResults=21: a value past the modelled range is refused, got {status} with {body}"
    );

    let name_rows: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("one name", vec!["alpha-one"], vec!["alpha-one"]),
        (
            "two names",
            vec!["beta-two", "delta-four"],
            vec!["beta-two", "delta-four"],
        ),
        ("a name no instance has", vec!["no-such-instance"], vec![]),
        ("every name", names.to_vec(), names.to_vec()),
    ];
    for (case, filter, expected) in name_rows {
        let query: Vec<String> = filter
            .iter()
            .map(|name| format!("instanceNameFilter={name}"))
            .collect();
        let (status, body) = emulator
            .send(
                "GET",
                &format!("/api/instance?maxResults=20&{}", query.join("&")),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case name filter {case}: expected 200, got {status} with {body}"
        );
        let mut got: Vec<String> = instances_of(case, &body)
            .iter()
            .filter_map(|instance| {
                instance
                    .get("instanceName")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        got.sort();
        let mut want: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        want.sort();
        assert_eq!(
            got, want,
            "case name filter {case}: expected {want:?}, got {got:?}"
        );
    }

    // The state filter is deterministic only once the clock-owed `Initializing` -> `Active`
    // transition has been materialised for every instance, so poll for THAT condition rather
    // than for a span of time. The values come from `InstanceState`'s modelled value set.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = emulator.send("GET", "/api/instance?maxResults=20", None).await;
        assert_eq!(
            status, 200,
            "case activation poll: expected 200, got {status} with {body}"
        );
        let listed = instances_of("activation poll", &body);
        let active = listed
            .iter()
            .filter(|instance| {
                instance.get("state").and_then(Value::as_str) == Some(INSTANCE_STATE_ACTIVE)
            })
            .count();
        if active == names.len() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case activation poll: only {active} of {} instances became Active, got {body}",
            names.len()
        );
        tokio::task::yield_now().await;
    }
    let state_rows: Vec<(&str, Vec<&str>, usize)> = vec![
        ("Active alone", vec![INSTANCE_STATE_ACTIVE], names.len()),
        ("Deleting alone", vec![INSTANCE_STATE_DELETING], 0),
        ("CreateFailed alone", vec!["CreateFailed"], 0),
        (
            "Active or Initializing",
            vec![INSTANCE_STATE_ACTIVE, INSTANCE_STATE_INITIALIZING],
            names.len(),
        ),
    ];
    for (case, filter, expected) in state_rows {
        let query: Vec<String> = filter
            .iter()
            .map(|state| format!("instanceStateFilter={state}"))
            .collect();
        let (status, body) = emulator
            .send(
                "GET",
                &format!("/api/instance?maxResults=20&{}", query.join("&")),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case state filter {case}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            instances_of(case, &body).len(),
            expected,
            "case state filter {case}: expected {expected} instances, got {body}"
        );
    }

    // A state the model does not declare is refused by the boundary validator.
    let (status, body) = emulator
        .send("GET", "/api/instance?instanceStateFilter=Pending", None)
        .await;
    assert_eq!(
        status, 400,
        "case state filter Pending: an unmodelled InstanceState is refused, got {status} with {body}"
    );

    let token_rows: Vec<(&str, String, u16)> = vec![
        ("not hex at all", "zzzz".to_string(), 400),
        ("odd-length hex", "abc".to_string(), 400),
        (
            "hex of a word this service never issued",
            raw_page_token("resume-from-here"),
            400,
        ),
        ("an unstamped position", raw_page_token("3"), 400),
        (
            "a stamped boundary that is not a position",
            stamped_page_token("not-a-position"),
            400,
        ),
        (
            // The form this service issued before the page order became the instances'
            // creation order: stamped, but with an `instanceId` where a position belongs.
            "a token in the format this service no longer issues",
            hex::encode(format!("scn:1:instance:{}", expected_ids[0]).as_bytes()),
            400,
        ),
        (
            // A token this service could have issued, naming a position before every
            // stored instance: honoured, and the page it resumes is empty.
            "a stamped position before the first instance",
            stamped_page_token("0"),
            200,
        ),
    ];
    for (case, token, expected) in token_rows {
        let (status, body) = emulator
            .send("GET", &format!("/api/instance?nextToken={token}"), None)
            .await;
        assert_eq!(
            status, expected,
            "case nextToken {case}: expected {expected}, got {status} with {body}"
        );
        if expected == 400 {
            assert!(
                body.get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("ValidationException"),
                "case nextToken {case}: expected a ValidationException envelope, got {body}"
            );
        } else {
            assert!(
                instances_of(case, &body).is_empty(),
                "case nextToken {case}: a boundary before the first position resumes an empty page, got {body}"
            );
        }
    }
}

/// A `nextToken` of the form this service issues: the stamped resume position, hex-encoded.
fn stamped_page_token(position: &str) -> String {
    hex::encode(format!("scn:2:instance:{position}").as_bytes())
}

/// A hex-encoded value carrying no stamp — well-formed hex that this service never
/// issued as a token.
fn raw_page_token(value: &str) -> String {
    hex::encode(value.as_bytes())
}

/// RULE: the store holds its instances for the life of the process and `CreateInstance`
/// is the only operation that adds one, so a create past the record cap is refused with
/// the modelled `ServiceQuotaExceededException` (HTTP 402) rather than stored — while a
/// replay of a token already recorded, which stores nothing, is still answered.
#[tokio::test]
async fn creating_past_the_instance_cap_is_refused_with_the_modelled_quota_error() {
    let emulator = Emulator::new();
    let token = client_token("at-the-cap-");
    let first = emulator
        .create("first", json!({ "clientToken": token }))
        .await;
    let first_id = id_of("first", &first);

    for index in 1..MAX_INSTANCES {
        let case = format!("filling the store: instance {index}");
        emulator.create(&case, json!({})).await;
    }

    let (status, body) = emulator.send("POST", "/api/instance", Some(json!({}))).await;
    assert_eq!(
        status, 402,
        "case past the cap: expected the modelled quota refusal, got {status} with {body}"
    );
    assert!(
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("ServiceQuotaExceededException"),
        "case past the cap: expected a ServiceQuotaExceededException envelope, got {body}"
    );

    // A replay stores nothing, so it is answered even at the cap.
    let replayed = emulator
        .create("replay at the cap", json!({ "clientToken": token }))
        .await;
    assert_eq!(
        id_of("replay at the cap", &replayed),
        first_id,
        "case replay at the cap: a recorded clientToken must still answer with its instance"
    );

    // And the refusal left the store readable and intact.
    let stored = emulator.get("read at the cap", &first_id).await;
    assert_eq!(
        id_of("read at the cap", &stored),
        first_id,
        "case read at the cap: GetInstance must still answer, got {stored}"
    );
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=20", None).await;
    assert_eq!(
        status, 200,
        "case list at the cap: expected 200, got {status} with {body}"
    );
    assert_eq!(
        instances_of("list at the cap", &body).len(),
        20,
        "case list at the cap: a page is still capped at maxResults, got {body}"
    );
}

/// RULE: an operation addressed to an instance id no live instance holds answers
/// `ResourceNotFoundException` (404); an id outside the modelled `UUID` pattern or length
/// is refused as a `ValidationException` (400) before any handler runs.
#[tokio::test]
async fn an_unknown_instance_is_404_and_a_malformed_id_is_400() {
    let emulator = Emulator::new();
    // One live instance, so a 404 cannot be an empty store answering by accident.
    let live = emulator
        .create("live", json!({ "instanceName": "live-instance" }))
        .await;
    let live_id = id_of("live", &live);

    let rows: Vec<(&str, &str, u16)> = vec![
        (
            "well-formed uuid no instance holds",
            "00000000-0000-0000-0000-000000000000",
            404,
        ),
        ("not a uuid at all", "not-a-uuid", 400),
        (
            "uppercase hex",
            "ABCDEF01-2345-6789-ABCD-EF0123456789",
            400,
        ),
        (
            "one character short",
            "0000000-0000-0000-0000-000000000000",
            400,
        ),
        (
            "one character long",
            "000000000-0000-0000-0000-000000000000",
            400,
        ),
        (
            "hex without the hyphens",
            "00000000000000000000000000000000",
            400,
        ),
        (
            "non-hex letters in a uuid-shaped id",
            "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz",
            400,
        ),
    ];

    for (case, id, expected) in rows {
        for method in ["GET", "PATCH", "DELETE"] {
            let body = (method == "PATCH").then(|| json!({ "instanceName": "rename-attempt" }));
            let (status, response) = emulator
                .send(method, &format!("/api/instance/{id}"), body)
                .await;
            assert_eq!(
                status, expected,
                "case {case} via {method}: expected {expected}, got {status} with {response}"
            );
            let kind = if expected == 404 {
                "ResourceNotFoundException"
            } else {
                "ValidationException"
            };
            assert!(
                response
                    .get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains(kind),
                "case {case} via {method}: expected a {kind} envelope, got {response}"
            );
        }
    }

    // None of the refusals touched the live instance.
    let still_there = emulator.get("live instance survives", &live_id).await;
    assert_eq!(
        still_there.get("instanceName").and_then(Value::as_str),
        Some("live-instance"),
        "case live instance survives: a refused request must not change the store, got {still_there}"
    );
    assert_eq!(
        number_of("live instance survives", &still_there, "versionNumber"),
        number_of("live instance survives", &live, "versionNumber"),
        "case live instance survives: a refused UpdateInstance must not change the version, got {still_there}"
    );
}

/// RULE: a `webAppDnsDomain` is a subdomain only one live instance may hold — its own
/// documentation allows a value "as long as the domain isn't already being used by
/// someone else" — so a create reusing a live instance's domain is refused
/// `ConflictException` (409), and the domain is free again once the holder is deleted.
#[tokio::test]
async fn a_web_app_dns_domain_is_held_by_one_live_instance_at_a_time() {
    let emulator = Emulator::new();
    // Rows differing along the shape of the domain the model's pattern allows
    // (`^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`): short, hyphenated, digits, mixed case.
    for domain in ["demo", "demo-two-parts", "a1", "MixedCase9"] {
        let first = emulator
            .create(domain, json!({ "webAppDnsDomain": domain }))
            .await;
        let holder = id_of(domain, &first);

        let (status, body) = emulator
            .send(
                "POST",
                "/api/instance",
                Some(json!({ "webAppDnsDomain": domain })),
            )
            .await;
        assert_eq!(
            status, 409,
            "case {domain}: a second instance on a live domain must be refused, got {status} with {body}"
        );
        assert!(
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("ConflictException"),
            "case {domain}: expected a ConflictException envelope, got {body}"
        );

        // Deleting the holder frees the domain, so the same create now succeeds.
        let (status, body) = emulator
            .send("DELETE", &format!("/api/instance/{holder}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {domain}: DeleteInstance expected 200, got {status} with {body}"
        );
        let reused = emulator
            .create(domain, json!({ "webAppDnsDomain": domain }))
            .await;
        assert_ne!(
            id_of(domain, &reused),
            holder,
            "case {domain}: the domain must be reusable by a NEW instance after the holder is deleted"
        );
        assert_eq!(
            reused.get("webAppDnsDomain").and_then(Value::as_str),
            Some(domain),
            "case {domain}: the reused domain must be persisted on the new instance, got {reused}"
        );
    }
}

/// The states this service stores are values the model declares, and the span the clock
/// owes an instance before it reports `Active` is the emulator's 250-750 ms window — long
/// enough that a caller reading straight back still sees `Initializing`.
#[test]
fn the_stored_states_are_modelled_values_and_the_activation_span_is_observable() {
    // Source: `InstanceState`'s value set, rendered from the Smithy model into
    // `ops::types::INSTANCE_STATE_VALUES`.
    for state in [
        INSTANCE_STATE_INITIALIZING,
        INSTANCE_STATE_ACTIVE,
        INSTANCE_STATE_DELETING,
    ] {
        assert!(
            INSTANCE_STATE_VALUES.contains(&state),
            "case {state}: not a modelled InstanceState value ({INSTANCE_STATE_VALUES:?})"
        );
    }
    assert!(
        INSTANCE_ACTIVATION_DELAY >= Duration::from_millis(250)
            && INSTANCE_ACTIVATION_DELAY <= Duration::from_millis(750),
        "case activation span: expected 250-750 ms, got {INSTANCE_ACTIVATION_DELAY:?}"
    );
}
