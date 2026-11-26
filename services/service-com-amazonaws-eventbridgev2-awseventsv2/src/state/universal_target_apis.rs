// SPDX-License-Identifier: CC0-1.0

//! Where this service reads the APIs a UNIVERSAL target may name.
//!
//! Which operations an AWS SDK service has, which of them only READ and what an
//! operation's input requires are facts of the CALLED service's own model, not
//! of this one's, and more than one service here takes a target that names an
//! API operation rather than a resource. They are therefore stated once for the
//! tree, in [`::service_common::universal_target_apis`], and this module is this
//! crate's name for that statement — so every reading of it inside the crate
//! goes through one path, and a second reader does not reach for a copy.
//!
//! What IS this service's own is read beside the target it belongs to
//! ([`super::subscriber_targets`]): the grammar of a universal target's ARN
//! under this service's namespace, and which calls a subscriber may not be
//! pointed at.

pub use ::service_common::universal_target_apis::{apis_of, ServiceApis};
