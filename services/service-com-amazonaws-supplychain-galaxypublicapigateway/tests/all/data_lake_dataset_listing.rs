// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of `ListDataLakeDatasets`, driven over the wire (restJson1,
//! signing name `scn`) through the same `MultiServiceRouter` the emulator's front door
//! uses — so every assertion here is about what a real client observes, not about a
//! handler's internals.
//!
//! RULE under test: the listing reports exactly the data-lake datasets the store holds for
//! the addressed instance and namespace — every member of each one, as
//! `GetDataLakeDataset` reports it — in one stable name order, cut into pages of at most
//! `maxResults` with an opaque `nextToken` that resumes where the page stopped; an
//! `instanceId` naming no instance of this account is an authorization failure, a
//! namespace this data lake does not hold is a not-found, a `nextToken` the service did
//! not issue for this query is a validation failure, and the listing changes nothing a
//! later caller sees.
//!
//! Every dataset a case lists back was created through `CreateDataLakeDataset`, so the
//! listing is pinned against persisted state rather than against anything a test wrote
//! into the store directly.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    service_state, PREDEFINED_NAMESPACES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for.
const REGION: &str = "us-east-1";

/// The published bounds of `DataLakeDatasetMaxResults` (`@range(min: 0, max: 20)`).
const MAX_RESULTS_MIN: i64 = 0;
const MAX_RESULTS_MAX: i64 = 20;

/// The `@default` the model gives `maxResults`, applied when a request omits it.
const MAX_RESULTS_DEFAULT: usize = 10;

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can count the datasets the
    /// store holds the way a handler reads them.
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("scn.{REGION}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A live instance of this account, created through `CreateInstance`.
    async fn given_instance(&self, name: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/instance", Some(json!({ "instanceName": name })))
            .await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// A custom namespace of `instance_id`, created through `CreateDataLakeNamespace`.
    async fn given_namespace(&self, instance_id: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
                Some(json!({ "description": "a namespace a case lists datasets of" })),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeNamespace {name} expected 200, got {status} with {body}"
        );
    }

    /// One dataset of `(instance_id, namespace, name)`, created through
    /// `CreateDataLakeDataset` with `request` as its body.
    async fn given_dataset(&self, instance_id: &str, namespace: &str, name: &str, request: Value) {
        let (status, body) = self
            .send("PUT", &dataset_path(instance_id, namespace, name), Some(request))
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeDataset {namespace}/{name} expected 200, got {status} with {body}"
        );
    }

    /// `GetDataLakeDataset` for one triple.
    async fn get_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send("GET", &dataset_path(instance_id, namespace, name), None)
            .await
    }

    /// `ListDataLakeDatasets` for one namespace, with `query` appended verbatim.
    async fn list_datasets(&self, instance_id: &str, namespace: &str, query: &str) -> (u16, Value) {
        let path =
            format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets");
        let path = if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        };
        self.send("GET", &path, None).await
    }

    /// How many datasets the store holds for `instance_id`, across its namespaces.
    fn stored_datasets(&self, instance_id: &str) -> usize {
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .dataset_count(instance_id)
    }
}

/// The URI every single-dataset operation is addressed at, as the model's `http` trait
/// spells it.
fn dataset_path(instance_id: &str, namespace: &str, name: &str) -> String {
    format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}")
}

/// A one-field schema, the smallest the model allows (`fields` is `@length` 1..500).
fn minimal_schema() -> Value {
    json!({
        "name": "MinimalSchema",
        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
    })
}

/// A create body carrying every optional member of `CreateDataLakeDatasetRequest`, so a
/// listed dataset is compared on a fully populated record rather than a bare one.
fn fully_populated_request() -> Value {
    json!({
        "schema": {
            "name": "PopulatedSchema",
            "fields": [
                { "name": "id", "type": "STRING", "isRequired": true },
                { "name": "quantity", "type": "DOUBLE", "isRequired": false },
            ],
            "primaryKeys": [{ "name": "id" }],
        },
        "description": "a dataset carrying every optional member",
        "partitionSpec": { "fields": [{ "name": "id", "transform": { "type": "IDENTITY" } }] },
        "tags": { "team": "supply" },
    })
}

/// The `datasets` member of a success body, as a list.
fn datasets_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("datasets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carries no `datasets` list: {body}"))
}

/// The `name` of every entry of a success body's `datasets`, in the order answered.
fn listed_names(case: &str, body: &Value) -> Vec<String> {
    datasets_of(case, body)
        .iter()
        .map(|dataset| {
            dataset
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: listed dataset carries no `name`: {dataset}")
                })
                .to_string()
        })
        .collect()
}

/// The `nextToken` a success body carries, if any.
fn next_token(body: &Value) -> Option<String> {
    body.get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The error code of a refusal envelope (restJson1 renders it in `__type`).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no `__type`: {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a refusal envelope.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no message: {body}"))
        .to_string()
}

/// How the gateway refuses a call whose `instanceId` names no instance of this account.
fn expected_authorization_failure(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {}",
        engine::emulator_account_id()
    )
}

/// A distinct dataset name of the modelled `DataLakeDatasetName` shape (`^[a-z0-9_]+$`),
/// built so that ascending `index` is ascending name.
fn dataset_name(index: usize) -> String {
    format!("dataset_{index:03}")
}

/// Walk every page of one listing and return the names answered, in order, together with
/// how many pages it took.
async fn walk_pages(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    namespace: &str,
    max_results: Option<i64>,
) -> (Vec<String>, usize) {
    let limit = match max_results {
        Some(size) => usize::try_from(size).expect("a page size a case asks for is not negative"),
        None => MAX_RESULTS_DEFAULT,
    };
    let mut names = Vec::new();
    let mut pages = 0;
    let mut token: Option<String> = None;
    // A listing of a bounded store terminates in a bounded number of pages; the cap is
    // here so a token that failed to advance fails the case instead of looping forever.
    while pages < 64 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(value) = &token {
            query.push(format!("nextToken={value}"));
        }
        let (status, body) = emulator
            .list_datasets(instance_id, namespace, &query.join("&"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {pages} expected 200, got {status} with {body}"
        );
        let page = listed_names(case, &body);
        assert!(
            page.len() <= limit,
            "case {case:?}: page {pages} holds {} datasets, more than maxResults {limit}",
            page.len()
        );
        pages += 1;
        token = next_token(&body);
        if token.is_some() {
            assert!(
                !page.is_empty(),
                "case {case:?}: page {pages} is empty yet carries a nextToken, so a client paging \
                 in a loop could never advance"
            );
        }
        names.extend(page);
        if token.is_none() {
            return (names, pages);
        }
    }
    panic!("case {case:?}: the listing never ran out of pages after {pages} pages");
}

/// RULE: the listing reports exactly the datasets the store holds for the addressed
/// instance and namespace, in name order, each one exactly as `GetDataLakeDataset` reports
/// it — and nothing of another namespace or another instance.
#[tokio::test]
async fn listing_reports_exactly_the_stored_datasets_of_the_addressed_namespace() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("listing-host").await;
    let other_instance = emulator.given_instance("other-listing-host").await;
    emulator.given_namespace(&instance, "logistics").await;
    emulator.given_namespace(&instance, "empty_ns").await;
    emulator.given_namespace(&other_instance, "logistics").await;

    // Rows differ along the namespace (both pre-defined ones — source:
    // `PREDEFINED_NAMESPACES`, the `namespace` member's own documentation — and a custom
    // one), along how many datasets it holds (none, one, several), and along the names,
    // which are deliberately created OUT of name order and span the ends of the modelled
    // `^[a-z0-9_]+$` / 1..75 shape.
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "the default namespace, several datasets created out of order",
            PREDEFINED_NAMESPACES[1],
            vec!["shipment_lines_2024", "a", "orders"],
        ),
        (
            "the asc namespace, one dataset",
            PREDEFINED_NAMESPACES[0],
            vec!["inbound_order"],
        ),
        (
            "a custom namespace, two datasets",
            "logistics",
            vec!["zz_last_1", "l0"],
        ),
        ("a custom namespace holding nothing", "empty_ns", vec![]),
    ];

    for (case, namespace, names) in &rows {
        for (index, name) in names.iter().enumerate() {
            // Alternate the request between the smallest body the model admits and one
            // carrying every optional member, so the listing is pinned on both.
            let request = if index % 2 == 0 {
                json!({ "schema": minimal_schema() })
            } else {
                fully_populated_request()
            };
            emulator
                .given_dataset(&instance, namespace, name, request)
                .await;
        }
        // The same names under another instance, so a listing that leaked across the
        // instance boundary would report them too.
        for name in names {
            emulator
                .given_dataset(
                    &other_instance,
                    namespace,
                    name,
                    json!({ "schema": minimal_schema() }),
                )
                .await;
        }
        let _ = case;
    }

    for (case, namespace, names) in &rows {
        let (status, body) = emulator
            .list_datasets(&instance, namespace, &format!("maxResults={MAX_RESULTS_MAX}"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );

        let mut expected: Vec<String> = names.iter().map(|name| name.to_string()).collect();
        expected.sort();
        let listed = listed_names(case, &body);
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the stored datasets in name order {expected:?}, got {listed:?}"
        );
        assert!(
            next_token(&body).is_none(),
            "case {case:?}: one page holds every dataset, so it must carry no nextToken: {body}"
        );

        // Every listed dataset is the record the single-dataset read reports, member for
        // member: the listing projects persisted state, not a summary of its own.
        for dataset in datasets_of(case, &body) {
            let name = dataset
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: listed dataset has no name: {dataset}"));
            let (status, read) = emulator.get_dataset(&instance, namespace, name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetDataLakeDataset {name} expected 200, got {status} with {read}"
            );
            let read_dataset = read.get("dataset").cloned().unwrap_or(Value::Null);
            assert_eq!(
                dataset, read_dataset,
                "case {case:?}: the listed {name} must equal what GetDataLakeDataset reports"
            );
        }
    }
}

/// RULE: a listing is cut into pages of at most `maxResults`, and the pages a client walks
/// with the tokens it is handed concatenate to the whole name order exactly once —
/// whatever the page size.
#[tokio::test]
async fn pagination_walks_the_whole_order_exactly_once_at_every_page_size() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("paging-host").await;
    emulator.given_namespace(&instance, "paging").await;

    // 24 datasets: more than the largest page (20) and more than the default (10), so
    // every row below needs at least two pages.
    const SEEDED: usize = 24;
    let mut all_names = Vec::new();
    for index in 1..=SEEDED {
        let name = dataset_name(index);
        emulator
            .given_dataset(&instance, "paging", &name, json!({ "schema": minimal_schema() }))
            .await;
        all_names.push(name);
    }
    all_names.sort();

    // Rows differ along the page size: the largest page the model publishes, sizes in
    // between (one of which divides the total exactly), the smallest page that can
    // advance, and the modelled default, which a row omits `maxResults` to get.
    let rows: Vec<(&str, Option<i64>)> = vec![
        ("one dataset per page", Some(1)),
        ("two datasets per page", Some(2)),
        ("eight datasets per page", Some(8)),
        ("seven datasets per page", Some(7)),
        ("the largest page", Some(MAX_RESULTS_MAX)),
        ("the modelled default page", None),
    ];

    for (case, max_results) in rows {
        let (names, pages) = walk_pages(&emulator, case, &instance, "paging", max_results).await;
        assert_eq!(
            names, all_names,
            "case {case:?}: the pages must concatenate to the whole name order exactly once"
        );
        let size = match max_results {
            Some(size) => usize::try_from(size).expect("a page size a row asks for is positive"),
            None => MAX_RESULTS_DEFAULT,
        };
        let expected_pages = all_names.len().div_ceil(size);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: {} datasets in pages of {size} expected {expected_pages} pages",
            all_names.len()
        );
    }
}

/// RULE: `maxResults` is served at each bound the model publishes and refused outside
/// them.
#[tokio::test]
async fn max_results_is_served_at_its_bounds_and_refused_outside_them() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("bounds-host").await;
    const SEEDED: usize = 3;
    for index in 1..=SEEDED {
        emulator
            .given_dataset(
                &instance,
                "default",
                &dataset_name(index),
                json!({ "schema": minimal_schema() }),
            )
            .await;
    }

    // Rows: both published bounds of `DataLakeDatasetMaxResults`, and one past each.
    let rows: Vec<(&str, i64, u16)> = vec![
        ("one below the minimum", MAX_RESULTS_MIN - 1, 400),
        ("the minimum", MAX_RESULTS_MIN, 200),
        ("the maximum", MAX_RESULTS_MAX, 200),
        ("one past the maximum", MAX_RESULTS_MAX + 1, 400),
    ];

    for (case, max_results, expected_status) in rows {
        let (status, body) = emulator
            .list_datasets(&instance, "default", &format!("maxResults={max_results}"))
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: maxResults {max_results} expected HTTP {expected_status}, got {status} \
             with {body}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(case, &body),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {body}"
            );
            continue;
        }
        let listed = listed_names(case, &body);
        let expected = usize::try_from(max_results)
            .expect("a served bound is not negative")
            .min(SEEDED);
        assert_eq!(
            listed.len(),
            expected,
            "case {case:?}: expected {expected} datasets on the page, got {listed:?}"
        );
        if expected == 0 {
            // A page that holds nothing carries no token: a token a client cannot advance
            // with would make it page forever.
            assert!(
                next_token(&body).is_none(),
                "case {case:?}: a page holding nothing must carry no nextToken: {body}"
            );
        }
    }
}

/// RULE: only a `nextToken` this service issued for THIS query is a page boundary;
/// anything else is a validation failure rather than a silently truncated or restarted
/// page.
#[tokio::test]
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("token-host").await;
    let other_instance = emulator.given_instance("other-token-host").await;
    emulator.given_namespace(&instance, "other_ns").await;
    for index in 1..=3 {
        for (instance_id, namespace) in [
            (&instance, "default"),
            (&instance, "other_ns"),
            (&other_instance, "default"),
        ] {
            emulator
                .given_dataset(
                    instance_id,
                    namespace,
                    &dataset_name(index),
                    json!({ "schema": minimal_schema() }),
                )
                .await;
        }
    }

    // The token this service hands out for a query is the control every forged row below
    // is a variation on: it names the instance, the namespace, and the dataset to resume
    // at.
    let issued = |instance_id: &str, namespace: &str, name: &str| {
        hex::encode(format!(
            "scn:1:data-lake-dataset:{instance_id}:{namespace}:{name}"
        ))
    };

    // Rows differ along HOW the value fails to be a token this service issued for this
    // query: not hex at all, hex of a foreign payload, hex of a plausible store key with
    // no stamp, the stamp over something no `DataLakeDatasetName` can be, a token issued
    // for another instance, and one issued for another namespace of this instance.
    let rows: Vec<(&str, String)> = vec![
        ("not hex", "not-a-token".to_string()),
        (
            "hex of a foreign payload",
            hex::encode("someone-elses-cursor"),
        ),
        ("hex of a bare store key", hex::encode(dataset_name(2))),
        (
            "the stamp over a name the model cannot admit",
            issued(&instance, "default", "Not-A-Dataset"),
        ),
        (
            "a token issued for another instance",
            issued(&other_instance, "default", &dataset_name(2)),
        ),
        (
            "a token issued for another namespace",
            issued(&instance, "other_ns", &dataset_name(2)),
        ),
    ];

    for (case, token) in rows {
        let (status, body) = emulator
            .list_datasets(&instance, "default", &format!("nextToken={token}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
    }

    // The control: the token the service itself hands out resumes the listing.
    let case = "the token the service issued";
    let (status, first) = emulator
        .list_datasets(&instance, "default", "maxResults=1")
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {first}"
    );
    let token = next_token(&first)
        .unwrap_or_else(|| panic!("case {case:?}: a truncated page must carry a nextToken"));
    let (status, second) = emulator
        .list_datasets(&instance, "default", &format!("maxResults=1&nextToken={token}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {second}"
    );
    assert_eq!(
        listed_names(case, &second),
        vec![dataset_name(2)],
        "case {case:?}: the token must resume at the dataset the first page stopped at"
    );

    // ...and that same token is refused once the query it was cut from changes.
    let case = "the issued token against another namespace";
    let (status, body) = emulator
        .list_datasets(
            &instance,
            "other_ns",
            &format!("maxResults=1&nextToken={token}"),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(case, &body),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {body}"
    );
}

/// RULE: a listing is answered only for a namespace of a live instance of this account —
/// an unknown instance as the gateway's authorization failure, a namespace the data lake
/// does not hold as a not-found — while a namespace that exists and holds nothing is a
/// successful empty page.
#[tokio::test]
async fn an_unknown_instance_or_namespace_is_refused_rather_than_answered_as_an_empty_page() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("refusal-host").await;

    // An `instanceId` of the modelled `UUID` shape that names no instance.
    let unknown_instance = "00000000-0000-4000-8000-000000000000";
    let case = "an instanceId naming no instance of this account";
    let (status, body) = emulator
        .list_datasets(unknown_instance, "default", "")
        .await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} with {body}"
    );
    assert_eq!(
        error_code(case, &body),
        "AccessDeniedException",
        "case {case:?}: expected the modelled AccessDeniedException, got {body}"
    );
    assert_eq!(
        error_message(case, &body),
        expected_authorization_failure(unknown_instance),
        "case {case:?}: the refusal must carry the gateway's authorization sentence"
    );

    // Rows differ along the namespace that does not exist: one never created, and one
    // whose name only resembles a pre-defined one.
    let rows: Vec<(&str, &str)> = vec![
        ("a namespace never created", "logistics"),
        ("a name resembling a pre-defined namespace", "default2"),
    ];
    for (case, namespace) in rows {
        let (status, body) = emulator.list_datasets(&instance, namespace, "").await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(case, &body),
            "Namespace not found.",
            "case {case:?}: the refusal must carry the service's not-found sentence"
        );
    }

    // Every namespace an instance comes with exists without anybody creating it, so it is
    // listed as an empty page rather than refused. Source: `PREDEFINED_NAMESPACES`.
    for namespace in PREDEFINED_NAMESPACES {
        let case = format!("the pre-defined namespace {namespace}, holding nothing");
        let (status, body) = emulator.list_datasets(&instance, namespace, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            listed_names(&case, &body),
            Vec::<String>::new(),
            "case {case:?}: a namespace holding nothing is an empty page"
        );
        assert!(
            next_token(&body).is_none(),
            "case {case:?}: an empty page must carry no nextToken: {body}"
        );
    }

    // A namespace a caller created and then emptied is still a namespace: listing it is
    // an empty page, not a not-found.
    emulator.given_namespace(&instance, "logistics").await;
    let case = "a created namespace holding nothing";
    let (status, body) = emulator.list_datasets(&instance, "logistics", "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_names(case, &body),
        Vec::<String>::new(),
        "case {case:?}: a created namespace holding nothing is an empty page"
    );
}

/// RULE: listing is a report — it changes nothing a later caller sees, however many times
/// and at whatever page size it is called.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("idempotent-host").await;
    const SEEDED: usize = 5;
    for index in 1..=SEEDED {
        emulator
            .given_dataset(
                &instance,
                "default",
                &dataset_name(index),
                json!({ "schema": minimal_schema() }),
            )
            .await;
    }
    let before = emulator.stored_datasets(&instance);
    assert_eq!(
        before, SEEDED,
        "the store must hold the {SEEDED} datasets the setup created, got {before}"
    );

    let case = "the same listing twice";
    let (first, _) = walk_pages(&emulator, case, &instance, "default", Some(2)).await;
    let (second, _) = walk_pages(&emulator, case, &instance, "default", Some(2)).await;
    assert_eq!(
        first, second,
        "case {case:?}: two identical listings must answer identically"
    );

    let case = "a listing at another page size";
    let (whole, pages) = walk_pages(&emulator, case, &instance, "default", Some(MAX_RESULTS_MAX))
        .await;
    assert_eq!(
        whole, first,
        "case {case:?}: the page size must not change which datasets are reported"
    );
    assert_eq!(
        pages, 1,
        "case {case:?}: {SEEDED} datasets fit in one page of {MAX_RESULTS_MAX}"
    );

    let after = emulator.stored_datasets(&instance);
    assert_eq!(
        after, before,
        "case {case:?}: listing must leave the store holding the same {before} datasets, got \
         {after}"
    );

    // ...and the single-dataset read still reports each of them.
    for index in 1..=SEEDED {
        let name = dataset_name(index);
        let (status, body) = emulator.get_dataset(&instance, "default", &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataLakeDataset {name} after listing expected 200, got {status} \
             with {body}"
        );
    }
}
