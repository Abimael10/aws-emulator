// SPDX-License-Identifier: CC0-1.0

//! The validation a put of an `aws:ec2:image` parameter is held to, and what a put whose
//! value fails it leaves behind: nothing.
//!
//! "If you call PutParameter with aws:ec2:image data type, a successful HTTP 200 response
//! does not guarantee that your parameter was successfully created or updated. The
//! aws:ec2:image value is validated asynchronously, and the PutParameter call returns
//! before the validation is complete. If you submit an invalid AMI value, the PutParameter
//! operation will return success, but the asynchronous validation will fail and the
//! parameter will not be created or updated"
//! (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`). Two
//! observable things are stated there, and both are this module's contract:
//!
//! 1. the call ANSWERS SUCCESS whatever the value is — no error is modelled for an AMI id
//!    the account cannot see, so the put reports 200 with the version number it allocated;
//! 2. a value that fails the validation NEVER LANDS — "the parameter will not be created or
//!    updated", so the parameter a read serves is the one the put found: a create leaves no
//!    parameter behind, an overwrite leaves the versions, the value and the data type the
//!    parameter already had, and its history holds no trace of the write.
//!
//! So the write is what the validation gates, not a version already in the store that a
//! later read takes back out: nothing a caller reads ever reports the unvalidated value,
//! which is what a read of the parameter immediately after such a put reports against the
//! real service. The asynchrony is in the ANSWER — success for a write that did not land —
//! and that is the part of it a client can observe.
//!
//! What the validation DECIDES is whether the value "is in the required format … and that
//! the specified AMI is available in your Amazon Web Services account" (the same doc). The
//! catalogue of images the account can see belongs to Amazon EC2, so that is where the
//! question is asked (`ComAmazonawsEc2Amazonec2State::images`, the collection its own
//! `DescribeImages` reports from) rather than against a second copy of it here: a value
//! Parameter Store serves under this data type is an id that service really resolves, and
//! an id it does not is one no `AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>` resolution
//! could ever have read.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// The catalogue the validation is decided against: a handle to the collection that OWNS the
/// images, so every decision is read from the live store rather than from a copy of it.
///
/// Held, rather than resolved per call, by a writer that reaches this rule from ANOTHER API —
/// Cloud Control API's `AWS::SSM::Parameter`, say, which holds the parameter store itself the
/// same way ([`validates_against`]).
pub type ImageCatalogue = ::engine::ServiceState<ec2::ComAmazonawsEc2Amazonec2State>;

/// The catalogue this engine carries, or `None` when it carries none.
pub fn image_catalogue<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<ImageCatalogue> {
    ec2::service_state(engine)
}

/// Whether a put of `value` under `data_type` passes the validation the service makes
/// before its write lands.
///
/// [`super::DATA_TYPE_EC2_IMAGE`] is the one data type carrying such a validation: `text`
/// holds a value to nothing beyond the value's own constraints, and `aws:ssm:integration`
/// names a subscription this emulator holds nothing to validate against — so a put under
/// either lands whatever it carries.
///
/// Call it with NO guard of this service's store held: it reads Amazon EC2's own store.
///
/// A catalogue this emulator HAS NONE OF answers nothing about the value, so the value
/// lands: an engine carrying no Amazon EC2 store, and one whose image collection is empty —
/// an endpoint where no image exists at all, which no account of the real service is — hold
/// no catalogue for the question to be put to. A check this emulator cannot make is not
/// evidence against the value, the same reading [`super::node_registration`] takes of a grant
/// it cannot read.
pub fn validates<Eng: Send + Sync + 'static>(engine: &Eng, data_type: &str, value: &str) -> bool {
    validates_against(image_catalogue(engine).as_ref(), data_type, value)
}

/// [`validates`] against a catalogue already in hand, for a writer that holds one.
pub fn validates_against(
    catalogue: Option<&ImageCatalogue>,
    data_type: &str,
    value: &str,
) -> bool {
    if data_type != super::DATA_TYPE_EC2_IMAGE {
        return true;
    }
    let Some(catalogue) = catalogue else {
        return true;
    };
    let images = catalogue.lock();
    images.images.is_empty() || images.images.contains_key(value)
}
