// SPDX-License-Identifier: CC0-1.0

//! Two cross-service rules, asserted on the ASSEMBLED router — the arrangement a client
//! (and the behavioural corpus) actually talks to, where every service is mounted on one
//! engine rather than hand-picked two or three at a time.
//!
//! Rule 1: tagging is a cross-service INDEX. A tag written through the service that owns
//! a resource is, from that moment, what the Resource Groups Tagging API reports for that
//! resource's ARN — `GetResources` returns a `ResourceTagMapping` for it carrying exactly
//! the owning service's own tag map, on every query route (no filter, a `TagFilters` key,
//! a key and value, a `ResourceTypeFilters` naming the owner's namespace or one of its
//! resource types, and both filters at once) — and a tag the owning service REMOVED is out
//! of that index just as immediately, with an untagged resource reported by nothing
//! ("GetResources does not return untagged resources", and
//! `GetResourcesInput$ResourceTypeFilters`: "All tagged resources, whether the tagging
//! used the Resource Groups Tagging API or not, are returned by the Get* operation").
//!
//! Rule 2: a service that resolves an identifier against ANOTHER service's published data
//! plane gets its answer from that plane's own interactions. A FHIR data store's
//! advertised endpoint (`DatastoreProperties$DatastoreEndpoint`) serves the type-level
//! `search` interaction — `GET [base]/{type}?<param>=<value>` answers 200 and a Bundle of
//! `type` `searchset` carrying one `entry` per resource of that type satisfying the
//! criterion, `total` matches, and an empty result rather than a failure when nothing
//! does — so the patient a `StartPatientInsightsJob` names is ACCEPTED when that store
//! holds it under either kind of identifier (the resource's logical id or the business
//! identifier it carries), and REFUSED with the modelled 400 when it does not, while an
//! endpoint naming no data store at all is refused as that instead.
//!
//! Each rule is asserted over a TABLE whose rows differ along what it generalises over:
//! for rule 1 how the resource came to carry its tags (its own create, and a later
//! `TagResource`), how many tags it carries, which query route selects it and which tag
//! write removes it; for rule 2 the identifier the patient is found by, the resource type
//! and criterion a search is addressed with, how many resources match, and the way a
//! resolution can fail. The two together are the full sequence a tag-discovery client and
//! a patient-insights client run, which is why they live here rather than in either
//! service's crate: neither can be driven from one service alone.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so each test builds it
//! — and drives its requests — on a sized thread.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_connecthealth_connecthealth::service::error_detail::REQUEST_VALIDATION_FAILED_MESSAGE;
use service_com_amazonaws_connecthealth_connecthealth::state::{
    patient_insights_job_arn, DATASTORE_NOT_FOUND_MESSAGE, PATIENT_NOT_IN_DATASTORE_MESSAGE,
};

/// Keep this at or above `boot_serve.rs`'s stack: assembling the whole router is what
/// every test below does first.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and therefore the one every endpoint
/// serves and the one the ARNs minted here belong to.
const REGION: &str = "eu-west-1";

/// The SigV4 signing name the Resource Groups Tagging API is routed on.
const TAGGING: &str = "tagging";

/// The SigV4 signing name connecthealth is routed on — its `endpointPrefix` /
/// `arnNamespace`, which is also the service field of every ARN it mints and the token a
/// `ResourceTypeFilters` entry names its resources by.
const HEALTH_AGENT: &str = "health-agent";

/// The SigV4 signing name HealthLake is routed on, which is also the one its data stores'
/// FHIR data plane is served over.
const HEALTHLAKE: &str = "healthlake";

/// The media type a FHIR JSON representation travels as (FHIR R4 §2.2).
const FHIR_JSON: &str = "application/fhir+json";

/// How long a poll keeps LOOKING for a condition before it gives up: many times over the
/// spans this emulator's own lifecycles take, so a loaded host has room while a fast one
/// never waits at all.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// A data store id nothing was ever created under, for the row whose endpoint names no
/// data store: `DatastoreId` is a 32-character hexadecimal token.
const UNHELD_DATASTORE_ID: &str = "0123456789abcdef0123456789abcdef";

/// What one call answered: the HTTP status and the parsed JSON body (`Value::Null` for a
/// body that is empty or not JSON).
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code, from either envelope this suite drives (`__type` for awsJson
    /// and restJson1 alike), without its namespace.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split('#')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    /// The error message, whichever casing the envelope carries it under.
    fn error_message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A string member the answer must carry.
    fn string_at(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("member {member} must be a string in {:?}", self.body)
            })
            .to_string()
    }
}

/// The authority of an absolute URL, so a request addressed to an endpoint a service
/// ADVERTISED carries that endpoint's own host.
fn authority_of(uri: &str) -> Option<String> {
    let rest = uri.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    match authority.is_empty() {
        true => None,
        false => Some(authority.to_string()),
    }
}

/// Dispatch one request addressed to `signing_name` in [`REGION`], the way a signed SDK
/// call is: the credential scope is what names the service, and `uri` may be either a path
/// or the absolute URL a service advertised.
fn send(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = authority_of(uri)
        .unwrap_or_else(|| format!("{signing_name}.{REGION}.amazonaws.com"));
    let addressed = match uri.contains("://") {
        true => uri.to_string(),
        false => format!("https://{host}{uri}"),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(addressed)
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = match bytes.is_empty() {
        true => Value::Null,
        false => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    };
    Answer { status, body }
}

/// One Resource Groups Tagging API operation, over its awsJson wire.
fn tagging(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Answer {
    send(
        router,
        runtime,
        TAGGING,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
            ),
        ],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
}

/// One connecthealth operation, over the restJson1 routes that model publishes.
fn connecthealth(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body
        .map(|body| serde_json::to_vec(&body).expect("a serializable body"))
        .unwrap_or_default();
    send(
        router,
        runtime,
        HEALTH_AGENT,
        method,
        path,
        &[("content-type", "application/json")],
        payload,
    )
}

/// One HealthLake control-plane operation, over its awsJson1_0 wire.
fn healthlake(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Answer {
    send(
        router,
        runtime,
        HEALTHLAKE,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("HealthLake.{operation}")),
        ],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
}

/// One FHIR interaction against a data store's own endpoint.
fn fhir(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body
        .map(|body| serde_json::to_vec(body).expect("a serializable resource"))
        .unwrap_or_default();
    send(
        router,
        runtime,
        HEALTHLAKE,
        method,
        uri,
        &[("content-type", FHIR_JSON)],
        payload,
    )
}

/// `/tags/{resourceArn}`, connecthealth's tagging route, with the ARN percent-encoded into
/// its single non-greedy label exactly as an SDK encodes it.
fn tags_path(arn: &str) -> String {
    format!("/tags/{}", arn.replace(':', "%3A").replace('/', "%2F"))
}

/// A Domain created through connecthealth's own `CreateDomain`, carrying `tags` when the
/// row asks for them: `(domainId, domain ARN)`.
fn domain(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    tags: Value,
) -> (String, String) {
    let mut body = json!({ "name": name });
    if !tags.is_null() {
        body["tags"] = tags;
    }
    let created = connecthealth(router, runtime, "POST", "/domain", Some(body));
    assert_eq!(
        created.status, 201,
        "setup: CreateDomain({name:?}) must answer 201, got {} with {:?}",
        created.status, created.body
    );
    (created.string_at("domainId"), created.string_at("arn"))
}

/// The tag map connecthealth's own `ListTagsForResource` reports for one ARN — the set the
/// index must agree with, because the owning service is where that set lives.
fn owner_tags(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    arn: &str,
) -> BTreeMap<String, String> {
    let listed = connecthealth(router, runtime, "GET", &tags_path(arn), None);
    assert_eq!(
        listed.status, 200,
        "ListTagsForResource({arn}) must answer 200, got {} with {:?}",
        listed.status, listed.body
    );
    listed.body["tags"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `GetResources` with the filters given, asserting the modelled success and handing back
/// the page.
fn get_resources(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    filters: Value,
) -> Value {
    let page = tagging(router, runtime, "GetResources", filters);
    assert_eq!(
        page.status, 200,
        "case {case:?}: GetResources must answer 200, got {} with {:?}",
        page.status, page.body
    );
    page.body
}

/// The ARNs a `GetResources` page carries.
fn page_arns(case: &str, page: &Value) -> BTreeSet<String> {
    page["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a page with no ResourceTagMappingList: {page}"))
        .iter()
        .map(|mapping| {
            mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a mapping with no ResourceARN: {mapping}"))
                .to_string()
        })
        .collect()
}

/// The tag map a `GetResources` page reports for one ARN.
fn page_tags(case: &str, page: &Value, arn: &str) -> BTreeMap<String, String> {
    let mapping = page["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a page with no ResourceTagMappingList: {page}"))
        .iter()
        .find(|mapping| mapping["ResourceARN"] == json!(arn))
        .unwrap_or_else(|| panic!("case {case:?}: {arn} is not in the page: {page}"));
    mapping["Tags"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a mapping with no Tags: {mapping}"))
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_domain_tagged_through_its_owning_service_is_in_the_cross_service_tag_index() {
    std::thread::Builder::new()
        .name("connecthealth-tag-index".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // Three resources of the owning service, differing in how they came to carry
            // tags and in how many they carry: one tagged by its own create with two tags,
            // one tagged after the fact through that service's `TagResource` with one, and
            // one carrying none at all.
            let (_, at_create) = domain(
                &router,
                &runtime,
                "clinic-tagged-at-create",
                json!({ "probe": "alpha", "team": "cardiology" }),
            );
            let (_, after_create) = domain(&router, &runtime, "clinic-tagged-later", Value::Null);
            let written = connecthealth(
                &router,
                &runtime,
                "POST",
                &tags_path(&after_create),
                Some(json!({ "tags": { "probe": "beta" } })),
            );
            assert_eq!(
                written.status, 204,
                "TagResource must answer 204, got {} with {:?}",
                written.status, written.body
            );
            let (_, untagged) = domain(&router, &runtime, "clinic-untagged", Value::Null);

            // Every query route a tag-discovery client uses, and what each must select.
            // `ResourceTypeFilters` takes "service[:resourceType]", so the bare namespace
            // and the qualified token are both rows.
            let routes: Vec<(&str, Value, BTreeSet<String>)> = vec![
                (
                    "no filter at all",
                    json!({}),
                    [at_create.clone(), after_create.clone()].into_iter().collect(),
                ),
                (
                    "a tag filter by key and value",
                    json!({ "TagFilters": [{ "Key": "probe", "Values": ["alpha"] }] }),
                    [at_create.clone()].into_iter().collect(),
                ),
                (
                    "a tag filter by key alone",
                    json!({ "TagFilters": [{ "Key": "probe" }] }),
                    [at_create.clone(), after_create.clone()].into_iter().collect(),
                ),
                (
                    "the bare service namespace as the resource type",
                    json!({ "ResourceTypeFilters": [HEALTH_AGENT] }),
                    [at_create.clone(), after_create.clone()].into_iter().collect(),
                ),
                (
                    "the qualified resource type",
                    json!({ "ResourceTypeFilters": [format!("{HEALTH_AGENT}:domain")] }),
                    [at_create.clone(), after_create.clone()].into_iter().collect(),
                ),
                (
                    "both filters at once",
                    json!({
                        "TagFilters": [{ "Key": "probe", "Values": ["beta"] }],
                        "ResourceTypeFilters": [HEALTH_AGENT],
                    }),
                    [after_create.clone()].into_iter().collect(),
                ),
                (
                    "a tag value nothing carries",
                    json!({ "TagFilters": [{ "Key": "probe", "Values": ["nowhere"] }] }),
                    BTreeSet::new(),
                ),
            ];

            for (case, filters, expected) in &routes {
                let page = get_resources(&router, &runtime, case, filters.clone());
                let selected = page_arns(case, &page);
                for arn in expected {
                    assert!(
                        selected.contains(arn),
                        "case {case:?}: {arn} must be selected, got {selected:?} in {page}"
                    );
                }
                // The resources this route must NOT select: the ones it does not name, and
                // the untagged Domain, which no route selects.
                for arn in [&at_create, &after_create, &untagged] {
                    if !expected.contains(arn) {
                        assert!(
                            !selected.contains(arn),
                            "case {case:?}: {arn} must not be selected, got {selected:?} in \
                             {page}"
                        );
                    }
                }
            }

            // And what the index REPORTS for each resource is the owning service's own tag
            // map, read back through that service's own reader — not a second copy.
            let whole = get_resources(&router, &runtime, "the whole index", json!({}));
            for (case, arn) in [
                ("a Domain tagged by its create", &at_create),
                ("a Domain tagged after the fact", &after_create),
            ] {
                let indexed = page_tags(case, &whole, arn);
                let held = owner_tags(&router, &runtime, arn);
                assert_eq!(
                    indexed, held,
                    "case {case:?}: the index must report the owning service's own tags, \
                     got {indexed:?} against {held:?}"
                );
            }

            // The other half of the rule: a tag the owning service REMOVED leaves the
            // index. One row per removal — one key of several, and the last key a resource
            // carries, which takes it out of the index entirely.
            let removed = connecthealth(
                &router,
                &runtime,
                "DELETE",
                &format!("{}?tagKeys=probe", tags_path(&at_create)),
                None,
            );
            assert_eq!(
                removed.status, 204,
                "UntagResource must answer 204, got {} with {:?}",
                removed.status, removed.body
            );
            let page = get_resources(
                &router,
                &runtime,
                "after one key of several was removed",
                json!({
                    "TagFilters": [{ "Key": "probe", "Values": ["alpha"] }],
                    "ResourceTypeFilters": [HEALTH_AGENT],
                }),
            );
            let selected = page_arns("after one key of several was removed", &page);
            assert!(
                !selected.contains(&at_create),
                "the removed key must select nothing, got {selected:?} in {page}"
            );
            let page = get_resources(
                &router,
                &runtime,
                "the key it still carries",
                json!({ "TagFilters": [{ "Key": "team" }] }),
            );
            assert!(
                page_arns("the key it still carries", &page).contains(&at_create),
                "the key it still carries must select it, got {page}"
            );

            let removed = connecthealth(
                &router,
                &runtime,
                "DELETE",
                &format!("{}?tagKeys=probe", tags_path(&after_create)),
                None,
            );
            assert_eq!(
                removed.status, 204,
                "UntagResource must answer 204, got {} with {:?}",
                removed.status, removed.body
            );
            // Asserted twice, so the absence is a property of the index rather than of one
            // read.
            for attempt in ["the first read", "the second read"] {
                let page = get_resources(
                    &router,
                    &runtime,
                    attempt,
                    json!({
                        "TagFilters": [{ "Key": "probe", "Values": ["beta"] }],
                        "ResourceTypeFilters": [HEALTH_AGENT],
                    }),
                );
                assert!(
                    !page_arns(attempt, &page).contains(&after_create),
                    "{attempt}: an untagged resource must not be in the index, got {page}"
                );
            }
        })
        .expect("spawn the tag-index thread")
        .join()
        .expect("a tag the owning service wrote must be in the cross-service tag index");
}

/// A minimal FHIR `Patient` carrying `identifier` as its business identifier.
fn patient_resource(identifier: &str) -> Value {
    json!({
        "resourceType": "Patient",
        "identifier": [ { "system": "urn:emulator:mrn", "value": identifier } ],
    })
}

/// A `StartPatientInsightsJob` body naming `patient_id`, resolved against `endpoint`, with
/// its output written under `s3_output_path`.
fn job_body(patient_id: &str, endpoint: &str, s3_output_path: &str) -> Value {
    json!({
        "patientContext": { "patientId": patient_id },
        "insightsContext": { "insightsType": "PRE_VISIT" },
        "encounterContext": { "encounterReason": "Annual checkup" },
        "userContext": { "role": "CLINICIAN", "userId": "clinician-0001" },
        "inputDataConfig": { "fhirServer": { "fhirEndpoint": endpoint } },
        "outputDataConfig": { "s3OutputPath": s3_output_path },
    })
}

/// The logical ids the entries of a searchset Bundle carry.
fn matched_ids(case: &str, bundle: &Value) -> BTreeSet<String> {
    match bundle.get("entry") {
        None => BTreeSet::new(),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| {
                entry["resource"]["id"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every entry carries a resource with an id, got {entry}")
                    })
                    .to_string()
            })
            .collect(),
        Some(other) => panic!("case {case:?}: entry must be a list, got {other}"),
    }
}

#[test]
fn a_datastores_search_resolves_the_patient_an_insights_job_names() {
    std::thread::Builder::new()
        .name("connecthealth-fhir-resolution".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // A data store, and the endpoint it ADVERTISES for its FHIR data plane: every
            // interaction below is addressed to exactly that URL, as a client handed it
            // sends them. The CONDITION is what is waited for, never an interval.
            let created = healthlake(
                &router,
                &runtime,
                "CreateFHIRDatastore",
                json!({ "DatastoreTypeVersion": "R4", "DatastoreName": "clinic-insights" }),
            );
            assert_eq!(
                created.status, 200,
                "CreateFHIRDatastore must answer 200, got {} with {:?}",
                created.status, created.body
            );
            let datastore_id = created.string_at("DatastoreId");
            let deadline = Instant::now() + POLL_DEADLINE;
            let endpoint = loop {
                let described = healthlake(
                    &router,
                    &runtime,
                    "DescribeFHIRDatastore",
                    json!({ "DatastoreId": datastore_id }),
                );
                assert_eq!(
                    described.status, 200,
                    "DescribeFHIRDatastore must answer 200, got {} with {:?}",
                    described.status, described.body
                );
                let properties = &described.body["DatastoreProperties"];
                if properties["DatastoreStatus"].as_str() == Some("ACTIVE") {
                    break properties["DatastoreEndpoint"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("an ACTIVE data store reports an endpoint, got {properties}")
                        })
                        .to_string();
                }
                assert!(
                    Instant::now() < deadline,
                    "the data store was still not serving after {POLL_DEADLINE:?}, last read \
                     {properties}"
                );
                std::thread::yield_now();
            };
            let base = endpoint.trim_end_matches('/').to_string();

            // Two Patients, differing in the form of their logical id and in the business
            // identifier each carries, so a search has more than one resource to choose
            // between.
            let seeded: Vec<(&str, &str)> =
                vec![("patient-alpha", "mrn-alpha"), ("patient.beta", "mrn-beta")];
            for (logical_id, identifier) in &seeded {
                let stored = fhir(
                    &router,
                    &runtime,
                    "PUT",
                    &format!("{base}/Patient/{logical_id}"),
                    Some(&patient_resource(identifier)),
                );
                assert!(
                    stored.status == 200 || stored.status == 201,
                    "seeding Patient/{logical_id} must be accepted, got {} with {:?}",
                    stored.status,
                    stored.body
                );
                let read = fhir(
                    &router,
                    &runtime,
                    "GET",
                    &format!("{base}/Patient/{logical_id}"),
                    None,
                );
                assert_eq!(
                    read.status, 200,
                    "the seeded Patient/{logical_id} must be readable, got {} with {:?}",
                    read.status, read.body
                );
            }

            // The type-level search interaction itself, over a table of criteria: the
            // business identifier, the logical id, a value nothing carries, and another
            // resource type — one row per way a criterion can select none, one or several.
            let searches: Vec<(&str, String, BTreeSet<String>)> = vec![
                (
                    "a business identifier one Patient carries",
                    format!("{base}/Patient?identifier=mrn-alpha"),
                    ["patient-alpha".to_string()].into_iter().collect(),
                ),
                (
                    "the other Patient's business identifier",
                    format!("{base}/Patient?identifier=mrn-beta"),
                    ["patient.beta".to_string()].into_iter().collect(),
                ),
                (
                    "a logical id",
                    format!("{base}/Patient?_id=patient-alpha"),
                    ["patient-alpha".to_string()].into_iter().collect(),
                ),
                (
                    "no criterion at all selects every Patient held",
                    format!("{base}/Patient"),
                    ["patient-alpha".to_string(), "patient.beta".to_string()]
                        .into_iter()
                        .collect(),
                ),
                (
                    "an identifier nothing carries",
                    format!("{base}/Patient?identifier=mrn-nobody"),
                    BTreeSet::new(),
                ),
                (
                    "a resource type the data store holds nothing of",
                    format!("{base}/Observation?identifier=mrn-alpha"),
                    BTreeSet::new(),
                ),
            ];

            for (case, uri, expected) in &searches {
                let answer = fhir(&router, &runtime, "GET", uri, None);
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: a search answers 200 whether or not anything matched, \
                     got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    (
                        answer.body["resourceType"].as_str(),
                        answer.body["type"].as_str()
                    ),
                    (Some("Bundle"), Some("searchset")),
                    "case {case:?}: a search answers a searchset Bundle, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.body["total"].as_u64(),
                    Some(expected.len() as u64),
                    "case {case:?}: total states how many matched, got {:?}",
                    answer.body
                );
                let matched = matched_ids(case, &answer.body);
                assert_eq!(
                    &matched, expected,
                    "case {case:?}: expected the matches {expected:?}, got {matched:?} in \
                     {:?}",
                    answer.body
                );
            }

            // A domain to start jobs in, and the endpoint of a data store nothing holds,
            // for the row whose FHIR server resolves to nothing.
            let (domain_id, domain_arn) = domain(&router, &runtime, "clinic-insights", Value::Null);
            let unheld_endpoint = endpoint.replace(&datastore_id, UNHELD_DATASTORE_ID);
            assert_ne!(
                unheld_endpoint, endpoint,
                "the unheld endpoint must differ from the data store's own, got \
                 {unheld_endpoint}"
            );

            // One row per outcome a start can have once the patient it names is resolved
            // against that server: accepted under either kind of identifier, refused for a
            // patient the store does not hold, refused differently for an endpoint naming
            // no store, and refused on its own output location before any of that.
            let accepted: Vec<(&str, String)> = vec![
                (
                    "the business identifier the record carries",
                    "mrn-alpha".to_string(),
                ),
                (
                    "the other patient's business identifier",
                    "mrn-beta".to_string(),
                ),
                ("the resource's own logical id", "patient-alpha".to_string()),
                (
                    "a logical id with the characters FHIR allows in one",
                    "patient.beta".to_string(),
                ),
            ];
            for (case, patient_id) in &accepted {
                let started = connecthealth(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/domain/{domain_id}/patient-insights-job"),
                    Some(job_body(
                        patient_id,
                        &endpoint,
                        "s3://clinic-output/insights/",
                    )),
                );
                assert_eq!(
                    started.status, 200,
                    "case {case:?}: a patient the data store holds must be accepted, got {} \
                     with {:?}",
                    started.status, started.body
                );
                // Round trip: the job is readable back through its own reader, which
                // reports the patient it was started for out of the stored record.
                let job_id = started.string_at("jobId");
                let fetched = connecthealth(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/domain/{domain_id}/patient-insights-job/{job_id}"),
                    None,
                );
                assert_eq!(
                    (
                        fetched.status,
                        fetched.body["jobId"].as_str(),
                        fetched.body["patientContext"]["patientId"].as_str()
                    ),
                    (200, Some(job_id.as_str()), Some(patient_id.as_str())),
                    "case {case:?}: the started job must be readable, got {} with {:?}",
                    fetched.status,
                    fetched.body
                );
            }

            // A second start presenting a `clientToken` already used is a start of its
            // own: `@idempotencyToken` is a rule about how an SDK FILLS the member, not a
            // promise that this service deduplicates on it, so both jobs exist and each
            // reports the patient it was started for.
            let mut minted: BTreeSet<String> = BTreeSet::new();
            for (case, patient_id) in [
                ("the first start carrying the token", "mrn-alpha"),
                ("a second start carrying the same token", "mrn-beta"),
            ] {
                let mut body = job_body(patient_id, &endpoint, "s3://clinic-output/insights/");
                body["clientToken"] = json!("token-reused");
                let started = connecthealth(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/domain/{domain_id}/patient-insights-job"),
                    Some(body),
                );
                assert_eq!(
                    started.status, 200,
                    "case {case:?}: the start must be accepted, got {} with {:?}",
                    started.status, started.body
                );
                let job_id = started.string_at("jobId");
                assert!(
                    minted.insert(job_id.clone()),
                    "case {case:?}: the start must mint a job of its own, got {job_id} again"
                );
                let fetched = connecthealth(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/domain/{domain_id}/patient-insights-job/{job_id}"),
                    None,
                );
                assert_eq!(
                    (
                        fetched.status,
                        fetched.body["patientContext"]["patientId"].as_str()
                    ),
                    (200, Some(patient_id)),
                    "case {case:?}: each job must report its own patient, got {} with {:?}",
                    fetched.status,
                    fetched.body
                );
            }

            let refused: Vec<(&str, String, String, &str)> = vec![
                (
                    "a well-formed id the data store holds no patient under",
                    "mrn-nobody".to_string(),
                    endpoint.clone(),
                    PATIENT_NOT_IN_DATASTORE_MESSAGE,
                ),
                (
                    "a logical id nothing was stored at",
                    "patient-nobody".to_string(),
                    endpoint.clone(),
                    PATIENT_NOT_IN_DATASTORE_MESSAGE,
                ),
                (
                    "an endpoint that names no data store",
                    "mrn-alpha".to_string(),
                    unheld_endpoint.clone(),
                    DATASTORE_NOT_FOUND_MESSAGE,
                ),
            ];
            for (case, patient_id, server, message) in &refused {
                // Twice: a refused start binds nothing, so the retry is refused in exactly
                // the same words rather than answered with a job.
                for attempt in ["the first attempt", "the retry"] {
                    let answer = connecthealth(
                        &router,
                        &runtime,
                        "POST",
                        &format!("/domain/{domain_id}/patient-insights-job"),
                        Some(job_body(patient_id, server, "s3://clinic-output/insights/")),
                    );
                    assert_eq!(
                        (answer.status, answer.error_code().as_str()),
                        (400, "ValidationException"),
                        "case {case:?} ({attempt}): expected 400 ValidationException, got {} \
                         with {:?}",
                        answer.status,
                        answer.body
                    );
                    assert_eq!(
                        answer.error_message(),
                        *message,
                        "case {case:?} ({attempt}): expected {message:?}, got {:?}",
                        answer.body
                    );
                }
            }

            // The request's own values are judged before the patient is resolved, so an
            // output location outside the rules its shape declares is refused with this
            // endpoint's request-validation sentence even for a patient that does resolve.
            let answer = connecthealth(
                &router,
                &runtime,
                "POST",
                &format!("/domain/{domain_id}/patient-insights-job"),
                Some(job_body("mrn-alpha", &endpoint, "not-an-s3-uri")),
            );
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (400, "ValidationException"),
                "an output location outside its shape must be refused, got {} with {:?}",
                answer.status,
                answer.body
            );
            assert_eq!(
                answer.error_message(),
                REQUEST_VALIDATION_FAILED_MESSAGE,
                "expected the endpoint's request-validation sentence, got {:?}",
                answer.body
            );

            // And a jobId that resolves to no job under a domain that DOES resolve stays
            // the modelled not-found, naming the job by the ARN it would be held under.
            for (case, requested) in [
                ("a jobId nothing minted", "job-nobody"),
                ("a one-character jobId", "x"),
            ] {
                let answer = connecthealth(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/domain/{domain_id}/patient-insights-job/{requested}"),
                    None,
                );
                assert_eq!(
                    (answer.status, answer.error_code().as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: expected 404 ResourceNotFoundException, got {} with {:?}",
                    answer.status,
                    answer.body
                );
                let expected = format!(
                    "Job {} does not exist.",
                    patient_insights_job_arn(&domain_arn, requested)
                );
                assert_eq!(
                    answer.error_message(),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    answer.body
                );
            }
        })
        .expect("spawn the FHIR resolution thread")
        .join()
        .expect("a data store's search must resolve the patient an insights job names");
}
