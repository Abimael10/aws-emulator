// SPDX-License-Identifier: CC0-1.0

//! `GetNotebookMetadata`, driven end to end over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.GetNotebookMetadata`): "Retrieves notebook
//! metadata for the specified notebook ID".
//!
//! THE RULE UNDER TEST: the read reports the metadata of the notebook its `NotebookId`
//! names out of what the create or the import that minted that notebook persisted — the
//! same record every other reader of the same notebook reports, with no member invented and
//! none left out — and, being a read, leaves what a later caller sees exactly as it was; a
//! `NotebookId` naming no notebook this account holds names nothing to report and is
//! refused as a bad request under the notebook-miss class.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along what
//! the rule generalises over — how the notebook came to exist (a create, an import carrying
//! its document inline, an import from an object-store location), the shape and length of
//! the name it was minted under, the size and character set of the document it holds, and,
//! for the refusals, the shape of an id that names no notebook, including the id of one
//! whose notebook went with the workgroup that held it.
//!
//! The object-store row mounts the emulator's own Amazon S3 beside Athena on one engine,
//! exactly the way the assembled emulator runs them, and stages the notebook file with a
//! real `PutObject` — so that row's notebook is minted from real object bytes.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_NOTEBOOK_NOT_FOUND, NOTEBOOK_TYPE_IPYNB, PYSPARK_ENGINE_VERSION,
};
use service_com_amazonaws_athena_amazonathena::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call};

/// The Region both services are addressed in: Amazon S3 places a bucket created with no
/// `LocationConstraint` in us-east-1, which is also the Region the Athena calls below are
/// signed for.
const REGION: &str = "us-east-1";

/// A SigV4 credential scope naming Amazon S3, which is what routes a request to the object
/// store. Request authentication is off below, so nothing here is verified: the scope is
/// what claims the request.
const S3_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The bucket the staged notebook file lives in.
const BUCKET: &str = "athena-notebook-reads";

/// The Spark enabled workgroup the notebooks read below live in.
const WORK_GROUP: &str = "spark-notebook-reads";

/// A second Spark enabled workgroup, deleted with the notebook it holds: a notebook belongs
/// to the workgroup it was minted in, so an id whose notebook went with that workgroup
/// names no notebook any more.
const DOOMED_WORK_GROUP: &str = "spark-notebook-reads-doomed";

/// How a notebook came to exist — the mint whose persisted record a read must report.
#[derive(Debug, Clone)]
enum Source {
    /// `CreateNotebook`: the empty ipynb document, carried by no part of the request.
    Create,
    /// `ImportNotebook` carrying the ipynb document inline as its `Payload`.
    Inline(String),
    /// `ImportNotebook` naming the object-store key the ipynb document is staged under.
    Location(String),
}

/// One emulator: Athena and Amazon S3 on a single shared engine behind the production
/// router — what the assembled emulator mounts, narrowed to the two services under test.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so a suite does not depend on how the emulator process was started.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One path-style request to the emulator's Amazon S3 front door: the bucket and the key
/// live in the URI and the body is the object's own bytes.
async fn s3(router: &MultiServiceRouter, path: &str, body: &[u8]) -> u16 {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", S3_AUTHORIZATION)
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the request builds");
    router.dispatch(request).await.status().as_u16()
}

/// `PUT /<bucket>` — `CreateBucket` — so the staged notebook file has a bucket to live in.
async fn given_bucket(router: &MultiServiceRouter) {
    let status = s3(router, &format!("/{BUCKET}"), &[]).await;
    assert_eq!(
        status, 200,
        "setup: the notebook bucket must be created, got {status}"
    );
}

/// `PUT /<bucket>/<key>` — `PutObject` — storing `body` as the object's own bytes.
async fn given_object(router: &MultiServiceRouter, key: &str, body: &str) {
    let status = s3(router, &format!("/{BUCKET}/{key}"), body.as_bytes()).await;
    assert_eq!(
        status, 200,
        "setup: the notebook file {key:?} must be stored, got {status}"
    );
}

/// Create the Spark enabled workgroup `name`: a PySpark engine version and the execution
/// role a Spark workgroup accesses user resources through.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Mint the notebook `name` in `work_group` through `source`, and hand back the
/// `NotebookId` the mint answered with.
async fn given_notebook(
    router: &MultiServiceRouter,
    work_group: &str,
    name: &str,
    source: &Source,
) -> String {
    let (operation, request) = match source {
        Source::Create => (
            "CreateNotebook",
            json!({ "WorkGroup": work_group, "Name": name }),
        ),
        Source::Inline(payload) => (
            "ImportNotebook",
            json!({
                "WorkGroup": work_group,
                "Name": name,
                "Type": NOTEBOOK_TYPE_IPYNB,
                "Payload": payload,
            }),
        ),
        Source::Location(key) => (
            "ImportNotebook",
            json!({
                "WorkGroup": work_group,
                "Name": name,
                "Type": NOTEBOOK_TYPE_IPYNB,
                "NotebookS3LocationUri": format!("s3://{BUCKET}/{key}"),
            }),
        ),
    };
    let (status, minted) = call(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "setup: {operation} of {name:?} must answer 200, got {status} with {minted:?}"
    );
    minted["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: {operation} must answer with an id, got {minted:?}"))
        .to_string()
}

/// `GetNotebookMetadata`'s whole answer for `notebook_id`.
async fn read(router: &MultiServiceRouter, notebook_id: &str) -> (u16, Value) {
    call(
        router,
        "GetNotebookMetadata",
        &json!({ "NotebookId": notebook_id }),
    )
    .await
}

/// The `NotebookMetadata` the read reports for `notebook_id`, which must be readable.
async fn metadata(router: &MultiServiceRouter, case: &str, notebook_id: &str) -> Value {
    let (status, body) = read(router, notebook_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetNotebookMetadata({notebook_id}) must answer 200, got {status} with \
         {body:?}"
    );
    body["NotebookMetadata"].clone()
}

/// `ExportNotebook`'s view of the same notebook: the metadata it reports beside the content
/// it holds.
async fn exported(router: &MultiServiceRouter, case: &str, notebook_id: &str) -> Value {
    let (status, body) = call(
        router,
        "ExportNotebook",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ExportNotebook({notebook_id}) must answer 200, got {status} with \
         {body:?}"
    );
    body
}

/// An id of the modelled `NotebookId` form that differs from `id` in exactly one character,
/// so it names no notebook while staying inside the pattern
/// (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
fn differing_in_one_character(id: &str) -> String {
    let mut characters: Vec<char> = id.chars().collect();
    if let Some(last) = characters.last_mut() {
        *last = if *last == 'a' { 'b' } else { 'a' };
    }
    characters.into_iter().collect()
}

/// RULE: the read reports the record the mint persisted — however the notebook came to
/// exist — and reports it the way every other reader of that notebook does.
#[tokio::test]
async fn a_read_reports_the_record_the_mint_persisted() {
    let router = emulator();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_bucket(&router).await;

    // The document each imported row carries: one plain ipynb file, one whose text is
    // outside ASCII, one of a hundred thousand bytes, and the staged one the object-store
    // row imports from.
    let plain = "{\"cells\": [{\"cell_type\": \"code\", \"source\": [\"spark.sql('SELECT 1')\"]}], \
                 \"metadata\": {}, \"nbformat\": 4, \"nbformat_minor\": 5}";
    let outside_ascii = "{\"cells\": [{\"cell_type\": \"markdown\", \"source\": [\"# 分析ノート\"]}]}";
    let large = format!("{{\"cells\": [\"{}\"]}}", "a".repeat(100_000));
    let staged = "{\"cells\": [\"staged in the object store\"], \"nbformat\": 4}";
    given_object(&router, "notebooks/staged.ipynb", staged).await;

    // The names span the modelled `NotebookName` (1 to 255 characters, any character but
    // `/`, `:` and `\`): a plain one, the floor, the ceiling, and one carrying a space.
    let longest = "n".repeat(255);
    let rows: Vec<(&str, String, Source, String)> = vec![
        (
            "a create, which stores the empty ipynb document",
            "created.ipynb".to_string(),
            Source::Create,
            state::EMPTY_IPYNB.to_string(),
        ),
        (
            "a create under a one-character name",
            "n".to_string(),
            Source::Create,
            state::EMPTY_IPYNB.to_string(),
        ),
        (
            "a create under the longest name the model admits",
            longest.clone(),
            Source::Create,
            state::EMPTY_IPYNB.to_string(),
        ),
        (
            "an import carrying its document inline",
            "imported inline.ipynb".to_string(),
            Source::Inline(plain.to_string()),
            plain.to_string(),
        ),
        (
            "an import of a document outside ASCII",
            "imported-wide.ipynb".to_string(),
            Source::Inline(outside_ascii.to_string()),
            outside_ascii.to_string(),
        ),
        (
            "an import of a document of a hundred thousand bytes",
            "imported-large.ipynb".to_string(),
            Source::Inline(large.clone()),
            large.clone(),
        ),
        (
            "an import from an object-store location",
            "imported-from-s3.ipynb".to_string(),
            Source::Location("notebooks/staged.ipynb".to_string()),
            staged.to_string(),
        ),
    ];

    // Every row's first read, kept so the reads the later rows make can be shown to have
    // changed none of them.
    let mut first_reads: Vec<(String, String, Value)> = Vec::new();

    for (case, name, source, content) in &rows {
        let notebook_id = given_notebook(&router, WORK_GROUP, name, source).await;

        let reported = metadata(&router, case, &notebook_id).await;
        assert_eq!(
            reported["NotebookId"].as_str(),
            Some(notebook_id.as_str()),
            "case {case:?}: the read reports the notebook the request named, got {reported:?}"
        );
        assert_eq!(
            reported["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the read reports the name the mint carried, got {reported:?}"
        );
        assert_eq!(
            reported["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {case:?}: the read reports the workgroup the notebook was minted in, got \
             {reported:?}"
        );
        assert_eq!(
            reported["Type"].as_str(),
            Some(NOTEBOOK_TYPE_IPYNB),
            "case {case:?}: expected the only notebook type the model declares \
             ({NOTEBOOK_TYPE_IPYNB}), got {reported:?}"
        );
        let creation_time = reported["CreationTime"].as_f64();
        assert!(
            creation_time.is_some(),
            "case {case:?}: the read reports when the notebook was created, got {reported:?}"
        );
        assert_eq!(
            reported["LastModifiedTime"].as_f64(),
            creation_time,
            "case {case:?}: a notebook nothing has written to was last modified when it was \
             created, got {reported:?}"
        );

        // One record, every reader: the export of the same notebook reports the same
        // metadata beside the content the mint stored.
        let export = exported(&router, case, &notebook_id).await;
        assert_eq!(
            export["NotebookMetadata"], reported,
            "case {case:?}: the export and the metadata read must report one record, got \
             {export:?}"
        );
        assert_eq!(
            export["Payload"].as_str(),
            Some(content.as_str()),
            "case {case:?}: the notebook must hold the document its mint stored ({} bytes), \
             got {} bytes",
            content.len(),
            export["Payload"].as_str().unwrap_or_default().len()
        );

        // A read changes nothing: reading again reports the same record, member for member.
        let again = metadata(&router, case, &notebook_id).await;
        assert_eq!(
            again, reported,
            "case {case:?}: a second read must report the same record, got {again:?}"
        );

        first_reads.push((case.to_string(), notebook_id, reported));
    }

    // Nothing any of those reads did changed what a later caller sees: every notebook still
    // reports the record its first read reported.
    for (case, notebook_id, reported) in &first_reads {
        assert_eq!(
            &metadata(&router, case, notebook_id).await,
            reported,
            "case {case:?}: the reads of the other notebooks must leave this one as it was"
        );
    }
}

/// RULE: a `NotebookId` naming no notebook this account holds names nothing to report, and
/// the refusal leaves the notebooks it does hold as they were.
#[tokio::test]
async fn a_read_needs_a_notebook_this_account_holds() {
    let router = emulator();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, DOOMED_WORK_GROUP).await;

    let held = given_notebook(&router, WORK_GROUP, "held.ipynb", &Source::Create).await;
    let before = metadata(&router, "the notebook this account holds", &held).await;

    // A notebook of another workgroup, which goes with that workgroup when it is deleted:
    // a notebook belongs to the workgroup it was minted in.
    let doomed = given_notebook(
        &router,
        DOOMED_WORK_GROUP,
        "doomed.ipynb",
        &Source::Inline("{\"cells\": []}".to_string()),
    )
    .await;
    let (status, deleted) = call(
        &router,
        "DeleteWorkGroup",
        &json!({ "WorkGroup": DOOMED_WORK_GROUP, "RecursiveDeleteOption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the workgroup holding the doomed notebook must be deleted, got {status} with \
         {deleted:?}"
    );

    // The rows differ along the SHAPE of an id of the modelled form that names no notebook:
    // a freshly minted one, the all-zero one, the all-f one, one character away from an id
    // this account does hold, and one whose notebook was removed with its workgroup.
    let misses = [
        (
            "an id no notebook was minted under",
            state::new_notebook_id(),
        ),
        (
            "the all-zero id of the modelled form",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "the all-f id of the modelled form",
            "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
        ),
        (
            "an id one character away from a held one",
            differing_in_one_character(&held),
        ),
        (
            "the id of a notebook removed with its workgroup",
            doomed,
        ),
    ];

    for (case, missing) in &misses {
        let (status, refused) = read(&router, missing).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_NOTEBOOK_NOT_FOUND,
            &state::notebook_not_found(missing),
        );
    }

    // No refusal wrote anything: the notebook this account does hold still reports the
    // record it reported before them.
    assert_eq!(
        metadata(&router, "the notebook this account holds", &held).await,
        before,
        "a refused read must leave the notebook this account holds exactly as it was"
    );
}
