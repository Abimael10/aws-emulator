// SPDX-License-Identifier: CC0-1.0

//! WHERE a Capacity Manager data export would be delivered, resolved against the service that
//! OWNS that destination.
//!
//! The rule under test: `CreateCapacityManagerDataExport` points EC2 at a bucket of ANOTHER
//! service — "the name of the S3 bucket where the capacity data export files will be
//! delivered. The bucket must exist and you must have write permissions to it"
//! (`CreateCapacityManagerDataExportRequest$S3BucketName`) — so a create is served only once
//! Amazon S3 reports holding that bucket. A create naming a bucket the object store does not
//! hold is a configuration whose every scheduled delivery would be written into nothing, so it
//! is refused with HTTP 400, code `InvalidParameterValue` and the fixed sentence
//! `The S3 Bucket Name isn't valid.`, and the account holds no configuration afterwards.
//!
//! Two orderings are part of that rule and are pinned here as well:
//!
//! * `DryRun` is answered BEFORE the destination is resolved: the flag asks "whether you have
//!   the required permissions for the action, without actually making the request"
//!   (`$DryRun`) — a question about the caller rather than about another service's resources —
//!   so a probe is `DryRunOperation` whether or not the bucket is there, and stores nothing.
//! * A replayed `ClientToken` answers the configuration it already created without resolving
//!   anything, the destination included.
//!
//! The rows span what the rule generalises over rather than one request: the FORM of a legal
//! bucket name (plain, dotted, and each published length bound), whether a key prefix is named
//! beside it, and whether Amazon S3 never held the bucket or held it and then let it go.
//!
//! Both services are mounted on ONE shared engine, exactly as the assembled emulator mounts
//! them, so each bucket is created through Amazon S3's own wire and resolved through the state
//! that service serves its own reads from.

use std::sync::Arc;

use bytes::Bytes;
use ec2::state::{
    CAPACITY_MANAGER_DATA_EXPORT_ID_PREFIX, ComAmazonawsEc2Amazonec2State as Ec2State,
    INVALID_PARAMETER_VALUE, MAX_S3_BUCKET_NAME_CHARS, MIN_S3_BUCKET_NAME_CHARS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_query::{call, element, error_code, error_message};
use crate::wire::{Param, child_text, document, is_long_id, param};

/// The Region every call here is signed for — and the Region an Amazon S3 `CreateBucket` with
/// no `CreateBucketConfiguration` places a bucket in.
const REGION: &str = "us-east-1";

/// `Schedule`'s one member — `hourly`
/// (`smithy_query(query="shape",shape="Schedule")`).
const HOURLY: &str = "hourly";

/// The `OutputFormat` members — `csv` and `parquet`
/// (`smithy_query(query="shape",shape="OutputFormat")`).
const OUTPUT_FORMATS: [&str; 2] = ["csv", "parquet"];

/// The fixed sentence EC2 refuses a destination bucket the object store does not hold with.
const ABSENT_BUCKET_MESSAGE: &str = "The S3 Bucket Name isn't valid.";

/// EC2 and the service that OWNS a data export's destination, over ONE shared engine, exactly
/// as the assembled router mounts them.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere;
        // these cases are about these services' behaviour.
        router.set_request_authentication(false);
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    /// The SigV4 `Authorization` header of a request signed for `service`, which is how the
    /// router picks the service a request belongs to.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTEC2/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=00"
        )
    }

    /// One `CreateCapacityManagerDataExport` carrying `params` exactly.
    async fn create(&self, params: &[Param]) -> (u16, String) {
        call(
            &self.router,
            REGION,
            "CreateCapacityManagerDataExport",
            params,
        )
        .await
    }

    /// Create a bucket through Amazon S3's own (REST) wire, path style and with no
    /// `CreateBucketConfiguration`: an absent `LocationConstraint` places it in [`REGION`].
    async fn given_bucket(&self, name: &str) {
        let (status, payload) = self.bucket_request("PUT", name).await;
        assert_eq!(
            status, 200,
            "CreateBucket({name}) must answer 200, got {status} with {payload}"
        );
    }

    /// Remove a bucket, so Amazon S3 holds it no longer.
    async fn delete_bucket(&self, name: &str) {
        let (status, payload) = self.bucket_request("DELETE", name).await;
        assert_eq!(
            status, 204,
            "DeleteBucket({name}) must answer 204, got {status} with {payload}"
        );
    }

    async fn bucket_request(&self, method: &str, name: &str) -> (u16, String) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("/{name}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::new())
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// Every configuration id the account holds, in store order — what a refused create must
    /// have left untouched.
    fn stored_ids(&self) -> Vec<String> {
        self.engine
            .state::<Ec2State>()
            .lock()
            .capacity_manager_data_exports
            .keys()
            .cloned()
            .collect()
    }

    /// The `s3BucketName` `DescribeCapacityManagerDataExports` reports for `export_id` — the
    /// destination read back out of the store rather than off the create's own answer.
    async fn reported_bucket(&self, case: &str, export_id: &str) -> Option<String> {
        let (status, body) = call(
            &self.router,
            REGION,
            "DescribeCapacityManagerDataExports",
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeCapacityManagerDataExports must answer 200, got {status} \
             with body {body}"
        );
        let parsed = document(case, &body);
        let export = parsed
            .descendants()
            .find(|node| {
                node.is_element()
                    && child_text(*node, "capacityManagerDataExportId").as_deref()
                        == Some(export_id)
            })
            .unwrap_or_else(|| panic!("case {case:?}: no data export {export_id} in {body}"));
        child_text(export, "s3BucketName")
    }
}

/// The form fields of one create: the bucket, the schedule and the output format every request
/// carries, with whatever else the row needs beside them.
fn create_params(bucket: &str, output_format: &str, extra: &[Param]) -> Vec<Param> {
    let mut params = vec![
        param("S3BucketName", bucket),
        param("Schedule", HOURLY),
        param("OutputFormat", output_format),
    ];
    params.extend_from_slice(extra);
    params
}

/// Whether Amazon S3 is to HOLD the bucket of a row, and how it came to hold it or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    /// Amazon S3 holds the bucket.
    Held,
    /// Nothing ever created it.
    NeverCreated,
    /// It was created and then deleted, so a destination that resolved once stops resolving.
    Deleted,
}

/// One row of the destination table: the case, the bucket name, how Amazon S3 holds it, and
/// the extra form fields the request carries beside the destination.
struct Row {
    case: &'static str,
    bucket: String,
    holding: Holding,
    extra: Vec<Param>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket Amazon S3 holds",
            bucket: "capacity-manager-exports".to_string(),
            holding: Holding::Held,
            extra: Vec::new(),
        },
        Row {
            case: "a bucket Amazon S3 holds, with a key prefix under it",
            bucket: "capacity-manager-exports-prefixed".to_string(),
            holding: Holding::Held,
            extra: vec![param("S3BucketPrefix", "capacity/2026/")],
        },
        Row {
            case: "a bucket Amazon S3 holds whose name carries dots",
            bucket: "capacity.manager.exports".to_string(),
            holding: Holding::Held,
            extra: Vec::new(),
        },
        Row {
            // The shortest name Amazon S3 admits
            // (`crate::state::MIN_S3_BUCKET_NAME_CHARS`).
            case: "the shortest bucket name Amazon S3 admits, held",
            bucket: "c".repeat(MIN_S3_BUCKET_NAME_CHARS),
            holding: Holding::Held,
            extra: Vec::new(),
        },
        Row {
            // The longest name Amazon S3 admits
            // (`crate::state::MAX_S3_BUCKET_NAME_CHARS`).
            case: "the longest bucket name Amazon S3 admits, held",
            bucket: "d".repeat(MAX_S3_BUCKET_NAME_CHARS),
            holding: Holding::Held,
            extra: Vec::new(),
        },
        Row {
            case: "a bucket nothing ever created",
            bucket: "capacity-manager-exports-never-created".to_string(),
            holding: Holding::NeverCreated,
            extra: Vec::new(),
        },
        Row {
            case: "a bucket nothing ever created, with a key prefix under it",
            bucket: "capacity-manager-exports-absent-prefixed".to_string(),
            holding: Holding::NeverCreated,
            extra: vec![param("S3BucketPrefix", "capacity/")],
        },
        Row {
            case: "a bucket whose name carries dots, never created",
            bucket: "capacity.manager.exports.absent".to_string(),
            holding: Holding::NeverCreated,
            extra: Vec::new(),
        },
        Row {
            case: "the shortest bucket name Amazon S3 admits, never created",
            bucket: "e".repeat(MIN_S3_BUCKET_NAME_CHARS),
            holding: Holding::NeverCreated,
            extra: Vec::new(),
        },
        Row {
            case: "the longest bucket name Amazon S3 admits, never created",
            bucket: "f".repeat(MAX_S3_BUCKET_NAME_CHARS),
            holding: Holding::NeverCreated,
            extra: Vec::new(),
        },
        Row {
            case: "a bucket Amazon S3 held and then let go",
            bucket: "capacity-manager-exports-deleted".to_string(),
            holding: Holding::Deleted,
            extra: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn a_create_resolves_its_destination_bucket_against_amazon_s3() {
    let emulator = Emulator::new();
    let rows = rows();
    for row in &rows {
        match row.holding {
            Holding::Held => emulator.given_bucket(&row.bucket).await,
            Holding::NeverCreated => {}
            Holding::Deleted => {
                emulator.given_bucket(&row.bucket).await;
                emulator.delete_bucket(&row.bucket).await;
            }
        }
    }

    // The output format is varied along the rows as well, so no row is the only one that
    // carries a given catalog word.
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let held_before = emulator.stored_ids();
        let (status, body) = emulator
            .create(&create_params(
                &row.bucket,
                OUTPUT_FORMATS[index % OUTPUT_FORMATS.len()],
                &row.extra,
            ))
            .await;

        if row.holding == Holding::Held {
            assert_eq!(
                status, 200,
                "case {case:?}: Amazon S3 holds the bucket, so the create must be served, got \
                 {status} with body {body}"
            );
            let export_id = element(&body, "capacityManagerDataExportId").unwrap_or_else(|| {
                panic!("case {case:?}: expected a capacityManagerDataExportId in {body}")
            });
            assert!(
                is_long_id(CAPACITY_MANAGER_DATA_EXPORT_ID_PREFIX, &export_id),
                "case {case:?}: expected an id of the data export id space, got {export_id:?}"
            );
            let reported = emulator.reported_bucket(case, &export_id).await;
            assert_eq!(
                reported.as_deref(),
                Some(row.bucket.as_str()),
                "case {case:?}: expected the destination read back as {:?}, got {reported:?}",
                row.bucket
            );
            let held_after = emulator.stored_ids();
            assert_eq!(
                held_after.len(),
                held_before.len() + 1,
                "case {case:?}: expected exactly one configuration added, got {held_after:?} \
                 after {held_before:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: Amazon S3 holds no such bucket, so the create must be refused, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the code {INVALID_PARAMETER_VALUE}, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            ABSENT_BUCKET_MESSAGE,
            "case {case:?}: expected the sentence {ABSENT_BUCKET_MESSAGE:?}, got body {body}"
        );
        let held_after = emulator.stored_ids();
        assert_eq!(
            held_after, held_before,
            "case {case:?}: a refused create stores nothing, got {held_after:?} after \
             {held_before:?}"
        );
    }
}

#[tokio::test]
async fn a_dry_run_is_answered_before_the_destination_is_resolved() {
    let emulator = Emulator::new();
    const HELD: &str = "capacity-manager-exports-probe";
    emulator.given_bucket(HELD).await;

    // (case, the bucket the probe names)
    let probes: [(&str, &str); 3] = [
        ("a bucket Amazon S3 holds", HELD),
        (
            "a bucket nothing ever created",
            "capacity-manager-exports-probe-absent",
        ),
        (
            "a bucket nothing ever created, whose name carries dots",
            "capacity.manager.probe.absent",
        ),
    ];

    for (case, bucket) in probes {
        let (status, body) = emulator
            .create(&create_params(
                bucket,
                OUTPUT_FORMATS[0],
                &[param("DryRun", "true")],
            ))
            .await;
        assert_eq!(
            status, 412,
            "case {case:?}: a probe asks about permissions, so it must answer 412, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected the probe's own code, got body {body}"
        );
        assert!(
            emulator.stored_ids().is_empty(),
            "case {case:?}: a probe stores nothing, got {:?}",
            emulator.stored_ids()
        );
    }
}

#[tokio::test]
async fn a_replayed_client_token_answers_its_configuration_without_resolving_the_destination() {
    let emulator = Emulator::new();
    const BUCKET: &str = "capacity-manager-exports-idempotent";
    const TOKEN: &str = "capacity-manager-export-token";
    let case = "the same token and request after the destination was deleted";

    emulator.given_bucket(BUCKET).await;
    let params = create_params(BUCKET, OUTPUT_FORMATS[1], &[param("ClientToken", TOKEN)]);
    let (status, body) = emulator.create(&params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first create must be served, got {status} with body {body}"
    );
    let export_id = element(&body, "capacityManagerDataExportId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a capacityManagerDataExportId in {body}"));

    // The configuration exists now, so a replay reports it whatever became of the destination
    // afterwards: a replay creates nothing and so resolves nothing.
    emulator.delete_bucket(BUCKET).await;
    let (replay_status, replay_body) = emulator.create(&params).await;
    assert_eq!(
        replay_status, 200,
        "case {case:?}: a replay answers the configuration its token created, got \
         {replay_status} with body {replay_body}"
    );
    assert_eq!(
        element(&replay_body, "capacityManagerDataExportId").as_deref(),
        Some(export_id.as_str()),
        "case {case:?}: expected the replay to answer {export_id:?}, got body {replay_body}"
    );
    assert_eq!(
        emulator.stored_ids(),
        vec![export_id.clone()],
        "case {case:?}: the replay created no second configuration, got {:?}",
        emulator.stored_ids()
    );
}

/// The other half of the rule: a refusal has to be grounded in what the OWNING service said, so
/// an emulator assembled with no object store to ask establishes nothing about the destination
/// and refuses nothing — the create is served and the configuration is read back for it.
#[tokio::test]
async fn an_emulator_with_no_object_store_to_ask_refuses_nothing() {
    // EC2 alone, through the crate's shared harness: no Amazon S3 is mounted, so nothing can
    // report that it holds no such bucket.
    let (router, engine) = crate::ec2_query::emulator();
    let case = "a bucket no mounted service can be asked about";

    let (status, body) = call(
        &router,
        REGION,
        "CreateCapacityManagerDataExport",
        &create_params("capacity-manager-exports-unasked", OUTPUT_FORMATS[0], &[]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: with no object store to ask, nothing refuses the create, got {status} \
         with body {body}"
    );
    let export_id = element(&body, "capacityManagerDataExportId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a capacityManagerDataExportId in {body}"));
    let held: Vec<String> = engine
        .state::<Ec2State>()
        .lock()
        .capacity_manager_data_exports
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        held,
        vec![export_id.clone()],
        "case {case:?}: expected the account to hold the configuration {export_id:?}, got \
         {held:?}"
    );
}
