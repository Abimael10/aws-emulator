// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHEN `PutBucketVersioning` requires the `x-amz-mfa` header.
//!
//! The rule under test, asserted over the whole class of inputs rather than one request:
//! MFA authentication is required only where MFA Delete is in play — a request that turns
//! MFA Delete on, and a request that changes the versioning state of a bucket MFA Delete is
//! already in force on (its recorded configuration names MFA Delete `Enabled`). Every other
//! `VersioningConfiguration` — one naming no MFA Delete, and one naming it `Disabled` on a
//! bucket it is not in force on — is applied with no `x-amz-mfa` header at all, and the
//! recorded state afterwards is exactly what that configuration carried.
//!
//! A request that needs the authentication and carries none is refused 400 `InvalidRequest`
//! and records NOTHING: the bucket keeps the versioning state it had.
//!
//! The table below spans the dimensions the rule generalises over: the MFA Delete state the
//! bucket starts in (never configured, configured and off, in force), each published
//! `MFADelete` value plus its absence, each published `BucketVersioningStatus`, and whether
//! the request carries the header.

use service_com_amazonaws_s3_amazons3::state::{
    INVALID_REQUEST_CODE, INVALID_REQUEST_STATUS, MFA_AUTHENTICATION_REQUIRED_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};

/// The values `BucketVersioningStatus` accepts on the wire, from the model
/// (`smithy_query(query="shape",shape="BucketVersioningStatus")` reports
/// `values: ["Enabled", "Suspended"]`).
const VERSIONING_STATUSES: &[&str] = &["Enabled", "Suspended"];

/// The values `MFADelete` accepts on the wire, from the model
/// (`smithy_query(query="shape",shape="MFADelete")` reports
/// `values: ["Enabled", "Disabled"]`).
const MFA_DELETE_VALUES: &[&str] = &["Enabled", "Disabled"];

/// A serial number and a token, the concatenation `MFA` is documented to carry ("The
/// concatenation of the authentication device's serial number, a space, and the value that
/// is displayed on your authentication device", `PutBucketVersioningRequest$MFA`). Nothing
/// about it is verified — what the rule turns on is whether the header is THERE.
const MFA_HEADER: (&str, &str) = ("x-amz-mfa", "arn:aws:iam::123456789012:mfa/device 123456");

/// The MFA Delete state a bucket starts a case in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prior {
    /// The bucket's versioning state has never been set at all.
    Unset,
    /// A recorded configuration naming MFA Delete `Disabled`: configured, and NOT in force.
    Disabled,
    /// A recorded configuration naming MFA Delete `Enabled`: in force.
    Enabled,
}

impl Prior {
    /// Whether MFA Delete is in force, which is what makes the header required for any
    /// later change of the bucket's versioning state.
    fn in_force(self) -> bool {
        self == Prior::Enabled
    }

    /// The `Status` and `MfaDelete` a read reports for a bucket in this state.
    fn reported(self) -> (Option<&'static str>, Option<&'static str>) {
        match self {
            Prior::Unset => (None, None),
            Prior::Disabled => (Some("Enabled"), Some("Disabled")),
            Prior::Enabled => (Some("Enabled"), Some("Enabled")),
        }
    }
}

/// A `VersioningConfiguration` payload carrying the members that are `Some`.
///
/// `VersioningConfiguration$MFADelete` carries `xmlName: "MfaDelete"`, so that — not the
/// member name — is the element on the wire.
fn configuration(status: Option<&str>, mfa_delete: Option<&str>) -> String {
    format!(
        "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{}{}\
         </VersioningConfiguration>",
        status
            .map(|status| format!("<Status>{status}</Status>"))
            .unwrap_or_default(),
        mfa_delete
            .map(|value| format!("<MfaDelete>{value}</MfaDelete>"))
            .unwrap_or_default()
    )
}

/// `PUT /<bucket>?versioning` with that configuration, carrying the MFA header when
/// `with_mfa`.
async fn put_versioning(
    router: &MultiServiceRouter,
    bucket: &str,
    status: Option<&str>,
    mfa_delete: Option<&str>,
    with_mfa: bool,
) -> Answer {
    let headers: Vec<(&str, &str)> = if with_mfa { vec![MFA_HEADER] } else { Vec::new() };
    send(
        router,
        "PUT",
        &format!("/{bucket}?versioning"),
        &headers,
        &configuration(status, mfa_delete),
    )
    .await
}

/// `GET /<bucket>?versioning`, as the reported `(Status, MfaDelete)` pair.
async fn reported_versioning(
    router: &MultiServiceRouter,
    bucket: &str,
    case: &str,
) -> (Option<String>, Option<String>) {
    let read = send(router, "GET", &format!("/{bucket}?versioning"), &[], "").await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetBucketVersioning must report the state, got {} with body {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    (read.element("Status"), read.element("MfaDelete"))
}

/// Create `bucket` and put it in `prior`, failing the test if the setup did not take.
async fn bucket_in(router: &MultiServiceRouter, bucket: &str, prior: Prior) {
    let created = create_bucket(router, bucket, None, None).await;
    assert_eq!(
        created.status,
        200,
        "case {prior:?}: the bucket {bucket:?} must be created, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let setup = match prior {
        Prior::Unset => return,
        // Configured and OFF, set WITHOUT the header: that is the request the rule makes
        // acceptable, so the setup itself asserts it.
        Prior::Disabled => put_versioning(router, bucket, Some("Enabled"), Some("Disabled"), false),
        // In force, which only a request carrying the header can put it in.
        Prior::Enabled => put_versioning(router, bucket, Some("Enabled"), Some("Enabled"), true),
    }
    .await;
    assert_eq!(
        setup.status,
        200,
        "case {prior:?}: the setup put must be accepted, got {} with body {:?}",
        setup.status,
        String::from_utf8_lossy(&setup.body)
    );
    let (status, mfa_delete) = reported_versioning(router, bucket, &format!("{prior:?} setup")).await;
    let (expected_status, expected_mfa_delete) = prior.reported();
    assert_eq!(
        (status.as_deref(), mfa_delete.as_deref()),
        (expected_status, expected_mfa_delete),
        "case {prior:?}: the setup must leave Status {expected_status:?} and MfaDelete \
         {expected_mfa_delete:?}, got {status:?} and {mfa_delete:?}"
    );
}

#[tokio::test]
async fn the_mfa_header_is_required_only_where_mfa_delete_is_in_play() {
    let router = router();
    let mut case_index = 0usize;
    for prior in [Prior::Unset, Prior::Disabled, Prior::Enabled] {
        for status in VERSIONING_STATUSES {
            // Every published `MFADelete` value, plus a configuration carrying none.
            let mfa_deletes: Vec<Option<&str>> = std::iter::once(None)
                .chain(MFA_DELETE_VALUES.iter().map(|value| Some(*value)))
                .collect();
            for mfa_delete in mfa_deletes {
                for with_mfa in [false, true] {
                    case_index += 1;
                    let bucket = format!("mfa-delete-gate-{case_index}");
                    let case = format!(
                        "prior {prior:?}, Status {status}, MfaDelete {mfa_delete:?}, \
                         x-amz-mfa carried: {with_mfa}"
                    );
                    bucket_in(&router, &bucket, prior).await;

                    // The rule: MFA authentication is required to turn MFA Delete on, and
                    // to change the state of a bucket it is already in force on.
                    let required = mfa_delete == Some("Enabled") || prior.in_force();
                    let accepted = !required || with_mfa;

                    let answer =
                        put_versioning(&router, &bucket, Some(status), mfa_delete, with_mfa).await;
                    let body = String::from_utf8_lossy(&answer.body).to_string();
                    if accepted {
                        assert_eq!(
                            answer.status, 200,
                            "case {case:?}: PutBucketVersioning must accept it, got {} with \
                             body {body:?}",
                            answer.status
                        );
                        assert_eq!(
                            body, "",
                            "case {case:?}: PutBucketVersioning answers an empty body, got \
                             {body:?}"
                        );
                    } else {
                        assert_eq!(
                            answer.status, INVALID_REQUEST_STATUS,
                            "case {case:?}: expected HTTP {INVALID_REQUEST_STATUS}, got {} with \
                             body {body:?}",
                            answer.status
                        );
                        assert_eq!(
                            answer.error_code(),
                            INVALID_REQUEST_CODE,
                            "case {case:?}: expected code {INVALID_REQUEST_CODE}, got {:?}",
                            answer.error_code()
                        );
                        assert_eq!(
                            answer.error_message(),
                            MFA_AUTHENTICATION_REQUIRED_MESSAGE,
                            "case {case:?}: expected message \
                             {MFA_AUTHENTICATION_REQUIRED_MESSAGE:?}, got {:?}",
                            answer.error_message()
                        );
                    }

                    // What the bucket carries afterwards: the submitted configuration when
                    // the put was served, and the state it already had when it was refused.
                    let (expected_status, expected_mfa_delete) = if accepted {
                        (Some(*status), mfa_delete)
                    } else {
                        prior.reported()
                    };
                    let (got_status, got_mfa_delete) =
                        reported_versioning(&router, &bucket, &case).await;
                    assert_eq!(
                        (got_status.as_deref(), got_mfa_delete.as_deref()),
                        (expected_status, expected_mfa_delete),
                        "case {case:?}: expected Status {expected_status:?} and MfaDelete \
                         {expected_mfa_delete:?}, got {got_status:?} and {got_mfa_delete:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn suspending_versioning_while_declaring_mfa_delete_disabled_lands() {
    let router = router();
    let bucket = "versioning-suspend-with-mfa-delete-disabled";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status,
        200,
        "the bucket {bucket:?} must be created, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    // Versioning on, then off again — the second put declaring MfaDelete `Disabled` the way
    // a client that sets the field at all emits it, and carrying NO x-amz-mfa header.
    for (case, status, mfa_delete) in [
        ("enable versioning", "Enabled", None),
        ("suspend, MfaDelete Disabled", "Suspended", Some("Disabled")),
    ] {
        let answer = put_versioning(&router, bucket, Some(status), mfa_delete, false).await;
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {body:?}",
            answer.status
        );
        assert_eq!(
            body, "",
            "case {case:?}: expected an empty body, got {body:?}"
        );
    }

    // And the read reports what landed, every time it is asked.
    for attempt in 0..3 {
        let case = format!("read {attempt} after the suspend");
        let (status, mfa_delete) = reported_versioning(&router, bucket, &case).await;
        assert_eq!(
            (status.as_deref(), mfa_delete.as_deref()),
            (Some("Suspended"), Some("Disabled")),
            "case {case:?}: expected Status Some(\"Suspended\") and MfaDelete \
             Some(\"Disabled\"), got {status:?} and {mfa_delete:?}"
        );
    }
}
