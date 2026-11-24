// SPDX-License-Identifier: CC0-1.0

//! WHERE an S3 Control request may be addressed, driven end to end on the assembled,
//! authenticating emulator with a whole S3 Batch Operations job behind it.
//!
//! The RULE under test: **every operation of this service is addressed IN an account, and
//! that account travels both in `x-amz-account-id` and as the `{AccountId}.` host prefix a
//! client prepends to WHATEVER endpoint authority it resolved — so a request for the
//! caller's own account is served, and carried out, identically at the prefixed form of
//! every authority this emulator is reached at, at the prefixed published regional endpoint,
//! and at the bare authority that carries no prefix at all; which form a request used is
//! addressing alone and names no part of the resource it reaches.** Source:
//! `smithy_query(query="operation", operation="CreateJob")` — `AccountId` is `@required`,
//! `hostLabel` and `httpHeader: x-amz-account-id`, and the operation's `endpoint.hostPrefix`
//! is `{AccountId}.` — which is why an SDK configured with ANY endpoint sends to the
//! account-prefixed form of THAT endpoint rather than to the bare one.
//!
//! The rows span what the rule generalises over: the AUTHORITY the prefix sits ahead of —
//! the published regional endpoint, a loopback address with the port a local endpoint is
//! reached on, the same address with no port, a loopback NAME with a port, an operator's own
//! DNS name for the endpoint — plus the bare authority, where the account travels in the
//! header alone. Each row runs the same job: a `CreateJob` with `ConfirmationRequired=false`
//! whose `S3PutObjectTagging` action tags every object of a CSV manifest, under a role whose
//! trust policy admits `batchoperations.s3.amazonaws.com`, polled to its terminal status
//! through `DescribeJob` at the SAME authority — a row submits its WRITE at its own address,
//! which is what the recorded workflow does — and then every job is read back at every other
//! authority, which is what pins the address form as addressing only.
//!
//! Every call carries `x-amz-account-id` as a client's does, so what the rows pin is the
//! ADDRESS: that no form of it is refused, routed to another service or answered about
//! another resource. Which of the two places the account may travel in is read, and what
//! happens when they disagree or name an account the caller holds no credentials for, is the
//! s3control crate's own `tests/all/account_authorization.rs`.
//!
//! Only the assembled router can pin this: the account is resolved from the request's host
//! ahead of the operation, the job's own run reads its manifest and writes its tags and its
//! report through Amazon S3's own operations over the in-process callback, the role it runs
//! as is IAM's, and every call below is made against a router that VERIFIES request
//! signatures — which cover the `Host` header, so the prefixed authority is signed as it is
//! sent. The harness, the job request shape and the terminal-status poll are
//! [`super::zz_batch_job_probe`]'s own, which drives the same job over a table of trust
//! policies at one authority; what only this suite reaches is the ADDRESS axis.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use crate::cloudformation_provisions_stack_resources::xml_texts;
use crate::zz_batch_job_probe::{
    Emulator, TaggingJobRequest, ACCOUNT, BATCH_OPERATIONS_PRINCIPAL, BOOT_STACK_SIZE, REGION,
    S3_XMLNS,
};

/// The bucket holding every object of this suite: the manifests, the objects they list and
/// the completion reports.
const BUCKET: &str = "xsvc-s3control-addressing";

/// The tag each job's action puts on the object its manifest lists.
const TAG_KEY: &str = "batch";
const TAG_VALUE: &str = "true";

/// The role every job below runs as: one role, created once, whose trust policy admits S3
/// Batch Operations — the rows vary the ADDRESS a request is sent to and nothing else.
const ROLE_NAME: &str = "s3control-addressing-batch-role";

/// The name of the inline policy granting that role the object actions a tagging job's tasks
/// perform.
const ROLE_POLICY_NAME: &str = "s3control-addressing-batch-policy";

/// One address a client reaches this service at: the authority its requests state, and the
/// label the objects and the job of that row are keyed under.
struct Row {
    case: &'static str,
    /// Built per row because the account prefix is part of the authority under test.
    authority: String,
    label: &'static str,
}

/// Every form of the address a client sends an S3 Control request to. The first five carry
/// the modelled `{AccountId}.` prefix ahead of a different authority each; the last carries
/// no prefix, so the account travels in the header alone.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the prefixed published regional endpoint",
            authority: format!("{ACCOUNT}.s3-control.{REGION}.amazonaws.com"),
            label: "published",
        },
        Row {
            case: "a prefixed loopback address with a port",
            authority: format!("{ACCOUNT}.127.0.0.1:9297"),
            label: "loopback-port",
        },
        Row {
            case: "a prefixed loopback address with no port",
            authority: format!("{ACCOUNT}.127.0.0.1"),
            label: "loopback",
        },
        Row {
            case: "a prefixed loopback name with a port",
            authority: format!("{ACCOUNT}.localhost:9297"),
            label: "loopback-name",
        },
        Row {
            case: "a prefixed name of the operator's own",
            authority: format!("{ACCOUNT}.emulator.test:9297"),
            label: "named-endpoint",
        },
        Row {
            case: "the bare authority, the account in the header alone",
            authority: "127.0.0.1:9297".to_string(),
            label: "bare",
        },
    ]
}

/// The key of the object one row's job tags.
fn target_key(label: &str) -> String {
    format!("data/{label}.txt")
}

/// The key of the CSV manifest one row's job reads.
fn manifest_key(label: &str) -> String {
    format!("manifests/{label}.csv")
}

/// `Report$Prefix` of one row's job.
fn report_prefix(label: &str) -> String {
    format!("reports/{label}")
}

/// The trust policy of [`ROLE_NAME`]: the grant AWS documents for a Batch Operations role —
/// `sts:AssumeRole` for the `batchoperations.s3.amazonaws.com` service principal.
fn trust_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{BATCH_OPERATIONS_PRINCIPAL}\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The inline policy of [`ROLE_NAME`]: the object actions a tagging job's tasks perform on
/// the bucket, and the bucket read that locates it.
fn role_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":[\"s3:GetObject\",\"s3:PutObject\",\"s3:PutObjectTagging\"],\
         \"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}},{{\"Effect\":\"Allow\",\
         \"Action\":\"s3:GetBucketLocation\",\"Resource\":\"arn:aws:s3:::{BUCKET}\"}}]}}"
    )
}

/// What one row submitted, kept so every job can be polled at the address it was submitted
/// at and then read back at every other one.
struct Submitted {
    case: &'static str,
    /// The address this job was submitted at, and is polled at.
    authority: String,
    /// The row's label, which the keys of its objects and its report prefix are built from.
    label: &'static str,
    job_id: String,
    role_arn: String,
}

#[test]
fn an_s3control_job_is_submitted_and_polled_at_every_address_its_account_prefix_builds() {
    std::thread::Builder::new()
        .name("xsvc-s3control-addressing".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::start();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("a multi-thread runtime");

            runtime.block_on(async {
                // The bucket every object of this suite lives in, written through Amazon S3
                // itself.
                let created = emulator
                    .s3(
                        "PUT",
                        &format!("/{BUCKET}"),
                        &format!(
                            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                             <LocationConstraint>{REGION}</LocationConstraint>\
                             </CreateBucketConfiguration>"
                        ),
                    )
                    .await;
                assert_eq!(
                    created.status, 200,
                    "setup: the bucket must be created, got {} with {}",
                    created.status, created.body
                );

                // The ONE role every job runs as, created through IAM's own operations: the
                // rows differ in the address they are sent to, not in what they may do.
                let role = emulator
                    .iam(&format!(
                        "Action=CreateRole&Version=2010-05-08&RoleName={ROLE_NAME}\
                         &AssumeRolePolicyDocument={}",
                        trust_policy()
                    ))
                    .await;
                assert_eq!(
                    role.status, 200,
                    "setup: the role must be created, got {} with {}",
                    role.status, role.body
                );
                let role_arn = role.text("Arn");
                assert!(
                    !role_arn.is_empty(),
                    "setup: CreateRole must answer the role's ARN, got {}",
                    role.body
                );
                let granted = emulator
                    .iam(&format!(
                        "Action=PutRolePolicy&Version=2010-05-08&RoleName={ROLE_NAME}\
                         &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
                        role_policy()
                    ))
                    .await;
                assert_eq!(
                    granted.status, 200,
                    "setup: the role's inline policy must be written, got {} with {}",
                    granted.status, granted.body
                );

                // Every row submits its job at its OWN address; the jobs are then polled
                // together, so the span the service holds a job for is waited out once.
                let mut submitted = Vec::new();
                for row in rows() {
                    let case = row.case;
                    let label = row.label;

                    let target = emulator
                        .s3(
                            "PUT",
                            &format!("/{BUCKET}/{}", target_key(label)),
                            "the payload of the object this job tags",
                        )
                        .await;
                    assert_eq!(
                        target.status, 200,
                        "setup {case:?}: the target object must be written, got {} with {}",
                        target.status, target.body
                    );
                    let manifest = emulator
                        .s3(
                            "PUT",
                            &format!("/{BUCKET}/{}", manifest_key(label)),
                            &format!("{BUCKET},{}\n", target_key(label)),
                        )
                        .await;
                    assert_eq!(
                        manifest.status, 200,
                        "setup {case:?}: the manifest must be written, got {} with {}",
                        manifest.status, manifest.body
                    );

                    let answer = emulator
                        .s3control_at(
                            &row.authority,
                            "POST",
                            "/v20180820/jobs",
                            &TaggingJobRequest {
                                token: &format!("token-{label}"),
                                bucket: BUCKET,
                                manifest_key: &manifest_key(label),
                                manifest_etag: &manifest.etag,
                                report_prefix: &report_prefix(label),
                                role_arn: &role_arn,
                                tag: (TAG_KEY, TAG_VALUE),
                            }
                            .xml(),
                        )
                        .await;
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?} at {:?}: CreateJob must be served, got {} with {}",
                        row.authority, answer.status, answer.body
                    );
                    let job_id = answer.text("JobId");
                    assert!(
                        !job_id.is_empty(),
                        "case {case:?} at {:?}: CreateJob must answer a JobId, got {}",
                        row.authority,
                        answer.body
                    );
                    submitted.push(Submitted {
                        case,
                        authority: row.authority.clone(),
                        label,
                        job_id,
                        role_arn: role_arn.clone(),
                    });
                }

                // Each job is polled at the address its own client submitted it at, and the
                // tag its tasks set is read back through Amazon S3.
                for job in &submitted {
                    let (case, label) = (job.case, job.label);
                    let described = emulator
                        .await_terminal_at(case, &job.authority, &job.job_id)
                        .await;
                    let status = xml_texts(&described, "Status")
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    assert_eq!(
                        status, "Complete",
                        "case {case:?} at {:?}: expected the job to settle Complete, got \
                         {described}",
                        job.authority
                    );
                    assert!(
                        described.contains("<TotalNumberOfTasks>1</TotalNumberOfTasks>")
                            && described
                                .contains("<NumberOfTasksSucceeded>1</NumberOfTasksSucceeded>"),
                        "case {case:?} at {:?}: the job must account for its one task, got \
                         {described}",
                        job.authority
                    );

                    let tags = emulator
                        .s3(
                            "GET",
                            &format!("/{BUCKET}/{}?tagging", target_key(label)),
                            "",
                        )
                        .await;
                    assert_eq!(
                        tags.status, 200,
                        "case {case:?}: GetObjectTagging must answer 200, got {} with {}",
                        tags.status, tags.body
                    );
                    assert!(
                        xml_texts(&tags.body, "Key").contains(&TAG_KEY.to_string())
                            && xml_texts(&tags.body, "Value").contains(&TAG_VALUE.to_string()),
                        "case {case:?} at {:?}: expected the manifest's object to carry \
                         {TAG_KEY}={TAG_VALUE}, got {}",
                        job.authority,
                        tags.body
                    );

                    let listed = emulator
                        .s3(
                            "GET",
                            &format!("/{BUCKET}?list-type=2&prefix={}", report_prefix(label)),
                            "",
                        )
                        .await;
                    assert!(
                        listed
                            .body
                            .contains(&format!("{}/job-{}/", report_prefix(label), job.job_id)),
                        "case {case:?} at {:?}: the completion report must be written under the \
                         job's own prefix, got {}",
                        job.authority,
                        listed.body
                    );
                }

                // The address a request used is addressing alone: every job is the same job
                // read at every other address, and no address scopes the account's jobs.
                for row in rows() {
                    for job in &submitted {
                        let case = format!("{} read at {:?}", job.case, row.authority);
                        let described = emulator
                            .s3control_at(
                                &row.authority,
                                "GET",
                                &format!("/v20180820/jobs/{}", job.job_id),
                                "",
                            )
                            .await;
                        assert_eq!(
                            described.status, 200,
                            "case {case:?}: DescribeJob must be served, got {} with {}",
                            described.status, described.body
                        );
                        assert_eq!(
                            (
                                described.text("JobId"),
                                described.text("Status"),
                                described.text("RoleArn")
                            ),
                            (
                                job.job_id.clone(),
                                "Complete".to_string(),
                                job.role_arn.clone()
                            ),
                            "case {case:?}: expected the same job, got {}",
                            described.body
                        );
                    }

                    // And the account's listing is the same listing at every address: one
                    // call per address, carrying every job this account submitted.
                    let jobs = emulator
                        .s3control_at(&row.authority, "GET", "/v20180820/jobs", "")
                        .await;
                    assert_eq!(
                        jobs.status, 200,
                        "case {:?}: ListJobs must be served, got {} with {}",
                        row.authority, jobs.status, jobs.body
                    );
                    let ids = xml_texts(&jobs.body, "JobId");
                    let missing: Vec<&str> = submitted
                        .iter()
                        .filter(|job| !ids.contains(&job.job_id))
                        .map(|job| job.job_id.as_str())
                        .collect();
                    assert!(
                        missing.is_empty(),
                        "case {:?}: expected the account's listing to carry every job it \
                         submitted, {missing:?} are absent from {}",
                        row.authority,
                        jobs.body
                    );
                }
            });
        })
        .expect("spawn the s3control addressing thread")
        .join()
        .expect("an S3 Control job is submitted and polled at every address of its account");
}
