// SPDX-License-Identifier: CC0-1.0

//! The Agent Registry DISCOVERY surface on the ASSEMBLED emulator — every service mounted,
//! as `aws_emulator_router::build_router` assembles the one the binary serves.
//!
//! THE RULE THESE SUITES PIN: a request is dispatched to the emulated service whose
//! ENDPOINT and ROUTE it names, so a SigV4 signing name several mounted services declare is
//! not ambiguity, and every request a service's own model binds a route for is answered by
//! that operation's own outcome — its modelled success, or its modelled error with that
//! error's status, code and message. A router-level refusal code, which belongs to no
//! service's model, is never an answer to such a request.
//!
//! Agent Registry is the case where that matters most: the DATA plane (`AgentRegistry`,
//! endpointPrefix `agent-registry`) and the CONTROL plane (`AgentRegistryControl`,
//! endpointPrefix `agent-registry-control`) both sign as `agent-registry`, so every request
//! of either plane resolves one shared routing key. The sibling suites
//! `agent_registry_signing_name_routing` and `agent_registry_discovery_routes_probe` pin
//! WHICH service claims a request; these suites pin what the claimed request is then
//! ANSWERED with, which is the surface a pre-dispatch refusal hides: the whole of each
//! read's authorization, its input validation, its not-found refusal and its success.
//!
//! The two suites below drive these classes over a SET of inputs rather than one. The first
//! drives one emulator through a registry and a record authored by the CONTROL plane and then
//! through the refusals, the second through the authorization decision:
//!
//! * a registry and a record authored through the CONTROL plane's own operations are what
//!   the three data-plane reads report, with the ARNs the control plane minted — both
//!   planes reachable through the one signing name, over one shared engine;
//! * a well-formed identifier the account holds no registry for is `404`
//!   `ResourceNotFoundException`, whose message is the fixed text plus the identifier
//!   exactly as the request supplied it, and one outside `RegistryIdentifier`'s own
//!   `@pattern` is `400` `ValidationException` in the AWS constraint envelope — the refusal
//!   follows from the request's value, not from which service claimed it;
//! * a caller whose identity-based policies do not allow the read's action is `403`
//!   `AccessDeniedException`, and the same caller, once allowed, meets the registry's own
//!   answer — the decision taken before the registry is resolved.
//!
//! No row may be answered by the router's own `AmbiguousService` (several claimants named an
//! operation) or `UnknownService` (none claimed it): neither is a shape of either Agent
//! Registry model.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
// Percent-encoding one non-greedy `@httpLabel` the way an AWS SDK sends it — an identifier in
// ARN form carries `:` and `/`, which must arrive escaped — is written once in this target,
// under one name, and shared rather than copied.
use crate::cloudfront_key_value_store_handshake::segment as encode_label;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below signs for, and so the Region of the registry ARN an
/// authorization decision is taken against.
const REGION: &str = "us-west-2";

/// The access key id the unrestricted rows sign with: a key neither STS nor IAM holds a
/// record for, which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIDTESTAGENTREGISTRY";

/// The access key id of the assumed-role session the authorization rows sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, the session's own name, and the name of the
/// inline policy a row puts in force on the role.
const ROLE: &str = "registry-reader";
const SESSION: &str = "registry-session";
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The router's own refusals. Neither is a shape of either Agent Registry model, so no row
/// of these suites may be answered with one.
const ROUTER_REFUSALS: [&str; 2] = ["AmbiguousService", "UnknownService"];

/// The envelope AWS states a constraint refusal in, as every other suite of these two
/// services reads it back: one clause, counted.
const ONE_VALIDATION_ERROR: &str = "1 validation error detected: ";

/// The fixed text a discovery read refuses an identifier it holds no registry for with; the
/// identifier the request supplied is the only variable part.
const NOT_FOUND_PREFIX: &str = "Registry not found: ";

/// The `searchType` every recorded successful search carries.
const SEARCH_TYPE: &str = "HYBRID";

/// The one read whose authorization decision is taken only for an entry that names its
/// registry by ARN.
const BATCH_READ: &str = "BatchGetDiscoverableRegistryRecord";
const LIST_READ: &str = "ListDiscoverableRegistryRecords";
const SEARCH_READ: &str = "SearchDiscoverableRegistryRecords";

/// The sentence the batch read words its own authorization refusal with: one fixed sentence
/// about the records asked for, carrying no part of the request.
const BATCH_READ_REFUSAL: &str = "Caller is not authorized to read the requested records.";

/// How long a wait for a transition the clock owes may take before a test gives up.
/// Generous on purpose: the wait is on the CONDITION, so a loaded host only polls more.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One answer from the assembled emulator, reduced to what these suites assert on.
struct Reply {
    status: u16,
    content_type: String,
    /// The error shape named in `x-amzn-errortype`, else the body's `__type`, without its
    /// namespace — so a router refusal and a modelled error are read the same way.
    code: String,
    message: String,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    /// The `recordId`s of the answer's `registryRecords`, in response order, or `None` when
    /// the answer carries no such member at all.
    fn record_ids(&self) -> Option<Vec<String>> {
        Some(
            self.body
                .get("registryRecords")?
                .as_array()?
                .iter()
                .map(|record| record["recordId"].as_str().unwrap_or_default().to_string())
                .collect(),
        )
    }
}

/// The whole emulator, exactly as the binary assembles it, over one shared engine. Request
/// authentication is off: these suites are about what a request is ANSWERED with, and the
/// signature of a hand-built request is not checked anywhere in this target.
fn assembled_emulator() -> (MultiServiceRouter, Arc<InMemoryEngine>) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    (router, engine)
}

/// Call one Agent Registry route signed with `access_key_id`, which is what names the
/// principal the request speaks for.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://agent-registry.{REGION}.amazonaws.com{path}"))
        .header("host", format!("agent-registry.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/agent-registry/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let content_type = header("content-type");
    let error_type = header("x-amzn-errortype");
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let code = if error_type.is_empty() {
        body["__type"].as_str().unwrap_or_default().to_string()
    } else {
        error_type
    };
    let code = code.rsplit(['#', '/']).next().unwrap_or_default().to_string();
    let message = body["message"].as_str().unwrap_or_default().to_string();
    Reply {
        status,
        content_type,
        code,
        message,
        body,
    }
}

/// Call a route with the account's own published credential, which no IAM policy bounds.
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    call_as(router, ACCOUNT_KEY_ID, method, path, body).await
}

/// The last `/`-separated segment of an ARN: a registry id, or a record id.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// The ARN of a registry of that id, as either plane publishes it in the Region the request
/// signed for.
fn registry_arn(registry_id: &str) -> String {
    format!(
        "arn:aws:agent-registry:{REGION}:{}:registry/{registry_id}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN the seeded session speaks for — what an IAM refusal must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        ::engine::emulator_account_id()
    )
}

/// The ARN the account holds the role under, which is the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

// --- the two planes, as a client calls them -------------------------------------------

async fn create_registry(router: &MultiServiceRouter, name: &str) -> Reply {
    // `ApprovalConfiguration.autoApprovalRules`: the rules that decide which records are
    // approved on submission; `APPROVE_ALL` is the one rule the model declares
    // (`AUTO_APPROVAL_RULE_VALUES`).
    call(
        router,
        "POST",
        "/registries",
        Some(json!({
            "name": name,
            "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] },
        })),
    )
    .await
}

async fn get_registry(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/registries/{}", encode_label(registry_id)),
        None,
    )
    .await
}

async fn create_record(router: &MultiServiceRouter, registry_id: &str, name: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(json!({
            "name": name,
            "description": format!("{name} description"),
            "recordType": "CUSTOM",
            "recordVersion": "1.0.0",
            "descriptors": { "custom": { "data": "{\"kind\":\"probe\"}" } },
        })),
    )
    .await
}

async fn get_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

async fn submit_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!(
            "/registries/{}/records/{}/submit-for-approval",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

/// The route and body of one discovery read for a request naming `identifier`.
fn read_of(operation: &str, identifier: &str, record_id: &str) -> (String, Value) {
    match operation {
        LIST_READ => (
            format!(
                "/registries/{}/discoverable-records-list",
                encode_label(identifier)
            ),
            json!({}),
        ),
        SEARCH_READ => (
            "/discoverable-records-search".to_string(),
            // A short non-empty query: `SearchQuery` is `@length(min: 0, max: 256)` today, and
            // a baseline row is about the registry the request names rather than the query's
            // own shape.
            json!({ "searchQuery": "probe", "registryIds": [identifier] }),
        ),
        _ => (
            "/discoverable-records-batch".to_string(),
            json!({ "entries": [{ "registryId": identifier, "recordIds": [record_id] }] }),
        ),
    }
}

/// Every discovery read, in model order, as the request that reaches it for `identifier`.
fn reads_of(identifier: &str, record_id: &str) -> [(&'static str, String, Value); 3] {
    [LIST_READ, SEARCH_READ, BATCH_READ].map(|operation| {
        let (route, body) = read_of(operation, identifier, record_id);
        (operation, route, body)
    })
}

/// Look at a registry until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn registry_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    wanted: &str,
) {
    let started = Instant::now();
    loop {
        let reply = get_registry(router, registry_id).await;
        if reply.str_field("status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at a record until it reports `wanted`, or give up at the deadline.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) {
    let started = Instant::now();
    loop {
        let reply = get_record(router, registry_id, record_id).await;
        if reply.str_field("status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the record never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Require that an answer came from a service's own model rather than from the router's
/// refusal path, and that it is framed in the protocol that service answers in.
fn assert_served_by_the_service(case: &str, reply: &Reply) {
    assert!(
        !ROUTER_REFUSALS.contains(&reply.code.as_str()),
        "case {case:?}: the request must be answered by the service whose route it names, \
         got {} {:?} {:?}",
        reply.status,
        reply.code,
        reply.message
    );
    assert!(
        reply.content_type.starts_with("application/json"),
        "case {case:?}: restJson1 frames every answer as application/json, got {:?} with {} \
         {:?}",
        reply.content_type,
        reply.status,
        reply.code
    );
}

// --- the identity the authorization rows speak for ------------------------------------

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLEREGISTRYROLE".to_string(),
            arn: role_arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: ::engine::emulator_account_id().to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Put `document` in force on the role as its inline policy, as `iam:PutRolePolicy` stores
/// one; `None` takes every policy off it, as `iam:DeleteRolePolicy` does.
fn given_role_policy(engine: &Arc<InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.role_inline_policies.remove(&iam::name_key(ROLE));
    if let Some(document) = document {
        store.role_inline_policies.insert(
            iam::name_key(ROLE),
            BTreeMap::from([(
                iam::name_key(POLICY_NAME),
                iam::InlinePolicy {
                    policy_name: POLICY_NAME.to_string(),
                    policy_document: document.to_string(),
                },
            )]),
        );
    }
}

/// IAM's own sentence for a request no identity-based statement allows.
fn default_deny_message(operation: &str, resource: &str) -> String {
    let action = format!("agent-registry:{operation}");
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource} because no \
         identity-based policy allows the {action} action",
        session_arn()
    )
}

// --- the suites -----------------------------------------------------------------------

/// RULE: both planes behind the one signing name are reachable on the assembled emulator, so
/// a registry and a record authored through the CONTROL plane's operations are what the
/// three DATA-plane reads report — with the ARNs the control plane minted, since the reads
/// resolve the registry in the store that owns it.
#[tokio::test]
async fn a_control_plane_registry_is_discoverable_through_the_data_plane_reads() {
    let (router, _engine) = assembled_emulator();
    let case = "a record authored through the control plane";

    let created = create_registry(&router, "assembled-discovery-probe").await;
    assert_served_by_the_service(case, &created);
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry answers 202, got {} {:?}",
        created.status, created.body
    );
    let registry_arn = created.str_field("registryArn").to_string();
    let registry_id = id_of(&registry_arn).to_string();
    registry_reaching(&router, case, &registry_id, "READY").await;

    // The name carries the token every baseline read's query is written with, so a row that
    // is about the registry is not silently about the query matching nothing.
    let record = create_record(&router, &registry_id, "probe-payments-agent").await;
    assert_served_by_the_service(case, &record);
    assert_eq!(
        record.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
        record.status, record.body
    );
    let record_arn = record.str_field("recordArn").to_string();
    let record_id = id_of(&record_arn).to_string();
    // Creation is asynchronous: the record is CREATING and the clock owes it DRAFT.
    record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
    submit_record(&router, &registry_id, &record_id).await;
    record_reaching(&router, case, &registry_id, &record_id, "APPROVED").await;

    // Each read, in both forms `RegistryIdentifier` admits, reports that one record.
    for identifier in [registry_id.clone(), registry_arn.clone()] {
        for (operation, route, body) in reads_of(&identifier, &record_id) {
            let row = format!("{operation} naming the registry as {identifier:?}");
            let reply = call(&router, "POST", &route, Some(body)).await;
            assert_served_by_the_service(&row, &reply);
            assert_eq!(
                reply.status, 200,
                "case {row:?}: an approved record is discoverable, got {} {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.record_ids(),
                Some(vec![record_id.clone()]),
                "case {row:?}: expected exactly the control plane's record, got {:?}",
                reply.body
            );
            let records = reply.body["registryRecords"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let reported = records
                .first()
                .map(|record| record["registryArn"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            assert_eq!(
                reported, registry_arn,
                "case {row:?}: a read reports the ARN the control plane minted, got {:?}",
                reply.body
            );
        }
    }

    // The two answer shapes recorded AWS behaviour carries beside the modelled members: a
    // successful search reports the search mode it ran, and a batch answer carries `errors`
    // present and empty when every requested record was returned.
    let (search_route, search_body) = read_of(SEARCH_READ, &registry_id, &record_id);
    let searched = call(&router, "POST", &search_route, Some(search_body)).await;
    assert_eq!(
        searched.str_field("searchType"),
        SEARCH_TYPE,
        "case {case:?}: every successful search reports its search type, got {:?}",
        searched.body
    );
    let (batch_route, batch_body) = read_of(BATCH_READ, &registry_id, &record_id);
    let fetched = call(&router, "POST", &batch_route, Some(batch_body)).await;
    assert_eq!(
        fetched.body["errors"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: errors is present and empty when every record was returned, got {:?}",
        fetched.body
    );

    // And the search APPLIES its query rather than reporting the registry whatever was asked
    // for: a query the record's own text carries matches it, one no record carries matches
    // nothing, and either way the answer is the search's own 200.
    for (row, query, matched) in [
        ("a query the record's name carries", "payments", 1_usize),
        ("a query no record carries", "nothing-matches-this", 0),
    ] {
        let queried = call(
            &router,
            "POST",
            "/discoverable-records-search",
            Some(json!({ "searchQuery": query, "registryIds": [registry_id.clone()] })),
        )
        .await;
        assert_served_by_the_service(row, &queried);
        assert_eq!(
            queried.record_ids().map(|ids| ids.len()),
            Some(matched),
            "case {row:?}: expected {matched} matching record(s), got {:?}",
            queried.body
        );
    }
    // The same emulator answers the REFUSALS by the class of the identifier it is given, not
    // by which of the two claimants served the request: a well-formed identifier this store
    // holds no registry for is the read's own `404 ResourceNotFoundException`, worded as the
    // fixed text plus the identifier exactly as supplied, while one outside
    // `RegistryIdentifier`'s `@pattern`
    // (`^(arn:aws(-[^:]+)?:agent-registry:[a-z0-9-]+:[0-9]{12}:registry/)?[a-zA-Z0-9]{12,16}$`,
    // `smithy_query(query="shape", shape="RegistryIdentifier")`) is `400 ValidationException`
    // in AWS's counted envelope, refused before any registry is resolved. The rows differ
    // along what each rule generalises over — the identifier's form and the length run's
    // bounds for the first, the way a value misses the pattern for the second; the exhaustive
    // per-member constraint tables stay with the owning crate's own suites.
    let absent: [(&str, &str); 2] = [
        ("a bare id at the pattern's maximum length", "ZZ99missing00000"),
        (
            "a registry ARN in another partition",
            "arn:aws-us-gov:agent-registry:us-gov-west-1:000000000000:registry/zzz999999999",
        ),
    ];
    for (form, identifier) in absent {
        for (operation, route, body) in reads_of(identifier, &record_id) {
            let row = format!("{operation} naming {form}");
            let reply = call(&router, "POST", &route, Some(body)).await;
            assert_served_by_the_service(&row, &reply);
            assert_eq!(
                (reply.status, reply.code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {row:?}: expected the read's own modelled 404, got {} {:?} {:?}",
                reply.status,
                reply.code,
                reply.message
            );
            assert_eq!(
                reply.message,
                format!("{NOT_FOUND_PREFIX}{identifier}"),
                "case {row:?}: expected the fixed text and the identifier as supplied, got \
                 {:?}",
                reply.message
            );
        }
    }
    let malformed: [(&str, &str); 2] = [
        ("a space and a punctuation character", "bad id!"),
        (
            "an ARN whose registry segment is malformed",
            "arn:aws:agent-registry:us-west-2:000000000000:registry/nope",
        ),
    ];
    for (form, identifier) in malformed {
        for (operation, route, body) in reads_of(identifier, &record_id) {
            let row = format!("{operation} naming {form}");
            let reply = call(&router, "POST", &route, Some(body)).await;
            assert_served_by_the_service(&row, &reply);
            assert_eq!(
                (reply.status, reply.code.as_str()),
                (400, "ValidationException"),
                "case {row:?}: expected the read's own modelled 400, got {} {:?} {:?}",
                reply.status,
                reply.code,
                reply.message
            );
            assert!(
                reply.message.starts_with(ONE_VALIDATION_ERROR),
                "case {row:?}: expected AWS's counted constraint envelope, got {:?}",
                reply.message
            );
            assert!(
                !reply.message.starts_with(NOT_FOUND_PREFIX),
                "case {row:?}: a malformed value is never reported as a missing registry, got \
                 {:?}",
                reply.message
            );
        }
    }
}

/// RULE: every discovery read evaluates the identity-based policies in force on the
/// principal its credentials name against `agent-registry:<OperationName>` BEFORE it
/// resolves the registry, so a caller no statement allows reads back `403
/// AccessDeniedException` — IAM's own sentence for the two registry-scoped reads, naming the
/// registry's canonical ARN whichever form the request spelled, and the batch read's own
/// sentence for an entry that named its registry by ARN — while the same caller, once
/// allowed, meets the registry's own answer.
///
/// One row is deliberately the asymmetry RECORDED AWS behaviour shows, which the owning
/// crate's gate documents and implements (`src/state/authorization.rs`,
/// `carried_registry_resource` / `authorize_batch_entry`): the batch read decides only for an
/// entry that names its registry by ARN, so an entry naming the same registry by id reaches
/// the records with no decision taken for it.
#[tokio::test]
async fn an_unpermissioned_caller_is_refused_by_the_read_and_an_allowed_one_reaches_it() {
    let (router, engine) = assembled_emulator();
    given_session(&engine);
    let record_id = "rec000000001";
    // A registry no store holds, so the row's answer is the authorization decision or —
    // once allowed — the registry's own not-found, and never something a seeded registry
    // contributed.
    let registry_id = "zzz999999999";

    // (case, the read, the identifier the request names, the refusal it reads back with no
    // policy in force).
    let rows: [(&str, &str, String, Option<String>); 6] = [
        (
            "the list read naming the registry by id",
            LIST_READ,
            registry_id.to_string(),
            Some(default_deny_message(LIST_READ, &registry_arn(registry_id))),
        ),
        (
            "the list read naming the registry by ARN",
            LIST_READ,
            registry_arn(registry_id),
            Some(default_deny_message(LIST_READ, &registry_arn(registry_id))),
        ),
        (
            "the search read naming the registry by id",
            SEARCH_READ,
            registry_id.to_string(),
            Some(default_deny_message(SEARCH_READ, &registry_arn(registry_id))),
        ),
        (
            "the search read naming the registry by ARN",
            SEARCH_READ,
            registry_arn(registry_id),
            Some(default_deny_message(SEARCH_READ, &registry_arn(registry_id))),
        ),
        (
            "the batch read whose entry named the registry by ARN",
            BATCH_READ,
            registry_arn(registry_id),
            Some(BATCH_READ_REFUSAL.to_string()),
        ),
        (
            "the batch read whose entry named the registry by id",
            BATCH_READ,
            registry_id.to_string(),
            None,
        ),
    ];

    for (case, operation, identifier, refusal) in rows {
        let (route, body) = read_of(operation, &identifier, record_id);

        // No policy at all: IAM's default deny, except where this read takes no decision.
        given_role_policy(&engine, None);
        let reply = call_as(&router, SESSION_KEY_ID, "POST", &route, Some(body.clone())).await;
        assert_served_by_the_service(case, &reply);
        match &refusal {
            Some(expected) => {
                assert_eq!(
                    (reply.status, reply.code.as_str()),
                    (403, "AccessDeniedException"),
                    "case {case:?}: expected the modelled 403, got {} {:?} {:?}",
                    reply.status,
                    reply.code,
                    reply.message
                );
                assert_eq!(
                    &reply.message, expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    reply.message
                );
                assert!(
                    reply.body.get("registryRecords").is_none(),
                    "case {case:?}: a refused call reports no records, got {:?}",
                    reply.body
                );
            }
            None => {
                assert_eq!(
                    (reply.status, reply.code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: this read takes no decision for that form, so it meets \
                     the registry's own answer, got {} {:?} {:?}",
                    reply.status,
                    reply.code,
                    reply.message
                );
            }
        }

        // An allow of this service's actions: the decision no longer refuses, so every row
        // meets the registry's own answer — which is where existence is decided.
        given_role_policy(
            &engine,
            Some(&json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "agent-registry:*", "Resource": "*" },
                ],
            })),
        );
        let allowed = call_as(&router, SESSION_KEY_ID, "POST", &route, Some(body)).await;
        assert_served_by_the_service(case, &allowed);
        assert_eq!(
            (allowed.status, allowed.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: an allowed caller meets the registry's own answer, got {} {:?} \
             {:?}",
            allowed.status,
            allowed.code,
            allowed.message
        );
        assert_eq!(
            allowed.message,
            format!("{NOT_FOUND_PREFIX}{identifier}"),
            "case {case:?}: expected the fixed text and the identifier as supplied, got {:?}",
            allowed.message
        );
    }
}
