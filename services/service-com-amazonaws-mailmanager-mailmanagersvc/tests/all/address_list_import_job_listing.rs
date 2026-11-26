// SPDX-License-Identifier: CC0-1.0

//! Listing the import jobs of an address list, end to end over the wire.
//!
//! Every assertion goes through the real request path — `MultiServiceRouter::dispatch` with
//! `awsJson1_0` bodies, `POST /` and `X-Amz-Target: MailManagerSvc.<Operation>` — so what it
//! pins is the emulator's HTTP behaviour and not an internal helper.
//!
//! The tests are tables: the rows vary the number of jobs a list holds (none, one, several),
//! the `PageSize` a walk asks for (both published bounds and values in between), which list
//! of several is being listed, and the ways an `AddressListId` fails to resolve (never
//! created, deleted, outside the minted form).

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "eu-west-1";

/// The endpoint host the test addresses.
const HOST: &str = "mail-manager.localhost:9297";

/// The `ImportJobStatus` a job that has imported nothing carries
/// (`smithy_query(query="shape",shape="ImportJobStatus")` lists `CREATED`, `PROCESSING`,
/// `COMPLETED`, `FAILED` and `STOPPED`).
const CREATED: &str = "CREATED";

/// The published bounds of `ListAddressListImportJobs`'s `PageSize`
/// (`smithy_query(query="operation",operation="ListAddressListImportJobs")` reports
/// `range {min: 1, max: 50}`).
const PAGE_SIZE_MIN: i64 = 1;
const PAGE_SIZE_MAX: i64 = 50;

/// One emulator holding one MailManager service on one shared engine.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, and the fixed test credential below
        // is not this process's signing key.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Call one modelled operation and return `(http status, parsed body)`.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", HOST)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("MailManagerSvc.{operation}"))
            // A signed SDK request routes by its SigV4 credential scope, whose 4th field is
            // this service's signing name (`ses`) and whose 3rd is the region.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/{REGION}/ses/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid HTTP request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            json!({})
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create an address list and return its `AddressListId`, failing the test on a refusal.
    async fn create_address_list(&self, name: &str) -> String {
        let (status, out) = self
            .call("CreateAddressList", json!({ "AddressListName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected CreateAddressList to answer 200, got {status} {out:?}"
        );
        out["AddressListId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: no AddressListId in {out:?}"))
            .to_string()
    }

    /// Create one import job against `address_list_id` and return its `JobId`.
    async fn create_job(&self, address_list_id: &str, name: &str, data_type: &str) -> String {
        let (status, out) = self
            .call(
                "CreateAddressListImportJob",
                json!({
                    "AddressListId": address_list_id,
                    "Name": name,
                    "ImportDataFormat": { "ImportDataType": data_type },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected CreateAddressListImportJob to answer 200, got {status} \
             {out:?}"
        );
        out["JobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: no JobId in {out:?}"))
            .to_string()
    }

    /// Walk every page of `ListAddressListImportJobs` for `address_list_id`, asking for
    /// `page_size` when it is given, and return the jobs in the order they were reported.
    ///
    /// Bounded by the number of jobs the caller says exist, so a listing that never stops
    /// handing out tokens fails the test instead of looping forever.
    async fn list_all(&self, case: &str, address_list_id: &str, page_size: Option<i64>) -> Vec<Value> {
        let mut collected: Vec<Value> = Vec::new();
        let mut next_token: Option<String> = None;
        for page_number in 0..64 {
            let mut body = json!({ "AddressListId": address_list_id });
            if let Some(size) = page_size {
                body["PageSize"] = json!(size);
            }
            if let Some(token) = &next_token {
                body["NextToken"] = json!(token);
            }

            let (status, out) = self.call("ListAddressListImportJobs", body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: page {page_number} must answer 200, got {status} {out:?}"
            );
            let jobs = out["ImportJobs"].as_array().unwrap_or_else(|| {
                panic!("case {case:?}: page {page_number} must carry ImportJobs, got {out:?}")
            });
            if let Some(size) = page_size {
                assert!(
                    i64::try_from(jobs.len()).unwrap_or(i64::MAX) <= size,
                    "case {case:?}: page {page_number} must hold at most {size} jobs, got {}",
                    jobs.len()
                );
            }
            collected.extend(jobs.iter().cloned());

            next_token = out["NextToken"].as_str().map(str::to_string);
            if next_token.is_none() {
                return collected;
            }
            assert!(
                !jobs.is_empty(),
                "case {case:?}: page {page_number} carries a NextToken but no job, got {out:?}"
            );
        }
        panic!("case {case:?}: the listing never stopped issuing continuation tokens");
    }
}

/// The wire error code of a failure body: the `__type` member, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// The `JobId`s of a reported page, in the order they were reported.
fn reported_ids(jobs: &[Value]) -> Vec<String> {
    jobs.iter()
        .map(|job| job["JobId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `JobId`s an answered page carries, in the order the page reports them.
fn page_ids(page: &Value) -> Vec<String> {
    match page["ImportJobs"].as_array() {
        Some(jobs) => reported_ids(jobs),
        None => Vec::new(),
    }
}

/// Whatever the page size, a walk reports each of an address list's import jobs exactly
/// once, in one order, and nothing else: the rows are lists holding none, one and several
/// jobs, walked at both published `PageSize` bounds, at the sizes between them and with no
/// `PageSize` at all.
#[tokio::test]
async fn every_page_size_walks_one_list_once() {
    let harness = Harness::new();

    // Rows: how many jobs the list under test holds.
    for job_count in [0_usize, 1, 2, 5] {
        let list_name = format!("walk.list-{job_count}");
        let address_list_id = harness.create_address_list(&list_name).await;
        let mut created: BTreeSet<String> = BTreeSet::new();
        for index in 0..job_count {
            let data_type = if index % 2 == 0 { "CSV" } else { "JSON" };
            created.insert(
                harness
                    .create_job(&address_list_id, &format!("walk.job-{index}"), data_type)
                    .await,
            );
        }

        // Rows: the page size the walk asks for — each published bound, two sizes inside
        // the range, and no size at all.
        let page_sizes: [Option<i64>; 5] = [
            Some(PAGE_SIZE_MIN),
            Some(2),
            Some(7),
            Some(PAGE_SIZE_MAX),
            None,
        ];
        let mut orders: Vec<Vec<String>> = Vec::new();
        for page_size in page_sizes {
            let case = format!("{job_count} jobs at PageSize {page_size:?}");
            let jobs = harness.list_all(&case, &address_list_id, page_size).await;
            let ids = reported_ids(&jobs);

            assert_eq!(
                ids.iter().cloned().collect::<BTreeSet<String>>(),
                created,
                "case {case:?}: the walk must report exactly the list's jobs, got {ids:?}"
            );
            assert_eq!(
                ids.len(),
                created.len(),
                "case {case:?}: no job may be reported twice, got {ids:?}"
            );
            for job in &jobs {
                assert_eq!(
                    job["AddressListId"].as_str(),
                    Some(address_list_id.as_str()),
                    "case {case:?}: every reported job names the list it was created for, got \
                     {job:?}"
                );
                assert_eq!(
                    job["Status"].as_str(),
                    Some(CREATED),
                    "case {case:?}: a job that has imported nothing is {CREATED}, got {job:?}"
                );
                assert!(
                    job["Name"].as_str().is_some_and(|name| !name.is_empty())
                        && job["PreSignedUrl"]
                            .as_str()
                            .is_some_and(|url| url.contains("/_aws/"))
                        && job["CreatedTimestamp"].as_f64().is_some_and(|t| t > 0.0),
                    "case {case:?}: every reported job carries its stored name, upload target \
                     and creation instant, got {job:?}"
                );
                assert!(
                    job["ImportDataFormat"]["ImportDataType"].as_str().is_some(),
                    "case {case:?}: every reported job carries its input format, got {job:?}"
                );
            }
            orders.push(ids);
        }

        // One stable order across every page size: a paginating caller and a caller asking
        // for everything at once see the same sequence.
        for (index, order) in orders.iter().enumerate() {
            assert_eq!(
                order, &orders[0],
                "case {job_count} jobs: walk {index} reported a different order, got {order:?} \
                 against {:?}",
                orders[0]
            );
        }
    }
}

/// A reported job carries the same values `GetAddressListImportJob` reads back for it, and
/// listing changes nothing a later caller sees.
#[tokio::test]
async fn a_listed_job_matches_the_job_read_back() {
    let harness = Harness::new();
    let address_list_id = harness.create_address_list("round.trip-list").await;

    // Rows: the input formats the model declares and names across `JobName`'s pattern.
    let rows: [(&str, &str); 3] = [
        ("round.trip-1", "CSV"),
        ("round_trip_2", "JSON"),
        ("r", "CSV"),
    ];
    for (name, data_type) in rows {
        harness.create_job(&address_list_id, name, data_type).await;
    }

    let listed = harness
        .list_all("round trip", &address_list_id, None)
        .await;
    assert_eq!(
        listed.len(),
        rows.len(),
        "case round trip: every created job must be listed, got {listed:?}"
    );
    for job in &listed {
        let job_id = job["JobId"].as_str().unwrap_or_default().to_string();
        let (status, read) = harness
            .call("GetAddressListImportJob", json!({ "JobId": &job_id }))
            .await;
        assert_eq!(
            status, 200,
            "case {job_id:?}: the listed job must be readable, got {status} {read:?}"
        );
        for member in [
            "JobId",
            "Name",
            "Status",
            "PreSignedUrl",
            "AddressListId",
            "CreatedTimestamp",
        ] {
            assert_eq!(
                job.get(member),
                read.get(member),
                "case {job_id:?}: the listing and the read must agree on {member}, got {job:?} \
                 against {read:?}"
            );
        }
        assert_eq!(
            job["ImportDataFormat"], read["ImportDataFormat"],
            "case {job_id:?}: the listing and the read must agree on ImportDataFormat, got \
             {job:?} against {read:?}"
        );
    }

    // A read changes nothing: a second walk answers the same.
    let again = harness
        .list_all("round trip, again", &address_list_id, None)
        .await;
    assert_eq!(
        again, listed,
        "case round trip: the listing must be stable across reads, got {again:?}"
    );
}

/// A listing is scoped to the address list it names: a job created against one list is
/// reported by that list's walk and by no other's, whatever the page size.
#[tokio::test]
async fn a_listing_reports_only_its_own_lists_jobs() {
    let harness = Harness::new();

    // Rows: three lists, holding a different number of jobs each.
    let lists = [
        ("scoped.list-a", 3_usize),
        ("scoped.list-b", 1),
        ("scoped.list-c", 0),
    ];
    let mut expected: Vec<(String, BTreeSet<String>)> = Vec::new();
    for (name, job_count) in lists {
        let address_list_id = harness.create_address_list(name).await;
        let mut ids = BTreeSet::new();
        for index in 0..job_count {
            ids.insert(
                harness
                    .create_job(&address_list_id, &format!("{name}.job-{index}"), "CSV")
                    .await,
            );
        }
        expected.push((address_list_id, ids));
    }

    for (address_list_id, ids) in &expected {
        for page_size in [Some(PAGE_SIZE_MIN), Some(2), None] {
            let case = format!("list {address_list_id} at PageSize {page_size:?}");
            let listed = harness.list_all(&case, address_list_id, page_size).await;
            assert_eq!(
                reported_ids(&listed)
                    .into_iter()
                    .collect::<BTreeSet<String>>(),
                *ids,
                "case {case:?}: a listing reports only the jobs of the list it names, got \
                 {listed:?}"
            );
        }
    }
}

/// An `AddressListId` that resolves to no stored address list is refused with the modelled
/// `ResourceNotFoundException`, at the status this service answers an id it cannot resolve
/// with and in the wording every address-list lookup miss of this service carries — whether
/// the list was never created or was removed by `DeleteAddressList`, which leaves its import
/// jobs reachable through no listing.
#[tokio::test]
async fn an_address_list_that_does_not_resolve_is_refused() {
    let harness = Harness::new();

    let live_list = harness.create_address_list("live.list").await;
    harness.create_job(&live_list, "live.job", "CSV").await;

    let deleted_list = harness.create_address_list("deleted.list").await;
    let orphan_job = harness
        .create_job(&deleted_list, "orphan.job", "JSON")
        .await;
    let (status, out) = harness
        .call("DeleteAddressList", json!({ "AddressListId": &deleted_list }))
        .await;
    assert_eq!(
        status, 200,
        "case delete: DeleteAddressList must answer 200, got {status} {out:?}"
    );

    // Rows: ids that satisfy the modelled pattern but resolve to nothing — the deleted
    // list, the shortest id, a hyphenated one, the longest the length bound admits, and a
    // live id with one character changed.
    let unresolvable: [(&str, String); 5] = [
        ("the deleted list", deleted_list.clone()),
        ("the shortest id", "x".to_string()),
        ("a hyphenated id", "no-such-address-list".to_string()),
        ("the longest id", "9".repeat(255)),
        (
            "a live id with one character changed",
            format!("{}0", &live_list[..live_list.len() - 1]),
        ),
    ];
    for (case, id) in &unresolvable {
        if id == &live_list {
            continue;
        }
        let (status, out) = harness
            .call("ListAddressListImportJobs", json!({ "AddressListId": id }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an unresolvable AddressListId must answer 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
        assert_eq!(
            out["Message"].as_str(),
            Some(format!("Could not find AddressList with id {id}").as_str()),
            "case {case:?}: unexpected refusal message, got {out:?}"
        );
    }

    // The orphaned job is still stored under its own id — the listing refuses because the
    // LIST is gone, not because the job was erased — and the live list is untouched.
    let (status, out) = harness
        .call("GetAddressListImportJob", json!({ "JobId": &orphan_job }))
        .await;
    assert_eq!(
        (status, error_code(&out)),
        (400, "ResourceNotFoundException".to_string()),
        "case the orphaned job: a read of it is refused for its missing list, got {status} \
         {out:?}"
    );
    let listed = harness.list_all("the live list", &live_list, None).await;
    assert_eq!(
        listed.len(),
        1,
        "case the live list: its listing must survive the refusals, got {listed:?}"
    );
}

/// A `NextToken` continues ONE walk: only a token this listing issued for THIS address list
/// names a position in it, and anything else is refused with the modelled
/// `ValidationException` rather than answered with a page the caller never asked for.
#[tokio::test]
async fn a_continuation_token_belongs_to_one_walk() {
    let harness = Harness::new();

    let first_list = harness.create_address_list("token.list-1").await;
    let second_list = harness.create_address_list("token.list-2").await;
    for index in 0..3 {
        harness
            .create_job(&first_list, &format!("token.job-{index}"), "CSV")
            .await;
        harness
            .create_job(&second_list, &format!("token.other-{index}"), "JSON")
            .await;
    }

    // The token the first page of the first list's walk issues.
    let (status, page) = harness
        .call(
            "ListAddressListImportJobs",
            json!({ "AddressListId": &first_list, "PageSize": 1 }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case the first page: expected 200, got {status} {page:?}"
    );
    let token = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case the first page: a further page must carry a token, got {page:?}"))
        .to_string();
    assert!(
        (1..=2048).contains(&token.len()),
        "case the first page: a NextToken must satisfy PaginationToken's modelled 1..2048 \
         length, got {} characters",
        token.len()
    );

    // Presented to the walk that issued it, the token continues it.
    let (status, resumed) = harness
        .call(
            "ListAddressListImportJobs",
            json!({ "AddressListId": &first_list, "PageSize": 1, "NextToken": &token }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case the resumed page: expected 200, got {status} {resumed:?}"
    );
    assert_ne!(
        page_ids(&resumed),
        page_ids(&page),
        "case the resumed page: it must move past the first page, got {resumed:?}"
    );

    // Rows: tokens that name no position in this walk — one issued for another address
    // list's walk, text a caller wrote, and hex that carries no authenticated cursor.
    let rejected: [(&str, String); 4] = [
        ("a token issued for another list's walk", token.clone()),
        ("text a caller wrote", "not-a-token".to_string()),
        ("hex that authenticates nothing", "00".repeat(48)),
        ("the empty-looking shortest token", "0".to_string()),
    ];
    for (case, candidate) in &rejected {
        let list = if case.starts_with("a token issued") {
            &second_list
        } else {
            &first_list
        };
        let (status, out) = harness
            .call(
                "ListAddressListImportJobs",
                json!({ "AddressListId": list, "NextToken": candidate }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a token naming no position in this walk must answer 400, got \
             {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
    }
}

/// `PageSize` outside its published range, and an `AddressListId` outside the modelled form,
/// are refused with a 400 `ValidationException` — never a 501, and never served as a page.
#[tokio::test]
async fn requests_outside_the_modelled_constraints_are_refused() {
    let harness = Harness::new();
    let address_list_id = harness.create_address_list("constrained.listing").await;
    harness
        .create_job(&address_list_id, "constrained.job", "CSV")
        .await;

    // Rows walk each published bound and one step past it: `PageSize` (range 1..50),
    // `AddressListId` (len 1..255, pattern `^[a-zA-Z0-9-]+$`) and the member absent.
    let refused: [(&str, Value); 6] = [
        (
            "PageSize 0, one below the minimum",
            json!({ "AddressListId": &address_list_id, "PageSize": PAGE_SIZE_MIN - 1 }),
        ),
        (
            "PageSize 51, one past the maximum",
            json!({ "AddressListId": &address_list_id, "PageSize": PAGE_SIZE_MAX + 1 }),
        ),
        (
            "a negative PageSize",
            json!({ "AddressListId": &address_list_id, "PageSize": -1 }),
        ),
        (
            "an underscore in AddressListId, outside its pattern",
            json!({ "AddressListId": "not_an_id" }),
        ),
        (
            "an empty AddressListId, one below its minimum length",
            json!({ "AddressListId": "" }),
        ),
        ("no AddressListId at all", json!({})),
    ];
    for (case, body) in refused {
        let (status, out) = harness.call("ListAddressListImportJobs", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
    }

    // Each published bound itself is served, so the refusals above are the bound and not a
    // blanket rejection.
    for size in [PAGE_SIZE_MIN, PAGE_SIZE_MAX] {
        let (status, out) = harness
            .call(
                "ListAddressListImportJobs",
                json!({ "AddressListId": &address_list_id, "PageSize": size }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case PageSize {size}: a size at the bound must be served, got {status} {out:?}"
        );
        assert_eq!(
            out["ImportJobs"].as_array().map(Vec::len),
            Some(1),
            "case PageSize {size}: the list's one job must be reported, got {out:?}"
        );
    }
}
