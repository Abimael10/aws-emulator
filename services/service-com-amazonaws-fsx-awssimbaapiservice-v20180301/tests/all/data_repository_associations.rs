// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FSx for Lustre data-repository-association slice, driven over the
//! wire.
//!
//! RULE: the S3 bucket a `DataRepositoryPath` names is Amazon S3's resource, so
//! `CreateDataRepositoryAssociation` resolves it where S3 holds it before it will accept the
//! association: a path whose bucket S3 reports no bucket for is refused `BadRequest` at HTTP
//! 400 with the fixed message for that class and stores NOTHING, while a path whose bucket
//! the account really holds is persisted in `CREATING` and read back by
//! `DescribeDataRepositoryAssociations`.
//!
//! So this suite mounts Amazon S3 beside Amazon FSx on ONE engine — what the assembled
//! emulator does — and creates its buckets through S3's own front door, so what the refusal
//! is grounded in is S3's answer rather than anything this test seeded. The tables span what
//! the rule generalises over: the form of the path (bucket only, bucket and prefix, a deep
//! prefix), the bucket name (length and the characters S3 admits), and whether the account
//! holds the bucket at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::file_system_lifecycle::{
    call, ec2_client, ok, with_client_request_token, PAST_TRANSITION, REGION,
};
use crate::not_found_messages::error_of;

/// How long a wait below gives a clock-owed removal to be materialised: far longer than the
/// service's own transition span, so what the wait is bounded by is the CONDITION and not how
/// fast the host runs.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(60);
/// How long a wait below pauses between two looks.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header in the `s3` scope — the routing signal that picks the S3
/// service for a request to the bucket front door.
const S3_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDTESTS3/20260101/us-west-2/s3/\
                       aws4_request, SignedHeaders=host, Signature=abcdef";

/// The modelled `DataRepositoryLifecycle` values this suite observes
/// (`smithy_query(query="shape", shape="DataRepositoryLifecycle")`).
const CREATING: &str = "CREATING";
/// See [`CREATING`].
const AVAILABLE: &str = "AVAILABLE";

/// The FIXED message Amazon FSx refuses an association whose S3 bucket it cannot validate
/// access to with. Entirely fixed text for this error class: no part of it is a value from
/// the request.
const UNVALIDATED_BUCKET: &str =
    "Amazon FSx is unable to validate the access to the S3 bucket prefix. S3 bucket does not \
     exist.";

/// An emulator holding Amazon FSx, the owner of subnets and the owner of buckets over ONE
/// shared engine, plus the subnets the account has in [`REGION`] as EC2 reports them.
struct Emulator {
    router: MultiServiceRouter,
    subnet: String,
}

/// Stand the three services up on one engine and read the account's subnets back from the
/// service that owns them.
async fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not the subject of these tests; the requests still carry the SigV4
    // credential scope, which is what routes them and names the region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_fsx_awssimbaapiservice_v20180301::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));

    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()), REGION);
    let described = ec2
        .describe_subnets()
        .send()
        .await
        .expect("EC2 must report the account's subnets");
    let subnet = described
        .subnets()
        .iter()
        .filter(|subnet| {
            subnet
                .availability_zone()
                .is_some_and(|zone| zone.starts_with(REGION))
        })
        .find_map(|subnet| subnet.subnet_id().map(str::to_string))
        .expect("the account must have a subnet in the region these requests are signed for");
    Emulator { router, subnet }
}

/// `PUT /<bucket>` against the emulator's S3 front door — `CreateBucket` in [`REGION`],
/// path-style because the emulator serves one endpoint per Region. Answers the HTTP status.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) -> u16 {
    let body = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://s3.{REGION}.amazonaws.com/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", S3_AUTH)
        .body(bytes::Bytes::from(body))
        .expect("build the CreateBucket request");
    router.dispatch(request).await.status().as_u16()
}

/// Create an FSx for Lustre file system a data repository association is supported on
/// ("supported on all FSx for Lustre 2.12 and 2.15 file systems, excluding scratch_1
/// deployment type") and answer its `FileSystemId`.
async fn lustre_file_system(emulator: &Emulator) -> String {
    create_file_system(
        emulator,
        &json!({
            "FileSystemType": "LUSTRE",
            "FileSystemTypeVersion": "2.12",
            "StorageCapacity": 1200,
            "SubnetIds": [emulator.subnet],
            "LustreConfiguration": { "DeploymentType": "SCRATCH_2" },
        }),
    )
    .await
}

/// Create the file system `body` describes and answer its `FileSystemId`.
async fn create_file_system(emulator: &Emulator, body: &Value) -> String {
    let created = ok(
        &emulator.router,
        "CreateFileSystem",
        &with_client_request_token(body),
    )
    .await;
    created["FileSystem"]["FileSystemId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `CreateDataRepositoryAssociation`, as `(status, body)`.
async fn create_association(
    router: &MultiServiceRouter,
    file_system_id: &str,
    file_system_path: &str,
    data_repository_path: &str,
) -> (u16, Value) {
    call(
        router,
        "CreateDataRepositoryAssociation",
        &json!({
            "FileSystemId": file_system_id,
            "FileSystemPath": file_system_path,
            "DataRepositoryPath": data_repository_path,
            "BatchImportMetaDataOnCreate": false,
        }),
    )
    .await
}

/// The associations `DescribeDataRepositoryAssociations` reports for one file system, through
/// the `file-system-id` filter the operation documents.
async fn associations_of(router: &MultiServiceRouter, file_system_id: &str) -> Vec<Value> {
    let listed = ok(
        router,
        "DescribeDataRepositoryAssociations",
        &json!({ "Filters": [{ "Name": "file-system-id", "Values": [file_system_id] }] }),
    )
    .await;
    listed["Associations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The wire error code of a response: `__type`, reduced to the shape name an SDK matches on.
fn error_code(body: &Value) -> &str {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The modelled `DataRepositoryAssociationId` pattern `^(dra-[0-9a-f]{8,})$`, with
/// `length 13..23`.
fn is_well_formed_association_id(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("dra-") else {
        return false;
    };
    (13..=23).contains(&id.len())
        && hex.len() >= 8
        && hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// RULE: a `DataRepositoryPath` whose bucket Amazon S3 holds no bucket for is refused with
/// `BadRequest` at HTTP 400 and the fixed message for that class, and the refused association
/// is never created.
///
/// The rows differ along what the rule generalises over: a plain bucket name, a name with a
/// prefix after it, a name at the published three-character minimum, a name with the dots and
/// hyphens S3 admits — and two segments S3 could hold no bucket under at all, which name no
/// bucket anywhere and so fall in the same class.
#[tokio::test]
async fn a_path_whose_bucket_s3_does_not_hold_is_refused_and_stores_nothing() {
    let emulator = emulator().await;
    let file_system_id = lustre_file_system(&emulator).await;

    let cases = [
        "s3://never-created-bucket",
        "s3://never-created-bucket/prefix/",
        "s3://abc",
        "s3://my.absent-repo-01/a/b/c/",
        // Outside the published general-purpose bucket naming rules, so no object store
        // anywhere holds a bucket under either.
        "s3://UPPERCASE",
        "s3://ab",
    ];
    for (index, data_repository_path) in cases.iter().enumerate() {
        let (status, answered) = create_association(
            &emulator.router,
            &file_system_id,
            &format!("/absent{index}"),
            data_repository_path,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {data_repository_path:?}: expected HTTP 400, got {status} with body {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "BadRequest",
            "case {data_repository_path:?}: expected Error.Code BadRequest, got body {answered}"
        );
        assert_eq!(
            answered["Message"].as_str().unwrap_or_default(),
            UNVALIDATED_BUCKET,
            "case {data_repository_path:?}: expected the fixed message for this class, got {:?}",
            answered["Message"]
        );
        assert_eq!(
            associations_of(&emulator.router, &file_system_id).await,
            Vec::<Value>::new(),
            "case {data_repository_path:?}: a refused association must never be created"
        );
    }

    // A path that is not an S3 bucket or prefix at all is a different refusal of the same
    // operation, and likewise stores nothing.
    for data_repository_path in ["/just/a/path", "nfs://filer.example/export"] {
        let (status, answered) = create_association(
            &emulator.router,
            &file_system_id,
            "/malformed",
            data_repository_path,
        )
        .await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "BadRequest"),
            "case {data_repository_path:?}: expected 400 BadRequest, got {status} with body \
             {answered}"
        );
        assert!(
            answered["Message"]
                .as_str()
                .unwrap_or_default()
                .contains("s3://bucket-name/prefix/"),
            "case {data_repository_path:?}: the refusal must state the documented format, got \
             {:?}",
            answered["Message"]
        );
    }
    assert_eq!(
        associations_of(&emulator.router, &file_system_id).await,
        Vec::<Value>::new(),
        "no refused call may leave an association behind"
    );
}

/// RULE: a `DataRepositoryPath` whose bucket the account really holds is accepted, persisted
/// in `CREATING` with the documented defaults materialised, and read back by
/// `DescribeDataRepositoryAssociations` — by `file-system-id` filter and by `AssociationIds`
/// alike. The clock then owes the association its way to `AVAILABLE`.
#[tokio::test]
async fn an_association_for_a_bucket_the_account_holds_is_persisted_and_read_back() {
    let emulator = emulator().await;
    let file_system_id = lustre_file_system(&emulator).await;

    // (bucket, path, file-system directory): the bucket names differ in length and in the
    // characters S3 admits, and the paths differ in whether and how deep a prefix follows.
    let cases: &[(&str, &str, &str)] = &[
        ("fsx", "s3://fsx", "/data"),
        ("fsx-lustre-repository", "s3://fsx-lustre-repository/", "/ns1"),
        (
            "my.repo-01",
            "s3://my.repo-01/imports/2026/",
            "/ns2/subdir",
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for case in cases {
        let (bucket, data_repository_path, file_system_path) = *case;
        assert_eq!(
            create_bucket(&emulator.router, bucket).await,
            200,
            "case {case:?}: the account must hold the bucket before FSx is asked"
        );
        let (status, answered) = create_association(
            &emulator.router,
            &file_system_id,
            file_system_path,
            data_repository_path,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {answered}"
        );
        let association = answered["Association"].clone();
        let association_id = association["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            is_well_formed_association_id(&association_id),
            "case {case:?}: expected an AssociationId matching ^dra-[0-9a-f]{{8,}}$ of length \
             13..23, got {association_id:?}"
        );
        assert!(
            !minted.contains(&association_id),
            "case {case:?}: expected a fresh AssociationId, got the already-minted \
             {association_id:?}"
        );
        assert_eq!(
            association["Lifecycle"], json!(CREATING),
            "case {case:?}: expected Lifecycle {CREATING} on create, got {:?}",
            association["Lifecycle"]
        );
        assert_eq!(
            association["DataRepositoryPath"], json!(data_repository_path),
            "case {case:?}: expected the DataRepositoryPath the request named, got {:?}",
            association["DataRepositoryPath"]
        );
        assert_eq!(
            association["FileSystemPath"], json!(file_system_path),
            "case {case:?}: expected the FileSystemPath the request named, got {:?}",
            association["FileSystemPath"]
        );
        assert_eq!(
            association["FileSystemId"], json!(file_system_id),
            "case {case:?}: expected the FileSystemId the request named, got {:?}",
            association["FileSystemId"]
        );
        assert_eq!(
            association["ResourceARN"],
            json!(format!(
                "arn:aws:fsx:{REGION}:000000000000:association/{association_id}"
            )),
            "case {case:?}: expected the association ARN for the signed region, got {:?}",
            association["ResourceARN"]
        );
        assert_eq!(
            association["BatchImportMetaDataOnCreate"], json!(false),
            "case {case:?}: expected the stated BatchImportMetaDataOnCreate, got {:?}",
            association["BatchImportMetaDataOnCreate"]
        );
        // "The default chunk size is 1,024 MiB (1 GiB)"
        // (`CreateDataRepositoryAssociation$ImportedFileChunkSize`).
        assert_eq!(
            association["ImportedFileChunkSize"], json!(1024),
            "case {case:?}: expected the documented default ImportedFileChunkSize, got {:?}",
            association["ImportedFileChunkSize"]
        );
        assert!(
            association["CreationTime"].as_f64().unwrap_or_default() > 0.0,
            "case {case:?}: expected a CreationTime, got {:?}",
            association["CreationTime"]
        );

        // The persisted record, read back by id through the implemented reader.
        let by_id = ok(
            &emulator.router,
            "DescribeDataRepositoryAssociations",
            &json!({ "AssociationIds": [association_id] }),
        )
        .await;
        assert_eq!(
            by_id["Associations"],
            json!([association]),
            "case {case:?}: DescribeDataRepositoryAssociations must report the stored record, \
             got {by_id}"
        );
        minted.push(association_id);
    }

    // And the `file-system-id` filter reports exactly the associations of that file system.
    let mut listed: Vec<String> = associations_of(&emulator.router, &file_system_id)
        .await
        .iter()
        .map(|association| {
            association["AssociationId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    listed.sort();
    let mut expected = minted.clone();
    expected.sort();
    assert_eq!(
        listed, expected,
        "the file-system-id filter must report exactly the stored associations"
    );

    // The clock owes each association its way out of CREATING.
    tokio::time::sleep(PAST_TRANSITION).await;
    for association in associations_of(&emulator.router, &file_system_id).await {
        assert_eq!(
            association["Lifecycle"], json!(AVAILABLE),
            "case {:?}: expected Lifecycle {AVAILABLE} once the transition fell due, got {:?}",
            association["AssociationId"], association["Lifecycle"]
        );
    }
}

/// RULE: a create names a file system the account HAS, of a type and deployment type that can
/// hold an association, and a file-system directory no other association already links.
#[tokio::test]
async fn a_create_is_refused_for_a_file_system_that_cannot_hold_the_association() {
    let emulator = emulator().await;
    let bucket = "fsx-refusal-cases";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket, so no case below is refused for the bucket"
    );
    let path = format!("s3://{bucket}");

    // An id matching the modelled `FileSystemId` pattern that names no file system: the
    // request itself is wrong, which this operation reports as `BadRequest` (the table over
    // that rule is `an_association_for_a_file_system_the_account_lacks_is_a_bad_request`).
    let (status, answered) = create_association(
        &emulator.router,
        "fs-0000000000000dead",
        "/data",
        &path,
    )
    .await;
    assert_eq!(
        (status, error_code(&answered)),
        (400, "BadRequest"),
        "case unknown file system: expected 400 BadRequest, got {status} with body \
         {answered}"
    );

    // (case, create body) rows: every file system that cannot hold a data repository
    // association — the three other modelled `FileSystemType`s, and the one Lustre deployment
    // type the operation excludes.
    let unsupported: &[(&str, Value)] = &[
        (
            "WINDOWS",
            json!({
                "FileSystemType": "WINDOWS",
                "StorageCapacity": 64,
                "SubnetIds": [emulator.subnet],
                "WindowsConfiguration": { "ThroughputCapacity": 32 },
            }),
        ),
        (
            "ONTAP",
            json!({
                "FileSystemType": "ONTAP",
                "StorageCapacity": 1024,
                "SubnetIds": [emulator.subnet],
                "OntapConfiguration": { "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 128 },
            }),
        ),
        (
            "OPENZFS",
            json!({
                "FileSystemType": "OPENZFS",
                "StorageCapacity": 64,
                "SubnetIds": [emulator.subnet],
                "OpenZFSConfiguration": { "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 64 },
            }),
        ),
        (
            "LUSTRE SCRATCH_1",
            json!({
                "FileSystemType": "LUSTRE",
                "FileSystemTypeVersion": "2.12",
                "StorageCapacity": 1200,
                "SubnetIds": [emulator.subnet],
                "LustreConfiguration": { "DeploymentType": "SCRATCH_1" },
            }),
        ),
    ];
    for (case, body) in unsupported {
        let id = create_file_system(&emulator, body).await;
        let (status, answered) =
            create_association(&emulator.router, &id, "/data", &path).await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "UnsupportedOperation"),
            "case {case:?}: expected 400 UnsupportedOperation, got {status} with body {answered}"
        );
        assert_eq!(
            associations_of(&emulator.router, &id).await,
            Vec::<Value>::new(),
            "case {case:?}: a refused association must never be created"
        );
    }

    // "Two data repository associations cannot have overlapping file system paths. For
    // example, if a data repository is associated with file system path /ns1/, then you
    // cannot link another data repository with file system path /ns1/ns2."
    let lustre = lustre_file_system(&emulator).await;
    let (status, answered) = create_association(&emulator.router, &lustre, "/ns1/", &path).await;
    assert_eq!(
        status, 200,
        "the first association must be accepted, got {status} with body {answered}"
    );
    for overlapping in ["/ns1", "/ns1/", "/ns1/ns2", "/"] {
        let (status, answered) =
            create_association(&emulator.router, &lustre, overlapping, &path).await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "BadRequest"),
            "case {overlapping:?}: expected 400 BadRequest for an overlapping file system \
             path, got {status} with body {answered}"
        );
    }
    // A directory that does not overlap it is accepted, so the refusal is about the overlap
    // and not about the file system already having an association.
    let (status, answered) = create_association(&emulator.router, &lustre, "/ns10", &path).await;
    assert_eq!(
        status, 200,
        "case \"/ns10\": a non-overlapping directory must be accepted, got {status} with body \
         {answered}"
    );
    assert_eq!(
        associations_of(&emulator.router, &lustre).await.len(),
        2,
        "exactly the two accepted associations must be stored"
    );
}

/// RULE: the listing selects by the filter names the operation documents and refuses any
/// other; every SELECTOR it carries (a filter value, an `AssociationIds` entry) selects, so
/// one matching nothing narrows the page instead of refusing the read.
#[tokio::test]
async fn the_listing_selects_only_by_the_documented_filters() {
    let emulator = emulator().await;
    let file_system_id = lustre_file_system(&emulator).await;
    let bucket = "fsx-listing-repository";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket"
    );
    let (status, _) = create_association(
        &emulator.router,
        &file_system_id,
        "/data",
        &format!("s3://{bucket}/exports/"),
    )
    .await;
    assert_eq!(status, 200, "the association under test must be created");

    // Every association this service stores links an S3 data repository, so the documented
    // `data-repository-type` filter selects it under `S3` and nothing under `NFS`.
    let cases: &[(&str, usize)] = &[("S3", 1), ("NFS", 0)];
    for case in cases {
        let (data_repository_type, expected) = *case;
        let listed = ok(
            &emulator.router,
            "DescribeDataRepositoryAssociations",
            &json!({
                "Filters": [{ "Name": "data-repository-type", "Values": [data_repository_type] }],
            }),
        )
        .await;
        assert_eq!(
            listed["Associations"].as_array().map(Vec::len),
            Some(expected),
            "case {case:?}: expected {expected} association(s), got {listed}"
        );
    }

    // A type no stored association carries is a selector that matches nothing, not a
    // refusal.
    let listed = ok(
        &emulator.router,
        "DescribeDataRepositoryAssociations",
        &json!({ "Filters": [{ "Name": "data-repository-type", "Values": ["GLACIER"] }] }),
    )
    .await;
    assert_eq!(
        listed["Associations"],
        json!([]),
        "case \"GLACIER\": expected an empty page, got {listed}"
    );

    // A filter name this listing does not select by is a client error, not an empty page.
    let (status, answered) = call(
        &emulator.router,
        "DescribeDataRepositoryAssociations",
        &json!({ "Filters": [{ "Name": "volume-id", "Values": ["fsvol-00000000000000001"] }] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered)),
        (400, "BadRequest"),
        "case \"volume-id\": expected 400 BadRequest for a filter name this listing does not \
         select by, got {status} with body {answered}"
    );

    // An id the store holds no association for selects nothing, so it narrows the page
    // rather than refusing the read: alone it leaves the page empty, and beside the stored
    // association it leaves just that one.
    let listed = ok(
        &emulator.router,
        "DescribeDataRepositoryAssociations",
        &json!({ "AssociationIds": ["dra-0000000000000dead"] }),
    )
    .await;
    assert_eq!(
        listed["Associations"],
        json!([]),
        "case unknown association: expected an empty page, got {listed}"
    );
    let stored = associations_of(&emulator.router, &file_system_id).await;
    let stored_id = stored[0]["AssociationId"].clone();
    let listed = ok(
        &emulator.router,
        "DescribeDataRepositoryAssociations",
        &json!({ "AssociationIds": ["dra-0000000000000dead", stored_id] }),
    )
    .await;
    assert_eq!(
        listed["Associations"].as_array().map(Vec::len),
        Some(1),
        "case one stored and one absent id: expected just the stored association, got {listed}"
    );

    // An unfiltered listing reports what the store holds.
    let all = ok(
        &emulator.router,
        "DescribeDataRepositoryAssociations",
        &json!({}),
    )
    .await;
    assert_eq!(
        all["Associations"].as_array().map(Vec::len),
        Some(1),
        "an unfiltered listing must report exactly the stored associations, got {all}"
    );
}

/// RULE: deleting an association unlinks it — the delete answers with the association's id
/// and its new `DELETING` lifecycle, a read reports the same lifecycle, and the clock then
/// removes the record so the listing no longer reports it. An id the store holds nothing for
/// is `DataRepositoryAssociationNotFound`.
#[tokio::test]
async fn deleting_an_association_unlinks_it_and_the_clock_removes_it() {
    let emulator = emulator().await;
    let file_system_id = lustre_file_system(&emulator).await;
    let bucket = "fsx-deletion-repository";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket"
    );

    // Rows: the flag the delete states, which the response reports back and whose documented
    // default is false.
    let cases: &[(&str, Option<bool>, bool)] = &[
        ("/keep-data", None, false),
        ("/drop-data", Some(true), true),
        ("/keep-data-explicitly", Some(false), false),
    ];
    for case in cases {
        let (file_system_path, delete_data, expected_flag) = *case;
        let (status, created) = create_association(
            &emulator.router,
            &file_system_id,
            file_system_path,
            &format!("s3://{bucket}{file_system_path}/"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the association under test must be created, got {created}"
        );
        let association_id = created["Association"]["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let mut body = json!({ "AssociationId": association_id });
        if let Some(flag) = delete_data {
            body["DeleteDataInFileSystem"] = json!(flag);
        }
        let deleted = ok(
            &emulator.router,
            "DeleteDataRepositoryAssociation",
            &body,
        )
        .await;
        assert_eq!(
            deleted["AssociationId"], json!(association_id),
            "case {case:?}: expected the deleted AssociationId, got {:?}",
            deleted["AssociationId"]
        );
        assert_eq!(
            deleted["Lifecycle"], json!("DELETING"),
            "case {case:?}: expected Lifecycle DELETING, got {:?}",
            deleted["Lifecycle"]
        );
        assert_eq!(
            deleted["DeleteDataInFileSystem"], json!(expected_flag),
            "case {case:?}: expected DeleteDataInFileSystem {expected_flag}, got {:?}",
            deleted["DeleteDataInFileSystem"]
        );

        // The stored record reports the same lifecycle until the clock removes it.
        let described = ok(
            &emulator.router,
            "DescribeDataRepositoryAssociations",
            &json!({ "AssociationIds": [association_id] }),
        )
        .await;
        assert_eq!(
            described["Associations"][0]["Lifecycle"], json!("DELETING"),
            "case {case:?}: a read before the removal falls due must report DELETING, got \
             {described}"
        );

        // The clock then removes the record. Waited for by looking again until the listing
        // reports it gone, so what bounds the wait is the CONDITION and not how fast the host
        // runs.
        let started = Instant::now();
        let listed = loop {
            let listed = ok(
                &emulator.router,
                "DescribeDataRepositoryAssociations",
                &json!({ "AssociationIds": [association_id] }),
            )
            .await;
            if listed["Associations"].as_array().is_some_and(Vec::is_empty) {
                break listed;
            }
            assert!(
                started.elapsed() < REMOVAL_DEADLINE,
                "case {case:?}: expected the association {association_id} to be removed, last \
                 read {listed}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        };
        assert_eq!(
            listed["Associations"],
            json!([]),
            "case {case:?}: the removed association must be gone, got {listed}"
        );
        assert_eq!(
            associations_of(&emulator.router, &file_system_id).await,
            Vec::<Value>::new(),
            "case {case:?}: the listing must no longer report the removed association"
        );
    }

    // An id that satisfies the modelled pattern but names no association.
    let (status, answered) = call(
        &emulator.router,
        "DeleteDataRepositoryAssociation",
        &json!({ "AssociationId": "dra-0000000000000dead" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered)),
        (400, "DataRepositoryAssociationNotFound"),
        "case unknown association: expected 400 DataRepositoryAssociationNotFound, got {status} \
         with body {answered}"
    );
}

/// RULE: a delete is not gated on the file system's lifecycle — `DeleteFileSystem` documents
/// exactly one 400 for it (a data repository TASK in `PENDING` or `EXECUTING`, which this
/// service stores none of) and otherwise "returns while the file system has the `DELETING`
/// status" — and the associations of a deleted file system go with it.
#[tokio::test]
async fn a_file_system_is_deleted_whatever_lifecycle_it_is_in_and_its_associations_go_with_it() {
    let emulator = emulator().await;
    let bucket = "fsx-lifecycle-repository";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket"
    );

    // Rows: the lifecycle the file system is in when the delete arrives — straight after the
    // create, while the clock still owes it `AVAILABLE`, and once it has settled there.
    for settle_first in [false, true] {
        let file_system_id = lustre_file_system(&emulator).await;
        let (status, created) = create_association(
            &emulator.router,
            &file_system_id,
            "/data",
            &format!("s3://{bucket}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case settle_first={settle_first}: the association under test must be created, got \
             {created}"
        );
        if settle_first {
            tokio::time::sleep(PAST_TRANSITION).await;
        }

        let deleted = ok(
            &emulator.router,
            "DeleteFileSystem",
            &json!({ "FileSystemId": file_system_id }),
        )
        .await;
        assert_eq!(
            deleted["Lifecycle"], json!("DELETING"),
            "case settle_first={settle_first}: expected Lifecycle DELETING, got {:?}",
            deleted["Lifecycle"]
        );

        tokio::time::sleep(PAST_TRANSITION).await;
        assert_eq!(
            associations_of(&emulator.router, &file_system_id).await,
            Vec::<Value>::new(),
            "case settle_first={settle_first}: the associations of a removed file system must \
             go with it"
        );
    }
}

/// RULE: `ClientRequestToken` makes the create idempotent — a retry carrying the same token
/// and the same parameters replays the first call's association instead of minting a second,
/// and one carrying the same token with different parameters is
/// `IncompatibleParameterError`.
#[tokio::test]
async fn a_client_request_token_replays_the_association_it_created() {
    let emulator = emulator().await;
    let file_system_id = lustre_file_system(&emulator).await;
    let bucket = "fsx-idempotent-repository";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket"
    );
    let body = json!({
        "FileSystemId": file_system_id,
        "FileSystemPath": "/data",
        "DataRepositoryPath": format!("s3://{bucket}"),
        "ClientRequestToken": "association-token",
    });

    let first = ok(
        &emulator.router,
        "CreateDataRepositoryAssociation",
        &body,
    )
    .await;
    let replay = ok(
        &emulator.router,
        "CreateDataRepositoryAssociation",
        &body,
    )
    .await;
    assert_eq!(
        replay["Association"]["AssociationId"], first["Association"]["AssociationId"],
        "case same token, same parameters: expected the first call's AssociationId, got {:?}",
        replay["Association"]["AssociationId"]
    );
    assert_eq!(
        associations_of(&emulator.router, &file_system_id).await.len(),
        1,
        "a replayed create must not mint a second association"
    );

    let mut changed = body.clone();
    changed["FileSystemPath"] = json!("/other");
    let (status, answered) = call(
        &emulator.router,
        "CreateDataRepositoryAssociation",
        &changed,
    )
    .await;
    assert_eq!(
        (status, error_code(&answered)),
        (400, "IncompatibleParameterError"),
        "case same token, different parameters: expected 400 IncompatibleParameterError, got \
         {status} with body {answered}"
    );
}

/// File system ids spanning the modelled `FileSystemId` shape (`length 11..21`, pattern
/// `^(fs-[0-9a-f]{8,})$`) that name no file system of the account: the shortest legal id, a
/// mid-length one and the longest legal id, with differing hex payloads.
const ABSENT_FILE_SYSTEM_IDS: &[&str] = &[
    "fs-deadbeef",
    "fs-00000000deadbeef",
    "fs-0123456789abcdef0",
    "fs-ffffffffffffffffff",
];

/// Association ids spanning the modelled `DataRepositoryAssociationId` shape (`length
/// 13..23`, pattern `^(dra-[0-9a-f]{8,})$`) that name no association of the account: the
/// shortest legal id, two mid-length ones and the longest legal id.
const ABSENT_ASSOCIATION_IDS: &[&str] = &[
    "dra-deadbeef0",
    "dra-00000000deadbeef",
    "dra-0123456789abcdef0",
    "dra-fffffffffffffffffff",
];

/// The FIXED sentence Amazon FSx refuses an association id it holds nothing for with. The
/// whole sentence is fixed text: no part of it comes from the request.
const ASSOCIATION_NOT_FOUND: &str = "Data repository association does not exist.";

/// RULE: `CreateDataRepositoryAssociation` links a directory of the file system the REQUEST
/// names, so a `FileSystemId` that resolves to nothing makes the request itself wrong: it is
/// refused with the generic client-request error the operation models (`BadRequest`, HTTP
/// 400), whose `Message` names the requested id. A file system that DOES exist but can hold
/// no association is a different refusal — the modelled `UnsupportedOperation`.
///
/// The table spans the modelled `FileSystemId` shape and the other members a create carries,
/// so what decides the refusal is the file system and not the path a row happens to name.
#[tokio::test]
async fn an_association_for_a_file_system_the_account_lacks_is_a_bad_request() {
    let emulator = emulator().await;

    // (case, the members a create carries beside its FileSystemId)
    let requests: &[(&str, Value)] = &[
        ("bucket only", json!({ "DataRepositoryPath": "s3://some-bucket" })),
        (
            "bucket and prefix",
            json!({
                "DataRepositoryPath": "s3://some-bucket/exports/",
                "FileSystemPath": "/ns1",
            }),
        ),
        (
            "a deep prefix and every optional member",
            json!({
                "DataRepositoryPath": "s3://some-bucket/a/b/c/",
                "FileSystemPath": "/ns1/subdir/",
                "BatchImportMetaDataOnCreate": true,
                "ImportedFileChunkSize": 512,
                "S3": { "AutoImportPolicy": { "Events": ["NEW"] } },
            }),
        ),
        (
            "a path no bucket could be named by",
            json!({ "DataRepositoryPath": "not-a-repository-path" }),
        ),
    ];

    for file_system_id in ABSENT_FILE_SYSTEM_IDS {
        for (request_case, members) in requests {
            let mut request = members.clone();
            request["FileSystemId"] = json!(file_system_id);
            let case = format!("absent {file_system_id} with {request_case}");
            let (status, code, message) = error_of(
                &emulator.router,
                "CreateDataRepositoryAssociation",
                &with_client_request_token(&request),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "BadRequest"),
                "case {case:?}: expected 400 BadRequest, got {status} {code} with message \
                 {message:?}"
            );
            assert_eq!(
                message,
                format!("File system '{file_system_id}' does not exist."),
                "case {case:?}: expected the refusal to name the requested file system, got \
                 {message:?}"
            );
        }
    }

    // The same create against a file system the account HAS, naming a bucket it holds, is
    // accepted: `BadRequest` answers a MISSING file system and not every create. (A file
    // system that exists but can hold no association is the modelled `UnsupportedOperation`,
    // tabled in `a_create_is_refused_for_a_file_system_that_cannot_hold_the_association`.)
    let bucket = "fsx-existing-file-system-repository";
    assert_eq!(
        create_bucket(&emulator.router, bucket).await,
        200,
        "the account must hold the bucket"
    );
    let file_system_id = lustre_file_system(&emulator).await;
    let (status, answered) = create_association(
        &emulator.router,
        &file_system_id,
        "/data",
        &format!("s3://{bucket}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case existing file system: a create on a file system the account has must be \
         accepted, got {status} with body {answered}"
    );
}

/// RULE: `DescribeDataRepositoryAssociations` is a read over a list — every selector the
/// request carries (an `AssociationIds` entry, a `Filter` value) SELECTS, so a selection
/// matching nothing is HTTP 200 with an empty `Associations` list, always serialized, rather
/// than a refusal. Only a request whose shape cannot be read as a selection — a `Filter`
/// name the operation does not define — is refused.
///
/// The table spans the forms of selection the rule generalises over: no selection at all,
/// one id, several ids, a repeated id, each documented filter name, a filter value no stored
/// association carries, two filters together, and both published page-size bounds.
#[tokio::test]
async fn a_listing_that_matches_nothing_is_an_empty_page() {
    let emulator = emulator().await;

    // (case, the listing request) — the store holds no association at all, so every row
    // matches nothing.
    let cases: &[(&str, Value)] = &[
        ("no selection", json!({})),
        ("one absent id", json!({ "AssociationIds": [ABSENT_ASSOCIATION_IDS[0]] })),
        ("several absent ids", json!({ "AssociationIds": ABSENT_ASSOCIATION_IDS })),
        (
            "the same absent id twice",
            json!({ "AssociationIds": [ABSENT_ASSOCIATION_IDS[1], ABSENT_ASSOCIATION_IDS[1]] }),
        ),
        (
            // The filter names the operation documents: "use the file-system-id filter with
            // the ID of the file system ... the file-cache-id filter with the ID of the cache
            // ... the data-repository-type filter with a value of S3 or NFS".
            "the file-system-id filter on an absent file system",
            json!({
                "Filters": [{ "Name": "file-system-id", "Values": [ABSENT_FILE_SYSTEM_IDS[0]] }],
            }),
        ),
        (
            "the file-cache-id filter",
            json!({ "Filters": [{ "Name": "file-cache-id", "Values": ["fc-0123456789abcdef0"] }] }),
        ),
        (
            "the data-repository-type filter on S3",
            json!({ "Filters": [{ "Name": "data-repository-type", "Values": ["S3"] }] }),
        ),
        (
            "the data-repository-type filter on NFS",
            json!({ "Filters": [{ "Name": "data-repository-type", "Values": ["NFS"] }] }),
        ),
        (
            "a data-repository-type no association carries",
            json!({
                "Filters": [{
                    "Name": "data-repository-type",
                    "Values": ["NOT_A_REPOSITORY_TYPE"],
                }],
            }),
        ),
        (
            "two filters together",
            json!({
                "Filters": [
                    { "Name": "file-system-id", "Values": [ABSENT_FILE_SYSTEM_IDS[1]] },
                    { "Name": "data-repository-type", "Values": ["S3"] },
                ],
            }),
        ),
        // `MaxResults` targets `LimitedMaxResults`, `range 1..25`: both published bounds.
        ("the smallest page", json!({ "MaxResults": 1 })),
        ("the largest page", json!({ "MaxResults": 25 })),
    ];

    for case in cases {
        let (label, request) = case;
        let listed = ok(&emulator.router, "DescribeDataRepositoryAssociations", request).await;
        assert_eq!(
            listed["Associations"],
            json!([]),
            "case {label:?}: expected HTTP 200 with an empty Associations list, got {listed}"
        );
        assert_eq!(
            listed.get("NextToken"),
            None,
            "case {label:?}: an empty page is the last page, got {listed}"
        );
    }

    // A filter NAME the operation does not define cannot be read as a selection at all, so
    // it stays a client error rather than an empty page.
    for unsupported in ["volume-id", "backup-type", ""] {
        let (status, code, message) = error_of(
            &emulator.router,
            "DescribeDataRepositoryAssociations",
            &json!({ "Filters": [{ "Name": unsupported, "Values": ["anything"] }] }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "BadRequest"),
            "case {unsupported:?}: expected 400 BadRequest for a filter name this listing does \
             not select by, got {status} {code} with message {message:?}"
        );
    }
}

/// RULE: the refusal for an association id this service holds nothing for is worded as one
/// FIXED sentence — it echoes no identifier back, unlike the not-found messages that name the
/// resource they were asked about.
///
/// The table spans the modelled `DataRepositoryAssociationId` shape and the request forms a
/// delete takes, so what it asserts is that the sentence does not vary with the request at
/// all.
#[tokio::test]
async fn an_unknown_association_is_refused_with_one_fixed_sentence() {
    let emulator = emulator().await;
    let mut seen: Vec<String> = Vec::new();

    for association_id in ABSENT_ASSOCIATION_IDS {
        for (case, request) in [
            ("a plain delete", json!({ "AssociationId": association_id })),
            (
                "a delete taking the file system's data with it",
                json!({ "AssociationId": association_id, "DeleteDataInFileSystem": true }),
            ),
        ] {
            let label = format!("{case} of absent {association_id}");
            let (status, code, message) = error_of(
                &emulator.router,
                "DeleteDataRepositoryAssociation",
                &with_client_request_token(&request),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "DataRepositoryAssociationNotFound"),
                "case {label:?}: expected 400 DataRepositoryAssociationNotFound, got {status} \
                 {code} with message {message:?}"
            );
            assert_eq!(
                message, ASSOCIATION_NOT_FOUND,
                "case {label:?}: expected the fixed sentence {ASSOCIATION_NOT_FOUND:?}, got \
                 {message:?}"
            );
            assert!(
                !message.contains(association_id),
                "case {label:?}: the sentence must not echo the requested id, got {message:?}"
            );
            seen.push(message);
        }
    }

    // Every id the store holds nothing for produces the SAME sentence.
    let mut distinct = seen.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        1,
        "every absent association id must produce one sentence, got {seen:?}"
    );
}
