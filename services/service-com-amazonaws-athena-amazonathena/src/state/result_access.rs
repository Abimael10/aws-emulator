// SPDX-License-Identifier: CC0-1.0

//! The results LOCATION a submission resolved: whether this account has it, and whether the
//! caller may write a query's results there.
//!
//! Athena "stores query results and metadata information for each query that runs in a query
//! result location that you can specify", and that location is a bucket of the CALLER's own
//! account rather than storage this service owns. Two things follow:
//!
//! 1. **The bucket has to be there.** Athena verifies the resolved location before it accepts
//!    the submission, and answers `InvalidRequestException` for one whose bucket it can
//!    neither verify nor create ([`unverifiable_output_bucket`]) — so a submission naming
//!    such a location mints no execution at all. What this emulator can verify is the one
//!    object store it has ([`holds_bucket`]): a bucket no `CreateBucket` put there is a
//!    bucket this account does not hold, and no account creates one on a client's behalf.
//! 2. **The caller has to be allowed to write it.** The object write is made with the
//!    CALLER's permissions, so it is decided by exactly the two documents Amazon S3 would
//!    decide it by: the identity-based policies in force on the caller, and the bucket's own
//!    policy ([`super::authorization::performed_for_caller_refused`]). A write the policies
//!    in force refuse is a write that does not happen, so the execution settles FAILED
//!    carrying that refusal ([`access_denied`]) and the location is left as it was — rather
//!    than reporting a success whose results were never persisted.
//! 3. **The bucket has to be owned by the account the submission expects.** A submission may
//!    name the account it expects to own the destination, and Athena makes its Amazon S3
//!    calls under that expectation ([`expected_owner_refusal`]): a destination owned by
//!    anyone else is a write Amazon S3 refuses, so the execution settles FAILED carrying
//!    that refusal and writes nothing — which is the whole point of a guard against writing
//!    results into a bucket another account has taken over.
//!
//! The action and the ARN are Amazon S3's own, named from that service's crate rather than
//! spelled again here, so a statement that covers one of its object writes covers this one.

use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::catalog_sql::data::parse_s3_uri;

/// The IAM action one result object is written under, which is Amazon S3's own action for an
/// object write.
pub const RESULT_OBJECT_WRITE_ACTION: &str =
    ::service_com_amazonaws_s3_amazons3::service::request_authorization::PUT_OBJECT_ACTION;

/// The bucket a resolved output location names, or `None` for a location that names no
/// Amazon S3 path at all.
///
/// Read through the one parser this crate reads an `s3://` location with, so the bucket a
/// submission is verified against and the bucket its results are written into are read out
/// of that location the same way.
pub fn output_bucket(location: &str) -> Option<String> {
    parse_s3_uri(location)
        .map(|(bucket, _)| bucket)
        .filter(|bucket| !bucket.is_empty())
}

/// Whether the one object store this emulator has holds `bucket`.
///
/// `true` for an engine carrying no object store at all: nothing about the destination can
/// be verified there, and a submission is not refused for what this emulator cannot see.
pub fn holds_bucket<Eng: Send + Sync + 'static>(engine: &Eng, bucket: &str) -> bool {
    let Some(state) = s3_state::service_state(engine) else {
        return true;
    };
    let store = state.lock();
    store.bucket(bucket).is_some()
}

/// The refusal `StartQueryExecution` answers a submission whose resolved output location
/// names a bucket this account can neither verify nor create with.
///
/// FIXED text around the bucket, which is the only part that comes from the request: the
/// bucket that location named.
pub fn unverifiable_output_bucket(bucket: &str) -> String {
    format!("Unable to verify/create output bucket {bucket}")
}

/// The `StateChangeReason` an execution settles FAILED with when the policies in force
/// refuse the caller the write of its result object: fixed text naming the object that was
/// not written, which is the key the submission resolved for it.
pub fn access_denied(location: &str) -> String {
    format!("Access denied when writing to location: {location}")
}

/// Why the account a submission EXPECTS to own its output bucket is not the account that
/// owns it, or `None` when the submission names the owner or names no account at all.
///
/// `ResultConfiguration$ExpectedBucketOwner` documents the rule: "The Amazon Web Services
/// account ID that you expect to be the owner of the Amazon S3 bucket specified by
/// `ResultConfiguration$OutputLocation`. If set, Athena uses the value for
/// `ExpectedBucketOwner` when it makes Amazon S3 calls to your specified output location. If
/// the `ExpectedBucketOwner` Amazon Web Services account ID does not match the actual owner
/// of the Amazon S3 bucket, the call fails with a permissions error." So the write Athena
/// would make is refused, the query settles FAILED carrying that refusal, and nothing is
/// written at the location. This emulator serves ONE account, so that account owns every
/// bucket it holds and any other id names an owner no bucket here has — the same reading the
/// object store itself applies to the header this member becomes
/// (`s3_state`'s own expected-owner guard).
///
/// The sentence is Athena's own for a result write its permissions refused: fixed text with
/// the object the write addressed in it, which is the only part the request and the state
/// decide. It names what the caller has to check — the bucket's permissions, the expected
/// owner, and the key results are encrypted under — because the refusal says only that the
/// write was denied.
pub fn expected_owner_refusal(expected: Option<&str>, uri: &str) -> Option<String> {
    let expected = expected?.trim();
    if expected.is_empty() || expected == ::engine::emulator_account_id() {
        return None;
    }
    Some(output_access_denied(uri))
}

/// The `StateChangeReason` an execution settles FAILED with when Athena's own write of its
/// results to `uri` was denied: wholly fixed text around that object.
pub fn output_access_denied(uri: &str) -> String {
    format!(
        "Access denied when writing output to url: {uri} . Please ensure you are allowed to \
         access the S3 bucket. If specifying an expected bucket owner, confirm the bucket is \
         owned by the expected account. If you are encrypting query results with KMS key, \
         please ensure you are allowed to access your KMS key"
    )
}

/// Why the caller may not write the result object `uri` names, or `None` when the policies in
/// force permit it.
///
/// The decision is taken on the OBJECT the run would write, against the identity-based
/// documents in force on the caller and the policy the destination bucket carries, both read
/// at the instant of the call — so a policy just put decides immediately and one just
/// deleted stops deciding immediately. A location that names no object store path is decided
/// by nothing here: there is no bucket whose policy could speak about it.
pub fn write_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    uri: &str,
) -> Option<String> {
    let (bucket, key) = parse_s3_uri(uri).filter(|(_, key)| !key.is_empty())?;
    // The document is CLONED out from under the object store's guard, so the decision below
    // is taken with no lock of either service held.
    let policy = {
        let state = s3_state::service_state(engine)?;
        let store = state.lock();
        store.bucket(&bucket).and_then(|held| held.policy.clone())
    };
    super::authorization::performed_for_caller_refused(
        engine,
        ctx,
        policy.as_deref(),
        RESULT_OBJECT_WRITE_ACTION,
        &s3_state::object_arn(&bucket, &key),
    )
    .then(|| access_denied(uri))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the bucket a submission is verified against is the one its output location
    /// names, whatever prefix follows it, and a location that names no object store path
    /// names no bucket.
    #[test]
    fn the_verified_bucket_is_the_one_the_output_location_names() {
        // (case, the location, the bucket it names). The rows differ along whether the
        // location carries a prefix, how deep that prefix is, whether it ends with the
        // separator, and whether it is an Amazon S3 URI at all.
        let cases: Vec<(&str, &str, Option<&str>)> = vec![
            ("a bucket and a prefix", "s3://results/athena/", Some("results")),
            ("a prefix of several parts", "s3://results/a/b/c/", Some("results")),
            ("no trailing separator", "s3://results/athena", Some("results")),
            ("the bucket alone", "s3://results", Some("results")),
            ("the bucket alone, with a separator", "s3://results/", Some("results")),
            ("another scheme", "https://results/athena/", None),
            ("no scheme at all", "results/athena/", None),
            ("the scheme alone", "s3://", None),
        ];

        for (case, location, expected) in &cases {
            let actual = output_bucket(location);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected the bucket {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: each refusal is its fixed sentence with the value the request decided taken from
    /// the request — the bucket for the submission's refusal, the resolved object key for the
    /// write's.
    #[test]
    fn each_refusal_names_what_the_request_decided() {
        for bucket in ["amazon-reviews-pds", "results-eu", "a.b-c"] {
            let stated = unverifiable_output_bucket(bucket);
            let expected = format!("Unable to verify/create output bucket {bucket}");
            assert_eq!(
                stated, expected,
                "case {bucket:?}: expected {expected:?}, got {stated:?}"
            );
        }
        for uri in [
            "s3://denied/results/0b1a.csv",
            "s3://denied/0b1a.txt",
            "s3://other/deep/prefix/0b1a.csv",
        ] {
            let stated = access_denied(uri);
            let expected = format!("Access denied when writing to location: {uri}");
            assert_eq!(
                stated, expected,
                "case {uri:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}
