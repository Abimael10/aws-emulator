// SPDX-License-Identifier: CC0-1.0

//! What a write of a web app's CUSTOMIZATION that lost the claim is refused with.
//!
//! `UpdateWebAppCustomization` and `DeleteWebAppCustomization` both write the single
//! customization object a web app holds, and both model `ConflictException`
//! (`smithy_query(query="operation",operation="UpdateWebAppCustomization,DeleteWebAppCustomization")`
//! lists it, HTTP 409, for each). The service applies one such write at a time, so two
//! writers that overlap have exactly one winner and the surviving customization is one
//! submitter's values rather than a mixture of both.
//!
//! The guard itself is [`crate::state::web_app_write_claim`], which is the ONE claim
//! every write of a web app's objects goes through — the web app's own configuration
//! included; this module is the customization object's half of it: the claim it takes,
//! and the two sentences the service refuses a lost write with.

use crate::ops::types::ConflictException;
use crate::state::web_app_write_claim::{self, Object};

pub use crate::state::web_app_write_claim::{admit_a_racing_writer, Refusal, WriteClaim};

/// Which write of a web app's customization a refusal is about: the two operations that
/// write the one object, and the two sentences the service refuses them with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// `UpdateWebAppCustomization`, which assigns the customization's members.
    Update,
    /// `DeleteWebAppCustomization`, which removes the whole object.
    Delete,
}

/// `ConflictException`'s `Message` for an UPDATE of a web app's customization that could
/// not be carried out because another write of the same object was still being applied.
///
/// The service's own fixed text for the condition: it states that the write could not be
/// done, names the object by the service's own name for it, and carries nothing from the
/// request or the store — the web app a caller addressed is the one it named. Worded for
/// the write it refuses, which is what [`DELETE_CONFLICT`] is for the other one, and what
/// [`crate::state::WEB_APP_UPDATE_CONFLICT`] is for a write of the web app itself.
pub const UPDATE_CONFLICT: &str = "Unable to update WebApp customization";

/// `ConflictException`'s `Message` for a DELETE of a web app's customization that lost
/// the claim — [`UPDATE_CONFLICT`]'s sentence for the other write of the one object.
pub const DELETE_CONFLICT: &str = "Unable to delete WebApp customization";

/// The conflict payload for a customization write that lost the claim, worded for the
/// write that lost it.
pub fn write_conflict(write: Write) -> ConflictException {
    ConflictException {
        message: match write {
            Write::Update => UPDATE_CONFLICT.to_string(),
            Write::Delete => DELETE_CONFLICT.to_string(),
        },
    }
}

/// Take the claim on `web_app_id`'s CUSTOMIZATION, when the web app exists and no other
/// write of that object holds it.
///
/// The caller must NOT hold the state lock: the claim takes it, and releases it before
/// answering.
pub fn claim(
    state: &::engine::ServiceState<crate::state::ComAmazonawsTransferTransferserviceState>,
    web_app_id: &str,
) -> Result<WriteClaim, Refusal> {
    web_app_write_claim::claim(state, web_app_id, Object::Customization)
}
