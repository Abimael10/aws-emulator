// SPDX-License-Identifier: CC0-1.0

//! The AMI OBJECT a store task writes, where Amazon S3 owns it.
//!
//! "Stores an AMI as a single object in an Amazon S3 bucket" (`CreateStoreImageTask`) and
//! "the AMI object appears in the bucket only after the upload task has completed"
//! (`CreateStoreImageTaskRequest$Bucket`): the key the create answers
//! (`CreateStoreImageTaskResult$ObjectKey`) names an object of that bucket, so the rules
//! under test are
//!
//! 1. A STORE TASK WRITES THE OBJECT ITS KEY NAMES: once the task reports the
//!    `StoreTaskState` `Completed`, Amazon S3's own `HeadObject` on the task's bucket and
//!    key answers HTTP 200, and `GetObject` serves the AMI's own stored definition — read
//!    back through the service that OWNS the object rather than out of EC2's state.
//! 2. WHAT THE OBJECT STORE ANSWERED IS THE TASK'S OUTCOME: a bucket Amazon S3 holds no
//!    bucket under leaves the task reaching `Failed` with a `StoreTaskFailureReason`, and no
//!    object anywhere — rather than a store nothing performed.
//!
//! Both services are mounted on ONE shared engine, exactly as the assembled emulator mounts
//! them, so the bucket is created through Amazon S3's own wire and the object is read back
//! from the state Amazon S3 serves its own reads from.
//!
//! The rows differ along what the rule generalises over: which AMI is stored, which bucket it
//! is stored into, how many tasks one bucket holds, and whether the destination is a bucket
//! the account holds at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ec2::state::{
    ImageSpec, STORE_IMAGE_OBJECT_KEY_SUFFIX, STORE_TASK_STATE_COMPLETED, STORE_TASK_STATE_FAILED,
    TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for, and the Region an Amazon S3 `CreateBucket` with
/// no `CreateBucketConfiguration` places a bucket in.
const REGION: &str = "us-east-1";

/// How long a case waits for a task to reach its state before giving up: far above the span
/// one upload takes, and driven off what the task REPORTS rather than off a fixed sleep, so a
/// slow host delays a case instead of breaking it.
const STATE_DEADLINE: Duration = Duration::from_secs(60);

/// EC2 and Amazon S3 over ONE shared engine, exactly as the assembled router mounts them: the
/// bucket a store task writes into, and the object in it, are that service's resources.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere.
        router.set_request_authentication(false);
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    /// The SigV4 `Authorization` whose credential scope names `service`, which is what routes
    /// the request. The signature is never checked.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTEC2/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=00"
        )
    }

    /// One of EC2's own ec2Query actions.
    async fn ec2_call(&self, action: &str, params: &[Param]) -> (u16, String) {
        call(&self.router, REGION, action, params).await
    }

    /// One of EC2's own actions, requiring a 200, and the body it answered.
    async fn ec2_ok(&self, case: &str, action: &str, params: &[Param]) -> String {
        let (status, body) = self.ec2_call(action, params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
        );
        body
    }

    /// One Amazon S3 REST call, path style: the status and the body.
    async fn s3(&self, method: &str, path: &str) -> (u16, String) {
        let request = http::Request::builder()
            .method(method)
            .uri(path.to_string())
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::new())
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8_lossy(response.body()).to_string();
        (status, body)
    }

    /// Create a bucket through Amazon S3's own wire.
    async fn given_bucket(&self, case: &str, name: &str) {
        let (status, body) = self.s3("PUT", &format!("/{name}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateBucket({name}) must succeed, got {status} with {body}"
        );
    }

    /// Write `count` AMIs of the calling account into the store, the way a register operation
    /// would, and report the ids they were given.
    fn given_images(&self, count: usize) -> Vec<String> {
        let store = self.engine.state::<ec2::state::ComAmazonawsEc2Amazonec2State>();
        let mut guard = store.lock();
        (0..count)
            .map(|index| {
                guard.insert_image(ImageSpec {
                    name: format!("stored-ami-{index}"),
                    ..ImageSpec::default()
                })
            })
            .collect()
    }

    /// Start one store task for `image_id` into `bucket`, and answer the object key it
    /// reported.
    async fn store(&self, case: &str, image_id: &str, bucket: &str) -> String {
        let body = self
            .ec2_ok(
                case,
                "CreateStoreImageTask",
                &[param("ImageId", image_id), param("Bucket", bucket)],
            )
            .await;
        element(&body, "objectKey").unwrap_or_else(|| {
            panic!("case {case:?}: CreateStoreImageTask reported no objectKey: {body}")
        })
    }

    /// The task `DescribeStoreImageTasks` reports for `image_id`, as
    /// `(storeTaskState, storeTaskFailureReason, s3objectKey, bucket)`, looked for again until
    /// its state is `wanted` or [`STATE_DEADLINE`] passes.
    async fn task_reaching(
        &self,
        case: &str,
        image_id: &str,
        wanted: &str,
    ) -> (String, Option<String>, String, String) {
        let deadline = Instant::now() + STATE_DEADLINE;
        loop {
            let body = self
                .ec2_ok(
                    case,
                    "DescribeStoreImageTasks",
                    &[param("ImageId.1", image_id)],
                )
                .await;
            let parsed = document(case, &body);
            let reported = set_items(parsed.root_element(), "storeImageTaskResultSet")
                .into_iter()
                .map(|item| {
                    (
                        child_text(item, "storeTaskState").unwrap_or_default(),
                        child_text(item, "storeTaskFailureReason"),
                        child_text(item, "s3objectKey").unwrap_or_default(),
                        child_text(item, "bucket").unwrap_or_default(),
                    )
                })
                .next();
            if let Some(task) = reported {
                if task.0 == wanted {
                    return task;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: the task for {image_id} must reach {wanted:?}, last read \
                     {task:?}"
                );
            } else {
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: a task for {image_id} must be reported, got {body}"
                );
            }
            tokio::time::sleep(TRANSITION_DELAY / 10).await;
        }
    }
}

/// RULE 1: the object the task's key names is in the bucket, served by Amazon S3 itself, and
/// it holds the AMI's own stored definition.
///
/// The rows span which AMI is stored and which bucket it is stored into, and one bucket holds
/// the objects of two AMIs — so no row is the specification.
#[tokio::test]
async fn a_completed_store_task_leaves_its_ami_object_in_the_bucket() {
    let emulator = Emulator::new();
    let case = "the AMI objects of two stores";
    let images = emulator.given_images(2);
    for bucket in ["ami-archive", "ami-archive-two"] {
        emulator.given_bucket(case, bucket).await;
    }

    // (case, the AMI, the bucket)
    let rows: Vec<(String, &String, &str)> = vec![
        (
            "the first AMI into the first bucket".to_string(),
            &images[0],
            "ami-archive",
        ),
        (
            "a second AMI into the same bucket".to_string(),
            &images[1],
            "ami-archive",
        ),
        (
            "the first AMI into another bucket as well".to_string(),
            &images[0],
            "ami-archive-two",
        ),
    ];

    for (case, image_id, bucket) in rows {
        let object_key = emulator.store(&case, image_id, bucket).await;
        assert_eq!(
            object_key,
            format!("{image_id}{STORE_IMAGE_OBJECT_KEY_SUFFIX}"),
            "case {case:?}: the key is derived from the AMI's id, got {object_key:?}"
        );

        let (state, failure, reported_key, reported_bucket) = emulator
            .task_reaching(&case, image_id, STORE_TASK_STATE_COMPLETED)
            .await;
        assert_eq!(
            (failure.as_deref(), reported_key.as_str(), reported_bucket.as_str()),
            (None, object_key.as_str(), bucket),
            "case {case:?}: a completed task names the object it wrote and no failure, got \
             state {state:?} / {failure:?} / {reported_key:?} / {reported_bucket:?}"
        );

        // The object, asked of the service that OWNS it.
        let (head, head_body) = emulator.s3("HEAD", &format!("/{bucket}/{object_key}")).await;
        assert_eq!(
            head, 200,
            "case {case:?}: HeadObject({bucket}/{object_key}) must answer 200, got {head} \
             with {head_body}"
        );
        let (get, served) = emulator.s3("GET", &format!("/{bucket}/{object_key}")).await;
        assert_eq!(
            get, 200,
            "case {case:?}: GetObject({bucket}/{object_key}) must answer 200, got {get} with \
             {served}"
        );
        // The bytes are the AMI's own definition, not filler: the document names the image
        // this task stored.
        let document: serde_json::Value = serde_json::from_str(&served).unwrap_or_else(|error| {
            panic!("case {case:?}: the stored object must be the AMI's definition ({error}): {served}")
        });
        assert_eq!(
            document["imageId"].as_str(),
            Some(image_id.as_str()),
            "case {case:?}: the stored object must name the AMI it holds, got {document}"
        );
        // ...and every member of it is the one `DescribeImages` reports for that AMI.
        let described = emulator
            .ec2_ok(&case, "DescribeImages", &[param("ImageId.1", image_id)])
            .await;
        for (member, reported) in [
            ("name", "name"),
            ("architecture", "architecture"),
            ("rootDeviceName", "rootDeviceName"),
            ("rootDeviceType", "rootDeviceType"),
            ("virtualizationType", "virtualizationType"),
        ] {
            let expected = element(&described, reported);
            assert_eq!(
                document[member].as_str().map(str::to_string),
                expected,
                "case {case:?}: the stored object's {member} must be the AMI's own, got \
                 {document}"
            );
        }
    }
}

/// RULE 2: a destination Amazon S3 holds no bucket under is the upload FAILING — the task
/// reaches `Failed` carrying the answer the object store gave, and no object is anywhere to
/// be read.
#[tokio::test]
async fn a_store_into_a_bucket_the_account_does_not_hold_fails_the_task() {
    let emulator = Emulator::new();
    let case = "a store into a bucket nothing holds";
    let images = emulator.given_images(1);
    let image_id = &images[0];
    let bucket = "no-such-ami-archive";

    let object_key = emulator.store(case, image_id, bucket).await;
    let (state, failure, _, _) = emulator
        .task_reaching(case, image_id, STORE_TASK_STATE_FAILED)
        .await;
    let reason = failure.unwrap_or_default();
    assert!(
        reason.contains(bucket),
        "case {case:?}: the failure reason must name the bucket the upload was refused for, \
         got state {state:?} with {reason:?}"
    );
    let (head, body) = emulator.s3("HEAD", &format!("/{bucket}/{object_key}")).await;
    assert_ne!(
        head, 200,
        "case {case:?}: no object was stored, so HeadObject must not answer 200, got {head} \
         with {body}"
    );
}

/// The task's own report and the object agree on ONE key: whatever a later describe reports
/// as `s3objectKey` is the object Amazon S3 serves, so a client that paged the tasks and a
/// client that kept the create's answer read the same object.
#[tokio::test]
async fn the_key_a_describe_reports_is_the_object_the_store_serves() {
    let emulator = Emulator::new();
    let case = "the keys a describe reports";
    let images = emulator.given_images(3);
    emulator.given_bucket(case, "ami-archive").await;

    for image_id in &images {
        emulator.store(case, image_id, "ami-archive").await;
    }
    for image_id in &images {
        emulator
            .task_reaching(case, image_id, STORE_TASK_STATE_COMPLETED)
            .await;
    }

    let body = emulator
        .ec2_ok(case, "DescribeStoreImageTasks", &[])
        .await;
    let keys = elements(&body, "s3objectKey");
    assert_eq!(
        keys.len(),
        images.len(),
        "case {case:?}: every task is reported, got {keys:?}"
    );
    for key in keys {
        let (head, head_body) = emulator.s3("HEAD", &format!("/ami-archive/{key}")).await;
        assert_eq!(
            head, 200,
            "case {case:?}: the reported key {key} must name an object the store serves, got \
             {head} with {head_body}"
        );
    }
}
