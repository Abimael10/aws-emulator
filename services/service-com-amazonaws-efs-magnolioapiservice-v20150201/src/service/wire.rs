// SPDX-License-Identifier: CC0-1.0

//! How this service's `restJson1` ERROR document carries a modelled error's members.
//!
//! RULE: every error document Amazon EFS answers with CARRIES the error shape's
//! `Message` member — when no rule produces a sentence for that error class the member
//! is written with JSON `null` rather than left out — so a client can tell "this class
//! carries no sentence" from "the sentence is the empty string".
//!
//! RULE: the operations that WRITE a replication configuration answer a not-found
//! refusal with a document holding the SENTENCE ALONE, naming the error shape in the
//! `x-amzn-errortype` header instead of in an `ErrorCode` member. [`MessageOnly`] is
//! that body, and its own documentation states the rule; the handlers reach it through
//! the [`FileSystemNotFound`] / [`ReplicationNotFound`] aliases.
//!
//! Every error shape in this service's model is a structure with an `ErrorCode` and an
//! OPTIONAL `Message` (target `ErrorMessage`, a plain string with no HTTP binding), some
//! of them with one more required member naming the resource:
//! `smithy_query(query="shape",shape="AccessPointAlreadyExists",
//! model_path="services/service-com-amazonaws-efs-magnolioapiservice-v20150201/smithy-model/com-amazonaws-efs-magnolioapiservice-v20150201.json")`.
//! The model marks that `ErrorCode` `@required` on most of them and OPTIONAL on six
//! (`InvalidPolicyException`, `PolicyNotFound`, `ReplicationNotFound`,
//! `ReplicationAlreadyExists`, `ConflictException`, `AvailabilityZonesMismatch`), and
//! WHICH members a refusal body carries is the operation's own contract: a handler that
//! leaves an optional member unset has it left out of the document, which is how
//! `PutFileSystemPolicy`'s policy-validation refusal travels as the sentence alone.
//! Most of this service's refusals DO carry a request-derived sentence (see
//! [`crate::state::file_system_not_found_message`] and its neighbours), and one class —
//! `AccessPointAlreadyExists` — carries none at all, because neither the model nor the
//! documentation states a rule that would produce one and the actionable datum travels
//! in its required `AccessPointId`.
//!
//! The framework's generic serializer writes an absent optional member by writing
//! NOTHING, which is right for a success body but drops the key out of the error
//! document — and an SDK cannot see the difference the rule is about: botocore
//! substitutes the EMPTY STRING for an absent `Message` key in a JSON error document
//! (`{"Error": {"Message": "", "Code": ""}}` is the dict it starts from and fills with
//! `body.get("message", body.get("Message", ""))`), and reports `None` only for a key
//! whose value is JSON null. An omitted member therefore reaches every client as
//! `Message == ""`, i.e. as "the sentence is empty", which is the opposite of what this
//! error class states.
//!
//! The first rule is not this service's alone, so the codec implementing it is the shared
//! [`service_common::error_message_member`]'s and this module only states which codec
//! this service takes. [`MessagePresentRestJson`] is the protocol `service.rs` hands the
//! dispatch sequence through [`smithy_server_runtime::Service::override_protocol`], so
//! EVERY modelled failure of EVERY operation goes out with the member present and no
//! handler, shape declaration or error constructor mentions the rendering at all.
//! Everything else — routing, request deserialization, success bodies (where an absent
//! optional member stays absent, as in every AWS JSON response), and an error whose
//! `Message` IS supplied — is the framework's own restJson1 protocol, delegated to
//! unchanged.

use std::marker::PhantomData;

use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;

use service_common::error_message_member::message_member;

pub use service_common::error_message_member::MessagePresentRestJson;

/// A modelled error shape whose refusal body this service answers with as the SENTENCE
/// ALONE — see [`MessageOnly`], which renders it.
pub trait MessageOnlyShape {
    /// The shape's own schema, as this crate's generated types declare it. The member
    /// written is that schema's message member, so the key on the wire stays the
    /// model's.
    const SCHEMA: &'static Schema;
    /// The shape name the error document's `__type` and the `x-amzn-errortype` header
    /// carry: with no `ErrorCode` member in the body, this is where a client of these
    /// operations reads the error class.
    const SHAPE_NAME: &'static str;
}

/// `FileSystemNotFound`, as the replication-write path renders it.
#[derive(Debug)]
pub struct FileSystemNotFoundShape;

impl MessageOnlyShape for FileSystemNotFoundShape {
    const SCHEMA: &'static Schema = &crate::ops::types::SCHEMA_FILE_SYSTEM_NOT_FOUND;
    const SHAPE_NAME: &'static str = "FileSystemNotFound";
}

/// `ReplicationNotFound`, as the replication-write path renders it.
#[derive(Debug)]
pub struct ReplicationNotFoundShape;

impl MessageOnlyShape for ReplicationNotFoundShape {
    const SCHEMA: &'static Schema = &crate::ops::types::SCHEMA_REPLICATION_NOT_FOUND;
    const SHAPE_NAME: &'static str = "ReplicationNotFound";
}

/// The body of a refusal that carries the SENTENCE ALONE: the message member of `S` and
/// nothing else of the shape, in particular no `ErrorCode`.
///
/// RULE: on the operations that WRITE a replication configuration
/// (`CreateReplicationConfiguration`, `DeleteReplicationConfiguration`) Amazon EFS
/// answers a not-found refusal with a document holding only the message, and names the
/// error shape in the `x-amzn-errortype` header (and the `__type` discriminator) rather
/// than in an `ErrorCode` member — unlike the replication READ, whose document carries
/// `ErrorCode` as well. The status, the shape name and the sentence are unchanged by
/// this type; only the member is left out.
///
/// It is used as the PAYLOAD of those operations' `FileSystemNotFound` /
/// `ReplicationNotFound` error variants (as [`FileSystemNotFound`] and
/// [`ReplicationNotFound`]): `#[operation_error]` names a variant's payload type after
/// the variant, so an operation chooses this rendering by importing the alias, which
/// shadows the generated shape of the same name.
pub struct MessageOnly<S: MessageOnlyShape> {
    message: String,
    shape: PhantomData<S>,
}

/// Written by hand rather than derived: a derive would bound the marker `S` on `Debug`,
/// and the marker carries no data to print. The shape name comes from `S` instead.
impl<S: MessageOnlyShape> std::fmt::Debug for MessageOnly<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(S::SHAPE_NAME)
            .field("message", &self.message)
            .finish()
    }
}

impl<S: MessageOnlyShape> MessageOnly<S> {
    /// The refusal body carrying `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            shape: PhantomData,
        }
    }

    /// The wire error code, which the generated `OperationError::shape_name` reads off
    /// the variant's payload type.
    pub fn shape_name() -> &'static str {
        S::SHAPE_NAME
    }
}

impl<S: MessageOnlyShape> std::fmt::Display for MessageOnly<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", S::SHAPE_NAME, self.message)
    }
}

impl<S: MessageOnlyShape> std::error::Error for MessageOnly<S> {}

impl<S: MessageOnlyShape> SerializableStruct for MessageOnly<S> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        // The shape's own message member, so `@jsonName` and the model's casing are
        // honoured; a shape that declares none has no sentence to write.
        match message_member(S::SCHEMA) {
            Some(message) => serializer.write_string(message, self.message.as_str()),
            None => Ok(()),
        }
    }
}

/// `FileSystemNotFound` with a body carrying the sentence alone; see [`MessageOnly`].
pub type FileSystemNotFound = MessageOnly<FileSystemNotFoundShape>;

/// `ReplicationNotFound` with a body carrying the sentence alone; see [`MessageOnly`].
pub type ReplicationNotFound = MessageOnly<ReplicationNotFoundShape>;
