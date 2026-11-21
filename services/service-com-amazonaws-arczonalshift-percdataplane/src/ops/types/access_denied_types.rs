// SPDX-License-Identifier: CC0-1.0

//! The ACCESS DENIAL this service answers an unauthorized call with, as AWS carries it.
//!
//! Every operation of this service declares `AccessDeniedException` (403), and ARC
//! answers it from the layer that decides the caller's authorization rather than from
//! the operation: the recorded denials name the error class in `x-amzn-errortype` as a
//! shape of that framework (`AccessDeniedException:…/com.amazon.coral.service/`), not as
//! this service's own shape, and the document they carry is that layer's — the denial
//! text under the member `Message`, and no `__type` discriminator at all.
//!
//! The MEMBER NAME is declared here, from the small overlay model
//! `smithy-model/access-denial.json`, so it is generated the same way every modelled
//! shape is: one member list, one set of traits, no hand-written schema or serializer.
//! The published snapshot declares the member as `message` and is kept as shipped (see
//! `smithy-model/LOCAL-ADDITIONS.md`), so the overlay restates the shape exactly as the
//! snapshot declares it and adds the `@jsonName` the recorded wire carries. The rest of
//! the envelope — leaving the discriminator out — is the service's error serialization
//! (`crate::service::denial_envelope`).
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/access-denial.json");

/// The denial every operation of this service answers an unauthorized call with.
///
/// `message` is optional in the model, and a denial taken before any target was
/// resolved states no reason at all (`crate::state::identifier_not_attributable`), so
/// such a denial's document carries no member and is the empty JSON object.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#AccessDeniedException")]
#[derive(Debug, Clone, Default)]
pub struct AccessDeniedException {
    pub message: Option<String>,
}
