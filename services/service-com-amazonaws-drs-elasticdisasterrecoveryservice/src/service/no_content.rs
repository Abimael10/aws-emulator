// SPDX-License-Identifier: CC0-1.0

//! What this service answers a success that carries no member with: the STATUS, and the
//! DOCUMENT such an answer carries.
//!
//! Several operations of this service answer a success a caller has nothing to read out
//! of — the tag write, the tag removal and each delete — and all of them answer it the same
//! way, which is why what that answer IS gets stated here once instead of per operation:
//!
//! * the STATUS, [`NO_CONTENT_STATUS`], which is `service_common::no_content_responses`',
//!   this tree's one reading of the status such an answer carries, named here rather than
//!   written out again;
//! * the DOCUMENT: the empty JSON object the `restJson1` serializer renders for an output
//!   structure that declares no member.
//!
//! A delete gets both halves from its model: the operation's `@http` code is the status, and
//! its generated response structure is what the serializer renders the object from. The
//! tagging route's mutations get neither — the model declares their output as
//! `smithy.api#Unit`, the one shape the renderers treat as bodyless, and leaves their
//! `@http` code defaulted — so each of those two operations declares the status from here
//! and a response structure of its own, so that what a caller gets for a tag write is what
//! it gets for a delete.
//!
//! This service does NOT take `service_common::no_content_responses::without_content`, the
//! correction for a service whose answers at this status carry no body at all: the recorded
//! answers of THIS service carry the document above, which is also what its modelled
//! deletes have always answered with.
//!
//! Nothing here frames the response: how many bytes a message states for its body is the
//! HTTP writer's decision, and what this service decides is the document in it.

pub(crate) use ::service_common::no_content_responses::NO_CONTENT_STATUS;
