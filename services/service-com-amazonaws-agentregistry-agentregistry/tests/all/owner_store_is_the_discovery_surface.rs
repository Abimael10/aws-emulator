// SPDX-License-Identifier: CC0-1.0

//! The Agent Registry DISCOVERY reads report the registries and records the Agent Registry
//! CONTROL plane holds — driven end to end over the real wire path, both planes mounted on
//! one router over one shared engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! THE RULE UNDER TEST: this model binds three discovery READS and no operation that
//! creates, updates or deletes anything, so the registries and records they report are the
//! control plane's resources and are resolved in the store that owns them. A record
//! created through `CreateRegistryRecord` and moved to `APPROVED` through the control
//! plane's own approval workflow is what a discovery read answers with — "a record is
//! `APPROVED` once it is approved and discoverable", while "`REJECTED` and `DEPRECATED`
//! records are not discoverable" and `DRAFT` / `PENDING_APPROVAL` describe one that has not
//! become discoverable yet (`RegistryRecordStatus`, `smithy_query(query="shape")`) — and a
//! registry the control plane does not hold is `ResourceNotFoundException` for every one of
//! them.
//!
//! The table spans what that rule generalises over: every status a record reaches through
//! the control plane's own operations, each of the three discovery reads, and both forms
//! `RegistryIdentifier` admits. Nothing here seeds a store directly: each row's record is
//! driven in through the control plane's operations and read back through the discovery
//! plane, so neither plane is asserted against a copy of the other's state.
//!
//! Two shapes of the ANSWER are pinned beside it, both recorded AWS behaviour that the
//! modelled output shape alone does not state: a successful search carries `searchType`
//! (unmodelled: `SearchDiscoverableRegistryRecordsResponse` declares only
//! `registryRecords`), and a batch-get always carries both required members, with `errors`
//! PRESENT and EMPTY when every requested record was returned.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below signs for.
const REGION: &str = "us-west-2";

/// A signed-looking request: its credential scope names the signing name BOTH Agent
/// Registry services answer on, so the route decides which one serves it. The signature is
/// never checked — the test router runs with authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// How long a wait for a transition the clock owes may take before a test gives up.
/// Generous on purpose: the wait is on the CONDITION, so a loaded host only polls more.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The `searchType` every recorded successful search carries, and the one search mode this
/// emulator runs.
const SEARCH_TYPE: &str = "HYBRID";

/// The `BatchGetDiscoverableRegistryRecordErrorCode` reported for a record a batch-get
/// could not retrieve (`smithy_query(query="shape")` lists the whole enum:
/// RESOURCE_NOT_FOUND, ACCESS_DENIED, INTERNAL_ERROR).
const RESOURCE_NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// One HTTP answer, reduced to what these tests assert on.
struct Reply {
    status: u16,
    content_type: String,
    error_type: String,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    /// The `recordId`s of the answer's `registryRecords`, in response order, or `None`
    /// when the answer carries no such member at all.
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

/// Both Agent Registry services on one router over ONE shared engine.
fn both_planes() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_agentregistry_agentregistry::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService::new(
            engine,
        ),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://agent-registry.{REGION}.amazonaws.com{path}"))
        .header("host", format!("agent-registry.{REGION}.amazonaws.com"))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(payload)
        .expect("a valid request");
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        content_type,
        error_type,
        body,
    }
}

/// Percent-encode a path segment the way an SDK encodes a non-greedy `@httpLabel`.
fn encode_label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The last `/`-separated segment of an ARN: a registry id, or a record id.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

// --- the control plane's own operations, as a client calls them -----------------------

async fn create_registry(router: &MultiServiceRouter, name: &str, auto_approve: bool) -> Reply {
    let mut body = json!({ "name": name });
    if auto_approve {
        // `ApprovalConfiguration.autoApprovalRules`: "the rules that determine which
        // registry records are automatically approved on submission"; `APPROVE_ALL` is the
        // one rule the model declares (`AUTO_APPROVAL_RULE_VALUES`).
        body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
    }
    call(router, "POST", "/registries", Some(body)).await
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

async fn create_record(
    router: &MultiServiceRouter,
    registry_id: &str,
    name: &str,
    description: &str,
) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(json!({
            "name": name,
            "description": description,
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

async fn set_record_status(
    router: &MultiServiceRouter,
    registry_id: &str,
    record_id: &str,
    status: &str,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!(
            "/registries/{}/records/{}/status",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        Some(json!({ "status": status, "statusReason": "driven by the suite" })),
    )
    .await
}

// --- the discovery plane's three reads ------------------------------------------------

async fn list_discoverable(router: &MultiServiceRouter, registry: &str) -> Reply {
    call(
        router,
        "POST",
        &format!(
            "/registries/{}/discoverable-records-list",
            encode_label(registry)
        ),
        Some(json!({})),
    )
    .await
}

async fn search_discoverable(router: &MultiServiceRouter, registry: &str, query: &str) -> Reply {
    call(
        router,
        "POST",
        "/discoverable-records-search",
        Some(json!({ "searchQuery": query, "registryIds": [registry] })),
    )
    .await
}

async fn batch_get_discoverable(
    router: &MultiServiceRouter,
    registry: &str,
    record_ids: &[&str],
) -> Reply {
    call(
        router,
        "POST",
        "/discoverable-records-batch",
        Some(json!({ "entries": [{ "registryId": registry, "recordIds": record_ids }] })),
    )
    .await
}

/// Look at a registry until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn registry_reaching(router: &MultiServiceRouter, case: &str, registry_id: &str, wanted: &str) {
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

/// A registry the control plane holds, READY, with the records of `names` created in it and
/// left in `DRAFT`. Returns the registry id and one record id per name, in order.
async fn registry_with_records(
    router: &MultiServiceRouter,
    case: &str,
    auto_approve: bool,
    names: &[&str],
) -> (String, Vec<String>) {
    let registry_name: String = case
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let created = create_registry(router, &registry_name, auto_approve).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry answers 202, got {} {:?}",
        created.status, created.body
    );
    let registry_id = id_of(created.str_field("registryArn")).to_string();
    registry_reaching(router, case, &registry_id, "READY").await;

    let mut record_ids = Vec::with_capacity(names.len());
    for name in names {
        let record = create_record(router, &registry_id, name, &format!("{name} description")).await;
        assert_eq!(
            record.status, 202,
            "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
            record.status, record.body
        );
        let record_id = id_of(record.str_field("recordArn")).to_string();
        // Creation is asynchronous: the record is CREATING and the clock owes it DRAFT.
        record_reaching(router, case, &registry_id, &record_id, "DRAFT").await;
        record_ids.push(record_id);
    }
    (registry_id, record_ids)
}

#[tokio::test]
async fn a_record_is_discoverable_exactly_while_the_control_plane_holds_it_approved() {
    let router = both_planes();

    // (case, how far through the control plane's workflow the record is taken, the status
    // it then reports, whether a discovery read may report it). The statuses are the ones
    // reachable through the control plane's own operations; `RegistryRecordStatus`' own
    // doc is what says which of them is discoverable.
    enum Workflow {
        /// Created and not submitted.
        Draft,
        /// Submitted to a registry that auto-approves submissions.
        Approved,
        /// Submitted to a registry that does not, so it waits for a curator.
        PendingApproval,
        /// Approved, then taken out of the discovery surface by the curation workflow.
        Deprecated,
        /// Submitted, then turned down by the curation workflow.
        Rejected,
    }
    let rows: [(&str, Workflow, &str, bool); 5] = [
        ("a created record", Workflow::Draft, "DRAFT", false),
        ("an approved record", Workflow::Approved, "APPROVED", true),
        (
            "a record awaiting manual review",
            Workflow::PendingApproval,
            "PENDING_APPROVAL",
            false,
        ),
        (
            "a record deprecated after approval",
            Workflow::Deprecated,
            "DEPRECATED",
            false,
        ),
        ("a rejected record", Workflow::Rejected, "REJECTED", false),
    ];

    for (case, workflow, status, discoverable) in rows {
        let name = format!("probe-{}", status.to_lowercase().replace('_', "-"));
        let auto_approve = !matches!(workflow, Workflow::PendingApproval);
        let (registry_id, record_ids) =
            registry_with_records(&router, case, auto_approve, &[name.as_str()]).await;
        let record_id = record_ids[0].clone();
        match workflow {
            Workflow::Draft => {}
            Workflow::Approved | Workflow::PendingApproval => {
                let submitted = submit_record(&router, &registry_id, &record_id).await;
                assert_eq!(
                    submitted.status, 202,
                    "case {case:?}: SubmitRegistryRecordForApproval answers 202, got {} {:?}",
                    submitted.status, submitted.body
                );
            }
            Workflow::Deprecated | Workflow::Rejected => {
                submit_record(&router, &registry_id, &record_id).await;
                let target = if matches!(workflow, Workflow::Rejected) {
                    "REJECTED"
                } else {
                    "DEPRECATED"
                };
                let updated = set_record_status(&router, &registry_id, &record_id, target).await;
                assert_eq!(
                    (updated.status, updated.str_field("status")),
                    (202, target),
                    "case {case:?}: UpdateRegistryRecordStatus answers 202 with the status it \
                     wrote, got {} {:?}",
                    updated.status,
                    updated.body
                );
            }
        }
        record_reaching(&router, case, &registry_id, &record_id, status).await;

        let expected: Vec<String> = if discoverable {
            vec![record_id.clone()]
        } else {
            Vec::new()
        };

        let listed = list_discoverable(&router, &registry_id).await;
        assert_eq!(
            (listed.status, listed.record_ids()),
            (200, Some(expected.clone())),
            "case {case:?} ({status}): the list reports exactly the discoverable records, \
             got {} {:?}",
            listed.status,
            listed.body
        );

        let searched = search_discoverable(&router, &registry_id, &name).await;
        assert_eq!(
            (searched.status, searched.record_ids()),
            (200, Some(expected.clone())),
            "case {case:?} ({status}): the search reports exactly the discoverable records, \
             got {} {:?}",
            searched.status,
            searched.body
        );

        let fetched = batch_get_discoverable(&router, &registry_id, &[record_id.as_str()]).await;
        assert_eq!(
            (fetched.status, fetched.record_ids()),
            (200, Some(expected.clone())),
            "case {case:?} ({status}): the batch-get reports exactly the discoverable \
             records, got {} {:?}",
            fetched.status,
            fetched.body
        );
        let errors = fetched.body["errors"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: errors must be a list, got {:?}", fetched.body));
        assert_eq!(
            errors.len(),
            usize::from(!discoverable),
            "case {case:?} ({status}): a record the batch-get could not return is reported \
             once in errors, got {errors:?}"
        );
        if !discoverable {
            assert_eq!(
                (
                    errors[0]["errorCode"].as_str().unwrap_or_default(),
                    errors[0]["registryId"].as_str().unwrap_or_default(),
                    errors[0]["recordId"].as_str().unwrap_or_default(),
                ),
                (RESOURCE_NOT_FOUND_CODE, registry_id.as_str(), record_id.as_str()),
                "case {case:?} ({status}): the error echoes the entry's ids under \
                 {RESOURCE_NOT_FOUND_CODE}, got {errors:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_discovery_read_resolves_a_registry_by_id_and_by_arn() {
    let router = both_planes();
    let case = "approved-record-by-either-identifier";
    let (registry_id, record_ids) =
        registry_with_records(&router, case, true, &["payments-mcp"]).await;
    let record_id = record_ids[0].clone();
    submit_record(&router, &registry_id, &record_id).await;
    record_reaching(&router, case, &registry_id, &record_id, "APPROVED").await;

    // `RegistryIdentifier`: "You can provide either the full Amazon Web Services Resource
    // Name (ARN) or the registry ID" — both name the same registry, so both reach the same
    // record. The ARN is the one the control plane minted and reported.
    let registry_arn = get_registry(&router, &registry_id)
        .await
        .str_field("registryArn")
        .to_string();
    assert!(
        registry_arn.ends_with(&format!("registry/{registry_id}")),
        "case {case:?}: the control plane reports the registry's ARN, got {registry_arn:?}"
    );

    for identifier in [registry_id.as_str(), registry_arn.as_str()] {
        for (read, reply) in [
            ("list", list_discoverable(&router, identifier).await),
            (
                "search",
                search_discoverable(&router, identifier, "payments").await,
            ),
            (
                "batch-get",
                batch_get_discoverable(&router, identifier, &[record_id.as_str()]).await,
            ),
        ] {
            assert_eq!(
                (reply.status, reply.record_ids()),
                (200, Some(vec![record_id.clone()])),
                "case {case:?}: the {read} read resolves {identifier:?} and reports the \
                 approved record, got {} {:?}",
                reply.status,
                reply.body
            );
        }
    }

    // Both planes report the same instants for the same record, in the ONE wire form the
    // model declares for them: `createdAt` / `updatedAt` target `DateTimestamp`, which
    // carries `@timestampFormat("date-time")`, so each is an RFC 3339 STRING and not the
    // epoch-seconds number `restJson1` defaults to.
    let held = get_record(&router, &registry_id, &record_id).await;
    let fetched = batch_get_discoverable(&router, &registry_id, &[record_id.as_str()]).await;
    let summary = &fetched.body["registryRecords"][0];
    for member in ["createdAt", "updatedAt"] {
        let reported = summary[member].as_str();
        assert_eq!(
            reported,
            held.body[member].as_str(),
            "case {case:?}: the discovery read reports the instant the control plane holds \
             for {member}, got {:?} against {:?}",
            summary[member],
            held.body[member]
        );
        assert!(
            reported.is_some_and(|text| ::aws_smithy_types::DateTime::from_str(
                text,
                ::aws_smithy_types::date_time::Format::DateTime
            )
            .is_ok()),
            "case {case:?}: {member} is an RFC 3339 date-time string, got {:?}",
            summary[member]
        );
    }

    // A registry id of the modelled shape that the control plane holds no registry under
    // is the modelled 404 for every read — the other side of the same rule.
    let absent = "zzz999999999";
    for (read, reply) in [
        ("list", list_discoverable(&router, absent).await),
        ("search", search_discoverable(&router, absent, "payments").await),
        (
            "batch-get",
            batch_get_discoverable(&router, absent, &[record_id.as_str()]).await,
        ),
    ] {
        assert_eq!(
            (reply.status, reply.str_field("__type")),
            (404, "ResourceNotFoundException"),
            "case \"a registry the control plane does not hold\": the {read} read answers \
             the modelled 404, got {} {:?}",
            reply.status,
            reply.body
        );
    }
}

#[tokio::test]
async fn the_search_answer_carries_its_search_type_and_the_batch_answer_both_members() {
    let router = both_planes();
    let case = "answer-shape";
    let (registry_id, record_ids) =
        registry_with_records(&router, case, true, &["alpha-agent", "beta-agent"]).await;
    for record_id in &record_ids {
        submit_record(&router, &registry_id, record_id).await;
        record_reaching(&router, case, &registry_id, record_id, "APPROVED").await;
    }

    // `searchType` is recorded on EVERY successful search, whatever the query matched, so
    // the rows differ along how many records the answer carries: both, one, none.
    let rows: [(&str, &str, usize); 3] = [
        ("a query both records match", "agent", 2),
        ("a query one record matches", "alpha", 1),
        ("a query no record matches", "nothing-matches-this", 0),
    ];
    for (row, query, matched) in rows {
        let searched = search_discoverable(&router, &registry_id, query).await;
        assert_eq!(
            searched.status, 200,
            "case {row:?}: a search over an existing registry answers 200, got {} {:?}",
            searched.status, searched.body
        );
        assert_eq!(
            searched.record_ids().map(|ids| ids.len()),
            Some(matched),
            "case {row:?}: expected {matched} matching record(s), got {:?}",
            searched.body
        );
        assert_eq!(
            searched.str_field("searchType"),
            SEARCH_TYPE,
            "case {row:?}: every successful search reports its search type, got {:?}",
            searched.body
        );
        assert!(
            searched.content_type.starts_with("application/json"),
            "case {row:?}: restJson1 frames the answer as application/json, got {:?}",
            searched.content_type
        );
    }

    // `errors` is REQUIRED on the batch answer and "is empty when all requested records
    // were returned": the rows differ along how many of the requested ids resolve.
    let present: Vec<&str> = record_ids.iter().map(String::as_str).collect();
    let absent = "zzz999999999";
    let rows: [(&str, Vec<&str>, usize, usize); 3] = [
        ("every id resolves", present.clone(), present.len(), 0),
        (
            "one id of two resolves",
            vec![present[0], absent],
            1,
            1,
        ),
        ("no id resolves", vec![absent], 0, 1),
    ];
    for (row, requested, returned, failed) in rows {
        let fetched = batch_get_discoverable(&router, &registry_id, &requested).await;
        assert_eq!(
            fetched.status, 200,
            "case {row:?}: a per-entry miss is never the whole call's failure, got {} {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.record_ids().map(|ids| ids.len()),
            Some(returned),
            "case {row:?}: expected {returned} record(s) in registryRecords, got {:?}",
            fetched.body
        );
        assert_eq!(
            fetched.body["errors"].as_array().map(Vec::len),
            Some(failed),
            "case {row:?}: errors is present and carries one entry per id that did not \
             resolve, got {:?}",
            fetched.body
        );
    }
}

#[tokio::test]
async fn a_request_this_endpoint_binds_no_route_for_is_refused_as_validation() {
    let router = both_planes();

    // Rows span the ways a request addressed to this endpoint can name none of its routes:
    // an `@httpLabel` that collapsed out of the path, one that expanded it (an ARN whose
    // `/` separators were not encoded), a method no route carries on a path a model does
    // bind, a path depth neither model reaches, and a path space neither publishes.
    let rows: [(&str, &str, &str); 6] = [
        (
            "the registry label collapsed out of the path",
            "POST",
            "/registries/discoverable-records-list",
        ),
        (
            "an unencoded registry ARN expanded the path",
            "POST",
            "/registries/arn:aws:agent-registry:us-west-2:000000000000:registry/aaaaaaaaaaaa/discoverable-records-list",
        ),
        (
            "a method no route carries on the search path",
            "GET",
            "/discoverable-records-search",
        ),
        (
            "a method no route carries on the registries path",
            "GET",
            "/registries",
        ),
        (
            "a path depth neither model reaches",
            "POST",
            "/registries/aaaaaaaaaaaa/records/bbbbbbbbbbbb/nothing-here",
        ),
        ("a path space neither model publishes", "POST", "/nothing-here"),
    ];

    for (case, method, path) in rows {
        let reply = call(&router, method, path, Some(json!({}))).await;
        assert_eq!(
            (reply.status, reply.str_field("__type"), reply.str_field("message")),
            (400, "ValidationException", "Request invalid"),
            "case {case:?} ({method} {path}): expected the modelled 400 ValidationException \
             with its fixed text, got {} {:?}",
            reply.status,
            reply.body
        );
        assert!(
            reply.content_type.starts_with("application/json"),
            "case {case:?}: restJson1 frames every answer as application/json, got {:?}",
            reply.content_type
        );
        assert_eq!(
            reply.error_type, "ValidationException",
            "case {case:?}: restJson1 names the error shape in x-amzn-errortype, got {:?}",
            reply.error_type
        );
    }

    // And a request either model DOES bind a route for is served by that plane, never by
    // this refusal: the control plane's create and the data plane's search.
    let created = create_registry(&router, "bound-route-probe", true).await;
    assert_eq!(
        created.status, 202,
        "case \"a route the control plane binds\": it is served by the control plane, got \
         {} {:?}",
        created.status, created.body
    );
    let registry_id = id_of(created.str_field("registryArn")).to_string();
    registry_reaching(&router, "bound-route-probe", &registry_id, "READY").await;
    let searched = search_discoverable(&router, &registry_id, "anything").await;
    assert_eq!(
        (searched.status, searched.record_ids()),
        (200, Some(Vec::new())),
        "case \"a route the data plane binds\": it is served by the data plane, got {} {:?}",
        searched.status,
        searched.body
    );
}
