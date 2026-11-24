// SPDX-License-Identifier: CC0-1.0

//! The destination bucket's DELIVERY GRANT: the precondition a report definition has to
//! satisfy before this service accepts it.
//!
//! A Cost and Usage Report is not written by the caller: the billing pipeline writes it
//! into the Amazon S3 bucket the definition names, in the name of its own service
//! principal ([`DELIVERY_SERVICE_PRINCIPAL`]). So the grant that makes a definition
//! deliverable is one the BUCKET's own policy carries, and the service evaluates it before
//! it accepts a definition at all: it reads the bucket's ACL and policy and writes the
//! report's files under the definition's prefix ([`BUCKET_DELIVERY_ACTIONS`],
//! [`OBJECT_DELIVERY_ACTION`]), and a bucket whose policy grants its principal none of
//! that is a bucket the service could never deliver to.
//!
//! The precondition is evaluated PER CALL rather than once per bucket, so the identical
//! call refused against an unpolicied bucket is served as soon as the policy is put on that
//! same bucket; and a call it refuses persists nothing, so the definition does not exist
//! until the grant does. Both operations that accept a `ReportDefinition` judge it —
//! `PutReportDefinition` and `ModifyReportDefinition` — so the create and the update
//! cannot drift apart about a destination either of them would have to deliver to.
//!
//! The decision itself is [`::service_common::iam_authorization`]'s resource-policy
//! evaluation, the same one every other service judges a bucket policy with, carrying the
//! condition keys a cross-service delivery is made under
//! ([`DELIVERY_CONDITION_KEYS`]) so a policy scoped to this account and to this service's
//! report definitions decides as it would in production.
//!
//! What this module does NOT judge is whether the named bucket is THERE: the bucket lives
//! in Amazon S3, not in this service's state, and a refusal here has to be grounded in a
//! policy that was actually read — so a bucket this emulator's object store does not hold
//! is left to the delivery that would later find it missing, which this service reports as
//! the delivery status of the report.

use crate::ops::types::{ReportDefinition, ValidationException};

/// The AWS service principal a Cost and Usage Report is delivered in the name of, and
/// therefore the principal a destination bucket's policy has to grant the delivery to.
pub const DELIVERY_SERVICE_PRINCIPAL: &str = "billingreports.amazonaws.com";

/// The Amazon S3 actions the delivery performs on the BUCKET itself: it reads the bucket's
/// ACL and its policy to establish that the report can be written there.
///
/// SOURCE: the bucket policy AWS publishes for a Cost and Usage Report destination, whose
/// bucket-level statement grants the report service principal `s3:GetBucketAcl` and
/// `s3:GetBucketPolicy` on the bucket.
pub const BUCKET_DELIVERY_ACTIONS: &[&str] = &["s3:GetBucketAcl", "s3:GetBucketPolicy"];

/// The Amazon S3 action the delivery performs on the bucket's CONTENTS: it writes the
/// report's files under the definition's `S3Prefix`.
///
/// SOURCE: the same published policy, whose object-level statement grants the report
/// service principal `s3:PutObject` on the bucket's objects.
pub const OBJECT_DELIVERY_ACTION: &str = "s3:PutObject";

/// The Region the report definitions of an account live in: the Cost and Usage Report
/// control plane answers from `us-east-1` whatever Region the destination bucket is in, so
/// every report-definition ARN it names carries that Region.
pub const CONTROL_PLANE_REGION: &str = "us-east-1";

/// The condition key naming the account a delivery is made on behalf of.
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// The condition key naming the resource a delivery is made on behalf of: the account's
/// report definitions ([`delivery_source_arn`]).
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The condition key saying whether the request arrived over a secure transport.
///
/// A delivery this service performs answers it rather than leaving it open: the emulator
/// serves every endpoint over TLS and the write goes through one of them, so a bucket whose
/// policy carries the usual clause refusing everything that did NOT arrive that way refuses
/// nothing here — where a key with no value would leave that clause undecided, and an
/// undecided refusal still refuses.
const SECURE_TRANSPORT_KEY: &str = "aws:SecureTransport";

/// The value that key carries, as IAM's boolean literals spell it.
const SECURE_TRANSPORT_VALUE: &str = "true";

/// The condition keys the verification carries a value for, so a clause over one of them is
/// decided rather than left undecided.
const DELIVERY_CONDITION_KEYS: &[&str] =
    &[SOURCE_ACCOUNT_KEY, SOURCE_ARN_KEY, SECURE_TRANSPORT_KEY];

/// The resource the verification is made on behalf of: the report definitions of this
/// account, in the Region the control plane holds them in
/// ([`CONTROL_PLANE_REGION`]).
///
/// It is the account's definitions as a WHOLE rather than one of them, because the check
/// establishes that the bucket would take this account's reports — on the create the
/// definition being judged does not exist yet — and because that is the resource the
/// published grant's `aws:SourceArn` clause names. The ARN is built through
/// `engine::emulator_arn`, so the account id is never a literal here.
pub fn delivery_source_arn() -> String {
    ::engine::emulator_arn("cur", CONTROL_PLANE_REGION, "definition/*")
}

/// Judge the destination bucket's own POLICY, the way the service does before it accepts a
/// definition: `Ok(())` when the policy grants [`DELIVERY_SERVICE_PRINCIPAL`] every
/// delivery action on the resource that action is performed on, and the modelled
/// `ValidationException` (HTTP 400) when it grants any of them to nobody.
///
/// A bucket this emulator's object store does not hold is not judged here (see the module
/// docs): there is no policy to ground a refusal in.
pub fn validate_delivery_grant<Eng: Send + Sync + 'static>(
    engine: &Eng,
    definition: &ReportDefinition,
) -> Result<(), ValidationException> {
    let Some(objects) = ::service_com_amazonaws_s3_amazons3::state::service_state(engine) else {
        return Ok(());
    };
    // The object store is read, the one document this decision is about is taken out of it,
    // and the lock is released before the policy is evaluated: the two services' stores are
    // never held at once.
    let policy = {
        let store = objects.lock();
        match store.bucket(&definition.s3_bucket) {
            // A bucket this emulator holds either carries a policy or carries none, and a
            // bucket with none grants nothing — which is exactly the recorded refusal.
            Some(bucket) => bucket.policy.clone().unwrap_or_default(),
            None => return Ok(()),
        }
    };
    if grants_delivery(&policy, definition) {
        return Ok(());
    }
    Err(delivery_grant_missing(definition))
}

/// Whether `policy` grants the delivery: EVERY action the delivery performs, each on the
/// resource it is performed on, allowed to this service's principal.
///
/// One missing action is a destination the service could not deliver to, so the grant is
/// the conjunction rather than any one of them.
fn grants_delivery(policy: &str, definition: &ReportDefinition) -> bool {
    let bucket = ::service_com_amazonaws_s3_amazons3::state::bucket_arn(&definition.s3_bucket);
    let objects = delivered_object_arn(definition);
    let principals = [DELIVERY_SERVICE_PRINCIPAL.to_string()];
    let source_arn = delivery_source_arn();
    let context = [
        (
            SOURCE_ACCOUNT_KEY.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
        (SOURCE_ARN_KEY.to_string(), source_arn),
        (
            SECURE_TRANSPORT_KEY.to_string(),
            SECURE_TRANSPORT_VALUE.to_string(),
        ),
    ];
    BUCKET_DELIVERY_ACTIONS
        .iter()
        .map(|action| (*action, bucket.as_str()))
        .chain(::std::iter::once((
            OBJECT_DELIVERY_ACTION,
            objects.as_str(),
        )))
        .all(|(action, resource)| {
            let request =
                ::service_common::iam_authorization::Request::new(action, resource, &principals)
                    .with_context(&context)
                    .with_published_keys(DELIVERY_CONDITION_KEYS);
            ::service_common::iam_authorization::resource_decision(policy, &request)
                == ::service_common::iam_authorization::Decision::Allowed
        })
}

/// The ARN of the objects a delivery of `definition` writes: the destination bucket and the
/// prefix the report's files are written below ("The prefix that Amazon Web Services adds to
/// the report name when Amazon Web Services delivers the report" — the modelled doc for
/// `S3Prefix`).
///
/// Spelled through Amazon S3's own object-ARN helper, so this service names an object the
/// way that service names one, and ending in the key separator because what is written under
/// the prefix is a path and not the prefix itself: a policy that grants the bucket's objects
/// (`arn:aws:s3:::<bucket>/*`) covers it, while one that grants an unrelated prefix does not.
fn delivered_object_arn(definition: &ReportDefinition) -> String {
    let prefix = definition.s3_prefix.trim_matches('/');
    let key = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    ::service_com_amazonaws_s3_amazons3::state::object_arn(&definition.s3_bucket, &key)
}

/// The modelled 400 for a destination bucket whose policy does not grant the delivery.
///
/// RECORDED AWS behaviour for the wire shape: the condition is reported under
/// `ValidationException` — "The input fails to satisfy the constraints specified by an
/// Amazon Web Services service" — and the `Message` states that the customer bucket's
/// permission could not be verified, naming the account the verification ran for and the
/// bucket and Region the request named. Those three are the message's only variable parts
/// and all come from the caller and the request: the account is this emulator's own
/// canonical one (read through `engine::emulator_account_id` rather than written as a
/// literal), the bucket is the definition's `S3Bucket` and the Region its `S3Region`.
fn delivery_grant_missing(definition: &ReportDefinition) -> ValidationException {
    ValidationException {
        message: Some(format!(
            "Failed to verify customer bucket permission. accountId= {}, bucket name: {}, \
             bucket region: {}",
            ::engine::emulator_account_id(),
            definition.s3_bucket,
            definition.s3_region
        )),
    }
}
