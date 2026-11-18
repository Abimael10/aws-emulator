// SPDX-License-Identifier: CC0-1.0

//! The validation a put of an `aws:ec2:image` parameter leaves OWING, and the version that
//! validation takes back out of the store.
//!
//! "If you call PutParameter with aws:ec2:image data type, a successful HTTP 200 response
//! does not guarantee that your parameter was successfully created or updated. The
//! aws:ec2:image value is validated asynchronously, and the PutParameter call returns
//! before the validation is complete. If you submit an invalid AMI value, the PutParameter
//! operation will return success, but the asynchronous validation will fail and the
//! parameter will not be created or updated"
//! (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`). So the put
//! answers 200 with the version it wrote, and the validation it still owes is performed
//! afterwards — which is what this module is.
//!
//! What the validation DECIDES is whether the value "is in the required format … and that
//! the specified AMI is available in your Amazon Web Services account" (the same doc). The
//! catalogue of images the account can see belongs to Amazon EC2, so that is where the
//! question is asked (`ComAmazonawsEc2Amazonec2State::images`, the collection its own
//! `DescribeImages` reports from) rather than against a second copy of it here: a value
//! Parameter Store serves under this data type is an id that service really resolves, and
//! an id it does not is one no `AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>` resolution
//! could ever have read.
//!
//! ## What is written, and why a report may write it
//!
//! The validation falls due [`VALIDATION_DELAY`] after the put, whether or not anybody
//! calls in between, so a caller reading straight after the 200 still sees the version the
//! put answered with — as it would against the real service, which answers "success" and
//! only then fails the validation. [`settle_validations`] is what materialises the
//! validations the clock has already made due: that write was owed at the same instant
//! whether or not anybody called, which is why a REPORTING operation of Parameter Store
//! takes it before it reads the store.
//!
//! A version whose validation FAILED is unwritten through the store's one seam for that
//! ([`super::ComAmazonawsSsmAmazonssmState::discard_parameter_version`]): the parameter
//! "will not be created or updated", so a create's only version leaves no parameter behind
//! and an overwrite's leaves the versions the parameter already had. A version whose
//! validation PASSED keeps its place and owes nothing more — the validation happens once.

use std::time::{Duration, Instant};

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// How long after the put the asynchronous `aws:ec2:image` validation falls due.
///
/// AWS publishes no span for it — "the PutParameter call returns before the validation is
/// complete" says only that the call does not wait for it — so this is the emulator's
/// CHOSEN transient window: three seconds, the span this emulator gives a transition
/// nobody documents (see [`super::DOCUMENT_TRANSITION_DELAY`]). It is deliberately never
/// zero: a caller that reads the parameter straight after the 200 sees the version that
/// call answered with, the way it would against the real service.
pub const VALIDATION_DELAY: Duration = Duration::from_secs(3);

/// When the asynchronous validation a put of a version of `data_type` leaves owing falls
/// due, or `None` for a data type the put itself validates completely.
///
/// [`super::DATA_TYPE_EC2_IMAGE`] is the one data type whose check is asynchronous; `text`
/// carries no constraint beyond the value's own, and `aws:ssm:integration` names a
/// subscription this emulator holds nothing to validate against.
pub fn validation_due_at(data_type: &str) -> Option<Instant> {
    (data_type == super::DATA_TYPE_EC2_IMAGE).then(|| Instant::now() + VALIDATION_DELAY)
}

/// Materialise every `aws:ec2:image` validation of `region` the clock already owes, so a
/// read of Parameter Store reports what was due whether or not anybody called.
///
/// Call it with NO guard of this service's store held: it reads this store, then Amazon
/// EC2's image catalogue, and then writes this one.
///
/// A version whose value names no image the account can see is discarded; one whose value
/// does is left in place with nothing more owed. An engine carrying no Amazon EC2 store at
/// all holds no catalogue to validate against, so the validation is not performed and the
/// version stands: a check this emulator cannot make is not evidence against the value,
/// the same reading [`super::node_registration`] takes of a grant it cannot read.
pub fn settle_validations<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = Instant::now();
    // The versions of this Region whose validation has fallen due, as the name they belong
    // to, their number and the value to validate. Collected under the guard and the guard
    // then released, so no state of this service is held while the catalogue is read.
    let due: Vec<(String, i64, String)> = {
        let store = state.lock();
        store
            .parameters_of_region(region)
            .flat_map(|(name, record)| {
                record.versions.iter().filter_map(move |version| {
                    version
                        .image_validation_due_at
                        .filter(|due_at| *due_at <= now)
                        .map(|_| (name.to_string(), version.version, version.value.clone()))
                })
            })
            .collect()
    };
    if due.is_empty() {
        return;
    }
    let Some(ec2_state) = ec2::service_state(engine) else {
        return;
    };
    // Whether each due version's value is an image the account can see, decided against
    // the one collection that answers that question for every reader of it.
    let validated: Vec<(String, i64, bool)> = {
        let images = ec2_state.lock();
        due.into_iter()
            .map(|(name, version, value)| {
                let holds = images.images.contains_key(value.as_str());
                (name, version, holds)
            })
            .collect()
    };
    let mut store = state.lock();
    for (name, version, passed) in validated {
        if !passed {
            store.discard_parameter_version(region, &name, version);
            continue;
        }
        // The validation happened, so nothing is owed for this version any more and a
        // later read does not ask the catalogue about it again.
        if let Some(record) = store.parameter_mut(region, &name) {
            if let Some(held) = record
                .versions
                .iter_mut()
                .find(|held| held.version == version)
            {
                held.image_validation_due_at = None;
            }
        }
    }
}
