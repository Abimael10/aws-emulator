// SPDX-License-Identifier: CC0-1.0

//! What a client that creates an Amazon OpenSearch Service domain and then WAITS for it
//! sees — driven end to end on the ASSEMBLED emulator as ONE sequence, the way a workload
//! runs it: create the domain, poll it to readiness, drive the index API on it, then upgrade
//! its engine and read the indexes back, with nothing but the operations a client calls in
//! between and no call of its own advancing anything.
//!
//! That ONE sequence across the operations is what this suite adds. Each rule it leans on is
//! already pinned per operation by the owning crate's own suites — `tests/all/
//! clock_owed_readiness.rs` and `tests/all/domain_lifecycle.rs` for the settle a read
//! publishes, `tests/all/index_management_rules.rs` and `tests/all/index_modelled_answers.rs`
//! for the index API's answers and its refusal classes, and `tests/all/
//! upgrade_progress_convergence/readiness_admits_the_next_upgrade.rs`, `tests/all/
//! upgrade_status.rs` and `tests/all/upgrade_history.rs` for an upgrade's admission on the
//! first quiet read and its projection — and the index life cycle itself is driven through
//! the assembled router by `cloudformation_deployed_domain_index_lifecycle.rs` beside this
//! file. This suite adds no second copy of their tables: what it adds is the one sequence a
//! client takes from the create to an upgraded domain, the refusal rows for a component name
//! the cluster cannot be handed taken on the domain that sequence just saw reported active,
//! and the indexes read back after the upgrade.
//!
//! Three rules, which the sequence asserts over a SET of inputs rather than one recorded
//! call:
//!
//! * **a reporting read publishes the provisioning the clock already owes the domain on
//!   THAT read**: a create answers the domain in its transient status
//!   (`DomainProcessingStatus` `Creating`, `Processing` true, no published address, which is
//!   the readiness signal a caller polls for), and the FIRST read taken once that span has
//!   passed reports it settled WHOLE — `Processing` false, `Active`, with the address its
//!   data plane answers on — whichever operation takes that read, with no further read
//!   needed to finish it and no read changing what the next caller sees;
//! * **the index API serves a domain from the moment that read reports it active**: one
//!   attempt of `CreateIndex` answers `CREATED` for every well-formed schema, `GetIndex`
//!   round-trips exactly what was stored, a mappings-only `UpdateIndex` answers `UPDATED`,
//!   `DeleteIndex` answers `DELETED` and frees the name — while a schema defining an
//!   analysis component under a name carrying a character outside ASCII is refused `424`
//!   `DependencyFailureException` on every attempt, storing nothing, so the name it was
//!   refused under reads back as a resource that does not exist;
//! * **an upgrade accepted on a domain that reports itself quiet is SERVICE-managed**:
//!   `UpgradeDomain` is admitted by the read that first reports `Processing` false, and the
//!   run reaches `UPGRADE` with a terminal `StepStatus` on the service's own clock — polling
//!   `GetUpgradeStatus` ALONE gets there, and so does a client that never reads the status
//!   until the run is over — the newest `GetUpgradeHistory` entry reaches the same terminal
//!   status with at least one step, the domain then reports the requested `EngineVersion`,
//!   and the indexes created before the upgrade read back unchanged after it.
//!
//! The rows span the engine release the domain runs, the shape of the schema, which kind of
//! analysis component carries the name and which script that name is written in, so nothing
//! asserted here can be a fixed value.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The API version this service binds its control plane under, the first path segment of
/// every call below.
const CONTROL_PREFIX: &str = "/2021-01-01";

/// The account's own published credential, which every call here is made with.
const AMBIENT_KEY: &str = "AKIDEXAMPLE";

/// How long a poll waits for work the clock owes a domain — its provisioning or an upgrade
/// run falling due — before it calls waiting a failure, and how often it looks while
/// waiting. Generous, because no case here is about how long either span is: every wait is
/// for the CONDITION the control plane reports.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `DomainProcessingStatusType`'s value for a domain whose cluster is still being stood up,
/// and for one whose configuration is live.
const PROCESSING_STATUS_CREATING: &str = "Creating";
const PROCESSING_STATUS_ACTIVE: &str = "Active";

/// The fixed sentence this service states a dependency failure of a domain's own cluster
/// with, as the recorded refusal of an unusable schema carries it — no variable part.
const DOMAIN_UNAVAILABLE: &str = "The domain is not available now, please try again later.";

/// `UpgradeStep`'s last value, and the terminal values of `UpgradeStatus`
/// (`smithy_query(query="shape", shape="UpgradeStep,UpgradeStatus")`), which are what a
/// polling client stops on — `SUCCEEDED` being the one an undegraded run reaches.
const UPGRADE_STEP_UPGRADE: &str = "UPGRADE";
const STEP_STATUS_SUCCEEDED: &str = "SUCCEEDED";
const TERMINAL_STEP_STATUS: [&str; 2] = [STEP_STATUS_SUCCEEDED, "SUCCEEDED_WITH_ISSUES"];

/// One answered request: its status, its body as JSON (`Value::Null` when the body is not
/// JSON) and the raw text, which is what an assertion reports.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text)
    }

    /// The error code the envelope of a refusal carries, as a client reads it.
    fn error_code(&self) -> String {
        self.body["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The message of a refusal, whichever member the envelope carries it under.
    fn error_message(&self) -> String {
        for member in ["message", "Message"] {
            if let Some(text) = self.body[member].as_str() {
                return text.to_string();
            }
        }
        String::new()
    }
}

/// The emulator as it is ASSEMBLED, on one engine, with request authentication off: the
/// suite presents its credential as a credential SCOPE rather than a real signature.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header naming this service in its credential scope.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/es/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One call of this service's control plane, as the account's own credential.
///
/// A copy of the call helper rather than the one
/// `cloudformation_provisions_opensearch_domains` exports, because this suite drives the
/// FULLY ASSEMBLED router it builds here (`aws_emulator_router::build_router`) and reads the
/// refusal envelopes through [`Answer::error_code`] and [`Answer::error_message`].
async fn control(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let host = format!("es.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{CONTROL_PREFIX}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, body, text }
}

/// `CreateDomain` for `domain` on `engine_version`, and the `DomainStatus` it answered.
async fn created_domain(router: &MultiServiceRouter, domain: &str, engine_version: &str) -> Value {
    let created = control(
        router,
        "POST",
        "/opensearch/domain",
        Some(json!({
            "DomainName": domain,
            "EngineVersion": engine_version,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {domain:?} on {engine_version:?}: CreateDomain expected 200, got {}",
        created.summary()
    );
    created.body["DomainStatus"].clone()
}

/// One `DescribeDomain` read of `domain`.
async fn describe(router: &MultiServiceRouter, domain: &str) -> Answer {
    control(router, "GET", &format!("/opensearch/domain/{domain}"), None).await
}

/// Whether a reported `DomainStatus` says the configuration is live.
fn is_active(status: &Value) -> bool {
    status["Processing"].as_bool() == Some(false)
}

/// Assert that `status` is a WHOLE report of a settled domain: the configuration is live, the
/// domain says so under its own lifecycle member, and it publishes the address its data plane
/// answers on — every part of the settle on the one read, with nothing half-applied.
fn assert_settled_whole(case: &str, status: &Value) {
    assert!(
        is_active(status),
        "case {case:?}: expected a settled read to report Processing=false, got {status}"
    );
    assert_eq!(
        status["DomainProcessingStatus"].as_str(),
        Some(PROCESSING_STATUS_ACTIVE),
        "case {case:?}: expected DomainProcessingStatus={PROCESSING_STATUS_ACTIVE:?} on the \
         read that reports the configuration live, got {status}"
    );
    assert_eq!(
        status["Created"].as_bool(),
        Some(true),
        "case {case:?}: expected Created=true on a settled read, got {status}"
    );
    assert!(
        status["Endpoint"].as_str().is_some_and(|host| !host.is_empty()),
        "case {case:?}: expected the address the domain publishes for its data plane on the \
         read that reports the configuration live, got {status}"
    );
    assert_eq!(
        status["UpgradeProcessing"].as_bool(),
        Some(false),
        "case {case:?}: expected UpgradeProcessing=false on a settled read, got {status}"
    );
}

/// Assert that `status` is a WHOLE report of a domain still being provisioned: it is in its
/// transient lifecycle status and publishes no address, which is the readiness signal a
/// client polls for.
fn assert_provisioning_whole(case: &str, status: &Value) {
    assert_eq!(
        status["DomainProcessingStatus"].as_str(),
        Some(PROCESSING_STATUS_CREATING),
        "case {case:?}: expected DomainProcessingStatus={PROCESSING_STATUS_CREATING:?} while \
         the configuration is still being applied, got {status}"
    );
    assert!(
        status["Endpoint"].is_null(),
        "case {case:?}: expected no published address before the cluster is up, got {status}"
    );
}

/// Poll `DescribeDomains` — the PLURAL read, so the settle is observed by an operation other
/// than the one asserted afterwards — until it reports `domain` with its configuration live.
async fn poll_domains_until_active(router: &MultiServiceRouter, case: &str, domain: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = control(
            router,
            "POST",
            "/opensearch/domain-info",
            Some(json!({ "DomainNames": [domain] })),
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeDomains expected 200, got {}",
            read.summary()
        );
        let status = &read.body["DomainStatusList"][0];
        if is_active(status) {
            assert_settled_whole(case, status);
            return;
        }
        assert_provisioning_whole(case, status);
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the domain never reported its provisioning settled, last read {}",
            read.summary()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One engine release a domain here is created on, from this service's own published
/// catalogue (`GetCompatibleVersions`/`ListVersions`), so the rule is asserted across
/// releases rather than on one.
struct ReadinessRow {
    case: &'static str,
    domain: &'static str,
    engine_version: &'static str,
}

fn readiness_rows() -> Vec<ReadinessRow> {
    vec![
        ReadinessRow {
            case: "a domain on an OpenSearch 2.x release",
            domain: "readiness-one",
            engine_version: "OpenSearch_2.11",
        },
        ReadinessRow {
            case: "a domain on an OpenSearch 1.x release",
            domain: "readiness-two",
            engine_version: "OpenSearch_1.3",
        },
        ReadinessRow {
            case: "a domain on a legacy Elasticsearch release",
            domain: "readiness-three",
            engine_version: "Elasticsearch_7.10",
        },
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_created_domain_publishes_its_settled_state_on_the_read_that_finds_it_due() {
    let router = build_router();
    for row in readiness_rows() {
        let case = row.case;
        // The create itself answers the domain in its transient status: a client acting
        // straight after the call sees provisioning in flight rather than a cluster that is
        // already up.
        let created = created_domain(&router, row.domain, row.engine_version).await;
        assert_eq!(
            created["Processing"].as_bool(),
            Some(true),
            "case {case:?}: expected the create to answer the domain still being provisioned, \
             got {created}"
        );
        assert_provisioning_whole(case, &created);

        // The settle is observed by the PLURAL read, which materialises what the clock owed.
        poll_domains_until_active(&router, case, row.domain).await;

        // ... and the very next single read of that domain reports it settled too: no read
        // has to be taken twice for the state to appear, and reading it again does not move
        // it.
        let read = describe(&router, row.domain).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeDomain expected 200, got {}",
            read.summary()
        );
        assert_settled_whole(case, &read.body["DomainStatus"]);
        let again = describe(&router, row.domain).await;
        assert_eq!(
            again.body["DomainStatus"]["Endpoint"],
            read.body["DomainStatus"]["Endpoint"],
            "case {case:?}: expected the published address to stand across reads, got {} then \
             {}",
            read.summary(),
            again.summary()
        );
        assert_settled_whole(case, &again.body["DomainStatus"]);
    }
}

/// Create `domain`, then poll it the way a client does and return from the FIRST read that
/// reports its configuration live — the signal the rest of the sequence is driven from.
async fn settled_domain(
    router: &MultiServiceRouter,
    case: &str,
    domain: &str,
    engine_version: &str,
) {
    created_domain(router, domain, engine_version).await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = describe(router, domain).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeDomain expected 200, got {}",
            read.summary()
        );
        if is_active(&read.body["DomainStatus"]) {
            assert_settled_whole(case, &read.body["DomainStatus"]);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the domain never reported its provisioning settled, last read {}",
            read.summary()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `CreateIndex` on `domain`, once.
async fn create_index(
    router: &MultiServiceRouter,
    domain: &str,
    index: &str,
    schema: &Value,
) -> Answer {
    control(
        router,
        "POST",
        &format!("/opensearch/domain/{domain}/index"),
        Some(json!({ "IndexName": index, "IndexSchema": schema })),
    )
    .await
}

async fn get_index(router: &MultiServiceRouter, domain: &str, index: &str) -> Answer {
    control(
        router,
        "GET",
        &format!("/opensearch/domain/{domain}/index/{index}"),
        None,
    )
    .await
}

async fn update_index(
    router: &MultiServiceRouter,
    domain: &str,
    index: &str,
    schema: &Value,
) -> Answer {
    control(
        router,
        "PUT",
        &format!("/opensearch/domain/{domain}/index/{index}"),
        Some(json!({ "IndexSchema": schema })),
    )
    .await
}

async fn delete_index(router: &MultiServiceRouter, domain: &str, index: &str) -> Answer {
    control(
        router,
        "DELETE",
        &format!("/opensearch/domain/{domain}/index/{index}"),
        None,
    )
    .await
}

/// One index a client creates as soon as the domain reports itself active: the schema it
/// sends, and the mappings-only change it then merges.
struct IndexRow {
    case: &'static str,
    index: &'static str,
    schema: Value,
    update: Value,
}

/// The rows differ in the shape of the schema — settings and mappings together, and mappings
/// alone over an analyzer the engine ships — and in the index name's own form, so neither the
/// answer nor the stored schema can be a fixed value.
fn index_rows() -> Vec<IndexRow> {
    vec![
        IndexRow {
            case: "a custom analyzer defined in the settings and named by a field",
            index: "catalog",
            schema: json!({
                "settings": { "index": { "analysis": { "analyzer": {
                    "s2r_custom_analyzer": { "type": "custom", "tokenizer": "standard" }
                } } } },
                "mappings": { "properties": {
                    "title": { "type": "text", "analyzer": "s2r_custom_analyzer" }
                } },
            }),
            update: json!({
                "mappings": { "properties": {
                    "body": { "type": "text", "analyzer": "s2r_custom_analyzer" }
                } },
            }),
        },
        IndexRow {
            case: "mappings alone, over an analyzer the engine ships",
            index: "orders.2026",
            schema: json!({
                "mappings": { "properties": {
                    "customer": { "type": "keyword" },
                    "note": { "type": "text", "analyzer": "english" },
                } },
            }),
            update: json!({
                "mappings": { "properties": { "total": { "type": "double" } } },
            }),
        },
    ]
}

/// One schema whose analysis section defines a component under a name the cluster cannot be
/// handed: the kind of component and the script the name is written in both vary, because the
/// rule is about the name's characters rather than about one analyzer.
struct UnnameableRow {
    case: &'static str,
    index: &'static str,
    schema: Value,
}

fn unnameable_rows() -> Vec<UnnameableRow> {
    vec![
        UnnameableRow {
            case: "an analyzer named across three non-Latin scripts",
            index: "mixed-script",
            schema: json!({
                "settings": { "index": { "analysis": { "analyzer": {
                    "自定义分析器_ünïcödé_анализатор": { "type": "custom", "tokenizer": "standard" }
                } } } },
                "mappings": { "properties": {
                    "title": { "type": "text", "analyzer": "自定义分析器_ünïcödé_анализатор" }
                } },
            }),
        },
        UnnameableRow {
            case: "a TOKEN FILTER named outside ASCII, in an otherwise ASCII chain",
            index: "unnameable-filter",
            schema: json!({
                "settings": { "index": { "analysis": {
                    "filter": { "フィルター": { "type": "lowercase" } },
                    "analyzer": { "folding": {
                        "type": "custom",
                        "tokenizer": "standard",
                        "filter": ["フィルター"],
                    } },
                } } },
                "mappings": { "properties": {
                    "title": { "type": "text", "analyzer": "folding" }
                } },
            }),
        },
        UnnameableRow {
            case: "a TOKENIZER named outside ASCII, in an otherwise ASCII chain",
            index: "unnameable-tokenizer",
            schema: json!({
                "settings": { "index": { "analysis": {
                    "tokenizer": { "τοκενιστής": { "type": "whitespace" } },
                    "analyzer": { "spaced": {
                        "type": "custom",
                        "tokenizer": "τοκενιστής",
                    } },
                } } },
                "mappings": { "properties": {
                    "title": { "type": "text", "analyzer": "spaced" }
                } },
            }),
        },
    ]
}

/// The sentence this service states an index that does not exist with: the whole message,
/// whose only variable part is the index name the request carried.
fn index_absent_message(index: &str) -> String {
    format!("Resource index {index} does not exist")
}

/// Drive the index API on `domain`, which the caller has just seen reported active, and leave
/// every row's index in place so a later read can show it survived the upgrade.
async fn exercise_index_api(router: &MultiServiceRouter, domain: &str) {
    for row in index_rows() {
        let case = row.case;
        // ONE attempt, taken straight after the read that reported the domain active: the
        // domain is usable from then on, so a well-formed schema is not answered with a
        // retryable dependency failure.
        let created = create_index(router, domain, row.index, &row.schema).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateIndex expected 200 on the first attempt once the domain \
             reports active, got {}",
            created.summary()
        );
        assert_eq!(
            created.body["Status"].as_str(),
            Some("CREATED"),
            "case {case:?}: expected Status=\"CREATED\", got {}",
            created.summary()
        );

        // What was stored is what was sent, member for member — reported the way the
        // engine's own mapping API reports one index, keyed by the index name.
        let read = get_index(router, domain, row.index).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetIndex expected 200, got {}",
            read.summary()
        );
        assert_eq!(
            read.body["IndexSchema"][row.index], row.schema,
            "case {case:?}: expected GetIndex to answer the schema that was stored, got {}",
            read.summary()
        );

        // A mappings-only change merges into it and answers the update's own status.
        let updated = update_index(router, domain, row.index, &row.update).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: UpdateIndex expected 200, got {}",
            updated.summary()
        );
        assert_eq!(
            updated.body["Status"].as_str(),
            Some("UPDATED"),
            "case {case:?}: expected Status=\"UPDATED\", got {}",
            updated.summary()
        );
        let merged = get_index(router, domain, row.index).await;
        for (field, mapping) in row.update["mappings"]["properties"]
            .as_object()
            .expect("the row's update maps fields")
        {
            assert_eq!(
                &merged.body["IndexSchema"][row.index]["mappings"]["properties"][field], mapping,
                "case {case:?}: expected the merged mappings to carry {field:?}, got {}",
                merged.summary()
            );
        }
        for (field, mapping) in row.schema["mappings"]["properties"]
            .as_object()
            .expect("the row's schema maps fields")
        {
            assert_eq!(
                &merged.body["IndexSchema"][row.index]["mappings"]["properties"][field], mapping,
                "case {case:?}: expected the fields mapped at create to stand after the \
                 update, got {}",
                merged.summary()
            );
        }

        // A delete answers its own status and frees the name, which a create takes again.
        let deleted = delete_index(router, domain, row.index).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteIndex expected 200, got {}",
            deleted.summary()
        );
        assert_eq!(
            deleted.body["Status"].as_str(),
            Some("DELETED"),
            "case {case:?}: expected Status=\"DELETED\", got {}",
            deleted.summary()
        );
        for absent in [
            get_index(router, domain, row.index).await,
            update_index(router, domain, row.index, &row.update).await,
        ] {
            assert_eq!(
                absent.status, 404,
                "case {case:?}: expected a deleted index to read back as absent, got {}",
                absent.summary()
            );
            assert_eq!(
                absent.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException for a deleted index, got {}",
                absent.summary()
            );
            assert_eq!(
                absent.error_message(),
                index_absent_message(row.index),
                "case {case:?}: expected the service's own sentence for an absent index, got {}",
                absent.summary()
            );
        }
        let again = create_index(router, domain, row.index, &row.schema).await;
        assert_eq!(
            again.body["Status"].as_str(),
            Some("CREATED"),
            "case {case:?}: expected the freed name to be reusable, got {}",
            again.summary()
        );
    }

    for row in unnameable_rows() {
        let case = row.case;
        // Refused on EVERY attempt: the condition is the name in the document, so retrying
        // cannot make it pass, and nothing is stored in between.
        for attempt in 1..=2 {
            let refused = create_index(router, domain, row.index, &row.schema).await;
            assert_eq!(
                refused.status, 424,
                "case {case:?} attempt {attempt}: expected 424, got {}",
                refused.summary()
            );
            assert_eq!(
                refused.error_code(),
                "DependencyFailureException",
                "case {case:?} attempt {attempt}: expected DependencyFailureException, got {}",
                refused.summary()
            );
            assert_eq!(
                refused.error_message(),
                DOMAIN_UNAVAILABLE,
                "case {case:?} attempt {attempt}: expected the service's dependency sentence, \
                 got {}",
                refused.summary()
            );
        }
        let absent = get_index(router, domain, row.index).await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: expected the refused index to exist nowhere, got {}",
            absent.summary()
        );
        assert_eq!(
            absent.error_message(),
            index_absent_message(row.index),
            "case {case:?}: expected the service's own sentence for an absent index, got {}",
            absent.summary()
        );
    }
}

/// One run of the whole sequence: the release the domain is created on, whose upgrade target
/// is taken from what `GetCompatibleVersions` publishes for that source, and HOW the client
/// waits the upgrade out.
struct SequenceRow {
    case: &'static str,
    domain: &'static str,
    source: &'static str,
    /// When true the run is waited out WITHOUT reading its status — the history is polled
    /// instead and `GetUpgradeStatus` is read exactly once, at the end. That read reports a
    /// step no poll of it could have advanced, which is what tells a service-managed run
    /// from one a client drives; the other row waits the way the contract says a client may,
    /// polling the status alone.
    wait_on_history: bool,
}

fn sequence_rows() -> Vec<SequenceRow> {
    vec![
        SequenceRow {
            // The oldest release the index-management operations are offered on
            // (`state::INDEX_MANAGEMENT_ENGINE_VERSION`), so the sequence runs on the floor
            // of that gate as well as above it.
            case: "a sequence whose upgrade is polled through its status alone",
            domain: "sequence-three-one",
            source: "OpenSearch_3.1",
            wait_on_history: false,
        },
        SequenceRow {
            case: "a sequence whose upgrade is never polled through its status",
            domain: "sequence-three-three",
            source: "OpenSearch_3.3",
            wait_on_history: true,
        },
    ]
}

/// One `GetUpgradeStatus` read of the run on `domain`.
async fn upgrade_status(router: &MultiServiceRouter, case: &str, domain: &str) -> Answer {
    let read = control(
        router,
        "GET",
        &format!("/opensearch/upgradeDomain/{domain}/status"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetUpgradeStatus expected 200, got {}",
        read.summary()
    );
    read
}

/// One `GetUpgradeHistory` read of the runs on `domain`.
async fn upgrade_history(router: &MultiServiceRouter, case: &str, domain: &str) -> Answer {
    let read = control(
        router,
        "GET",
        &format!("/opensearch/upgradeDomain/{domain}/history"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetUpgradeHistory expected 200, got {}",
        read.summary()
    );
    read
}

/// The entry of `history` for the run that started last — the one a client reads to see what
/// became of the upgrade it just asked for.
fn newest_entry(history: &Answer) -> Value {
    history.body["UpgradeHistories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .max_by(|left, right| {
            let instant = |entry: &Value| entry["StartTimestamp"].as_f64().unwrap_or_default();
            instant(left).total_cmp(&instant(right))
        })
        .unwrap_or(Value::Null)
}

/// Wait for the run on `domain` to finish WITHOUT reading its status: the history is what is
/// polled, so a later status read reports a step nothing polling it advanced.
async fn poll_history_until_terminal(router: &MultiServiceRouter, case: &str, domain: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let history = upgrade_history(router, case, domain).await;
        let newest = newest_entry(&history);
        let status = newest["UpgradeStatus"].as_str().unwrap_or_default();
        if TERMINAL_STEP_STATUS.contains(&status) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the upgrade never reached a terminal status in its history, last \
             read {}",
            history.summary()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The first `OpenSearch_` target the service publishes for `source`, read from
/// `GetCompatibleVersions` rather than written here.
async fn published_target(router: &MultiServiceRouter, case: &str, source: &str) -> String {
    let offered = control(router, "GET", "/opensearch/compatibleVersions", None).await;
    assert_eq!(
        offered.status, 200,
        "case {case:?}: GetCompatibleVersions expected 200, got {}",
        offered.summary()
    );
    let pairs = offered.body["CompatibleVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let target = pairs
        .iter()
        .find(|pair| pair["SourceVersion"].as_str() == Some(source))
        .and_then(|pair| pair["TargetVersions"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|version| version.as_str().map(str::to_string))
        .find(|version| version.starts_with("OpenSearch_"));
    target.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected an OpenSearch target published for {source:?}, got {}",
            offered.summary()
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sequence_a_client_runs_from_a_create_to_an_upgraded_domain() {
    let router = build_router();
    for row in sequence_rows() {
        let case = row.case;
        let target = published_target(&router, case, row.source).await;

        // 1. The create, and the poll a client runs until the control plane reports the
        //    configuration live.
        settled_domain(&router, case, row.domain, row.source).await;

        // 2. The index API, driven from that read on, with no retry of its own.
        exercise_index_api(&router, row.domain).await;

        // 3. The upgrade, issued on that same signal: no window follows in which the
        //    service still says the domain has background work.
        let accepted = control(
            &router,
            "POST",
            "/opensearch/upgradeDomain",
            Some(json!({ "DomainName": row.domain, "TargetVersion": target })),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: UpgradeDomain expected 200 while the domain reports \
             Processing=false, got {}",
            accepted.summary()
        );
        assert!(
            accepted.body["UpgradeId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the id of the upgrade that was started, got {}",
            accepted.summary()
        );
        assert_eq!(
            accepted.body["TargetVersion"].as_str(),
            Some(target.as_str()),
            "case {case:?}: expected the requested target echoed, got {}",
            accepted.summary()
        );

        // 4. The run advances on the service's own clock. One row waits the way the
        //    contract says a client may — polling the status alone — and the other never
        //    reads the status until the run is over, so the step that single read reports is
        //    one nothing polling it could have advanced.
        let terminal = if row.wait_on_history {
            poll_history_until_terminal(&router, case, row.domain).await;
            upgrade_status(&router, case, row.domain).await
        } else {
            let deadline = Instant::now() + SETTLE_DEADLINE;
            loop {
                let read = upgrade_status(&router, case, row.domain).await;
                let step = read.body["UpgradeStep"].as_str().unwrap_or_default();
                let status = read.body["StepStatus"].as_str().unwrap_or_default();
                if step == UPGRADE_STEP_UPGRADE && TERMINAL_STEP_STATUS.contains(&status) {
                    break read;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: the upgrade never reached {UPGRADE_STEP_UPGRADE} with a \
                     terminal step status, last read {}",
                    read.summary()
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        };
        assert_eq!(
            terminal.body["UpgradeStep"].as_str(),
            Some(UPGRADE_STEP_UPGRADE),
            "case {case:?}: expected the run to be reported on its last step, got {}",
            terminal.summary()
        );
        // Nothing can make an accepted upgrade degrade here, so the terminal status a client
        // stops on is the unqualified one.
        assert_eq!(
            terminal.body["StepStatus"].as_str(),
            Some(STEP_STATUS_SUCCEEDED),
            "case {case:?}: expected the run to finish without issues, got {}",
            terminal.summary()
        );
        assert!(
            terminal.body["UpgradeName"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
            "case {case:?}: expected a description of the run beside its step, got {}",
            terminal.summary()
        );

        // 5. The history reports the same run, terminal, with the steps it performed.
        let history = upgrade_history(&router, case, row.domain).await;
        let newest = newest_entry(&history);
        let newest_status = newest["UpgradeStatus"].as_str().unwrap_or_default();
        assert!(
            TERMINAL_STEP_STATUS.contains(&newest_status),
            "case {case:?}: expected the newest history entry terminal, got {}",
            history.summary()
        );
        assert!(
            newest["StepsList"]
                .as_array()
                .is_some_and(|steps| !steps.is_empty()),
            "case {case:?}: expected the newest history entry to list the steps performed, \
             got {}",
            history.summary()
        );
        assert_eq!(
            newest["UpgradeName"].as_str(),
            terminal.body["UpgradeName"].as_str(),
            "case {case:?}: expected the history and the status to name one run alike, got {} \
             and {}",
            history.summary(),
            terminal.summary()
        );

        // 6. The domain runs what was asked for, with nothing left in flight — and what the
        //    sequence created on it before the upgrade is still there afterwards.
        let after = describe(&router, row.domain).await;
        assert_eq!(
            after.body["DomainStatus"]["EngineVersion"].as_str(),
            Some(target.as_str()),
            "case {case:?}: expected the domain to report the version it was upgraded to, \
             got {}",
            after.summary()
        );
        assert_settled_whole(case, &after.body["DomainStatus"]);
        for index in index_rows() {
            let kept = get_index(&router, row.domain, index.index).await;
            assert_eq!(
                kept.status, 200,
                "case {case:?}: expected {:?} to outlive the upgrade, got {}",
                index.index,
                kept.summary()
            );
            assert_eq!(
                kept.body["IndexSchema"][index.index], index.schema,
                "case {case:?}: expected {:?} to carry the schema it was created with after \
                 the upgrade, got {}",
                index.index,
                kept.summary()
            );
        }
    }
}
