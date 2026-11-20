// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a body no input can be read from, and
//! a body value whose JSON class the modelled member it is bound to cannot be read from, are
//! refused.
//!
//! Three refusals happen before a handler runs, and the first two are NOT the same error
//! class as the third:
//!
//! * bytes that are not ONE complete JSON object are no body of this protocol at all. The
//!   protocol carries an operation's input as a single JSON object, so bytes that do not
//!   parse, bytes carrying anything after that object, and a document that is not an object
//!   are a defect of the BODY rather than of one value in it: there is no value whose class
//!   could be named, so the refusal is [`SERIALIZATION_EXCEPTION`] and no prose at all
//!   ([`body_reading`]);
//! * a value whose JSON class the modelled type of its member admits no reading of — a
//!   number where the model declares a string, a string where it declares a long, a scalar
//!   where it declares a list — never becomes a value at all, and neither does base64 text
//!   no blob can be decoded from. That is a failure of the
//!   MARSHALLING, which AWS answers at the deserialization layer: HTTP 400 under the
//!   protocol's own [`SERIALIZATION_EXCEPTION`], with the reason under the capitalized
//!   [`MESSAGE_MEMBER`] — the member a protocol-level error of this service states its prose
//!   in, which is not the lowercase `message` its MODELLED exceptions declare. Nothing is
//!   read, so nothing is looked up and nothing is written. Not every such refusal STATES a
//!   reason: a JSON ARRAY standing where the member it is bound to is read from no array is a
//!   value no conversion was attempted for at all, and the refusal is then the error class
//!   alone, with no prose member in the body ([`Refused`]);
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers as its `ValidationException`
//!   ([`super::EmulatedService::override_refusal`]).
//!
//! The framework renders a marshalling failure in the constraint-violation envelope
//! (`vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code and the lowercase member name), so a client branching on the error class
//! to decide whether to fix its serializer or fix a value takes the wrong branch, and a tool
//! that reads the capital-`Message` slot of this class finds nothing there. The vendored
//! tree is read-only from a service crate, so the separation is made here, inside the one
//! scope every request this service serves already passes through
//! ([`super::OperationScopedClauseForm`]) — as the request layers of
//! `services/service-com-amazonaws-emr-elasticmapreduce`,
//! `services/service-com-amazonaws-xray-awsxray` and
//! `services/service-com-amazonaws-kinesis-kinesis-20131202` do for their own services.
//!
//! # What an explicit `null` states
//!
//! ABSENCE: a member a body carries as `null` is a member that body does not state, so the
//! request is served exactly as the same request without it — which is what lets a client
//! marshal an unset optional as `null`, and is the same forward-compatibility the codec
//! already gives an UNKNOWN member by skipping it. The one `null` that is not an absence a
//! caller may send is one standing for a member the input REQUIRES: there is then no value to
//! build the input from, so the body fails to deserialize and is refused as the body-level
//! failure it is, with the reader's own diagnosis of it ([`BodyReading`], [`Prose`]).
//!
//! Nothing about the model is written down here. WHETHER a body can be read, which members
//! the input REQUIRES and what type each member is read as are all decided by running the
//! framework's OWN deserializer over the body ([`probe`], [`absent_member`]): the same
//! protocol, the same input schema and the same generated `DeserializableStruct` the dispatch
//! sequence uses, so this layer cannot disagree with the walk that follows it. [`Probe`]
//! wraps that deserializer only to learn WHICH read failed — the modelled type that read was
//! for, which the codec's error does not carry — and leaves the error itself untouched.
//! Because the real codec descends the whole body, a value nested in a structure, a list or a
//! map is reached exactly as a top-level member is.
//!
//! # What a value of another FORM states
//!
//! The value it spells, where the modelled type can be read from it: text spelling a JSON
//! flag standing where the model declares a boolean, and a number carrying a fraction
//! standing where it declares a whole number, are READ rather than refused, and the operation
//! is served the request carrying each as read ([`value_reading`], [`readable`]). The codec
//! itself already reads a number whose fraction is zero into an integral member, so what this
//! adds is the narrowing beside it; a literal needing no conversion never reaches that
//! question, because the codec read it. A value so read is then held to the model's own
//! constraints like any other, which is where a page size read as zero earns the range clause.
//!
//! It is read BEFORE the absences above, because what a body states is a fact of the body as
//! the protocol READS it: a `null` standing for a required member earns the reader's own
//! diagnosis whether or not a value the protocol reads in another form stands before it.
//!
//! # What is left to another layer, deliberately
//!
//! * a `null` ELEMENT of a list: no member stands there for it to state the absence of, and
//!   dropping it would renumber the elements beside it, so the codec answers it as it stands;
//! * a read that failed for a reason that is neither a value's JSON class nor a base64 text's
//!   length — text that is not a timestamp, base64 of a legal length that still does not
//!   decode, an absent `@required` member the body never carried at all — which the
//!   framework and this service's own constraint wording answer, unchanged;
//! * a target type this layer states no name for ([`named_type`]) or no expectation for
//!   ([`Expected::of`] — a structure, a union, a timestamp or a blob, none of which is read
//!   into one value the reader has an expectation for): the sentence's two halves are the
//!   rule, so a half this service is not known to state is never invented.

use std::cell::RefCell;

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_schema_server::constraints::path;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};
use serde::de::{Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess};
use serde_json::{json, Value};
use service_common::json_wire_values::{
    ARRAY_VALUE, BOOLEAN_VALUE, NULL_VALUE, NUMBER_VALUE, OBJECT_VALUE,
};
use smithy_server_runtime::{DeserializableStruct, Operation, ServerHttpProtocol, Service};

/// The error code a value the body could not be read from is refused with, as this
/// service's protocol spells it: the absolute id of the PROTOCOL's own error shape, not one
/// of this service's modelled exceptions — the request never became an input, so no
/// operation ran to answer with an error of its own. What it must NOT be is the
/// constraint-refusal code, which is the answer to a value that WAS read.
pub const SERIALIZATION_EXCEPTION: &str = "com.amazon.coral.service#SerializationException";

/// The body member a protocol-level error of this service carries its sentence in: the
/// capitalized `Message`, which is where a client of this service reads the prose of this
/// error class. A MODELLED exception keeps the lowercase `message` its model declares and is
/// rendered by the framework, untouched by this layer.
pub const MESSAGE_MEMBER: &str = "Message";

/// The body member the DESERIALIZER's own diagnosis of a body is carried in: the lowercase
/// `message`. The sentence stated there is the reader's rather than this service's — it names
/// the position in the request the read stopped at — and it is the only prose of this error
/// class that does ([`Prose`]).
pub const DIAGNOSIS_MEMBER: &str = "message";

/// The HTTP status a body this service could not read is refused with: the caller sent
/// something no operation could be given, so it is a client error.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// What the refusal states when a SINGLE value stands in a member a structure (or a union)
/// DECLARES — a field the body names — and the modelled type of that field is an AGGREGATE,
/// which holds other values: no conversion between two single values was even attempted, so
/// what the sentence states is that the FIELD's type was unexpected, naming neither a class
/// nor a type.
const UNEXPECTED_FIELD_TYPE: &str = "Unexpected field type";

/// What the same defect states when the value stands INSIDE a collection — an element of a
/// list or the value of a map entry — instead: no member declares that position, so there is
/// no field whose type could be called unexpected, and what the sentence names is the value
/// carried in the payload ([`Slot`]).
const UNEXPECTED_VALUE_TYPE: &str = "Unexpected value type in payload";

/// What the refusal states when text standing where a blob is modelled is not a whole number
/// of base64 groups, followed by the length the body carried.
///
/// Base64 encodes every three bytes as four characters, so text whose length is not a
/// multiple of four encodes no bytes at all, and the sentence names the one fact that makes
/// it undecodable: the length found. The length is the only variable part — the refusal
/// neither names the member nor echoes the text.
const BASE64_LENGTH: &str = "Base64 encoded length is expected a multiple of 4 bytes but found: ";

/// How many characters base64 encodes each group of bytes as: text that is not a whole
/// number of these groups is text no bytes can be decoded from.
const BASE64_GROUP: usize = 4;

/// What the refusal states when the body opened a STRUCTURE or a MAP where the member it
/// stands in is read from neither: the one defect of this class that is named after the
/// structural token the reader found rather than after the member or the value.
const STRUCTURE_OR_MAP: &str = "Start of structure or map found where not expected";

/// The tokens this protocol names the offending LITERAL by, where the JSON class of a value
/// is a class of several literals and the refusal names which one arrived: a `true` and a
/// `false` are two tokens rather than one "boolean", and a fractional number is a different
/// token from a whole one.
///
/// The class names the rest of the vocabulary shares with every other refusal in this tree
/// (`service_common::json_wire_values`) — a string is `STRING_VALUE` and a whole number
/// `NUMBER_VALUE` there as here — so only the literals this protocol splits are named below.
const TRUE_VALUE: &str = "TRUE_VALUE";

/// See [`TRUE_VALUE`].
const FALSE_VALUE: &str = "FALSE_VALUE";

/// See [`TRUE_VALUE`]: a number carrying a fraction or an exponent, which is a different
/// literal from a whole number however small the fraction is.
const DECIMAL_VALUE: &str = "DECIMAL_VALUE";

/// Dispatch one reader of a request's body on the routing key, one line per modelled
/// operation.
///
/// The arm's key is the operation type's own name, because this service's protocol
/// (`smithy_query(query="service_info")`: awsJson1_0) resolves its routing key to the
/// operation shape name, which is also the name of the generated operation type: there is no
/// second spelling of the surface to drift from the first.
macro_rules! keyed {
    ($run:ident, $key:expr, $protocol:expr, $req:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        match $key {
            $(
                stringify!($operation) => {
                    $run::<crate::ops::$module::$operation<Eng>>($protocol, $req)
                }
            )+
            _ => None,
        }
    };
}

/// Run `$run` for the input of the operation `$key` names: the one list of this service's
/// modelled operations, so every reader of a body dispatches over the same surface.
macro_rules! for_served_input {
    ($run:ident, $key:expr, $protocol:expr, $req:expr) => {
        keyed!(
            $run,
            $key,
            $protocol,
            $req,
            batch_execute_statement::BatchExecuteStatement,
            batch_get_item::BatchGetItem,
            batch_write_item::BatchWriteItem,
            create_backup::CreateBackup,
            create_global_table::CreateGlobalTable,
            create_table::CreateTable,
            delete_backup::DeleteBackup,
            delete_item::DeleteItem,
            delete_resource_policy::DeleteResourcePolicy,
            delete_table::DeleteTable,
            describe_backup::DescribeBackup,
            describe_continuous_backups::DescribeContinuousBackups,
            describe_contributor_insights::DescribeContributorInsights,
            describe_endpoints::DescribeEndpoints,
            describe_export::DescribeExport,
            describe_global_table::DescribeGlobalTable,
            describe_global_table_settings::DescribeGlobalTableSettings,
            describe_import::DescribeImport,
            describe_kinesis_streaming_destination::DescribeKinesisStreamingDestination,
            describe_limits::DescribeLimits,
            describe_table::DescribeTable,
            describe_table_replica_auto_scaling::DescribeTableReplicaAutoScaling,
            describe_time_to_live::DescribeTimeToLive,
            disable_kinesis_streaming_destination::DisableKinesisStreamingDestination,
            enable_kinesis_streaming_destination::EnableKinesisStreamingDestination,
            execute_statement::ExecuteStatement,
            execute_transaction::ExecuteTransaction,
            export_table_to_point_in_time::ExportTableToPointInTime,
            get_item::GetItem,
            get_resource_policy::GetResourcePolicy,
            import_table::ImportTable,
            list_backups::ListBackups,
            list_bulk_operations::ListBulkOperations,
            list_contributor_insights::ListContributorInsights,
            list_exports::ListExports,
            list_global_tables::ListGlobalTables,
            list_imports::ListImports,
            list_tables::ListTables,
            list_tags_of_resource::ListTagsOfResource,
            put_item::PutItem,
            put_resource_policy::PutResourcePolicy,
            query::Query,
            restore_table_from_backup::RestoreTableFromBackup,
            restore_table_to_point_in_time::RestoreTableToPointInTime,
            scan::Scan,
            search_vectors::SearchVectors,
            start_bulk_put_items::StartBulkPutItems,
            tag_resource::TagResource,
            transact_get_items::TransactGetItems,
            transact_write_items::TransactWriteItems,
            untag_resource::UntagResource,
            update_continuous_backups::UpdateContinuousBackups,
            update_contributor_insights::UpdateContributorInsights,
            update_global_table::UpdateGlobalTable,
            update_global_table_settings::UpdateGlobalTableSettings,
            update_item::UpdateItem,
            update_kinesis_streaming_destination::UpdateKinesisStreamingDestination,
            update_table::UpdateTable,
            update_table_replica_auto_scaling::UpdateTableReplicaAutoScaling,
            update_time_to_live::UpdateTimeToLive,
        )
    };
}

/// What this layer makes of the VALUES `req`'s body carries, for the operation `key` names:
/// a body every value of which reads as sent, the same request with the values the protocol
/// reads in another form carried in that form, or a value no modelled member can be read
/// from at all. Asked by [`body_reading`], of a body already known to be a document of this
/// protocol.
///
/// [`ValueReading::AsSent`] is also "nothing for THIS question to decide", which covers the
/// outcomes it has no answer for deliberately: a body that cannot be reached or read at all
/// (there is no value to describe, so the framework's own rendering stands); a read that
/// failed for a reason that is NOT a value's JSON class; a read that stopped at an explicit
/// `null`, which states a member's ABSENCE rather than a value of the wrong class
/// ([`BodyReading`]); and a key naming no modelled operation, which is left to the routing
/// table's own refusal.
fn value_reading<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> ValueReading {
    let Some(body) = req.body().bytes() else {
        return ValueReading::AsSent;
    };
    // The body as a DOCUMENT is only needed once a read has failed — to name the literal that
    // failed and to carry it in the form the protocol reads it in — so a body every value of
    // which reads is never parsed again here ([`Forms`]).
    let mut forms: Option<Forms> = None;
    // The request rebuilt from each: the one the operation is served, and the one the question
    // is ASKED of, which a body stating no absence does not have because it is asked about
    // itself.
    let mut read: Option<RtRequest> = None;
    let mut asked: Option<RtRequest> = None;
    // Each round READS one value the protocol carries in another form and asks again, because
    // the codec stops at the first value it cannot read: what the next round answers is the
    // value after it. The whole body is read per round, so what is bounded is the product of
    // the rounds and the body's size ([`MOST_JSON_READ_FOR_FORMS`]).
    let rounds = MOST_JSON_READ_FOR_FORMS / body.len().max(1);
    for _ in 0..=rounds {
        let standing = asked.as_ref().or(read.as_ref()).unwrap_or(req);
        let Some(found) = unreadable_value::<Eng>(key, protocol, standing) else {
            return ValueReading::read(read);
        };
        let held = match &mut forms {
            Some(held) => held,
            None => {
                let Ok(document) = ::serde_json::from_slice::<Value>(body) else {
                    // A body the reader cannot read is no body this layer describes a value
                    // of: the refusal it earns is the body-level one ([`body_reading`]).
                    return ValueReading::AsSent;
                };
                let held = forms.insert(Forms::of(document));
                // A body that states an absence is ASKED about the copy without it, so the
                // round that follows reaches the values standing behind that absence.
                if held.states_an_absence() {
                    let Some(next) = rebuilt(req, held.asked()) else {
                        return ValueReading::AsSent;
                    };
                    asked = Some(next);
                    continue;
                }
                held
            }
        };
        // The LITERAL is read off the body, which is the only place it survives: the codec's
        // own complaint names the JSON type it found, and this protocol names the literal.
        let found = as_literal(held.asked(), found);
        let form = found.standing().and_then(|(target, at)| {
            Some((at.to_vec(), readable(target, value_at(held.asked(), at)?)?))
        });
        let Some((at, form)) = form else {
            return match refusal(&found) {
                Some(refused) => ValueReading::Unreadable(refused),
                None => ValueReading::read(read),
            };
        };
        if !held.read(&at, form) {
            return ValueReading::read(read);
        }
        // A request that cannot be rebuilt is served as the last form that could be, which
        // leaves the value to the codec's own answer.
        let Some(next) = rebuilt(req, held.served()) else {
            return ValueReading::read(read);
        };
        if held.states_an_absence() {
            let Some(probe) = rebuilt(req, held.asked()) else {
                return ValueReading::read(read);
            };
            asked = Some(probe);
        }
        read = Some(next);
    }
    ValueReading::read(read)
}

/// What this layer makes of the values a request's body carries.
#[derive(Debug)]
enum ValueReading {
    /// Every value reads into the member it is bound to as the body carries it: the request is
    /// served exactly as it arrived.
    AsSent,
    /// The same request with the values the protocol reads in another form carried in that
    /// form, which is what the operation is served ([`readable`]). BOXED because a whole
    /// request dwarfs the other two answers and only this one carries it.
    Read(Box<RtRequest>),
    /// A value no modelled member can be read from, with what the refusal of it states.
    Unreadable(Refused),
}

impl ValueReading {
    /// The request as it is served, for a body nothing in which is refused: the rebuilt one
    /// where a value was read in another form, and the request as it arrived where none was.
    fn read(read: Option<RtRequest>) -> Self {
        match read {
            Some(read) => Self::Read(Box::new(read)),
            None => Self::AsSent,
        }
    }
}

/// The two bodies a FORM question works over: the one the operation is served, and the one the
/// question is asked of.
///
/// They differ in the absences the body states. A member an explicit `null` stands for is a
/// member the body does not state ([`BodyReading`]), so the codec's walk stops at such a
/// member and never reaches the values behind it — which would leave a value the protocol
/// reads in another form unread for no reason but its position. The copy asked about therefore
/// has those absences GONE, while the body the operation is served keeps them, because what
/// they state is answered by the absence layer over the body as the protocol reads it.
#[derive(Debug)]
struct Forms {
    /// The body the operation is served, with its absences still stated and every value read
    /// so far in the form the protocol reads it in.
    served: Value,
    /// The copy asked about — `None` for a body that states no absence, which is asked about
    /// itself.
    asked: Option<Value>,
}

impl Forms {
    /// The two bodies `document` makes.
    fn of(document: Value) -> Self {
        let asked = (absences_stated(&document) > 0)
            .then(|| rewrite(&document, &mut 0, &|_| Applied::Dropped));
        Self {
            served: document,
            asked,
        }
    }

    /// Whether the body states an absence at all, which is what makes the two bodies differ.
    fn states_an_absence(&self) -> bool {
        self.asked.is_some()
    }

    /// The body the operation is served.
    fn served(&self) -> &Value {
        &self.served
    }

    /// The body the question is asked of.
    fn asked(&self) -> &Value {
        self.asked.as_ref().unwrap_or(&self.served)
    }

    /// Read the value at `at` as `form`, in both bodies — the paths are the same in each,
    /// because an absence is dropped as a MEMBER and nothing beside it is renumbered
    /// ([`rewrite`]). `false` when the path names no value to replace, which leaves both as
    /// they stand.
    fn read(&mut self, at: &[Step], form: Value) -> bool {
        if let Some(asked) = self.asked.as_mut() {
            if !write_at(asked, at, form.clone()) {
                return false;
            }
        }
        write_at(&mut self.served, at, form)
    }
}

/// How much JSON this layer may read to carry a body's values in the form the protocol reads
/// them in.
///
/// One bound over the two factors a caller multiplies: each value read in another form costs
/// one read of the WHOLE body to find the value after it, so neither the number of such values
/// nor the body's size bounds the work on its own. Past the bound the values already read keep
/// their form and the rest are left to the codec, which refuses the first of them.
const MOST_JSON_READ_FOR_FORMS: usize = 4 << 20;

/// What this layer answers one value it could not read with: this error class states prose
/// for some defects and none at all for others, and which of the two it is is a fact of the
/// defect rather than of the request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// The error class ALONE, in a body carrying no prose member: a value no conversion was
    /// attempted for ([`refusal`]).
    Bare,
    /// The class and this service's sentence about the value, stated in the capitalized
    /// [`MESSAGE_MEMBER`] ([`Prose::Conversion`]).
    Stated(String),
}

impl Refused {
    /// The prose this refusal states, in the member that class of refusal states it in — and
    /// `None` for the one that states none.
    fn prose(self) -> Option<Prose> {
        match self {
            Self::Bare => None,
            Self::Stated(stated) => Some(Prose::Conversion(stated)),
        }
    }
}

/// The value the framework's own deserializer for the operation `key` names stopped at in
/// `req`'s body, with the modelled type that read was for and where in the body it stands —
/// or `None` when the body reads and when it cannot be reached at all.
///
/// A read that failed for a reason this layer states nothing about is reported too
/// ([`Unreadable::Unclassified`]): whether the value can be READ in another form is a
/// question about the value rather than about the codec's complaint, and it is asked of every
/// failing read ([`readable`]).
fn unreadable_value<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<Unreadable> {
    for_served_input!(probe, key, protocol, req)
}

/// The member the input of the operation `key` names REQUIRES and `req`'s body does not
/// carry, as the generated deserializer spells it — a dotted, lowerCamel member path
/// ([`path`]).
///
/// `None` when the body satisfies every member the input requires, and when it fails to read
/// for any other reason: the question asked here is only ever which member is MISSING.
fn absent_member<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    for_served_input!(unsatisfied_member, key, protocol, req)
}

/// The modelled type the read that one explicit `null` in `req`'s body answered was FOR, as
/// the framework's own deserializer reports it — the half of a diagnosis that says what was
/// expected where the body stated an absence.
///
/// A scalar read ANSWERS that question by failing, naming the type it read for; a structure,
/// a list and a map each read a `null` without failing, because they already take one as
/// absence, so the type those answer with is recorded as the read happens
/// ([`Probe::note_accepted`]). `None` when neither says — the walk stopped earlier, at a
/// value of the wrong class.
fn null_target<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<ShapeType> {
    for_served_input!(null_read_target, key, protocol, req)
}

/// The top-level members of the input of the operation `key` names, each as the generated
/// input SCHEMA declares it — which is where codegen states a member's Smithy name, the name
/// this protocol carries it under, and the modelled type it is read as.
fn declared_input_members<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<Vec<DeclaredMember>> {
    for_served_input!(input_members, key, protocol, req)
}

/// One member of an operation's input, as its schema declares it.
#[derive(Debug, Clone, Copy)]
struct DeclaredMember {
    /// The member's Smithy name, which is what the deserializer spells its absence from
    /// ([`path::member`]).
    name: &'static str,
    /// The name this protocol carries the member under — its `@jsonName` where the model
    /// gives one, else its Smithy name, which is the lookup the JSON codec performs.
    wire: &'static str,
    /// The modelled type the member is read as.
    target: ShapeType,
}

/// Read `O`'s input schema. The protocol and the request are not read: a member list is a
/// fact of the model, fixed per operation, and nothing about it is per-request — the two
/// parameters are the shape of the one dispatcher every reader of a body goes through
/// ([`for_served_input`]).
fn input_members<O: Operation>(
    _protocol: &dyn ServerHttpProtocol,
    _req: &RtRequest,
) -> Option<Vec<DeclaredMember>> {
    Some(
        O::INPUT_SCHEMA
            .members()
            .iter()
            .filter_map(|member| {
                Some(DeclaredMember {
                    name: member.member_name()?,
                    wire: wire_name(member),
                    target: member.shape_type(),
                })
            })
            .collect(),
    )
}

// =============================================================================
// THE BODY, AHEAD OF EVERY VALUE IN IT
//
// Two facts about a request's BODY are settled before any value it carries is read into a
// member: whether the bytes are a document an input could be read from at all, and what the
// explicit `null`s in it state.
//
// A body that is not ONE complete JSON object is no body of this protocol: the protocol
// carries an operation's input as a single JSON object, so bytes that do not parse, bytes
// that carry something after the object, and a document that is not an object are all a
// defect of the BODY rather than of one value in it. No value's class can be named for it,
// so the refusal is the error class alone, with no prose at all.
//
// An explicit `null` states ABSENCE on this protocol: a member the body carries as `null` is
// a member the body does not state, so the request is served exactly as the same request
// without it — which is also what makes an unknown member harmless, since the codec already
// skips one. A `null` standing for a member the input REQUIRES is the one case that is not
// absence a caller may send: there is no value to build the input from, so the body fails to
// deserialize, and the sentence is the deserializer's own — the class found, the type
// expected and the position in the body it stopped at.
//
// WHICH members the input requires is not transcribed here: it is asked of the generated
// deserializer, which is the only thing that knows ([`absent_member`]).
// =============================================================================

/// What this layer makes of a request's BODY and of the values in it.
#[derive(Debug)]
pub enum BodyReading {
    /// A document of this protocol every value of which reads as sent, stating no member's
    /// absence: the request is served exactly as it arrived.
    AsSent,
    /// The body as this layer READ it, which is what the operation is served: the members its
    /// explicit `null`s state the absence of are absent, and the values the protocol reads in
    /// another form than the one the model declares carry that form ([`readable`]). BOXED
    /// because a whole request dwarfs the other two answers and only this one carries it.
    Absences(Box<RtRequest>),
    /// The body is not one an input of this protocol can be read from, with the prose this
    /// service states for that defect where it states any.
    Unreadable(Option<Prose>),
}

/// The prose a protocol-level refusal of this service states, and which body member it
/// states it in.
///
/// The two are different refusals of the same class, and a client reads them in different
/// slots: the sentence about one VALUE is this service's own wording for a conversion, while
/// the diagnosis of the BODY is the deserializer's, and is the only one of the two that
/// names a position in the request.
#[derive(Debug, Clone)]
pub enum Prose {
    /// A value whose JSON class the modelled type of its member admits no reading of, stated
    /// in the capitalized [`MESSAGE_MEMBER`].
    Conversion(String),
    /// The DESERIALIZER's own diagnosis of a body it could not read an input from, stated in
    /// the lowercase [`DIAGNOSIS_MEMBER`].
    Diagnosis(String),
}

impl Prose {
    /// The member schema this prose is written under.
    fn member(&self) -> &'static Schema {
        match self {
            Self::Conversion(_) => &SERIALIZATION_EXCEPTION_MESSAGE,
            Self::Diagnosis(_) => &SERIALIZATION_EXCEPTION_DIAGNOSIS,
        }
    }

    /// The sentence itself.
    fn stated(&self) -> &str {
        match self {
            Self::Conversion(stated) | Self::Diagnosis(stated) => stated,
        }
    }
}

/// What `req`'s body is, for the operation `key` names: a document that is served as it
/// arrived, the same request with the values it carries in another form read and the members
/// it states as `null` absent, or a body no input can be read from.
///
/// Three questions in the order the body answers them: whether the bytes are a document of
/// this protocol at all, then which of its VALUES the protocol reads in another form than the
/// one the model declares ([`value_reading`]), and only then what its explicit `null`s state —
/// because an absence is a fact of the body as the protocol READS it, so a value read in
/// another form is read before the absences beside it are asked about.
pub fn body_reading<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> BodyReading {
    // A body the transport could not buffer is not read here at all: there is nothing to
    // decide about, and the request is served exactly as it arrived.
    let Some(body) = req.body().bytes() else {
        return BodyReading::AsSent;
    };
    // ONE complete JSON object, which is what this protocol carries an input as. The reader
    // is the maintained JSON parser, so what counts as complete is its answer rather than a
    // second parser's: bytes it leaves over are bytes that are not part of the document.
    let document = match ::serde_json::from_slice::<Value>(body) {
        Ok(document) => document,
        // Except for the one refusal that is not about the body's FORM: a document nesting
        // deeper than the reader descends is a document, and how deep a value a request
        // carries may nest is a documented bound on that value, answered where every other
        // bound on it is ([`crate::state`]).
        Err(stated) if nests_too_deep(&stated) => return BodyReading::AsSent,
        Err(_) => return BodyReading::Unreadable(None),
    };
    if !document.is_object() {
        return BodyReading::Unreadable(None);
    }
    let read = match value_reading::<Eng>(key, protocol, req) {
        ValueReading::AsSent => None,
        ValueReading::Read(read) => Some(read),
        ValueReading::Unreadable(refused) => return BodyReading::Unreadable(refused.prose()),
    };
    let Some(served) = read else {
        return StatedAbsences::of::<Eng>(key, protocol, req, body, document).reading::<Eng>();
    };
    // The absences are a fact of the body the values were read in, so they are asked about
    // that one rather than about the body as it arrived.
    let Some(read) = served.body().bytes().map(<[u8]>::to_vec) else {
        return BodyReading::AsSent;
    };
    let Ok(document) = ::serde_json::from_slice::<Value>(&read) else {
        return BodyReading::AsSent;
    };
    let stated = StatedAbsences::of::<Eng>(key, protocol, &served, &read, document);
    match stated.reading::<Eng>() {
        // The body was rewritten for the values it carries in another form even where it
        // states no absence, so the request the operation is served is that one.
        BodyReading::AsSent => BodyReading::Absences(served),
        reading => reading,
    }
}

/// The boolean a piece of TEXT standing where a boolean is modelled spells, or `None` when it
/// spells neither.
///
/// The two spellings are the protocol's own literals: this is the text a caller writes when
/// the value travelled through something that carries every value as text (a form field, a
/// shell variable, a template parameter), and the two words are exactly the ones the JSON
/// literals are written with. Nothing else is a boolean — text that spells no boolean is
/// refused as the unconvertible value it is, and so is text that spells one in any other case
/// than the literal's own.
///
/// NOT `service_common::query_boolean_values::boolean_spelled`, which is the lexical space of
/// an XML Schema boolean (`true`/`false`/`1`/`0`, whitespace collapsed) because that is what
/// the QUERY protocols carry a boolean in, and whose reader can decide a boolean IN THE READ
/// because a form field is positional — read it as text, then as a boolean, same value. This
/// protocol carries a boolean as a JSON literal, so the text that spells one is that
/// literal's own spelling and nothing wider; the numerals are a number in this wire's
/// vocabulary, which is a value no boolean member admits.
fn boolean_text(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Whether the JSON reader stopped because the document nests deeper than it descends,
/// rather than because the bytes are not a document.
///
/// Read off the reader's own wording, which is the only place it is stated — its refusals
/// carry no code a caller of the crate can branch on. PINNED by
/// [`tests::the_reader_wording_this_layer_reads`], so a change to it fails loudly here
/// instead of silently turning a deeply nested value into a body-level refusal.
fn nests_too_deep(stated: &::serde_json::Error) -> bool {
    stated.to_string().contains("recursion limit exceeded")
}

/// How much JSON this layer may read to ASK what a body's stated absences earn.
///
/// Two costs, and only one of them is a caller's to grow. APPLYING the absences is one pass
/// over the body ([`StatedAbsences::rewritten`]), whatever their number, so it needs no bound
/// beyond the body's own size. ASKING is where a caller's choices multiply: one absence costs
/// a probe for the modelled type its member is read as, and then up to one round per member of
/// the input to isolate that member from any other the body does not carry, and EACH of those
/// is a read of the whole body. So the bound is on the product of all three factors — the
/// number of `null`s, the rounds one of them may take, and the body's own size — rather than
/// on any one of them ([`StatedAbsences::of`]). It is applied BEFORE anything per-absence is
/// built, so neither a path nor a probe is materialised past it.
///
/// Past the cap the absences are still applied; what is skipped is the asking, which leaves
/// such a request the framework's own refusal of an absent required member.
const MOST_JSON_READ_FOR_ABSENCES: usize = 4 << 20;

/// One step from a body's root to a value it carries: a member of an object, or an element
/// of an array. The path a `null` stands at, which is what makes that value reachable in the
/// document and in the bytes the body arrived as.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Member(String),
    Element(usize),
}

/// What one stated absence becomes in a document this layer rebuilds.
#[derive(Debug, Clone)]
enum Applied {
    /// Gone, which is what the member's absence means.
    Dropped,
    /// Left as the `null` the body wrote, so the reader answers it.
    Stated,
    /// Carrying a value of the member's own modelled type, so the reader's answer is about
    /// another member ([`stands_in`]).
    StoodIn(Value),
}

/// One body, the members its explicit `null`s state the absence of, and the modelled type
/// each of those members is read as.
struct StatedAbsences<'r> {
    /// The routing key of the operation the body is being read for.
    key: &'r str,
    /// The protocol the body is carried in, which owns the deserializer every question here
    /// is asked of.
    protocol: &'r dyn ServerHttpProtocol,
    /// The request as it arrived, which every probe is a rebuilt copy of.
    req: &'r RtRequest,
    /// The body as it arrived: the bytes a diagnosis reads its position from.
    body: &'r [u8],
    /// The body read as a document.
    document: Value,
    /// How many absences the body states, counted whether or not any is asked about.
    stated: usize,
    /// Where each absence this layer ASKS about stands, in the order the body carries them —
    /// empty for a body past [`MOST_JSON_READ_FOR_ABSENCES`], which is applied and not asked
    /// about.
    at: Vec<Vec<Step>>,
    /// The modelled type the codec reads each of those members as, and `None` for a member
    /// whose read stopped at something else first.
    targets: Vec<Option<ShapeType>>,
    /// The input's own members, read off its schema once: how many there are bounds the rounds
    /// one absence may take to isolate ([`Self::required`]), which is a factor of the bound
    /// above, and each round reads the list rather than the schema again.
    declared: Vec<DeclaredMember>,
}

impl<'r> StatedAbsences<'r> {
    /// Read `document`'s explicit `null`s, each with the modelled type its member is read
    /// as.
    ///
    /// How many there are is counted first, over the document alone, and the rounds one of them
    /// may take is the input's own member count, so the bound on how much JSON a request may
    /// have read for them ([`MOST_JSON_READ_FOR_ABSENCES`]) is applied before a single path is
    /// recorded or a single probe built.
    fn of<Eng: Send + Sync + 'static>(
        key: &'r str,
        protocol: &'r dyn ServerHttpProtocol,
        req: &'r RtRequest,
        body: &'r [u8],
        document: Value,
    ) -> Self {
        let stated = absences_stated(&document);
        let declared = declared_input_members::<Eng>(key, protocol, req).unwrap_or_default();
        // The reads one absence costs: one for the type its member is read as, one per round
        // that isolates it, and one for the diagnosis it may earn.
        let each = declared.len().saturating_add(2);
        let asking = stated
            .saturating_mul(each)
            .saturating_mul(body.len())
            <= MOST_JSON_READ_FOR_ABSENCES;
        let at = if asking {
            absences(&document)
        } else {
            Vec::new()
        };
        let mut held = Self {
            key,
            protocol,
            req,
            body,
            document,
            stated,
            at,
            targets: Vec::new(),
            declared,
        };
        held.targets = (0..held.at.len())
            .map(|which| held.target::<Eng>(which))
            .collect();
        held
    }

    /// What the body states.
    fn reading<Eng: Send + Sync + 'static>(&self) -> BodyReading {
        if self.stated == 0 {
            return BodyReading::AsSent;
        }
        // In the order the BODY carries them, which is the order the deserializer reads them
        // in: the first `null` the input cannot do without is the one the caller has to fix.
        for which in 0..self.at.len() {
            let Some(expected) = self.targets[which].and_then(Expected::of) else {
                continue;
            };
            if !self.required::<Eng>(which) {
                continue;
            }
            if let Some(diagnosis) = expected.diagnosis(self.body, &self.at[which]) {
                return BodyReading::Unreadable(Some(Prose::Diagnosis(diagnosis)));
            }
        }
        match rebuilt(self.req, &self.without_absences()) {
            Some(served) => BodyReading::Absences(Box::new(served)),
            // A request that cannot be rebuilt is served exactly as it arrived, which is the
            // framework's own answer to the `null` it then reads.
            None => BodyReading::AsSent,
        }
    }

    /// The modelled type the member whose absence the `which`th `null` states is read as, as
    /// the framework's own deserializer reports it.
    ///
    /// Asked of a body in which every OTHER stated absence is gone, so the `null` that read
    /// answers is this one. `None` when the read it answered cannot be told — a value of the
    /// wrong class earlier in the body stopped the walk first, which is a defect of its own
    /// ([`value_reading`]).
    fn target<Eng: Send + Sync + 'static>(&self, which: usize) -> Option<ShapeType> {
        let probe = rebuilt(self.req, &self.only(which))?;
        null_target::<Eng>(self.key, self.protocol, &probe)
    }

    /// Whether the input REQUIRES the member whose absence the `which`th `null` states.
    ///
    /// Asked of the generated deserializer, which is the only thing that knows: the member is
    /// dropped from a document in which every OTHER stated absence carries a value the codec
    /// reads, so the member that reader then reports absent is this one.
    ///
    /// A member the body never carried AT ALL is a different defect, and one the reader may
    /// report first because it reports the absences of a body in the order the input declares
    /// its members, while a body is READ in the order it carries them: a `null` standing for a
    /// member the input requires stops the read before any member's absence is noticed at all.
    /// So a member reported absent that this `null` does not stand for is filled in turn and
    /// the reader asked again, until it reports this one or reports one that cannot be filled.
    fn required<Eng: Send + Sync + 'static>(&self, which: usize) -> bool {
        let mut document = self.filled(which);
        // Bounded by the input's own member count: each round fills a member the round before
        // reported absent, and a filled member is never reported absent again. That count is
        // also a factor of the bound on how much JSON one request may have read here
        // ([`MOST_JSON_READ_FOR_ABSENCES`]).
        for _ in 0..=self.declared.len() {
            let Some(probe) = rebuilt(self.req, &document) else {
                return false;
            };
            let Some(absent) = absent_member::<Eng>(self.key, self.protocol, &probe) else {
                return false;
            };
            if names(&self.at[which], &absent) {
                return true;
            }
            let filled = self
                .declared
                .iter()
                .find(|member| path::member(member.name) == absent)
                .and_then(|member| stands_in(member.target).map(|value| (member.wire, value)));
            let Some((wire, value)) = filled else {
                return false;
            };
            match document.as_object_mut() {
                Some(members) => members.insert(wire.to_string(), value),
                None => return false,
            };
        }
        false
    }

    /// The document with every member a `null` states the absence of gone — the body the
    /// operation is served.
    fn without_absences(&self) -> Value {
        self.rewritten(&|_| Applied::Dropped)
    }

    /// The document with every `null` but the `which`th gone.
    fn only(&self, which: usize) -> Value {
        self.rewritten(&|held| {
            if held == which {
                Applied::Stated
            } else {
                Applied::Dropped
            }
        })
    }

    /// The document with the `which`th member gone and every other stated absence carrying a
    /// value of its own modelled type, so the reader's answer is about this member alone.
    fn filled(&self, which: usize) -> Value {
        self.rewritten(&|held| {
            let stands = (held != which)
                .then(|| self.targets.get(held).copied().flatten())
                .flatten()
                .and_then(stands_in);
            match stands {
                Some(value) => Applied::StoodIn(value),
                None => Applied::Dropped,
            }
        })
    }

    /// The document with each absence rewritten as `how` says, in ONE pass over it: the
    /// absences are visited in the order the body carries them, which is the order they were
    /// counted and recorded in, so `how` is handed the same index everything else here uses.
    ///
    /// One pass whatever the number of absences, so applying them costs a copy of the body
    /// rather than a copy per absence.
    fn rewritten(&self, how: &dyn Fn(usize) -> Applied) -> Value {
        let mut visited = 0;
        rewrite(&self.document, &mut visited, how)
    }
}

/// How many absences `document` states — the cheap count the bound on asking about them is
/// applied to, which allocates nothing and visits the document once.
fn absences_stated(document: &Value) -> usize {
    match document {
        Value::Object(members) => members
            .iter()
            .map(|(_, held)| usize::from(held.is_null()) + absences_stated(held))
            .sum(),
        Value::Array(elements) => elements.iter().map(absences_stated).sum(),
        _ => 0,
    }
}

/// Where `document` states a member's absence with an explicit `null`, in the order the body
/// carries them — which is the order [`absences_stated`] counted them and the order
/// [`rewrite`] visits them.
///
/// A member of an object only: a `null` ELEMENT of an array states no member's absence —
/// there is no member for it to be about, and dropping it would renumber the elements beside
/// it — so it is left to the codec, exactly as it stands. Built only for a body inside
/// [`MOST_JSON_READ_FOR_ABSENCES`], which is what bounds what these paths cost to hold.
fn absences(document: &Value) -> Vec<Vec<Step>> {
    let mut found = Vec::new();
    descend(document, &mut Vec::new(), &mut found);
    found
}

/// Collect `value`'s stated absences, `at` being the path it stands at.
///
/// The recursion is as deep as the document, which is as deep as the reader that produced it
/// descended — a bound that reader holds, not one a request chooses ([`nests_too_deep`]).
fn descend(value: &Value, at: &mut Vec<Step>, found: &mut Vec<Vec<Step>>) {
    match value {
        Value::Object(members) => {
            for (name, held) in members {
                at.push(Step::Member(name.clone()));
                if held.is_null() {
                    found.push(at.clone());
                } else {
                    descend(held, at, found);
                }
                at.pop();
            }
        }
        Value::Array(elements) => {
            for (index, held) in elements.iter().enumerate() {
                at.push(Step::Element(index));
                descend(held, at, found);
                at.pop();
            }
        }
        _ => {}
    }
}

/// `value` with each absence rewritten as `how` says, visiting them in the order the body
/// carries them — `visited` is how many have been reached so far, which is the index `how` is
/// handed.
///
/// ONE pass, which is what makes applying the absences cost a copy of the body rather than a
/// copy per absence: the members beside an absence are moved into the rebuilt object in order,
/// so the document keeps the member order the body was written in. The recursion is as deep as
/// the document, which is as deep as the reader that produced it descended
/// ([`nests_too_deep`]).
fn rewrite(value: &Value, visited: &mut usize, how: &dyn Fn(usize) -> Applied) -> Value {
    match value {
        Value::Object(members) => {
            let mut kept = ::serde_json::Map::with_capacity(members.len());
            for (name, held) in members {
                if !held.is_null() {
                    kept.insert(name.clone(), rewrite(held, visited, how));
                    continue;
                }
                let which = *visited;
                *visited += 1;
                match how(which) {
                    Applied::Dropped => {}
                    Applied::Stated => {
                        kept.insert(name.clone(), Value::Null);
                    }
                    Applied::StoodIn(stands) => {
                        kept.insert(name.clone(), stands);
                    }
                }
            }
            Value::Object(kept)
        }
        Value::Array(elements) => Value::Array(
            elements
                .iter()
                .map(|held| rewrite(held, visited, how))
                .collect(),
        ),
        held => held.clone(),
    }
}

/// A value of the modelled type `target` that the codec reads.
///
/// For ONE purpose: asking the generated deserializer which members the input requires. A
/// member standing in a body is a member that reader cannot report absent, so a probe that
/// fills every OTHER stated absence isolates the one being asked about. Such a value stands
/// in a document this layer builds, reads once and drops — it reaches no handler, no response
/// and no store. `None` for a type no single value stands for, which leaves that absence
/// stated as the absence it is.
fn stands_in(target: ShapeType) -> Option<Value> {
    Some(match target {
        // A blob is carried as base64 text, of which the empty string is the empty blob.
        ShapeType::String | ShapeType::Blob => json!(""),
        ShapeType::Boolean => json!(false),
        ShapeType::Byte
        | ShapeType::Short
        | ShapeType::Integer
        | ShapeType::Long
        | ShapeType::Float
        | ShapeType::Double
        | ShapeType::BigInteger
        | ShapeType::BigDecimal
        // The protocol's default timestamp form is a number of seconds since the epoch.
        | ShapeType::Timestamp => json!(0),
        ShapeType::List => json!([]),
        ShapeType::Map | ShapeType::Structure | ShapeType::Document => json!({}),
        // A union states exactly one of its members, so no empty value stands for one.
        _ => return None,
    })
}

/// Whether `stated` — a dotted member path as the framework's deserializer spells one — names
/// the value at `at`.
///
/// The spelling is the framework's own ([`path`]), not a second transcription of it: a member
/// of a structure names itself in lowerCamel, while an entry of a map and an element of a
/// list name the key or the 1-based position and then the collection step. A path step that
/// is a JSON object member can be either of the first two, because the document alone does
/// not say whether the object is a structure or a map, so both spellings are admitted.
fn names(at: &[Step], stated: &str) -> bool {
    let segments: Vec<&str> = stated.split('.').collect();
    named_by(at, &segments)
}

/// [`names`], over the remaining steps and the remaining segments.
fn named_by(at: &[Step], segments: &[&str]) -> bool {
    let Some((step, rest)) = at.split_first() else {
        return segments.is_empty();
    };
    let two = || (segments.len() >= 2).then(|| segments[..2].join("."));
    match step {
        Step::Member(name) => {
            if segments.first() == Some(&path::member(name).as_str())
                && named_by(rest, &segments[1..])
            {
                return true;
            }
            two().is_some_and(|joined| joined == path::entry(name)) && named_by(rest, &segments[2..])
        }
        Step::Element(index) => {
            two().is_some_and(|joined| joined == path::element(index + 1))
                && named_by(rest, &segments[2..])
        }
    }
}

/// `req` with `document` as its body: the request this layer hands on, and the request every
/// probe above reads.
///
/// `None` when the request cannot be rebuilt — a body the transport could not replay — which
/// leaves the request to be served exactly as it arrived.
fn rebuilt(req: &RtRequest, document: &Value) -> Option<RtRequest> {
    let body = ::serde_json::to_vec(document).ok()?;
    let mut rebuilt = req.try_clone()?;
    let length = body.len();
    *rebuilt.body_mut() = SdkBody::from(body);
    // A request that declared its length declares the length of the body it now carries.
    if rebuilt.headers().get(CONTENT_LENGTH).is_some() {
        let _ = rebuilt
            .headers_mut()
            .try_insert(CONTENT_LENGTH, length.to_string());
    }
    Some(rebuilt)
}

/// The header a request declares the size of its body in.
const CONTENT_LENGTH: &str = "content-length";

/// The value the deserializer expected where a body stated a member's absence, as the ONE
/// reader that states it: each is the Rust type the framework's generated reader reads that
/// modelled type into (`watched_reads!` below and the generated `crate::ops::types` are where
/// this tree states that mapping — a string into a `String`, an integer into an `i32`, a list
/// into a `Vec`, a map into a `HashMap`), and the sentence is that reader's own for it — the
/// class found, the value expected and the position in the body, which is read from the bytes
/// the caller sent rather than fixed.
///
/// How the reader names each is ITS vocabulary, not a choice made here: a kind of value it
/// names in words ("a string", "a sequence", "a map", "a boolean") and a numeric primitive it
/// names by the type ("i32") are both what the recorded refusals of this service carry, so
/// asking the reader is what gets each right.
#[derive(Debug, Clone, Copy)]
enum Expected {
    Text,
    Flag,
    Byte,
    Short,
    Integer,
    Long,
    Float,
    Double,
    Sequence,
    Mapping,
}

impl Expected {
    /// What a member the model types as `target` is read into, or `None` for a modelled type
    /// this service states no expectation for — which leaves the absence stated as the
    /// absence it is, rather than inventing a half of the sentence.
    fn of(target: ShapeType) -> Option<Self> {
        Some(match target {
            ShapeType::String => Self::Text,
            ShapeType::Boolean => Self::Flag,
            ShapeType::Byte => Self::Byte,
            ShapeType::Short => Self::Short,
            ShapeType::Integer => Self::Integer,
            ShapeType::Long => Self::Long,
            ShapeType::Float => Self::Float,
            ShapeType::Double => Self::Double,
            ShapeType::List => Self::Sequence,
            ShapeType::Map => Self::Mapping,
            _ => return None,
        })
    }

    /// The deserializer's own diagnosis of `body`, read at `at`.
    ///
    /// The sentence is not written here: the maintained JSON reader is asked for this value
    /// at this position, and what it answers — the class it found, the value it expected and
    /// the line and column of the body it stopped at — is the sentence. `None` when the value
    /// reads after all, which is no diagnosis to state.
    fn diagnosis(self, body: &[u8], at: &[Step]) -> Option<String> {
        let mut reader = ::serde_json::Deserializer::from_slice(body);
        let read = ValueAt { at, expected: self }.deserialize(&mut reader);
        read.err().map(|stated| stated.to_string())
    }

    /// Read one value as the type the framework reads that member into, so the reader's own
    /// expectation of it is what a refusal states.
    fn read<'de, D: Deserializer<'de>>(self, reader: D) -> Result<(), D::Error> {
        match self {
            Self::Text => String::deserialize(reader).map(drop),
            Self::Flag => bool::deserialize(reader).map(drop),
            Self::Byte => i8::deserialize(reader).map(drop),
            Self::Short => i16::deserialize(reader).map(drop),
            Self::Integer => i32::deserialize(reader).map(drop),
            Self::Long => i64::deserialize(reader).map(drop),
            Self::Float => f32::deserialize(reader).map(drop),
            Self::Double => f64::deserialize(reader).map(drop),
            // The ELEMENTS and the ENTRIES are not read: the question is about the collection
            // itself, and a value that is no collection is refused before one is reached.
            Self::Sequence => Vec::<IgnoredAny>::deserialize(reader).map(drop),
            Self::Mapping => {
                ::std::collections::HashMap::<String, IgnoredAny>::deserialize(reader).map(drop)
            }
        }
    }
}

/// Read the value at one path of a body, as the type the member there is read into — the
/// seed that walks the maintained reader down to the value a refusal is about, so the
/// position the refusal names is that reader's own count of the bytes the caller sent.
///
/// Every value beside the path is skipped rather than read: the question is about one value,
/// and nothing else in the body decides it.
struct ValueAt<'a> {
    at: &'a [Step],
    expected: Expected,
}

impl<'de> DeserializeSeed<'de> for ValueAt<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, reader: D) -> Result<(), D::Error> {
        match self.at.split_first() {
            None => self.expected.read(reader),
            Some((Step::Member(name), rest)) => reader.deserialize_map(MemberAt {
                name,
                rest,
                expected: self.expected,
            }),
            Some((Step::Element(index), rest)) => reader.deserialize_seq(ElementAt {
                index: *index,
                rest,
                expected: self.expected,
            }),
        }
    }
}

/// Descend into the member `name` of an object, skipping the members beside it.
struct MemberAt<'a> {
    name: &'a str,
    rest: &'a [Step],
    expected: Expected,
}

impl<'de> ::serde::de::Visitor<'de> for MemberAt<'_> {
    type Value = ();

    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<(), A::Error> {
        while let Some(key) = entries.next_key::<String>()? {
            if key == self.name {
                entries.next_value_seed(ValueAt {
                    at: self.rest,
                    expected: self.expected,
                })?;
            } else {
                entries.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// Descend into the element at `index` of an array, skipping the elements beside it.
struct ElementAt<'a> {
    index: usize,
    rest: &'a [Step],
    expected: Expected,
}

impl<'de> ::serde::de::Visitor<'de> for ElementAt<'_> {
    type Value = ();

    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str("an array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut elements: A) -> Result<(), A::Error> {
        for position in 0.. {
            let read = if position == self.index {
                elements.next_element_seed(ValueAt {
                    at: self.rest,
                    expected: self.expected,
                })?
            } else {
                elements.next_element::<IgnoredAny>()?.map(drop)
            };
            if read.is_none() {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// `message` as the PROTOCOL's own refusal of a body that could not be read answers it, in
/// the envelope of the protocol `req` is served in — which also writes the `__type`
/// discriminator and the media type.
///
/// The shape is synthetic because no model declares it: the request never became an input,
/// so no operation ran to answer with one of its own errors. `None` when the body cannot be
/// encoded at all, which leaves the framework's own rendering to answer.
///
/// `None` for `prose` is a refusal that states NONE: a body that is not a document of this
/// protocol at all carries no value whose class could be named, and a value no conversion was
/// attempted for states nothing either, so the refusal is the class alone (see
/// [`BodyReading::Unreadable`], [`Refused::Bare`]).
pub fn serialization_refusal(
    svc: &dyn Service,
    req: &RtRequest,
    prose: Option<&Prose>,
) -> Option<aws_smithy_runtime_api::http::Response> {
    svc.definition()
        .wire_protocol(req)
        .protocol()
        .serialize_error(
            &SerializationException {
                prose: prose.cloned(),
            },
            &SERIALIZATION_EXCEPTION_SCHEMA,
            SERIALIZATION_EXCEPTION,
            SERIALIZATION_EXCEPTION_STATUS,
        )
        .ok()
}

/// Run the framework's own deserializer for `O` over the request, and report the first value
/// it could not read into the modelled type of the member it is bound to, with the type that
/// read was for and the position in the body it stood at.
///
/// A body that READS is this layer's business no further: whatever the model's own
/// constraints then say about its values is a constraint refusal, which the walk that
/// follows answers.
///
/// The CLASS recorded is the one the codec's own complaint named the value under, which is
/// read off the vendored codec's wording ([`value_class`]), and the LITERAL is read off the
/// body at the position recorded beside it ([`as_literal`]).
fn probe<O: Operation>(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> Option<Unreadable> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let found: RefCell<Option<Unreadable>> = RefCell::new(None);
    let at: Trail = RefCell::new(Vec::new());
    let read = {
        let mut probe = Probe {
            inner: &mut *deserializer,
            found: &found,
            at: &at,
            // The input IS a structure, so the members the body states for it are fields
            // of it.
            slot: Slot::Field,
            accepted: None,
        };
        <O::Input as DeserializableStruct>::deserialize(&mut probe)
    };
    if read.is_ok() {
        return None;
    }
    found.into_inner()
}

/// Run the framework's own deserializer for `O` over the request and report the modelled type
/// the read one explicit `null` in it answered was FOR, however the read answered: by failing
/// (a scalar, which names the type in its refusal) or by taking the `null` as absence (a
/// structure, a list or a map, each recorded as the read happens).
///
/// The body this is asked of carries ONE stated absence ([`StatedAbsences::only`]), so the
/// read reported is that one's.
fn null_read_target<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<ShapeType> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let found: RefCell<Option<Unreadable>> = RefCell::new(None);
    let accepted: RefCell<Option<ShapeType>> = RefCell::new(None);
    let at: Trail = RefCell::new(Vec::new());
    {
        let mut probe = Probe {
            inner: &mut *deserializer,
            found: &found,
            at: &at,
            slot: Slot::Field,
            accepted: Some(&accepted),
        };
        let _ = <O::Input as DeserializableStruct>::deserialize(&mut probe);
    }
    let failed = found.into_inner().and_then(|unreadable| match unreadable {
        Unreadable::Class { target, class, .. } if class == NULL_VALUE => Some(target),
        _ => None,
    });
    failed.or_else(|| accepted.into_inner())
}

/// Read `req`'s body into `O`'s input with the framework's own deserializer and report the
/// member that read found MISSING, if that is what stopped it
/// ([`SerdeError::MissingMember`], which the generated reader raises for a member the input
/// requires and the body does not carry).
fn unsatisfied_member<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    match <O::Input as DeserializableStruct>::deserialize(&mut *deserializer) {
        Err(SerdeError::MissingMember { member_name }) => Some(member_name),
        _ => None,
    }
}

/// What one unreadable value is refused with, or `None` when this service states no name for
/// the type the failing read was for.
///
/// Five answers, because a caller has five different defects to fix. A single value sent
/// where the model declares another single value is a CONVERSION that could not be
/// performed, and the sentence names the two facts to reconcile: the literal sent and the
/// type it could not become. A collection standing where the member it is bound to is read
/// from no collection is not a conversion at all and none was attempted for it, and which of
/// the two collections it is decides the answer: an ARRAY earns the error class alone, with
/// no prose ([`Refused::Bare`]), while an OBJECT is named after the structural token the
/// reader found ([`STRUCTURE_OR_MAP`]). A single value standing where the model declares a
/// type that HOLDS other values is named after WHERE it stood: a member a structure declares
/// is a field whose type was unexpected ([`UNEXPECTED_FIELD_TYPE`]), while a value inside a
/// collection is named by no member at all, so what the sentence states is the value the
/// payload carried ([`UNEXPECTED_VALUE_TYPE`], [`Slot`]). And base64 text no blob can be
/// decoded from is a defect of the TEXT rather than of its class ([`BASE64_LENGTH`]).
///
/// An explicit `null` is no refusal of this class at all: nothing was converted, because
/// `null` states that the member is ABSENT, which is what [`BodyReading`] answers.
fn refusal(unreadable: &Unreadable) -> Option<Refused> {
    let (target, class, slot) = match unreadable {
        Unreadable::Base64Length { characters } => {
            return Some(Refused::Stated(format!("{BASE64_LENGTH}{characters}")));
        }
        // A read this layer states nothing about: the framework's own answer to it stands.
        Unreadable::Unclassified { .. } => return None,
        Unreadable::Class {
            target,
            class,
            slot,
            ..
        } => (*target, *class, *slot),
    };
    if class == NULL_VALUE {
        return None;
    }
    if class == ARRAY_VALUE {
        return Some(Refused::Bare);
    }
    if class == OBJECT_VALUE {
        return Some(Refused::Stated(STRUCTURE_OR_MAP.to_string()));
    }
    if is_aggregate(target) {
        return Some(Refused::Stated(
            match slot {
                Slot::Field => UNEXPECTED_FIELD_TYPE,
                Slot::Inside => UNEXPECTED_VALUE_TYPE,
            }
            .to_string(),
        ));
    }
    Some(Refused::Stated(format!(
        "{class} cannot be converted to {}",
        named_type(target)?
    )))
}

/// `unreadable` with the class of the value it is about refined to the LITERAL `document`
/// carries at the position the failing read stood.
///
/// The codec's own complaint names the JSON TYPE it found ("boolean", "number"), which is a
/// class of several literals rather than one of them, while this protocol names the literal
/// ([`TRUE_VALUE`]). The body is the only place that fact survives the read, so the value the
/// failing read stood at is looked up in it. A path that no longer resolves and a literal of
/// another class than the codec named both leave the class exactly as the codec named it.
fn as_literal(document: &Value, unreadable: Unreadable) -> Unreadable {
    match unreadable {
        Unreadable::Class {
            target,
            class,
            slot,
            at,
        } => {
            let carried = value_at(document, &at);
            let class = carried.map_or(class, |held| literal_class(class, held));
            Unreadable::Class {
                target,
                class,
                slot,
                at,
            }
        }
        held => held,
    }
}

/// The value a member the model types as `target` READS the literal `carried` as, where the
/// two differ in form and the literal still spells a value of that type — and `None` for a
/// literal the member can be read from in no form at all, which is the value a refusal is
/// then about.
///
/// Two conversions, both of them the PROTOCOL's rather than rules of the model, so each is
/// applied wherever the model declares that type: a quoted literal spelling a flag is read as
/// the flag it spells, and a number carrying a fraction standing where a whole number is
/// modelled is read as the whole number it truncates to — the codec itself already reads one
/// whose fraction is zero, so what this adds is the truncation. A literal needing neither
/// conversion never reaches here: the codec read it.
fn readable(target: ShapeType, carried: &Value) -> Option<Value> {
    match (target, carried) {
        (ShapeType::Boolean, Value::String(text)) => boolean_text(text).map(Value::Bool),
        (
            ShapeType::Byte | ShapeType::Short | ShapeType::Integer | ShapeType::Long,
            Value::Number(number),
        ) => {
            // A number whose value is already a whole one is no fraction: whatever refused
            // it, it was not its form.
            if !fractional(number) {
                return None;
            }
            // Towards zero, which is the narrowing the reader itself performs on the
            // fraction it already reads (a number whose fraction is zero), carried to one
            // that is not zero. No recording states the direction, so it is this one reading
            // rather than a second rule.
            let whole = number.as_f64()?.trunc();
            // Only a fraction of a number a whole one can be made of at all; the rest is no
            // value of any integral type.
            if whole < -(2f64.powi(63)) || whole >= 2f64.powi(63) {
                return None;
            }
            #[allow(clippy::cast_possible_truncation)]
            Some(Value::Number(::serde_json::Number::from(whole as i64)))
        }
        _ => None,
    }
}

/// Whether the number's VALUE is not a whole one — the question the fractional token and the
/// narrowing both turn on.
///
/// Asked of the value rather than of the text the reader renders it as: the maintained reader
/// holds every number in the nearest type that fits, so a whole number too large for its
/// integral types is held as a double and is STILL a whole number, while a fraction is a
/// fraction however the body spelled it (`1.5`, `1e-7`).
fn fractional(number: &::serde_json::Number) -> bool {
    number.as_i64().is_none()
        && number.as_u64().is_none()
        && number.as_f64().is_some_and(|held| held.fract() != 0.0)
}

/// Write `form` at `at` in `document`, answering whether the path named a value to replace.
fn write_at(document: &mut Value, at: &[Step], form: Value) -> bool {
    let mut held = document;
    for step in at {
        let next = match step {
            Step::Member(name) => held.get_mut(name),
            Step::Element(index) => held.get_mut(index),
        };
        match next {
            Some(reached) => held = reached,
            None => return false,
        }
    }
    *held = form;
    true
}

/// The token the LITERAL `carried` is named by, where it is one of a class this protocol
/// splits and `class` — the class the codec named — agrees that it is the value the read
/// stopped at; otherwise `class` itself.
///
/// The agreement is what keeps a path that resolved to another value from renaming the
/// refusal: a literal whose class is not the one the codec complained about is not the value
/// that failed to read.
fn literal_class(class: &'static str, carried: &Value) -> &'static str {
    match (class, carried) {
        (BOOLEAN_VALUE, Value::Bool(true)) => TRUE_VALUE,
        (BOOLEAN_VALUE, Value::Bool(false)) => FALSE_VALUE,
        // A whole number keeps `NUMBER_VALUE`; one whose value carries a fraction is the
        // other token ([`fractional`]).
        (NUMBER_VALUE, Value::Number(number)) if fractional(number) => DECIMAL_VALUE,
        _ => class,
    }
}

/// The value `document` carries at `at`, or `None` when the path names nothing in it.
fn value_at<'v>(document: &'v Value, at: &[Step]) -> Option<&'v Value> {
    let mut held = document;
    for step in at {
        held = match step {
            Step::Member(name) => held.get(name)?,
            Step::Element(index) => held.get(index)?,
        };
    }
    Some(held)
}

/// Whether the modelled type is one that holds OTHER values rather than being one.
fn is_aggregate(target: ShapeType) -> bool {
    matches!(
        target,
        ShapeType::List | ShapeType::Map | ShapeType::Structure | ShapeType::Union
    )
}

/// What this service calls the type a value could not be converted to.
///
/// A member's modelled type is named by its own Smithy shape type, with no article in front
/// of it, which is what the recorded refusals of a string member and of a long member state
/// ("NUMBER_VALUE cannot be converted to String", "STRING_VALUE cannot be converted to
/// Long") — an enumerated type IS a string shape in these schemas, so it is named the same.
/// A target this service is not known to state a name for answers `None`, which leaves that
/// value to the codec rather than refusing it under a name nothing here could ground.
fn named_type(target: ShapeType) -> Option<&'static str> {
    match target {
        ShapeType::String => Some("String"),
        ShapeType::Boolean => Some("Boolean"),
        ShapeType::Byte => Some("Byte"),
        ShapeType::Short => Some("Short"),
        ShapeType::Integer => Some("Integer"),
        ShapeType::Long => Some("Long"),
        ShapeType::Float => Some("Float"),
        ShapeType::Double => Some("Double"),
        _ => None,
    }
}

/// What one unreadable value is refused with.
#[derive(Debug)]
enum Unreadable {
    /// A value whose JSON CLASS the modelled type of the slot it stands in admits no reading
    /// of: the type the failing read was for, the class the codec's own complaint named the
    /// submitted value under, the kind of slot the value stands in, and where in the body it
    /// stands — which is what makes the LITERAL the body carries there reachable
    /// ([`as_literal`]).
    Class {
        target: ShapeType,
        class: &'static str,
        slot: Slot,
        at: Vec<Step>,
    },
    /// Text standing where a blob is modelled that is not a whole number of base64 groups,
    /// with the number of characters the body carried — the one fact the sentence for it
    /// states ([`BASE64_LENGTH`]).
    Base64Length { characters: usize },
    /// A read that failed for a reason this layer states nothing about — a number no value of
    /// the modelled width can be made of, text that is not a timestamp — recorded only for
    /// the type it was for and the position it was at, because that is what says whether the
    /// value can be READ after all in the form the protocol carries it
    /// ([`readable`]). Refused by nobody here: the framework's own answer to it stands
    /// ([`refusal`]).
    Unclassified { target: ShapeType, at: Vec<Step> },
}

impl Unreadable {
    /// The modelled type the failing read was for, and where in the body the value it was for
    /// stands — `None` for a defect of a value's TEXT, which is about the text rather than
    /// about which value stood there.
    fn standing(&self) -> Option<(ShapeType, &[Step])> {
        match self {
            Self::Class { target, at, .. } | Self::Unclassified { target, at } => {
                Some((*target, at))
            }
            Self::Base64Length { .. } => None,
        }
    }
}

/// WHERE the value a read is for stands, which is what a defect involving a collection is
/// named after ([`refusal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A member a structure or a union DECLARES, which the body names: a field. The input
    /// itself is a structure, so its own members stand here too.
    Field,
    /// A value INSIDE a collection — an element of a list, or the value of a map entry —
    /// which no modelled member names.
    Inside,
}

/// The framework's deserializer, watched.
///
/// Every method forwards to the real deserializer and returns its answer unchanged — this is
/// not a codec, and it reimplements no part of one. The ONE thing it adds is that a read
/// which fails on a JSON class mismatch is recorded with the modelled type that read was
/// for and the kind of slot it stood in, neither of which the error carries. The first such
/// read wins: it is the one the caller has to fix first, and the reads after it are reading a
/// body the parser has already left.
struct Probe<'d, 'f> {
    inner: &'d mut (dyn ShapeDeserializer + 'd),
    found: &'f RefCell<Option<Unreadable>>,
    /// Where in the BODY the value this probe reads stands: the members, keys and positions
    /// the reads descended through to reach it. Each container read pushes the step it
    /// descends through and pops it on the way out, so a failing read records the path the
    /// body carries that value at ([`note_in`]).
    at: &'f Trail,
    /// Where the value this probe reads stands: a field of the structure (or union) being
    /// read, or a value inside a collection. Each container read hands the probe it builds
    /// for what is INSIDE it the slot that applies there ([`Slot`]).
    slot: Slot,
    /// Where a `null` the codec reads WITHOUT failing is recorded, as the modelled type that
    /// read was for: a structure, a list and a map each take a `null` as absence, so the type
    /// expected where a body stated one is otherwise unstated. `None` leaves those reads
    /// unwatched, which is what the coercion probe wants: a `null` is no value of the wrong
    /// class, and recording one would hide the value that IS.
    accepted: Option<&'f RefCell<Option<ShapeType>>>,
}

impl Probe<'_, '_> {
    /// Record the modelled type of a container read the next value is a `null` for.
    ///
    /// The first record wins, like a failing read's: the body this is watched over carries one
    /// stated absence ([`null_read_target`]).
    fn note_accepted(&self, schema: &Schema) {
        let Some(recorder) = self.accepted else {
            return;
        };
        if !self.inner.is_null() {
            return;
        }
        let mut held = recorder.borrow_mut();
        if held.is_none() {
            *held = Some(schema.shape_type());
        }
    }
}

/// Record `error` as a coercion failure into `target`, when that is what it is, and return
/// it untouched.
///
/// A free function rather than a method, so a failing read can hand it the recorder while
/// the deserializer it is reading through is still borrowed. The first record wins, which is
/// why a nested read's own failure is never overwritten by the container it propagates
/// through.
///
/// `target` is the type the FAILING READ was for, not the shape type of the `Schema` that
/// read was handed: the two agree for a member of a structure, and only the read knows the
/// element type of a collection, because the JSON codec hands a collection's elements the
/// collection member's own schema.
fn note_in(
    found: &RefCell<Option<Unreadable>>,
    at: &Trail,
    target: ShapeType,
    slot: Slot,
    error: SerdeError,
) -> SerdeError {
    let mut held = found.borrow_mut();
    if held.is_none() {
        *held = Some(match value_class(&error) {
            Some(class) => Unreadable::Class {
                target,
                class,
                slot,
                at: at.borrow().clone(),
            },
            // A read this layer states nothing about is still recorded, because the value it
            // was for may be one the protocol READS in another form ([`readable`]).
            None => Unreadable::Unclassified {
                target,
                at: at.borrow().clone(),
            },
        });
    }
    error
}

/// Where the reads have descended to in the body, as they descend: one step per container
/// entered, pushed on the way in and popped on the way out, so the path is whatever the
/// reader is standing at ([`Probe::at`]).
type Trail = RefCell<Vec<Step>>;

/// The name this protocol carries `member` under — its `@jsonName` where the model gives one,
/// else its Smithy member name, which is the key the JSON codec matched it by and so the key
/// the body carries it under.
fn wire_name(member: &Schema) -> &str {
    member
        .json_name()
        .map(|given| given.value())
        .or_else(|| member.member_name())
        .unwrap_or_default()
}

/// Record `text` — standing where a blob is modelled, and undecodable — as the defect of its
/// LENGTH, when that is what makes it undecodable.
///
/// Base64 carries every three bytes as a group of four characters, so text that is not a
/// whole number of those groups encodes no bytes at all, whatever characters it holds. Text
/// of a legal length that still does not decode is left unrecorded: this service states no
/// sentence for that defect, so inventing one here would answer a caller with prose nothing
/// grounds, and the codec's own refusal of it stands.
fn note_base64(found: &RefCell<Option<Unreadable>>, text: &str) {
    let characters = text.chars().count();
    if characters.is_multiple_of(BASE64_GROUP) {
        return;
    }
    let mut held = found.borrow_mut();
    if held.is_none() {
        *held = Some(Unreadable::Base64Length { characters });
    }
}

/// The CLASS of JSON value a codec refusal is about, or `None` when the refusal is not about
/// a value's type.
///
/// The codec states a type mismatch as "expected <the type it wanted>, got <the JSON type it
/// found>" and a number outside the modelled integral width as "number … does not fit in
/// <width>" (`vendor/smithy-schema-server/src/json.rs` — `TokenKind`'s `Display` is the
/// vocabulary of the tail, chosen there precisely so it names a JSON type a caller can act
/// on). Those are the two refusals a value's own type earns; every other codec failure — an
/// absent member, text that is not a timestamp, a truncated body — says nothing about a JSON
/// type and is left to the framework.
///
/// JSON `null` IS one of the names, and is the one that is not a value of the wrong class: a
/// `null` states that the member is absent, so what it earns is decided by whether the input
/// requires that member ([`BodyReading`]) rather than by a conversion ([`refusal`] states no
/// conversion for it). It is recorded here because the modelled type the read was FOR is the
/// only place the type a `null` stood where is stated.
///
/// The wordings this reads are PINNED by [`tests::the_codec_wordings_this_layer_reads`]
/// below, so a change to them in the vendored codec fails loudly here instead of silently
/// sending every coercion failure back to `ValidationException`.
fn value_class(error: &SerdeError) -> Option<&'static str> {
    use service_common::json_wire_values as classes;
    let stated = error.to_string();
    if stated.contains("does not fit in") {
        return Some(classes::NUMBER_VALUE);
    }
    let found = stated.strip_prefix("expected ")?.split(", got ").nth(1)?;
    match found {
        "string" => Some(classes::STRING_VALUE),
        "number" => Some(classes::NUMBER_VALUE),
        "boolean" => Some(classes::BOOLEAN_VALUE),
        "array start" => Some(classes::ARRAY_VALUE),
        "object start" => Some(classes::OBJECT_VALUE),
        "null" => Some(classes::NULL_VALUE),
        // The framing tokens ("object key", "array end", …), which describe where the parser
        // is rather than a value a caller sent.
        _ => None,
    }
}

/// Forward one scalar read to the real deserializer, recording a JSON class mismatch against
/// the type that read is FOR — which is the read's own target type, named beside it here,
/// rather than the shape type of the schema it is handed.
///
/// `found` and the slot are copied out of `self` BEFORE the read starts, so the recorder is
/// still reachable while the deserializer underneath is mutably borrowed.
macro_rules! watched_reads {
    ($($method:ident -> $value:ty as $target:ident),+ $(,)?) => {
        $(
            fn $method(&mut self, schema: &Schema) -> Result<$value, SerdeError> {
                let found = self.found;
                let at = self.at;
                let slot = self.slot;
                self.inner
                    .$method(schema)
                    .map_err(|error| note_in(found, at, ShapeType::$target, slot, error))
            }
        )+
    };
}

impl ShapeDeserializer for Probe<'_, '_> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.note_accepted(schema);
        let found = self.found;
        let accepted = self.accepted;
        let at = self.at;
        let slot = self.slot;
        // What a structure (or a union) holds are the members it DECLARES, so every value
        // read inside this one stands in a field of it, under the name the body carries that
        // member by.
        let mut watched = |member: &Schema, inner: &mut dyn ShapeDeserializer| {
            at.borrow_mut()
                .push(Step::Member(wire_name(member).to_string()));
            let read = {
                let mut probe = Probe {
                    inner,
                    found,
                    at,
                    slot: Slot::Field,
                    accepted,
                };
                consumer(member, &mut probe)
            };
            at.borrow_mut().pop();
            read
        };
        self.inner
            .read_struct(schema, &mut watched)
            .map_err(|error| note_in(found, at, schema.shape_type(), slot, error))
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.note_accepted(schema);
        let found = self.found;
        let accepted = self.accepted;
        let at = self.at;
        let slot = self.slot;
        // An element is named by no member: it stands INSIDE the collection, at the position
        // the body carries it — which is the order the elements are read in.
        let mut position = 0usize;
        let mut watched = |inner: &mut dyn ShapeDeserializer| {
            at.borrow_mut().push(Step::Element(position));
            position += 1;
            let read = {
                let mut probe = Probe {
                    inner,
                    found,
                    at,
                    slot: Slot::Inside,
                    accepted,
                };
                consumer(&mut probe)
            };
            at.borrow_mut().pop();
            read
        };
        self.inner
            .read_list(schema, &mut watched)
            .map_err(|error| note_in(found, at, schema.shape_type(), slot, error))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.note_accepted(schema);
        let found = self.found;
        let accepted = self.accepted;
        let at = self.at;
        let slot = self.slot;
        // An entry's value is named by its KEY rather than by a member the model declares,
        // so it too stands inside the collection rather than in a field.
        let mut watched = |key: String, inner: &mut dyn ShapeDeserializer| {
            at.borrow_mut().push(Step::Member(key.clone()));
            let read = {
                let mut probe = Probe {
                    inner,
                    found,
                    at,
                    slot: Slot::Inside,
                    accepted,
                };
                consumer(key, &mut probe)
            };
            at.borrow_mut().pop();
            read
        };
        self.inner
            .read_map(schema, &mut watched)
            .map_err(|error| note_in(found, at, schema.shape_type(), slot, error))
    }

    // Every value read, forwarded and watched. The list is the trait's own scalar surface,
    // each read beside the modelled type it is for.
    watched_reads! {
        read_boolean -> bool as Boolean,
        read_byte -> i8 as Byte,
        read_short -> i16 as Short,
        read_integer -> i32 as Integer,
        read_long -> i64 as Long,
        read_float -> f32 as Float,
        read_double -> f64 as Double,
        read_big_integer -> BigInteger as BigInteger,
        read_big_decimal -> BigDecimal as BigDecimal,
        read_string -> String as String,
        read_document -> Document as Document,
        read_timestamp -> DateTime as Timestamp,
    }

    /// A blob, read as the base64 TEXT this protocol carries one as.
    ///
    /// The value handed to this read is a string either way, and reading it as the text it is
    /// leaves the DECODING to be asked about on its own: the codec's refusal of text no bytes
    /// can be decoded from names no JSON class, so nothing above it could otherwise tell that
    /// failure from a value that was read and then broke a rule ([`note_base64`]). The
    /// decoder asked is the one the codec itself decodes a blob with, so this read admits
    /// exactly the texts the read it stands in for admits.
    fn read_blob(&mut self, schema: &Schema) -> Result<Blob, SerdeError> {
        let found = self.found;
        let at = self.at;
        let slot = self.slot;
        let text = self
            .inner
            .read_string(schema)
            .map_err(|error| note_in(found, at, ShapeType::Blob, slot, error))?;
        match ::aws_smithy_types::base64::decode(&text) {
            Ok(bytes) => Ok(Blob::new(bytes)),
            Err(undecodable) => {
                note_base64(found, &text);
                Err(SerdeError::custom(format!("invalid base64: {undecodable}")))
            }
        }
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

/// The `Message` member of the synthetic serialization-refusal shape.
static SERIALIZATION_EXCEPTION_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    MESSAGE_MEMBER,
    0,
);

/// The `message` member of that same shape: the slot the DESERIALIZER's own diagnosis of a
/// body is stated in ([`Prose::Diagnosis`]).
static SERIALIZATION_EXCEPTION_DIAGNOSIS: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    DIAGNOSIS_MEMBER,
    1,
);

/// The synthetic shape of a protocol-level serialization refusal: no model declares it and
/// no operation lists it, because it is the PROTOCOL's own error rather than one of this
/// service's.
static SERIALIZATION_EXCEPTION_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#SerializationException",
        "smithy.framework",
        "SerializationException",
    ),
    ShapeType::Structure,
    &[
        &SERIALIZATION_EXCEPTION_MESSAGE,
        &SERIALIZATION_EXCEPTION_DIAGNOSIS,
    ],
);

/// The payload of a serialization refusal: what the request could not be read as, in the
/// member that class of refusal states it in — and nothing at all for a body no value of
/// which could be named.
#[derive(Debug, Clone)]
struct SerializationException {
    prose: Option<Prose>,
}

impl SerializableStruct for SerializationException {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        let Some(prose) = self.prose.as_ref() else {
            return Ok(());
        };
        serializer.write_string(prose.member(), prose.stated())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_schema::codec::Codec;
    use ::aws_smithy_schema_server::json::JsonCodec;

    /// Read `body` into `T` through the same watching probe the request layer drives, and
    /// report what it would be refused with — the JSON codec alone, so the case is about the
    /// codec's refusal, the literal the body carries and nothing else.
    fn recorded<T: DeserializableStruct>(body: &str) -> Option<Refused> {
        let codec = JsonCodec::new();
        let mut deserializer = codec.create_deserializer(body.as_bytes());
        let found: RefCell<Option<Unreadable>> = RefCell::new(None);
        let at: Trail = RefCell::new(Vec::new());
        {
            let mut probe = Probe {
                inner: &mut deserializer,
                found: &found,
                at: &at,
                slot: Slot::Field,
                accepted: None,
            };
            if T::deserialize(&mut probe).is_ok() {
                return None;
            }
        }
        let unreadable = found.into_inner()?;
        let document = ::serde_json::from_slice::<Value>(body.as_bytes())
            .expect("every case reads as one JSON document");
        refusal(&as_literal(&document, unreadable))
    }

    /// The refusal that states `sentence`.
    fn stated(sentence: &str) -> Option<Refused> {
        Some(Refused::Stated(sentence.to_string()))
    }

    /// RULE: a value of a class the modelled type of its member admits no reading of earns
    /// the refusal of its defect — the conversion that could not be performed between two
    /// single values, naming the literal sent; the unexpected field or payload value where a
    /// single value stands in an aggregate; and the class alone, with no prose, where a
    /// collection stands in a member read from none — at whatever depth of the body it
    /// stands.
    ///
    /// This layer learns WHETHER a read failed on a value's JSON class from the text of the
    /// vendored codec's own refusal ("expected <type>, got <json type>", "number … does not
    /// fit in <width>"), which is the only place it is stated. The tree it comes from is
    /// read-only from here, so this case is also what makes a change to those wordings fail
    /// loudly instead of quietly sending every coercion failure back to the framework's
    /// `ValidationException`.
    #[test]
    fn the_codec_wordings_this_layer_reads() {
        use crate::ops::types::{CreateTableInput, ListTablesInput, TagResourceInput};

        // Every class, where the model declares a STRING: `CreateTable$TableName` targets
        // `TableArn` (`smithy_query(query="operation", operation="CreateTable")`). A
        // collection sent for it is not a conversion at all.
        let string_rows = [
            ("17", stated("NUMBER_VALUE cannot be converted to String")),
            ("17.5", stated("DECIMAL_VALUE cannot be converted to String")),
            // A fraction however the body spelled it, and a WHOLE number however large.
            ("1e-7", stated("DECIMAL_VALUE cannot be converted to String")),
            ("1e20", stated("NUMBER_VALUE cannot be converted to String")),
            ("true", stated("TRUE_VALUE cannot be converted to String")),
            ("false", stated("FALSE_VALUE cannot be converted to String")),
            (r#"["t"]"#, Some(Refused::Bare)),
            (r#"{"Name":"t"}"#, stated(STRUCTURE_OR_MAP)),
        ];
        for (sent, expected) in string_rows {
            let body = format!(r#"{{"TableName":{sent}}}"#);
            let stated = recorded::<CreateTableInput>(&body);
            assert_eq!(
                stated, expected,
                "case {sent:?}: expected the refusal {expected:?}, got {stated:?}"
            );
        }

        // A value nested inside a STRUCTURE, reached by the codec's own descent:
        // `ProvisionedThroughput$ReadCapacityUnits` targets `PositiveLongObject` (long).
        let nested = recorded::<CreateTableInput>(
            r#"{"TableName":"t","ProvisionedThroughput":{"ReadCapacityUnits":"5"}}"#,
        );
        assert_eq!(
            nested,
            stated("STRING_VALUE cannot be converted to Long"),
            "a string where a nested long is declared: got {nested:?}"
        );
        let flagged = recorded::<CreateTableInput>(
            r#"{"TableName":"t","ProvisionedThroughput":{"ReadCapacityUnits":true}}"#,
        );
        assert_eq!(
            flagged,
            stated("TRUE_VALUE cannot be converted to Long"),
            "a flag where a nested long is declared: got {flagged:?}"
        );

        // A value nested inside a LIST's element structure: `Tag$Key` targets `TagKeyString`
        // (string), and the element itself is a structure.
        let in_element = recorded::<TagResourceInput>(
            r#"{"ResourceArn":"arn","Tags":[{"Key":1,"Value":"v"}]}"#,
        );
        assert_eq!(
            in_element,
            stated("NUMBER_VALUE cannot be converted to String"),
            "a number where a list element's string member is declared: got {in_element:?}"
        );
        let in_second = recorded::<TagResourceInput>(
            r#"{"ResourceArn":"arn","Tags":[{"Key":"k","Value":"v"},{"Key":false,"Value":"v"}]}"#,
        );
        assert_eq!(
            in_second,
            stated("FALSE_VALUE cannot be converted to String"),
            "the literal is read at the element it stood at: got {in_second:?}"
        );
        let element = recorded::<TagResourceInput>(r#"{"ResourceArn":"arn","Tags":["k=v"]}"#);
        assert_eq!(
            element,
            stated(UNEXPECTED_VALUE_TYPE),
            "a scalar INSIDE a list of structures, which no member names: got {element:?}"
        );

        // A scalar where the model declares the LIST itself, and a number past the modelled
        // integral width (`ListTables$Limit` targets `ListTablesInputLimit`, an integer).
        let whole_list = recorded::<TagResourceInput>(r#"{"ResourceArn":"arn","Tags":"k=v"}"#);
        assert_eq!(
            whole_list,
            stated(UNEXPECTED_FIELD_TYPE),
            "a scalar where a list is declared: got {whole_list:?}"
        );
        let wide = recorded::<ListTablesInput>(r#"{"Limit":2147483648}"#);
        assert_eq!(
            wide,
            stated("NUMBER_VALUE cannot be converted to Integer"),
            "a number past the modelled integer width: got {wide:?}"
        );

        // And the reads that are NOT about a value's JSON class: an explicit null, and a
        // body whose every value reads. Neither is this layer's to answer.
        for body in [r#"{"TableName":null}"#, r#"{"TableName":"t","Limit":7}"#] {
            let stated = recorded::<CreateTableInput>(body);
            assert_eq!(
                stated, None,
                "case {body:?}: nothing here is about a value's JSON class, got {stated:?}"
            );
        }
    }

    /// RULE: a body is read as one complete JSON document, and the ONE refusal of the
    /// maintained reader that is not about the body's form — a document nesting deeper than
    /// that reader descends — is told apart from every other by the reader's own wording.
    ///
    /// The wording is the only place the reader states which refusal this is, so this case is
    /// what makes a change to it fail loudly instead of quietly turning a deeply nested value
    /// into a body-level refusal. The rows span the defects of a body's FORM against the
    /// nesting depth, on both sides of the reader's own bound.
    #[test]
    fn the_reader_wording_this_layer_reads() {
        // (case, the body, whether the reader stopped on the nesting alone)
        let deep = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
        let rows: Vec<(&str, String, bool)> = vec![
            ("bytes that are not JSON at all", "not json".to_string(), false),
            ("a truncated object", r#"{"Limit":"#.to_string(), false),
            (
                "content after a complete object",
                r#"{"Limit":1} and more"#.to_string(),
                false,
            ),
            ("nesting the reader descends", deep(32), false),
            ("nesting past what the reader descends", deep(200), true),
        ];
        for (case, body, expected) in rows {
            let stated = ::serde_json::from_slice::<Value>(body.as_bytes())
                .err()
                .as_ref()
                .map(nests_too_deep);
            assert_eq!(
                stated.unwrap_or(false),
                expected,
                "case {case:?}: expected nests_too_deep = {expected}, got {stated:?}"
            );
        }
    }
}
