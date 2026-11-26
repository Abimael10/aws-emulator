// SPDX-License-Identifier: CC0-1.0

//! The ORDER `StartExportTask` answers its destination and its export role in.
//!
//! One rule, asserted over a table: Amazon RDS resolves the destination bucket — that the
//! object store holds one of that name, and that it holds it in the Region the call was made
//! in — BEFORE it judges what the export role may do there, so a destination that cannot be
//! found is the modelled `InvalidS3BucketFault` ("the specified Amazon S3 bucket name can't
//! be found", `InvalidS3BucketFault`) whatever the role's own policies are scoped to, and
//! `IamRoleMissingPermissionsFault` is reported only about a destination that IS there. The
//! role's TRUST is the one question answered ahead of the destination: an identity the export
//! principal cannot assume makes no call at all, which is `IamRoleNotFoundFault`.
//!
//! The rows differ along what the rule generalises over: the destination's condition (absent
//! from the object store, held in another Region, held in this one) crossed with what the
//! role's identity policy covers (another bucket only, nothing at all, every bucket) and with
//! a role the export principal cannot assume — plus the two controls the rule must leave
//! alone: an existing destination the role is not authorized on, which still carries the
//! missing-permissions fault naming the call that failed, and the export that is ACCEPTED.
//!
//! Every call goes over the wire as a real awsQuery form POST, with Amazon S3, IAM and KMS
//! mounted beside this service on ONE engine exactly as the assembled router mounts them, so
//! the destination is a bucket Amazon S3 really holds (or really does not) and the role is one
//! IAM really holds.

use crate::db_instances::{call, expect_error, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    available_instance, create_bucket, emulator, export_role_policy, kms, role,
};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A second Region, for the row whose destination exists OUTSIDE the export's own Region.
const OTHER_REGION: &str = "eu-west-1";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The sentence `InvalidS3BucketFault` carries for a destination the export cannot be run to
/// at all, spelled out here so the table pins the wording rather than the code under test.
fn invalid_bucket(bucket: &str) -> String {
    format!(
        "The S3 bucket {bucket} isn't valid. Either it isn't located in the current AWS \
         Region or it doesn't exist."
    )
}

/// The sentence a destination that EXISTS outside the export's Region is refused with.
fn elsewhere(bucket: &str) -> String {
    format!("The S3 bucket {bucket} isn't located in the current AWS Region.")
}

/// The sentence `IamRoleMissingPermissionsFault` carries: the role, the call that failed and
/// the destination.
fn missing_permissions(role_arn: &str, action: &str, bucket: &str) -> String {
    format!("The IAM role {role_arn} isn't authorized to call {action} on the S3 bucket {bucket}.")
}

/// The sentence `IamRoleNotFoundFault` carries for a role the export principal cannot assume.
fn role_not_found(role_arn: &str) -> String {
    format!(
        "The principal {} isn't allowed to assume the IAM role {role_arn} or the IAM role \
         {role_arn} doesn't exist.",
        st::EXPORT_SERVICE_PRINCIPAL
    )
}

/// Everything the table needs: the snapshot the exports read, the key they encrypt under, and
/// the router every call goes over.
struct Fixture {
    router: MultiServiceRouter,
    snapshot_arn: String,
    key_arn: String,
    /// A destination this Region's object store really holds, which the controls export to.
    held_bucket: String,
    /// A destination it holds in ANOTHER Region.
    other_region_bucket: String,
}

/// Stand up the source snapshot, the key and the two buckets the rows name.
async fn fixture() -> Fixture {
    let (router, _engine) = emulator();
    let instance = "emu-order-source";
    available_instance(&router, instance, "mysql").await;

    let snapshot = "emu-order-snapshot";
    expect_ok(
        "snapshot the source",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    // The snapshot is `creating` when the call answers and an export reads one that is
    // available: wait for that CONDITION rather than for a span.
    let described = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        "snapshot the source",
        "the DB snapshot",
        || call(&router, "DescribeDBSnapshots", &described),
        |status, body| status == 200 && body.contains("<Status>available</Status>"),
    )
    .await;

    let held_bucket = "emu-order-destination".to_string();
    create_bucket(&router, &held_bucket, REGION).await;
    let other_region_bucket = "emu-order-elsewhere".to_string();
    create_bucket(&router, &other_region_bucket, OTHER_REGION).await;

    let created = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key");
    let key_arn = created
        .key_metadata()
        .and_then(|metadata| metadata.arn())
        .expect("the created key reports its ARN")
        .to_string();

    Fixture {
        snapshot_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}"),
        key_arn,
        held_bucket,
        other_region_bucket,
        router,
    }
}

/// Which destination a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// A well-formed bucket name the object store holds nothing under.
    Absent(&'static str),
    /// The bucket it holds in another Region.
    OtherRegion,
    /// The bucket it holds in this one.
    Held,
}

/// What the row's export role carries — its trust, and what its identity policy covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Trusted by the export principal, granted the export's calls on ANOTHER bucket only:
    /// the role of a caller that typed the destination wrong.
    ScopedElsewhere,
    /// Trusted by the export principal, carrying no identity policy at all.
    Unprivileged,
    /// Trusted by the export principal, granted the export's calls on every bucket.
    ScopedEverywhere,
    /// Granted the export's calls on every bucket, but trusted by another service principal,
    /// so the export principal cannot assume it.
    Untrusting,
}

/// What a row must be answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The modelled `InvalidS3BucketFault`, HTTP 400.
    InvalidBucket,
    /// `InvalidParameterValue`, HTTP 400: a destination that exists outside this Region.
    Elsewhere,
    /// The modelled `IamRoleMissingPermissionsFault`, HTTP 400, naming this S3 action.
    MissingPermissions(&'static str),
    /// The modelled `IamRoleNotFoundFault`, HTTP 404.
    RoleNotFound,
    /// An accepted export.
    Accepted,
}

/// One row: the destination, the role, and the answer the rule requires.
struct Row {
    case: &'static str,
    identifier: &'static str,
    destination: Destination,
    role: Role,
    answer: Answer,
}

/// The rows: the destination's condition crossed with what the role carries.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a destination that does not exist, with a role scoped to another bucket",
            identifier: "emu-order-absent-scoped",
            destination: Destination::Absent("emu-order-no-such-bucket"),
            role: Role::ScopedElsewhere,
            answer: Answer::InvalidBucket,
        },
        Row {
            case: "a destination that does not exist, with a role carrying no policy at all",
            identifier: "emu-order-absent-bare",
            destination: Destination::Absent("emu-order-also-no-such-bucket"),
            role: Role::Unprivileged,
            answer: Answer::InvalidBucket,
        },
        Row {
            case: "a destination that does not exist, with a role granted every bucket",
            identifier: "emu-order-absent-open",
            destination: Destination::Absent("emu-order-missing-destination"),
            role: Role::ScopedEverywhere,
            answer: Answer::InvalidBucket,
        },
        Row {
            case: "a destination held in another Region, with a role scoped to another bucket",
            identifier: "emu-order-elsewhere-scoped",
            destination: Destination::OtherRegion,
            role: Role::ScopedElsewhere,
            answer: Answer::Elsewhere,
        },
        Row {
            case: "a destination held in another Region, with a role granted every bucket",
            identifier: "emu-order-elsewhere-open",
            destination: Destination::OtherRegion,
            role: Role::ScopedEverywhere,
            answer: Answer::Elsewhere,
        },
        Row {
            // The trust question is the one answered AHEAD of the destination, so a role the
            // export principal cannot assume is reported as the role whatever the
            // destination's own condition is.
            case: "a destination that does not exist, with a role the export cannot assume",
            identifier: "emu-order-absent-untrusting",
            destination: Destination::Absent("emu-order-unreachable-destination"),
            role: Role::Untrusting,
            answer: Answer::RoleNotFound,
        },
        Row {
            case: "a destination that is held, with a role the export cannot assume",
            identifier: "emu-order-held-untrusting",
            destination: Destination::Held,
            role: Role::Untrusting,
            answer: Answer::RoleNotFound,
        },
        Row {
            // The control on the other half of the rule: a destination that IS there and a
            // role nothing authorizes on it is still the missing-permissions fault, named
            // after the first call the export makes there.
            case: "a destination that is held, with a role scoped to another bucket",
            identifier: "emu-order-held-scoped",
            destination: Destination::Held,
            role: Role::ScopedElsewhere,
            answer: Answer::MissingPermissions("s3:GetBucketLocation"),
        },
        Row {
            case: "a destination that is held, with a role carrying no policy at all",
            identifier: "emu-order-held-bare",
            destination: Destination::Held,
            role: Role::Unprivileged,
            answer: Answer::MissingPermissions("s3:GetBucketLocation"),
        },
        Row {
            case: "the control: a destination that is held, with a role granted every bucket",
            identifier: "emu-order-held-open",
            destination: Destination::Held,
            role: Role::ScopedEverywhere,
            answer: Answer::Accepted,
        },
    ]
}

/// Rule: the destination of an export is resolved before the export role is judged for the
/// calls it would make there, and the role's trust before both.
#[tokio::test]
async fn an_unresolvable_destination_is_answered_before_the_export_role() {
    let fixture = fixture().await;
    // A bucket no row exports to: what the scoped-elsewhere role's policy covers, so that
    // role allows the export's calls on SOMETHING and is refused only about the destination
    // each row names.
    let covered = "emu-order-covered";
    create_bucket(&fixture.router, covered, REGION).await;

    let scoped_elsewhere = role(
        &fixture.router,
        "emu-order-scoped-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(covered)),
    )
    .await;
    let unprivileged = role(
        &fixture.router,
        "emu-order-bare-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        None,
    )
    .await;
    let scoped_everywhere = role(
        &fixture.router,
        "emu-order-open-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy("*")),
    )
    .await;
    // Trusted by the Amazon RDS service principal rather than by the EXPORT one: a role IAM
    // holds that the export cannot act as.
    let untrusting = role(
        &fixture.router,
        "emu-order-untrusting-role",
        st::RDS_SERVICE_PRINCIPAL,
        Some(export_role_policy("*")),
    )
    .await;

    for row in rows() {
        let case = row.case;
        let bucket = match row.destination {
            Destination::Absent(name) => name.to_string(),
            Destination::OtherRegion => fixture.other_region_bucket.clone(),
            Destination::Held => fixture.held_bucket.clone(),
        };
        let role_arn = match row.role {
            Role::ScopedElsewhere => scoped_elsewhere.clone(),
            Role::Unprivileged => unprivileged.clone(),
            Role::ScopedEverywhere => scoped_everywhere.clone(),
            Role::Untrusting => untrusting.clone(),
        };
        let answered = call(
            &fixture.router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => row.identifier,
                "SourceArn" => fixture.snapshot_arn,
                "S3BucketName" => bucket,
                "IamRoleArn" => role_arn,
                "KmsKeyId" => fixture.key_arn,
            ],
        )
        .await;

        let (status, code, expected_message) = match row.answer {
            Answer::InvalidBucket => (400, "InvalidS3BucketFault", invalid_bucket(&bucket)),
            Answer::Elsewhere => (400, "InvalidParameterValue", elsewhere(&bucket)),
            Answer::MissingPermissions(action) => (
                400,
                "IamRoleMissingPermissions",
                missing_permissions(&role_arn, action, &bucket),
            ),
            Answer::RoleNotFound => (404, "IamRoleNotFound", role_not_found(&role_arn)),
            Answer::Accepted => {
                let body = expect_ok(case, answered);
                let reported = text(&body, "ExportTaskIdentifier");
                assert_eq!(
                    reported, row.identifier,
                    "case {case:?}: expected the accepted export to report its own \
                     identifier, got {reported:?} in {body}"
                );
                continue;
            }
        };

        let body = expect_error(case, answered, status, code);
        let message = text(&body, "Message");
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {message:?} in {body}"
        );
        // A refused export records nothing: the Region holds no task under the identifier.
        let reported = expect_ok(
            case,
            call(
                &fixture.router,
                "DescribeExportTasks",
                &params!["ExportTaskIdentifier" => row.identifier],
            )
            .await,
        );
        assert!(
            texts(&reported, "ExportTaskIdentifier").is_empty(),
            "case {case:?}: a refused export records no task, got {reported}"
        );
    }
}
