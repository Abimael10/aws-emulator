// SPDX-License-Identifier: CC0-1.0

//! The neptunedata bulk LOADER, end to end over HTTP: what a load reads, what the graph
//! holds afterwards, what the job reports, and what a `loadId` nothing issued is refused
//! with.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes. The emulator's own Amazon S3 is mounted beside this service on ONE engine
//! exactly as the assembled router mounts them, and every object a load reads is staged
//! through that service's OWN `PutObject`, so what a load reads is what Amazon S3 really
//! holds.
//!
//! The rules under test:
//!
//! * `StartLoaderJob` READS the objects its `source` names in the `format` the request
//!   asked for, writes the elements they hold into this engine's one graph, and hands back
//!   the `loadId` of the job it recorded for that run — so the numbers
//!   `GetLoaderJobStatus` reports are measurements of that run and the graph a later query
//!   reads is the graph the load left behind;
//! * a job reaches its terminal status when the clock makes it due, so a caller that reads
//!   it straight away still sees the load in progress;
//! * `GetLoaderJobStatus` and `CancelLoaderJob` refuse a `loadId` the registry does not
//!   hold, each through its own modelled error;
//! * `ListLoaderJobs` reports the load ids of the loader jobs the registry holds — the
//!   loads the loader keeps, finished ones among them — with the queued load requests
//!   among them unless the request excludes them, newest admission first and at most
//!   `limit` of them.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::state::{
    ComAmazonawsNeptunedataAmazonneptunedataplaneState, LoaderJobRecord, LoaderJobState,
    MAX_LOAD_OBJECT_BYTES,
};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The Region the loads below name, which is also the Region the object store is reached
/// in.
const REGION: &str = "us-east-1";

/// The bucket every load's objects are staged in.
const BUCKET: &str = "neptune-loader-fixtures";

/// An IAM role ARN the loader can resolve a credential for.
const ROLE_ARN: &str = "arn:aws:iam::000000000001:role/NeptuneLoadFromS3";

/// `ListLoaderJobsOutput$status` for a job-list request the loader served, spelled out
/// here rather than imported so a wrong value in `src/state.rs` fails this test.
const JOB_LIST_SERVED: &str = "200 OK";

/// The published bounds of `ListLoaderJobsInput$limit`.
/// Source: smithy_query(query="shape", shape="ListLoaderJobsInput") — `limit` targets
/// PositiveInteger with range {min: 1, max: 100}, and its documentation names 100 as the
/// default as well as the maximum.
const LIMIT_BOUNDS: (i32, i32) = (1, 100);

/// The load statuses the loader reports, spelled out here rather than imported.
const LOAD_IN_PROGRESS: &str = "LOAD_IN_PROGRESS";
const LOAD_COMPLETED: &str = "LOAD_COMPLETED";
const LOAD_FAILED: &str = "LOAD_FAILED";
const LOAD_CANCELLED_BY_USER: &str = "LOAD_CANCELLED_BY_USER";
const LOAD_S3_READ_ERROR: &str = "LOAD_S3_READ_ERROR";

/// How long a poll waits for a load to reach its terminal status. Generous: the span the
/// service owes is its own, and this only has to be longer than it on any machine.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One emulator: a router carrying this service and the emulator's own Amazon S3 over one
/// shared engine. The engine is handed back so a test can read the graph a load wrote and
/// seed the registry the way a loader that really loads would.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// An Amazon S3 client over this emulator's own in-process callback — the SAME
/// construction a load reads its source through (`service_common::s3_client::s3_client`,
/// which `src/state/loader/source.rs` uses), so an object staged with this client is the
/// object that load reads.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    service_common::s3_client::s3_client(&callback, REGION)
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "neptune-db.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_NEPTUNE_DB);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
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

async fn list_jobs(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, "GET", &format!("/loader{query}"), None).await
}

/// The load ids a list answer carries, in the order it carries them.
fn listed_ids(answer: &Answer) -> Vec<String> {
    answer.body["payload"]["loadIds"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|id| id.as_str().unwrap_or_default().to_string())
        .collect()
}

/// One seeded loader job: the record to put in the registry, and how far it has got.
#[derive(Debug, Clone, Copy)]
struct Job {
    load_id: &'static str,
    state: LoaderJobState,
    /// Seconds before "now" the loader admitted the job.
    admitted_secs_ago: u64,
}

/// The seeded rows. Load ids are the UUID-shaped strings Neptune reports a load under.
/// The rows differ along everything the rule generalises over: how far each job has got
/// (every state the record can hold), and when it was admitted — including two jobs
/// admitted in the SAME instant, so the order the list reports is total rather than
/// whatever the registry happens to iterate.
const JOBS: [Job; 7] = [
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000001",
        state: LoaderJobState::Loading,
        admitted_secs_ago: 30,
    },
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000002",
        state: LoaderJobState::Queued,
        admitted_secs_ago: 20,
    },
    // Admitted in the same instant as the row above, and listed after it because the
    // load id breaks the tie.
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000007",
        state: LoaderJobState::Loading,
        admitted_secs_ago: 20,
    },
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000003",
        state: LoaderJobState::Queued,
        admitted_secs_ago: 10,
    },
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000004",
        state: LoaderJobState::Completed,
        admitted_secs_ago: 40,
    },
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000005",
        state: LoaderJobState::Failed,
        admitted_secs_ago: 50,
    },
    Job {
        load_id: "3a8b1f00-0000-4000-8000-000000000006",
        state: LoaderJobState::Cancelled,
        admitted_secs_ago: 60,
    },
];

/// Put [`JOBS`] in the registry, the way a loader that really loads would.
fn seed(engine: &engine::InMemoryEngine) {
    let now = SystemTime::now();
    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let mut store = state.lock();
    for job in &JOBS {
        let record = LoaderJobRecord::admitted(
            job.load_id,
            now - Duration::from_secs(job.admitted_secs_ago),
            job.state,
        );
        store.loader_jobs.insert(record.load_id.clone(), record);
    }
}

/// The ids of the seeded rows the list must report, newest first and ties broken by load
/// id — the jobs the registry holds, minus the queued requests when the request excludes
/// them.
fn listed_newest_first(include_queued: bool) -> Vec<&'static str> {
    let mut listed: Vec<&Job> = JOBS
        .iter()
        .filter(|job| include_queued || !matches!(job.state, LoaderJobState::Queued))
        .collect();
    listed.sort_by_key(|job| (job.admitted_secs_ago, job.load_id));
    listed.into_iter().map(|job| job.load_id).collect()
}

#[tokio::test]
async fn an_empty_registry_lists_no_load_ids() {
    let Emulator { router, .. } = emulator();

    let answer = list_jobs(&router, "").await;
    assert_eq!(
        answer.status, 200,
        "GET /loader: expected 200, got {} with body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["status"].as_str(),
        Some(JOB_LIST_SERVED),
        "status: expected the served job-list status, got {:?}",
        answer.body["status"]
    );
    assert_eq!(
        listed_ids(&answer),
        Vec::<String>::new(),
        "an engine that has admitted no load must list no load ids, got {:?}",
        answer.body
    );
}

#[tokio::test]
async fn the_list_reports_the_jobs_the_registry_holds() {
    let Emulator { router, engine } = emulator();
    seed(&engine);

    // The rows: how the request spells `includeQueuedLoads`, and the ids that must come
    // back. An absent member defaults to TRUE, so the queued requests are listed; FALSE
    // excludes exactly them.
    let cases: [(&str, Vec<&'static str>); 4] = [
        ("", listed_newest_first(true)),
        ("?includeQueuedLoads=true", listed_newest_first(true)),
        ("?includeQueuedLoads=false", listed_newest_first(false)),
        (
            "?includeQueuedLoads=true&limit=100",
            listed_newest_first(true),
        ),
    ];

    for (query, expected) in cases {
        let answer = list_jobs(&router, query).await;
        assert_eq!(
            answer.status, 200,
            "case {query:?}: expected 200, got {} with body {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            listed_ids(&answer),
            expected,
            "case {query:?}: expected {expected:?}, got {:?}",
            answer.body
        );
    }

    // The load id of a job that has FINISHED is one the loader keeps, so it is listed too:
    // that is what lets a client find the id of a load it started and then read its
    // status.
    let listed = listed_ids(&list_jobs(&router, "?includeQueuedLoads=true").await);
    for job in JOBS.iter().filter(|job| {
        matches!(
            job.state,
            LoaderJobState::Completed | LoaderJobState::Failed | LoaderJobState::Cancelled
        )
    }) {
        assert!(
            listed.contains(&job.load_id.to_string()),
            "case {:?}: a {:?} job is one the loader keeps, so its id must be listed, got \
             {listed:?}",
            job.load_id,
            job.state
        );
    }
}

#[tokio::test]
async fn limit_takes_the_newest_jobs_and_is_bounded_by_the_published_range() {
    let Emulator { router, engine } = emulator();
    seed(&engine);

    let newest_first = listed_newest_first(true);
    let (min_limit, max_limit) = LIMIT_BOUNDS;

    // Inside the published range the answer is the first `limit` ids of the same order,
    // so the newest jobs are the ones a bounded request sees. Both published bounds are
    // among the rows, and `limit` 2 and 3 straddle the two jobs admitted in the same
    // instant, so the tie-break decides which of them a bounded request sees.
    for limit in [min_limit, 2, 3, max_limit] {
        let answer = list_jobs(&router, &format!("?limit={limit}")).await;
        assert_eq!(
            answer.status, 200,
            "case limit={limit}: expected 200, got {} with body {:?}",
            answer.status, answer.body
        );
        let wanted: Vec<&str> = newest_first
            .iter()
            .take(usize::try_from(limit).unwrap_or(0))
            .copied()
            .collect();
        assert_eq!(
            listed_ids(&answer),
            wanted,
            "case limit={limit}: expected {wanted:?}, got {:?}",
            answer.body
        );
    }

    // One past each published bound is outside the modelled range, so the request is
    // refused rather than served a guess, and the refusal names the member and the bound
    // the value missed.
    for (limit, named_bound) in [(min_limit - 1, min_limit), (max_limit + 1, max_limit)] {
        let answer = list_jobs(&router, &format!("?limit={limit}")).await;
        assert_eq!(
            answer.status, 400,
            "case limit={limit}: a limit outside {min_limit}..={max_limit} must be refused, got {} with body {:?}",
            answer.status, answer.body
        );
        let message = answer.body["message"]
            .as_str()
            .or_else(|| answer.body["detailedMessage"].as_str())
            .unwrap_or_default();
        assert!(
            message.contains("limit") && message.contains(&named_bound.to_string()),
            "case limit={limit}: the refusal must name the member and the bound {named_bound}, got {:?}",
            answer.body
        );
    }

    // The refusal left the registry exactly as it was.
    let listed = list_jobs(&router, "?includeQueuedLoads=true").await;
    assert_eq!(
        listed_ids(&listed),
        newest_first,
        "a refused limit must not change the jobs the list reports, got {:?}",
        listed.body
    );
}

#[tokio::test]
async fn listing_the_jobs_changes_nothing_a_later_caller_sees() {
    let Emulator { router, engine } = emulator();
    seed(&engine);

    let first = list_jobs(&router, "?includeQueuedLoads=true").await;
    // A narrower read in between must not consume, reorder or retire anything.
    let _ = list_jobs(&router, "?includeQueuedLoads=false&limit=1").await;
    let again = list_jobs(&router, "?includeQueuedLoads=true").await;

    assert_eq!(
        listed_ids(&again),
        listed_ids(&first),
        "a read must not change what a later caller sees: {:?} then {:?}",
        first.body,
        again.body
    );

    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    assert_eq!(
        state.lock().loader_jobs.len(),
        JOBS.len(),
        "every seeded job must still be in the registry after the reads"
    );
}

/// One load request and what the loader answers it: the role it would run under, and
/// whether a credential can be resolved for that value at all.
#[derive(Debug, Clone, Copy)]
struct LoadRequest {
    case: &'static str,
    source: &'static str,
    iam_role_arn: &'static str,
    /// Whether the value names an IAM role, which is what the loader resolves a credential
    /// for.
    usable: bool,
}

/// The rows differ along what the value IS: a role ARN of either account form, text that
/// is not an ARN at all, an ARN of another service, an ARN naming another kind of IAM
/// entity, and an empty value.
/// Source: smithy_query(query="operation", operation="StartLoaderJob") — `iamRoleArn` is an
/// unconstrained String, so no modelled constraint refuses any of these at the boundary.
const LOAD_REQUESTS: &[LoadRequest] = &[
    LoadRequest {
        case: "an IAM role ARN",
        source: "s3://example-bucket/vertices/",
        iam_role_arn: "arn:aws:iam::000000000001:role/NeptuneLoadFromS3",
        usable: true,
    },
    LoadRequest {
        case: "an IAM role ARN carrying a path",
        source: "s3://example-bucket/edges/",
        iam_role_arn: "arn:aws:iam::000000000001:role/service-role/NeptuneLoad",
        usable: true,
    },
    LoadRequest {
        case: "an IAM role ARN of another partition",
        source: "s3://example-bucket/data/",
        iam_role_arn: "arn:aws-us-gov:iam::000000000001:role/NeptuneLoad",
        usable: true,
    },
    LoadRequest {
        case: "text that is not an ARN",
        source: "s3://example-bucket/data/",
        iam_role_arn: "not-a-valid-arn",
        usable: false,
    },
    LoadRequest {
        case: "an ARN of another service",
        source: "s3://example-bucket/data/",
        iam_role_arn: "arn:aws:s3:::example-bucket/data",
        usable: false,
    },
    LoadRequest {
        case: "an ARN naming an IAM user rather than a role",
        source: "s3://another-bucket/load/",
        iam_role_arn: "arn:aws:iam::000000000001:user/loader",
        usable: false,
    },
    LoadRequest {
        case: "an ARN with no resource",
        source: "s3://example-bucket/data/",
        iam_role_arn: "arn:aws:iam::000000000001:",
        usable: false,
    },
    LoadRequest {
        case: "an empty value",
        source: "s3://example-bucket/data/",
        iam_role_arn: "",
        usable: false,
    },
];

/// A load runs UNDER the role the request names, so a value the loader can resolve no
/// credential for is a load that could never start: it is refused
/// `InvalidParameterException` (HTTP 400) naming the source and the role, and admits no
/// job. A request whose role IS one is taken on, whatever its source holds.
#[tokio::test]
async fn a_load_whose_role_resolves_no_credential_is_refused_and_admits_no_job() {
    let Emulator { router, engine } = emulator();
    seed(&engine);
    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let before = state.lock().loader_jobs.len();
    let mut admitted = 0usize;

    for request in LOAD_REQUESTS {
        let answer = start_load(
            &router,
            json!({
                "source": request.source,
                "format": "csv",
                "region": REGION,
                "iamRoleArn": request.iam_role_arn,
            }),
        )
        .await;
        let case = request.case;
        if request.usable {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a load whose role resolves a credential must be taken on, \
                 got {} with body {:?}",
                answer.status, answer.body
            );
            admitted += 1;
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["code"].as_str(),
            Some("InvalidParameterException"),
            "case {case:?}: expected InvalidParameterException, got {:?}",
            answer.body
        );
        let expected = format!(
            "Failed to start new load from the source {}. Couldn't find the aws credential \
             for iam_role_arn: {}",
            request.source, request.iam_role_arn
        );
        for member in ["message", "detailedMessage"] {
            assert_eq!(
                answer.body[member].as_str(),
                Some(expected.as_str()),
                "case {case:?}/{member}: expected {expected:?}, got {:?}",
                answer.body[member]
            );
        }
    }

    // No refused request admitted a job, so the registry holds the jobs it held before
    // them plus exactly the loads that WERE taken on.
    let after = state.lock().loader_jobs.len();
    assert_eq!(
        after,
        before + admitted,
        "a refused load must add no job: the registry held {before} jobs and holds {after}"
    );
}

// --- The load itself -------------------------------------------------------------------

/// `StartLoaderJob` is `POST /loader`.
/// Source: smithy_query(query="operation", operation="StartLoaderJob").
async fn start_load(router: &MultiServiceRouter, body: Value) -> Answer {
    call(router, "POST", "/loader", Some(body)).await
}

/// The `loadId` a start answered, which must be there for a load the loader took on.
fn load_id_of(case: &str, answer: &Answer) -> String {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from StartLoaderJob, got {} with body {:?}",
        answer.status, answer.body
    );
    let load_id = answer.body["payload"]["loadId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !load_id.is_empty(),
        "case {case:?}: a load the loader took on must be answered with a loadId, got {:?}",
        answer.body
    );
    load_id
}

/// `GetLoaderJobStatus` is `GET /loader/{loadId}`.
async fn job_status(router: &MultiServiceRouter, load_id: &str, query: &str) -> Answer {
    call(router, "GET", &format!("/loader/{load_id}{query}"), None).await
}

/// `CancelLoaderJob` is `DELETE /loader/{loadId}`.
async fn cancel_load(router: &MultiServiceRouter, load_id: &str) -> Answer {
    call(router, "DELETE", &format!("/loader/{load_id}"), None).await
}

/// The status one job reports right now.
fn reported_status(answer: &Answer) -> String {
    answer.body["payload"]["overallStatus"]["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll one job until it is no longer in progress, and answer that read.
///
/// Waits for the CONDITION rather than for a span: the service sleeps for nothing, and
/// nothing here asserts how long the wait took.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    case: &str,
    load_id: &str,
    query: &str,
) -> Answer {
    let deadline = SystemTime::now() + SETTLE_DEADLINE;
    let mut last = job_status(router, load_id, query).await;
    while SystemTime::now() < deadline {
        assert_eq!(
            last.status, 200,
            "case {case:?}: expected 200 from GetLoaderJobStatus, got {} with body {:?}",
            last.status, last.body
        );
        if reported_status(&last) != LOAD_IN_PROGRESS {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        last = job_status(router, load_id, query).await;
    }
    panic!(
        "case {case:?}: the load never reached a terminal status, last read {:?}",
        last.body
    );
}

/// Create the bucket every load's objects are staged in, through Amazon S3's own
/// `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(router).create_bucket().bucket(BUCKET).send().await;
    assert!(
        created.is_ok(),
        "Amazon S3 must create the fixture bucket, got {created:?}"
    );
}

/// Stage one source object, through Amazon S3's own `PutObject`.
async fn stage(router: &MultiServiceRouter, key: &str, body: &str) {
    let put = s3(router)
        .put_object()
        .bucket(BUCKET)
        .key(key)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            body.as_bytes().to_vec(),
        ))
        .send()
        .await;
    assert!(
        put.is_ok(),
        "Amazon S3 must hold the source object {key}, got {put:?}"
    );
}

/// The `s3://` source naming one staged prefix.
fn source_of(prefix: &str) -> String {
    format!("s3://{BUCKET}/{prefix}")
}

/// One format's fixture: the objects to stage, the source that names them, how many
/// records the load reads out of them, and one element the graph must hold afterwards.
struct FormatCase {
    /// The modelled `Format` value.
    format: &'static str,
    prefix: &'static str,
    objects: &'static [(&'static str, &'static str)],
    records: u64,
    /// A vertex the load writes, and one property it must carry.
    node_id: &'static str,
    property: (&'static str, Value),
    /// How many edges the load writes.
    edges: usize,
    /// How many records named an element the load already held and brought data to it.
    duplicates: u64,
}

/// The fixtures, one per modelled `Format` value this loader READS.
/// Source: smithy_query(query="operation", operation="StartLoaderJob") — `format` targets
/// the `Format` enum, whose values are `csv | nquads | ntriples | opencypher | rdfxml |
/// turtle`; RDF/XML is the one this loader cannot read, and
/// [`a_load_of_a_format_this_loader_cannot_read_is_answered_as_not_implemented`] covers it.
/// The Gremlin and openCypher CSV fixtures carry a vertex file and an edge file under one
/// prefix, a multi-labelled vertex, and typed and untyped property columns; each RDF
/// fixture carries a literal statement (a property), a `rdf:type` statement (a label) where
/// its syntax spells one, and a resource statement (an edge).
fn format_cases() -> Vec<FormatCase> {
    vec![
        FormatCase {
            format: "csv",
            prefix: "gremlin/",
            objects: &[
                (
                    "gremlin/vertices.csv",
                    "~id,~label,name:String,age:Int\n\
                     gremlin-v1,Person;Employee,Alice,30\n\
                     gremlin-v2,Person,Bob,41\n",
                ),
                (
                    "gremlin/edges.csv",
                    "~id,~from,~to,~label,since:Int\n\
                     gremlin-e1,gremlin-v1,gremlin-v2,knows,2020\n",
                ),
            ],
            records: 3,
            node_id: "gremlin-v1",
            property: ("age", json!(30)),
            edges: 1,
            duplicates: 0,
        },
        FormatCase {
            format: "opencypher",
            prefix: "opencypher/",
            objects: &[
                (
                    "opencypher/nodes.csv",
                    ":ID,:LABEL,name:String,height:Double\n\
                     oc-v1,Person;Employee,Carol,1.75\n\
                     oc-v2,Person,Dan,1.8\n",
                ),
                (
                    "opencypher/rels.csv",
                    ":ID,:START_ID,:END_ID,:TYPE\n\
                     oc-e1,oc-v1,oc-v2,KNOWS\n",
                ),
            ],
            records: 3,
            node_id: "oc-v1",
            property: ("name", json!("Carol")),
            edges: 1,
            duplicates: 0,
        },
        FormatCase {
            format: "ntriples",
            prefix: "ntriples/",
            objects: &[(
                "ntriples/data.nt",
                "<http://example.com/nt/s1> <http://example.com/name> \"Erin\" .\n\
                 <http://example.com/nt/s1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
                 <http://example.com/Person> .\n\
                 <http://example.com/nt/s1> <http://example.com/knows> \
                 <http://example.com/nt/s2> .\n",
            )],
            records: 3,
            node_id: "http://example.com/nt/s1",
            property: ("http://example.com/name", json!("Erin")),
            edges: 1,
            // The `rdf:type` statement brings a LABEL to the subject the statement before
            // it created, which is one record restating an element the load already held.
            duplicates: 1,
        },
        FormatCase {
            format: "nquads",
            prefix: "nquads/",
            objects: &[(
                "nquads/data.nq",
                "<http://example.com/nq/s1> <http://example.com/name> \"Frank\" \
                 <http://example.com/g1> .\n\
                 <http://example.com/nq/s1> <http://example.com/knows> \
                 <http://example.com/nq/s2> <http://example.com/g1> .\n",
            )],
            records: 2,
            node_id: "http://example.com/nq/s1",
            property: ("http://example.com/name", json!("Frank")),
            edges: 1,
            // The endpoints of the resource statement bring no data to the subject the
            // statement before it created, so nothing was loaded twice.
            duplicates: 0,
        },
        FormatCase {
            format: "turtle",
            prefix: "turtle/",
            objects: &[(
                "turtle/data.ttl",
                "@prefix ex: <http://example.com/ttl/> .\n\
                 ex:s1 ex:name \"Grace\" .\n\
                 ex:s1 ex:knows ex:s2 .\n",
            )],
            records: 2,
            node_id: "http://example.com/ttl/s1",
            property: ("http://example.com/ttl/name", json!("Grace")),
            edges: 1,
            duplicates: 0,
        },
    ]
}
/// RULE: a load reads the objects its source names in the format the request asked for and
/// writes what they held into this engine's graph, and the job reports that run.
#[tokio::test]
async fn a_load_of_each_format_writes_what_its_objects_held() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;
    let cases = format_cases();
    for case in &cases {
        for (key, body) in case.objects {
            stage(&router, key, body).await;
        }
    }

    // Every load is started before any of them is polled, so the spans the clock owes run
    // together rather than one after another.
    let mut started: Vec<(&FormatCase, String)> = Vec::new();
    for case in &cases {
        let answer = start_load(
            &router,
            json!({
                "source": source_of(case.prefix),
                "format": case.format,
                "region": REGION,
                "iamRoleArn": ROLE_ARN,
            }),
        )
        .await;
        let load_id = load_id_of(case.format, &answer);
        // A load the engine has taken on but not finished is reported as in progress, and
        // is one of the active jobs the list reports.
        let in_progress = job_status(&router, &load_id, "").await;
        assert_eq!(
            reported_status(&in_progress),
            LOAD_IN_PROGRESS,
            "case {:?}: a load the loader has just taken on must be reported as in \
             progress, got {:?}",
            case.format,
            in_progress.body
        );
        assert!(
            listed_ids(&list_jobs(&router, "").await).contains(&load_id),
            "case {:?}: a load the loader took on must be one of the jobs the list \
             reports",
            case.format
        );
        started.push((case, load_id));
    }
    assert_eq!(
        started
            .iter()
            .map(|(_, load_id)| load_id.clone())
            .collect::<std::collections::BTreeSet<String>>()
            .len(),
        cases.len(),
        "every load must be given an id of its own"
    );

    for (case, load_id) in &started {
        let answer = wait_for_terminal_status(&router, case.format, load_id, "?details=true").await;
        let overall = &answer.body["payload"]["overallStatus"];
        assert_eq!(
            reported_status(&answer),
            LOAD_COMPLETED,
            "case {:?}: expected the load to complete, got {:?}",
            case.format,
            answer.body
        );
        assert_eq!(
            overall["totalRecords"].as_u64(),
            Some(case.records),
            "case {:?}: expected {} records, got {:?}",
            case.format,
            case.records,
            answer.body
        );
        for counter in ["parsingErrors", "datatypeMismatchErrors", "insertErrors"] {
            assert_eq!(
                overall[counter].as_u64(),
                Some(0),
                "case {:?}: expected no {counter}, got {:?}",
                case.format,
                answer.body
            );
        }
        assert_eq!(
            overall["totalDuplicates"].as_u64(),
            Some(case.duplicates),
            "case {:?}: expected {} records restating an element the load held, got {:?}",
            case.format,
            case.duplicates,
            answer.body
        );
        assert_eq!(
            overall["fullUri"].as_str(),
            Some(source_of(case.prefix).as_str()),
            "case {:?}: expected the source the load read, got {:?}",
            case.format,
            answer.body
        );
        assert_eq!(
            answer.body["payload"]["feedCount"],
            json!([{ LOAD_COMPLETED: 1 }]),
            "case {:?}: expected one feed counted in the load's own status, got {:?}",
            case.format,
            answer.body
        );
        assert_eq!(
            answer.body["payload"]["failedFeeds"],
            json!([]),
            "case {:?}: a load that completed failed on no feed, got {:?}",
            case.format,
            answer.body
        );
        // A job the loader keeps is still one the list reports once it has finished.
        assert!(
            listed_ids(&list_jobs(&router, "").await).contains(load_id),
            "case {:?}: a finished load is one the loader keeps, so it must still be \
             listed",
            case.format
        );
    }

    // And the graph really holds what each load read.
    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let store = state.lock();
    let mut edges = 0usize;
    for case in &cases {
        let node = store.graph.node(case.node_id).cloned();
        let node = node.unwrap_or_else(|| {
            panic!(
                "case {:?}: the graph must hold the vertex {:?} the load wrote",
                case.format, case.node_id
            )
        });
        let (key, value) = &case.property;
        assert_eq!(
            node.properties.get(*key),
            Some(value),
            "case {:?}: expected the vertex to carry {key}={value}, got {:?}",
            case.format,
            node.properties
        );
        edges += case.edges;
    }
    assert_eq!(
        store.graph.relationship_count(),
        edges,
        "every edge the loads read must be in the graph"
    );
}

/// RULE: a load of a format this loader cannot READ is answered "nobody has written it
/// yet" rather than with a load id, and it admits no job — a load id for a load nothing
/// performed would be a fabrication. RDF/XML is that format: every maintained parser for
/// that syntax is built on an XML reader whose released versions this project's dependency
/// audit refuses, so nothing here can decode such a file.
#[tokio::test]
async fn a_load_of_a_format_this_loader_cannot_read_is_answered_as_not_implemented() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;
    stage(
        &router,
        "rdfxml/data.rdf",
        "<?xml version=\"1.0\"?>\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
         xmlns:ex=\"http://example.com/xml/\">\n\
         <rdf:Description rdf:about=\"http://example.com/xml/s1\">\n\
         <ex:name>Heidi</ex:name>\n\
         </rdf:Description>\n\
         </rdf:RDF>\n",
    )
    .await;

    let answer = start_load(
        &router,
        json!({
            "source": source_of("rdfxml/"),
            "format": "rdfxml",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
        }),
    )
    .await;
    assert_eq!(
        answer.status, 501,
        "a load of a format this loader cannot read must be answered as not implemented, \
         got {} with body {:?}",
        answer.status, answer.body
    );

    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let store = state.lock();
    assert_eq!(
        (store.loader_jobs.len(), store.graph.node_count()),
        (0, 0),
        "a load that was not performed must record no job and write nothing"
    );
}

/// RULE: a load that did not read everything its source held did not COMPLETE, whatever
/// its error flag says — an object past the bytes this emulator reads in one stops the
/// load, and the job reports that rather than a complete feed for data it never read.
#[tokio::test]
async fn a_load_that_stopped_short_of_its_source_does_not_report_that_it_completed() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;
    // One readable object, and one past the bytes this emulator reads in one object of a
    // load: the first is loaded, the second stops the load where it is.
    stage(
        &router,
        "oversized/a-vertices.csv",
        "~id,~label\noversized-v1,Person\n",
    )
    .await;
    let mut oversized = String::from("~id,~label\n");
    while oversized.len() <= MAX_LOAD_OBJECT_BYTES {
        oversized.push_str("oversized-v2,Person\n");
    }
    stage(&router, "oversized/b-vertices.csv", &oversized).await;

    // `failOnError` FALSE, so nothing but the ceiling itself can keep this load from
    // completing.
    let answer = start_load(
        &router,
        json!({
            "source": source_of("oversized/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
            "failOnError": false,
        }),
    )
    .await;
    let case = "an object past the bytes one load reads";
    let load_id = load_id_of(case, &answer);
    let settled = wait_for_terminal_status(&router, case, &load_id, "?errors=true").await;
    assert_eq!(
        reported_status(&settled),
        LOAD_FAILED,
        "a load that stopped short of its source must not report that it completed, got \
         {:?}",
        settled.body
    );
    let logs = settled.body["payload"]["errors"]["errorLogs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        logs.iter()
            .any(|log| log["errorCode"].as_str() == Some("S3_READ_ERROR")),
        "the object it stopped on must be in the job's error log, got {:?}",
        settled.body
    );

    // What it read before stopping persists, and nothing of the object it stopped on was
    // loaded.
    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let store = state.lock();
    assert!(
        store.graph.node("oversized-v1").is_some(),
        "the object the load did read must be loaded"
    );
    assert!(
        store.graph.node("oversized-v2").is_none(),
        "nothing of the object past the ceiling may be loaded"
    );
}

/// RULE: what a load writes is what a later query of this engine's graph reads — the load
/// writes into the one graph every reader answers from.
#[tokio::test]
async fn what_a_load_writes_is_what_a_later_query_reads() {
    let Emulator { router, .. } = emulator();
    create_bucket(&router).await;
    stage(
        &router,
        "query/vertices.csv",
        "~id,~label,name:String\n\
         query-v1,Loaded,Ivan\n\
         query-v2,Loaded,Judy\n",
    )
    .await;
    stage(
        &router,
        "query/edges.csv",
        "~id,~from,~to,~label\n\
         query-e1,query-v1,query-v2,knows\n",
    )
    .await;

    let answer = start_load(
        &router,
        json!({
            "source": source_of("query/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
        }),
    )
    .await;
    let load_id = load_id_of("a load a query reads back", &answer);
    let settled = wait_for_terminal_status(&router, "a load a query reads back", &load_id, "").await;
    assert_eq!(
        reported_status(&settled),
        LOAD_COMPLETED,
        "the load must complete, got {:?}",
        settled.body
    );

    // Read back through `ExecuteOpenCypherQuery`, which answers from the graph this
    // engine holds.
    let rows: &[(&str, Value)] = &[
        (
            "MATCH (n:Loaded) RETURN count(n) AS total",
            json!({ "total": 2 }),
        ),
        (
            "MATCH (n:Loaded) WHERE n.name = 'Ivan' RETURN n.name AS name",
            json!({ "name": "Ivan" }),
        ),
        (
            "MATCH (a:Loaded)-[r:knows]->(b:Loaded) RETURN count(r) AS total",
            json!({ "total": 1 }),
        ),
    ];
    for (query, expected) in rows {
        let answered = call(&router, "POST", "/opencypher", Some(json!({ "query": query }))).await;
        assert_eq!(
            answered.status, 200,
            "case {query:?}: expected 200, got {} with body {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body["results"],
            json!([expected]),
            "case {query:?}: expected {expected:?}, got {:?}",
            answered.body
        );
    }
}

/// RULE: a `loadId` the registry does not hold names no load, so the operations that
/// report on and stop a job each refuse the value the request carried — there is no load
/// to report on or to stop — and nothing is recorded for it.
///
/// The rows differ along the SHAPE of the value: the UUID form the loader mints, another
/// one, a value that is no UUID at all, and one the registry held under a different
/// spelling (ids are matched exactly, so a case change names no load).
#[tokio::test]
async fn an_unknown_load_id_is_refused_by_the_status_and_the_cancel() {
    let Emulator { router, .. } = emulator();
    create_bucket(&router).await;
    stage(
        &router,
        "unknown/vertices.csv",
        "~id,~label\nunknown-v1,Person\n",
    )
    .await;
    // One load that REALLY exists, so the refusals below are about the id and not about an
    // engine that holds no load at all.
    let answer = start_load(
        &router,
        json!({
            "source": source_of("unknown/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
        }),
    )
    .await;
    let issued = load_id_of("an issued load id", &answer);

    let unknown: Vec<String> = vec![
        "9f8f8b6e-0000-4000-8000-00000000abcd".to_string(),
        "00000000-0000-4000-8000-000000000000".to_string(),
        "not-a-load-id".to_string(),
        issued.to_uppercase(),
    ];
    for load_id in &unknown {
        if load_id == &issued {
            continue;
        }
        let status = job_status(&router, load_id, "").await;
        assert_eq!(
            status.status, 400,
            "case {load_id:?}: expected HTTP 400 from GetLoaderJobStatus, got {} with body {:?}",
            status.status, status.body
        );
        assert_eq!(
            status.body["code"].as_str(),
            Some("InvalidParameterException"),
            "case {load_id:?}: expected InvalidParameterException, got {:?}",
            status.body
        );
        let expected_get = format!("The load with id = {load_id} does not exist");
        for member in ["message", "detailedMessage"] {
            assert_eq!(
                status.body[member].as_str(),
                Some(expected_get.as_str()),
                "case {load_id:?}/{member}: expected {expected_get:?}, got {:?}",
                status.body[member]
            );
        }

        let cancelled = cancel_load(&router, load_id).await;
        assert_eq!(
            cancelled.status, 400,
            "case {load_id:?}: expected HTTP 400 from CancelLoaderJob, got {} with body {:?}",
            cancelled.status, cancelled.body
        );
        assert_eq!(
            cancelled.body["code"].as_str(),
            Some("InvalidParameterException"),
            "case {load_id:?}: expected InvalidParameterException, got {:?}",
            cancelled.body
        );
        // A cancel acts on a load the loader still owes an outcome, so its refusal states
        // both readings of what was wrong with the value.
        let expected_cancel = format!("{expected_get} or not active");
        for member in ["message", "detailedMessage"] {
            assert_eq!(
                cancelled.body[member].as_str(),
                Some(expected_cancel.as_str()),
                "case {load_id:?}/{member}: expected {expected_cancel:?}, got {:?}",
                cancelled.body[member]
            );
        }
    }

    // The id the loader really issued is served, and the refusals left it alone.
    let served = job_status(&router, &issued, "").await;
    assert_eq!(
        served.status, 200,
        "the load id the loader issued must be served, got {} with body {:?}",
        served.status, served.body
    );
    assert_eq!(
        served.body["payload"]["overallStatus"]["fullUri"].as_str(),
        Some(source_of("unknown/").as_str()),
        "the served job must report the load it was started for, got {:?}",
        served.body
    );
}

/// RULE: `failOnError` decides what a load does with a record it could not read — "When
/// this parameter is set to FALSE, the loader tries to load all the data in the location
/// specified, skipping any entries with errors. When this parameter is set to TRUE, the
/// loader stops as soon as it encounters an error. Data loaded up to that point persists."
#[tokio::test]
async fn fail_on_error_decides_what_a_bad_record_does_to_the_load() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;
    // The second row's `age:Int` cell is not an integer, so it does not read as the type
    // its column declared; the rows around it are well formed.
    for prefix in ["halting/", "skipping/"] {
        let stem = prefix.trim_end_matches('/');
        let rows = format!(
            "~id,~label,age:Int\n\
             {stem}-v1,Person,30\n\
             {stem}-v2,Person,not-a-number\n\
             {stem}-v3,Person,41\n"
        );
        stage(&router, &format!("{prefix}vertices.csv"), &rows).await;
    }

    // The rows: how the request spells `failOnError`, the status the load must end in, and
    // how many of the three vertices the graph must hold afterwards.
    let cases: [(&str, &str, Option<bool>, &str, usize); 3] = [
        (
            "an absent failOnError, which is TRUE",
            "halting/",
            None,
            LOAD_FAILED,
            1,
        ),
        (
            "failOnError TRUE",
            "halting/",
            Some(true),
            LOAD_FAILED,
            1,
        ),
        (
            "failOnError FALSE",
            "skipping/",
            Some(false),
            LOAD_COMPLETED,
            2,
        ),
    ];

    for (case, prefix, fail_on_error, expected_status, loaded) in cases {
        // Each case loads the SAME source again, so the mode is NEW: a previous load of
        // the source would otherwise mean nothing is reloaded.
        let mut body = json!({
            "source": source_of(prefix),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
            "mode": "NEW",
        });
        if let Some(flag) = fail_on_error {
            body["failOnError"] = json!(flag);
        }
        let answer = start_load(&router, body).await;
        let load_id = load_id_of(case, &answer);
        let settled = wait_for_terminal_status(&router, case, &load_id, "?errors=true").await;
        assert_eq!(
            reported_status(&settled),
            expected_status,
            "case {case:?}: expected {expected_status}, got {:?}",
            settled.body
        );
        let overall = &settled.body["payload"]["overallStatus"];
        assert_eq!(
            overall["datatypeMismatchErrors"].as_u64(),
            Some(1),
            "case {case:?}: the cell that does not read as its declared type is one \
             datatype mismatch, got {:?}",
            settled.body
        );
        let logs = settled.body["payload"]["errors"]["errorLogs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            logs.len(),
            1,
            "case {case:?}: expected one error log entry, got {:?}",
            settled.body
        );
        assert_eq!(
            logs[0]["errorCode"].as_str(),
            Some("DATATYPE_MISMATCH_ERROR"),
            "case {case:?}: expected the datatype-mismatch code, got {:?}",
            logs[0]
        );
        assert_eq!(
            logs[0]["recordNum"].as_u64(),
            Some(3),
            "case {case:?}: expected the row's own position in the file, got {:?}",
            logs[0]
        );
        assert_eq!(
            settled.body["payload"]["errors"]["loadId"].as_str(),
            Some(load_id.as_str()),
            "case {case:?}: the error page names its own load, got {:?}",
            settled.body
        );

        // What was loaded before the error persists, and nothing past it was read when the
        // load halted.
        let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
        let store = state.lock();
        let stem = prefix.trim_end_matches('/');
        let held = ["v1", "v2", "v3"]
            .iter()
            .filter(|suffix| store.graph.node(&format!("{stem}-{suffix}")).is_some())
            .count();
        assert_eq!(
            held, loaded,
            "case {case:?}: expected {loaded} of the three vertices in the graph, got {held}"
        );
    }
}

/// RULE: the `mode` decides whether a load reads its source again — "RESUME ... the loader
/// looks for a previous load from this source, and if it finds one, resumes that load job.
/// If no previous load job is found, the loader stops ... If a previous load job loaded all
/// files from the same source successfully, nothing is reloaded, and the loader returns
/// success. NEW - In NEW mode, the creates a new load request regardless of any previous
/// loads."
#[tokio::test]
async fn the_mode_decides_whether_a_source_is_read_again() {
    let Emulator { router, .. } = emulator();
    create_bucket(&router).await;
    stage(
        &router,
        "mode/vertices.csv",
        "~id,~label\nmode-v1,Person\nmode-v2,Person\n",
    )
    .await;

    // A load of a source nothing has loaded yet reads it, whatever the mode — except
    // RESUME, which has no previous load to resume and stops.
    let resumed = start_load(
        &router,
        json!({
            "source": source_of("mode/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
            "mode": "RESUME",
        }),
    )
    .await;
    let load_id = load_id_of("RESUME with no previous load", &resumed);
    let settled =
        wait_for_terminal_status(&router, "RESUME with no previous load", &load_id, "?errors=true")
            .await;
    assert_eq!(
        reported_status(&settled),
        LOAD_FAILED,
        "case \"RESUME with no previous load\": the loader stops, got {:?}",
        settled.body
    );
    assert_eq!(
        settled.body["payload"]["overallStatus"]["totalRecords"].as_u64(),
        Some(0),
        "case \"RESUME with no previous load\": a load that stopped read nothing, got {:?}",
        settled.body
    );

    // The rows: the mode, the records the load must read, and the run number it reports.
    // The first AUTO load reads the source; the second finds the completed load of the
    // same source and reloads nothing; NEW reads it again regardless.
    let cases: [(&str, Option<&str>, u64, u64); 4] = [
        ("the first load, in the default mode", None, 2, 2),
        ("a second load of the same source", None, 0, 3),
        ("an explicit AUTO load of it", Some("AUTO"), 0, 4),
        ("a NEW load of it", Some("NEW"), 2, 5),
    ];
    for (case, mode, records, run_number) in cases {
        let mut body = json!({
            "source": source_of("mode/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
        });
        if let Some(mode) = mode {
            body["mode"] = json!(mode);
        }
        let answer = start_load(&router, body).await;
        let load_id = load_id_of(case, &answer);
        let settled = wait_for_terminal_status(&router, case, &load_id, "").await;
        assert_eq!(
            reported_status(&settled),
            LOAD_COMPLETED,
            "case {case:?}: expected the load to complete, got {:?}",
            settled.body
        );
        let overall = &settled.body["payload"]["overallStatus"];
        assert_eq!(
            overall["totalRecords"].as_u64(),
            Some(records),
            "case {case:?}: expected {records} records read, got {:?}",
            settled.body
        );
        assert_eq!(
            overall["runNumber"].as_u64(),
            Some(run_number),
            "case {case:?}: expected run {run_number} of this source, got {:?}",
            settled.body
        );
    }
}

/// RULE: a source the loader cannot read is a LOAD that failed — the request is taken on
/// and the job reports what could not be read — and nothing is written into the graph.
///
/// The rows differ along why nothing could be read: a bucket the object store does not
/// hold, a prefix holding no object, and a source that names no Amazon S3 location at all.
#[tokio::test]
async fn a_source_nothing_can_be_read_from_is_a_load_that_failed() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;

    let cases: [(&str, String); 3] = [
        (
            "a bucket the object store does not hold",
            "s3://neptune-loader-absent/data/".to_string(),
        ),
        ("a prefix holding no object", source_of("nothing-here/")),
        (
            "a source that names no Amazon S3 location",
            "not-an-s3-location".to_string(),
        ),
    ];
    for (case, source) in cases {
        let answer = start_load(
            &router,
            json!({
                "source": source,
                "format": "csv",
                "region": REGION,
                "iamRoleArn": ROLE_ARN,
            }),
        )
        .await;
        let load_id = load_id_of(case, &answer);
        let settled = wait_for_terminal_status(&router, case, &load_id, "?errors=true").await;
        assert_eq!(
            reported_status(&settled),
            LOAD_S3_READ_ERROR,
            "case {case:?}: expected the load to report that its source could not be \
             read, got {:?}",
            settled.body
        );
        let logs = settled.body["payload"]["errors"]["errorLogs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            logs.len(),
            1,
            "case {case:?}: expected one error log entry, got {:?}",
            settled.body
        );
        assert_eq!(
            logs[0]["errorCode"].as_str(),
            Some("S3_READ_ERROR"),
            "case {case:?}: expected the read-error code, got {:?}",
            logs[0]
        );
    }

    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let store = state.lock();
    assert_eq!(
        (store.graph.node_count(), store.graph.relationship_count()),
        (0, 0),
        "a load that read nothing must write nothing"
    );
}

/// RULE: `CancelLoaderJob` stops a load the engine still owes an outcome for: the job
/// reports that it was cancelled by its user, and a later cancel of it is refused because
/// there is nothing left to stop.
#[tokio::test]
async fn cancelling_a_load_stops_it_and_the_job_reports_it() {
    let Emulator { router, engine } = emulator();
    // A job the engine owes an outcome for, which is the state a cancel acts on.
    let load_id = "7c1d2f00-0000-4000-8000-00000000000a";
    {
        let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
        let mut store = state.lock();
        let record =
            LoaderJobRecord::admitted(load_id, SystemTime::now(), LoaderJobState::Loading);
        store.loader_jobs.insert(record.load_id.clone(), record);
    }
    assert!(
        listed_ids(&list_jobs(&router, "").await).contains(&load_id.to_string()),
        "the job the loader owes an outcome must be listed"
    );

    let cancelled = cancel_load(&router, load_id).await;
    assert_eq!(
        cancelled.status, 200,
        "expected 200 from CancelLoaderJob, got {} with body {:?}",
        cancelled.status, cancelled.body
    );
    assert_eq!(
        cancelled.body["status"].as_str(),
        Some("200 OK"),
        "the answer reports the status of the cancellation request, got {:?}",
        cancelled.body
    );

    let after = job_status(&router, load_id, "?details=true").await;
    assert_eq!(
        reported_status(&after),
        LOAD_CANCELLED_BY_USER,
        "a cancelled load reports that its user stopped it, got {:?}",
        after.body
    );
    assert!(
        listed_ids(&list_jobs(&router, "").await).contains(&load_id.to_string()),
        "a cancelled load is one the loader keeps, so its id must still be listed"
    );

    // Cancelling it again is refused: there is nothing left to stop, so the value the
    // request carried names no load the loader owes an outcome.
    let again = cancel_load(&router, load_id).await;
    assert_eq!(
        again.status, 400,
        "cancelling a load that has already stopped must be refused, got {} with body {:?}",
        again.status, again.body
    );
    let expected = format!("The load with id = {load_id} does not exist or not active");
    assert_eq!(
        again.body["detailedMessage"].as_str(),
        Some(expected.as_str()),
        "expected {expected:?}, got {:?}",
        again.body
    );
    let unchanged = job_status(&router, load_id, "").await;
    assert_eq!(
        reported_status(&unchanged),
        LOAD_CANCELLED_BY_USER,
        "the second cancel must not change what the job reports, got {:?}",
        unchanged.body
    );
}

/// RULE: reporting on a job changes nothing a later caller sees — the one thing a read
/// materialises is the terminal status the clock had already made due.
#[tokio::test]
async fn reading_a_job_changes_nothing_a_later_caller_sees() {
    let Emulator { router, engine } = emulator();
    create_bucket(&router).await;
    stage(
        &router,
        "stable/vertices.csv",
        "~id,~label\nstable-v1,Person\n",
    )
    .await;
    let answer = start_load(
        &router,
        json!({
            "source": source_of("stable/"),
            "format": "csv",
            "region": REGION,
            "iamRoleArn": ROLE_ARN,
        }),
    )
    .await;
    let load_id = load_id_of("a job read twice", &answer);
    let first = wait_for_terminal_status(&router, "a job read twice", &load_id, "?details=true").await;
    // Reads in between, including the paged error list and the detailed form.
    let _ = job_status(&router, &load_id, "?errors=true&page=1&errorsPerPage=1").await;
    let _ = job_status(&router, &load_id, "").await;
    let again = job_status(&router, &load_id, "?details=true").await;

    assert_eq!(
        again.body["payload"]["overallStatus"], first.body["payload"]["overallStatus"],
        "a read must not change what a later caller sees: {:?} then {:?}",
        first.body, again.body
    );
    let state = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let store = state.lock();
    assert_eq!(
        (store.graph.node_count(), store.loader_jobs.len()),
        (1, 1),
        "the reads must not add or drop anything the engine holds"
    );
}
