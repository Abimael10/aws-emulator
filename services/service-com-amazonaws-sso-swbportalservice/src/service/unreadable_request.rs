// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a request it cannot READ: one whose wire value no value of the
//! modelled member's own type can be read from.
//!
//! This service answers a request it will not read with the error its model declares for
//! exactly that — `InvalidRequestException`, "a problem occurred with the input to the
//! request" — and that holds for every way a request can fail to be readable, not only for
//! the ones the framework hands back structurally. The framework turns an ABSENT required
//! member and a value outside a declared constraint into a `Refusal`, which
//! [`crate::service::EmulatedService::override_refusal`] words; a value the CODEC could not
//! read never becomes one (its failure is a plain codec error,
//! `vendor/smithy-server-runtime/src/pre_dispatch.rs`'s `refusal_of` gives it no structured
//! form), so that seam is never consulted for it and the framework's own generic rendering
//! would stand — naming an error shape this service's model does not declare at all, which
//! a restJson1 SDK resolves its error shape from and so cannot map onto any modelled error.
//!
//! The vendored tree is read-only from a service crate, so the rule is stated here, at the
//! one seam a service has before the framework reads anything
//! ([`crate::service::EmulatedService::override_operation`]) — the same seam the portal's
//! authentication gate uses, and asked AFTER it, so a request presenting no usable session
//! is still unauthenticated whatever value it carried.
//!
//! Nothing about the model is written down here. WHETHER this service can read a request is
//! decided by running the framework's OWN deserializer over it ([`refusal_for`]): the same
//! protocol, the same input schema and the same generated reader the dispatch sequence
//! uses, so this layer cannot disagree with the walk that follows it, and a request it
//! finds readable is served exactly as before. [`MemberReads`] wraps that deserializer only
//! to learn WHICH member's read failed — the failing read is handed that member's own
//! `Schema`, which names it and the modelled type it targets — and leaves the error itself
//! untouched.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::SystemTime;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, ShapeDeserializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{constraints::path, ProtocolError};
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};
use smithy_server_runtime::{
    BoxFuture, DeserializableStruct, ErasedOperation, Operation, OperationContext, Refused,
    ServerHttpProtocol, Service,
};

/// Why this service will not read a request, and the sentence it states that with.
pub(super) struct Unreadable {
    /// The codec's own failure, carried untouched so the refusal is rendered in the class
    /// the framework renders a request it could not read in — HTTP 400, the status this
    /// service's modelled `InvalidRequestException` also carries — rather than in a status
    /// chosen here. Only the CODE and the message are this service's.
    error: SerdeError,
    /// The sentence the refusal states ([`unreadable_message`]).
    message: String,
}

impl Unreadable {
    /// The refusal for a read that failed without naming a member of the input.
    ///
    /// Reached for a request whose BODY the codec cannot parse at all: the restJson1 reader
    /// sweeps the body for members after it has bound the request line and the headers, so
    /// a body that is no JSON document fails the read without any member's value having
    /// been read — and this service's four inputs bind nothing from a body, so such a
    /// request carries nothing the refusal could name. It is still a request this service
    /// could not read, and so still the modelled refusal, stated with the sentence
    /// [`crate::service::EmulatedService::override_refusal`] states when the framework's
    /// violations render none.
    fn unstated() -> Self {
        Self {
            error: SerdeError::custom(super::REQUEST_NOT_VALID_MESSAGE),
            message: super::REQUEST_NOT_VALID_MESSAGE.to_string(),
        }
    }
}

/// The operation this service serves for a request it cannot read into the modelled input
/// of the operation that request named.
///
/// An [`ErasedOperation`] rather than a response built in `override_operation`, because
/// that seam hands the router a BORROWED operation: the service holds one of these and the
/// refusal is re-derived from the same request the router passes back into
/// [`ErasedOperation::run`].
pub(super) struct UnreadableRequest<Eng> {
    /// The SAME shared engine the generated entry threaded into every operation, for the one
    /// write a request this service refuses makes (see [`ErasedOperation::run`] below). The
    /// refusal itself reads nothing out of it.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> UnreadableRequest<Eng> {
    /// Build the refusal on the service's shared engine.
    pub(super) fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for UnreadableRequest<Eng> {
    /// Answer the request [`crate::service`] found unreadable, in the envelope of the
    /// protocol it is served in, with this service's modelled code and the sentence naming
    /// what could not be read.
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The one write a portal request this service reads or refuses makes, and only
            // what the clock already owed: a minted role credential past its advertised
            // expiration is spent at that instant, whether or not the request that happens
            // to arrive next is one this service will serve, so it is ended here exactly as
            // the authentication gate and every handler end it — through the one reading of
            // that sweep ([`crate::state::sweep_expired_credentials`]). Nothing about the
            // refusal below changes because this ran.
            crate::state::sweep_expired_credentials(
                &*self.engine,
                ctx.credential_store(),
                SystemTime::now(),
            );
            let protocol = svc.definition().wire_protocol(req).protocol();
            // The same decision `override_operation` made, re-derived from the same
            // request: this operation is reached only when it answered `Some`, so the
            // failing read is found again.
            let unreadable = protocol
                .routing_key(req)
                .ok()
                .and_then(|key| refusal_for::<Eng>(key, protocol, req))
                .unwrap_or_else(Unreadable::unstated);
            svc.definition().refusal_response(
                ProtocolError::Serde(unreadable.error),
                Some(Refused {
                    code: super::INVALID_REQUEST_CODE.to_string(),
                    message: unreadable.message,
                }),
                req,
            )
        })
    }
}

/// Dispatch [`probe`] on the routing key, one line per modelled operation.
///
/// The arm's key is the operation type's own name, because a REST protocol's routing key IS
/// the operation shape name: there is no second spelling of the surface to drift from the
/// first.
macro_rules! probe_by_key {
    ($key:expr, $protocol:expr, $req:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        match $key {
            $(
                stringify!($operation) => {
                    probe::<crate::ops::$module::$operation<Eng>>($protocol, $req)
                }
            )+
            _ => None,
        }
    };
}

/// Why this service will not read `req` as the input of the operation `key` names, or
/// `None` when it reads it.
///
/// `key` is the routing key the protocol resolved — for restJson1 that is the operation's
/// shape name, which is also the name of the generated operation type — so the match below
/// is the ONE place this file names the service's surface, and a key naming no modelled
/// operation is left to the router's own routing failure.
pub(super) fn refusal_for<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<Unreadable> {
    probe_by_key!(
        key,
        protocol,
        req,
        get_role_credentials::GetRoleCredentials,
        list_account_roles::ListAccountRoles,
        list_accounts::ListAccounts,
        logout::Logout,
    )
}

/// Run the framework's own reader for `O` over `req` and report what this service has to
/// refuse: a value the modelled member it is bound to could not be read from.
///
/// `None` is "nothing for this layer to refuse", which covers three outcomes deliberately:
/// the request read cleanly, so the dispatch sequence serves it as before; the request
/// could not be reached at all, so there is nothing to describe and the framework's own
/// answer stands; or the read failed because a member the model REQUIRES was absent, which
/// the framework hands back structurally and
/// [`crate::service::EmulatedService::override_refusal`] already words as this service's
/// modelled refusal.
///
/// Reading is a pure read of the request: it binds values out of the request line, the
/// query string and the headers, writes nothing and consumes no body, so running it here
/// and again in the dispatch sequence cannot change what either sees.
fn probe<O: Operation>(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> Option<Unreadable> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let failed: RefCell<Option<Unreadable>> = RefCell::new(None);
    let read = {
        let mut watched = MemberReads {
            inner: &mut *deserializer,
            failed: &failed,
        };
        <O::Input as DeserializableStruct>::deserialize(&mut watched)
    };
    match read {
        // The request reads, so this layer has nothing to say about it.
        Ok(_) => None,
        // An absent `@required` member is the refusal the framework gives a structured
        // form, and this service already words that one.
        Err(SerdeError::MissingMember { .. }) => None,
        // A read that named no member at all (see [`Unreadable::unstated`]) keeps the
        // error it failed with and states the sentence that names nothing of the request.
        Err(error) => Some(failed.into_inner().unwrap_or(Unreadable {
            error,
            message: super::REQUEST_NOT_VALID_MESSAGE.to_string(),
        })),
    }
}

/// The sentence this service states for a value the modelled member it is bound to could
/// not be read from: the member, in the lowerCamel path every refusal of this service names
/// a member at, and the modelled type the value had to be one of.
///
/// Stated in the envelope the framework renders every other refusal of an unreadable
/// request in — the count word and the separator are
/// `service_common::validation_message::envelope_over`'s, which reads them out of the
/// framework's own rendering, so nothing about the envelope is spelled here — and with the
/// VALUE half of the clause dropped, which is the form the framework itself writes for a
/// clause it has no value rendering for: what this refusal states is the type the member
/// takes, not the one text that was not of it.
fn unreadable_message(member: &Schema) -> Option<String> {
    let path = path::member(member.member_name()?);
    let clause = format!(
        "Value at '{path}' failed to satisfy constraint: Member must be a valid {:?}",
        member.shape_type()
    );
    ::service_common::validation_message::envelope_over(&[clause])
}

/// Record the first read that failed on a value's own form, with the member it was reading.
///
/// An absent `@required` member is left unrecorded: the framework states that refusal
/// structurally and this service words it elsewhere, so recording it here would reword a
/// refusal that is already right. The FIRST failing read wins: it is the one the caller has
/// to fix first.
fn note_in(failed: &RefCell<Option<Unreadable>>, member: &Schema, error: SerdeError) -> SerdeError {
    if matches!(error, SerdeError::MissingMember { .. }) {
        return error;
    }
    let mut held = failed.borrow_mut();
    if held.is_none() {
        if let Some(message) = unreadable_message(member) {
            *held = Some(Unreadable {
                error: SerdeError::custom(error.to_string()),
                message,
            });
        }
    }
    error
}

/// The framework's deserializer, watched.
///
/// Every method forwards to the real deserializer and returns its answer unchanged — this
/// is not a codec, and it reimplements no part of one. The ONE thing it adds is that a
/// member whose read fails is recorded with its own schema, which the error alone does not
/// carry.
struct MemberReads<'d, 'f> {
    inner: &'d mut (dyn ShapeDeserializer + 'd),
    failed: &'f RefCell<Option<Unreadable>>,
}

/// Forward one scalar read to the real deserializer, unwatched: a scalar read reached
/// through here is the INPUT STRUCTURE's own, and the member reads this layer reports on
/// are the ones [`MemberReads::read_struct`] hands to the consumer.
macro_rules! forwarded_reads {
    ($($method:ident -> $value:ty),+ $(,)?) => {
        $(
            fn $method(&mut self, schema: &Schema) -> Result<$value, SerdeError> {
                self.inner.$method(schema)
            }
        )+
    };
}

impl ShapeDeserializer for MemberReads<'_, '_> {
    /// The one watched read: the structure's members are read through the consumer this
    /// wraps, so a member whose value could not be read is recorded with the schema the
    /// failing read was handed.
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let failed = self.failed;
        let mut watched = |member: &Schema, inner: &mut dyn ShapeDeserializer| {
            consumer(member, inner).map_err(|error| note_in(failed, member, error))
        };
        self.inner.read_struct(schema, &mut watched)
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.inner.read_list(schema, consumer)
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.inner.read_map(schema, consumer)
    }

    // Every value read, forwarded. The list is the trait's own scalar surface.
    forwarded_reads! {
        read_boolean -> bool,
        read_byte -> i8,
        read_short -> i16,
        read_integer -> i32,
        read_long -> i64,
        read_float -> f32,
        read_double -> f64,
        read_big_integer -> BigInteger,
        read_big_decimal -> BigDecimal,
        read_string -> String,
        read_blob -> Blob,
        read_timestamp -> DateTime,
        read_document -> Document,
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.inner.read_null()
    }

    fn is_null(&self) -> bool {
        self.inner.is_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.inner.container_size()
    }
}
