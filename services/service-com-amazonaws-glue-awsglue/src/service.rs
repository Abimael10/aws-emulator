// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-glue-awsglue` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. Glue states it in AWS's own constraint envelope at the
//!   member paths Glue's own API spells; see [`glue_constraint_refusal`].
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.
//!
//! Below the seams, this file also holds the service-wide WORDING of the Data Catalog's
//! own refusal of a member value outside what the model admits ([`database_name_refusal`]
//! and the functions beside it): those members are judged by the operation that reads
//! them rather than by the framework boundary, and several operations declare the same
//! member, so the sentences live here once instead of once per operation. The OTHER
//! layer's refusal — the boundary's constraint envelope — is rendered here too
//! ([`glue_constraint_refusal`]), once for both paths that answer one.
//!
//! # The request a response names
//!
//! Every response AWS sends names the request it answered, and an SDK surfaces that name as
//! the call's response metadata — the value a caller correlates its own logs with the
//! service's by. This service's protocol (`awsJson1_1`) carries it in the `x-amzn-requestid`
//! response HEADER alone, which the shared report
//! (`service_common::query_error_envelope::with_request_id_header`) writes, wired at the two
//! layers every response of this service passes through:
//!
//! * the operation [`Service::override_operation`] hands the router — the catalog wrapper
//!   below under [`RequestIdReportingOperation::header`], and [`RequestIdHeaderOperation`]
//!   for every other modelled operation — which covers a success, a modelled error, a
//!   refusal this file words itself and a constraint refusal raised while an operation was
//!   being served;
//! * [`RequestIdEnvelopeDefinition::header_only`], the definition wrapper
//!   [`Service::definition`] hands back, which covers a response NO operation produced: a
//!   routing failure, a transport failure detected up front, and a pre-dispatch
//!   authentication refusal.
//!
//! Nothing else about a response changes: the status, the other headers and every byte of
//! the body stay as the renderer wrote them.

use service_common::query_error_envelope::{
    RequestIdEnvelopeDefinition, RequestIdHeaderOperation, RequestIdReportingOperation,
};
use smithy_server_runtime::{
    http, BoxFuture, ErasedOperation, ErasedServiceDefinition, Operation, OperationContext,
    Refusal, Refused, ServerHttpProtocol, Service, Violation, ViolationKind,
};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

/// The PARTITION INDEX records of the Data Catalog's tables: state this service keeps beside
/// the catalog itself, whose source file lives with the rest of this service's state
/// (`src/state/partition_indexes.rs`, beside [`crate::state::catalog_encryption`]).
///
/// It is DECLARED from this file because this is the file the service owns and which is never
/// regenerated: `src/ops/` holds one file per operation, so a module declared by a file there
/// is read as an operation this service's model does not have, and `src/state.rs` — the state
/// module root — is already at the length this project keeps a source file to.
#[path = "state/partition_indexes.rs"]
pub mod partition_indexes;

/// The rules the zero-ETL integration PROPERTY operations share: how each of them reads its
/// `ResourceArn`, and the sentences they refuse one with. Six operations address a record by
/// that member, so the reading and the wording live in one file rather than in six.
///
/// Declared from here for the same reason [`partition_indexes`] is, and its source file sits
/// with the rest of this service's state.
#[path = "state/integration_properties.rs"]
pub mod integration_properties;

/// The `com-amazonaws-glue-awsglue` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    /// The generated entry, with its three framework-level error renderers wrapped so a
    /// response no operation produced still names the request it answers. Identity, the
    /// protocol and the operations are the generated entry's untouched.
    entry: RequestIdEnvelopeDefinition<crate::ServiceDefinition<Eng>>,
    /// One [`GlueInputValidation`] per operation whose own input validation this service
    /// words itself, keyed by the routing key the model names that operation by
    /// ([`Operation::NAME`], the key the generated routing table uses). Built once here,
    /// never per request; each entry is zero-sized, so this is a table of NAMES rather
    /// than a second set of operations — the operation that serves a request is still the
    /// one the generated table built from the shared engine.
    ///
    /// Each wrapper is composed inside [`RequestIdReportingOperation::header`], so the
    /// refusal it words names the request it answered exactly as every other response of
    /// this service does.
    input_refusals: HashMap<&'static str, Box<dyn ErasedOperation>>,
    /// Serves every OTHER modelled operation exactly as the generated routing table would,
    /// and names the request its response answers. Held, not built per request:
    /// `override_operation` hands back a borrow of it.
    request_id: RequestIdHeaderOperation,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: RequestIdEnvelopeDefinition::header_only(crate::ServiceDefinition::new(engine)),
            input_refusals: glue_input_validation_table::<Eng>(),
            request_id: RequestIdHeaderOperation,
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry: what is
        // handed back is the shared `RequestIdEnvelopeDefinition`, which reads identity,
        // protocol and the routing table straight off that entry and adds the request-id
        // header to the three error renderers (see the module docs).
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // Serve the operations whose own input validation this service words itself
        // through their [`GlueInputValidation`] wrapper, so a request that leaves one of
        // those members ABSENT is refused in the operation's own modelled
        // `InvalidInputException` instead of the framework's refusal body — the same
        // answer an EMPTY name earns inside the handler, because the catalog's sentence
        // for the check covers both. The wrapper hands every request it does not refuse to
        // the generated operation in the routing table, which remains the only thing that
        // serves one.
        //
        // Keyed by the protocol's own routing key, so a request the model names no
        // operation for resolves to nothing here and keeps the framework's routing
        // failure.
        let key = self.entry.protocol().routing_key(req).ok()?;
        if let Some(wrapper) = self.input_refusals.get(key) {
            return Some(&**wrapper);
        }
        // NOT a re-route: every other request is served by the operation this service's
        // own routing table names for it, through the shared `RequestIdHeaderOperation` —
        // the one place every other operation's response passes, so no operation can
        // answer without naming the request it answered. A request the table does not
        // answer declines here exactly as it would there, leaving the router's
        // routing-error path (which names the request itself, through the definition
        // wrapper) unchanged.
        self.entry
            .routes(req)
            .then_some(&self.request_id as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this service
        // reword it. Glue states such a refusal in AWS's own constraint envelope, which
        // differs from the framework's default in two renderings this service does not
        // choose for itself ([`glue_constraint_refusal`]): the member path is the one
        // Glue's own API spells, and a `@pattern` is quoted without the anchoring pair
        // the model wraps it in. Both the code and the message come from there, so a
        // client never sees one layer's message under the other layer's code.
        glue_constraint_refusal(refusal)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}

// ---------------------------------------------------------------------------
// How Glue states the boundary's constraint refusal
// ---------------------------------------------------------------------------

/// State `refusal` the way Glue states a constraint refusal, or `None` to keep the
/// framework's own wording.
///
/// The envelope is AWS's own and belongs to the shared renderer
/// (`service_common::validation_message::aws_validation_refusal`): HTTP 400
/// `ValidationException`, whose lowercase `message` carries
/// `N validation error(s) detected: Value '<value>' at '<member path>' failed to satisfy
/// constraint: <requirement>`, with a `@pattern` requirement quoted WITHOUT the anchoring
/// pair the model wraps it in, which is the text AWS publishes for it. Two things this
/// service adds, both in [`as_glue_clause`]: the MEMBER PATH, because Glue states the path
/// of its own request model ([`api_request_path`]), which spells the member carrying a
/// resource definition for the resource it carries; and the ORDER an enumerated type's
/// admissible value set is listed in, which is the order the layer in front of this service
/// enumerates its own table of those values. Everything else — which refusals are restated,
/// the code, the envelope, the clause order and every other requirement — stays the shared
/// renderer's and the framework's, so two Glue refusal paths cannot drift apart.
///
/// Both paths that answer a boundary refusal in this service go through here: this
/// service's [`Service::override_refusal`], and the pre-handler wrapper at the end of this
/// file, which additionally restores each clause's echoed value to the bytes the caller
/// sent. Only the wrapper can do that — the refusal seam is handed the violations and not
/// the request — so a clause stated through `override_refusal` carries the framework's own
/// elision of a value past its echo ceiling.
fn glue_constraint_refusal(refusal: &Refusal) -> Option<Refused> {
    ::service_common::validation_message::aws_validation_refusal(&as_glue_states_it(refusal))
}

/// [`glue_constraint_refusal`] for a caller holding the operation's INPUT SHAPE as well as
/// the violations: the same refusal, with each clause's value half additionally written the
/// way the member's modelled TYPE is written ([`in_floating_point_form`]).
///
/// The extra rendering needs the shape and the refusal seam is handed neither — only the
/// violations — so the pre-handler wrapper at the end of this file, which knows the
/// operation it serves, is the path that states it. Everything else is
/// [`glue_constraint_refusal`]'s.
fn glue_constraint_refusal_of(
    input_schema: &'static ::aws_smithy_schema::Schema,
    violations: &[Violation],
) -> Option<Refused> {
    let stated: Vec<Violation> = in_floating_point_form(input_schema, violations);
    glue_constraint_refusal(&Refusal::Invalid(stated))
}

/// [`glue_constraint_refusal_of`], with the value half of every clause DROPPED — the
/// rendering of an operation whose recorded clauses name the member and the requirement and
/// quote no value ([`JSON_ENVELOPE_OPERATIONS`]).
///
/// The drop is the shared renderer's own
/// (`service_common::validation_message::aws_validation_refusal_withholding`), so a clause
/// that states no value reads exactly like one that does minus that half.
fn glue_constraint_refusal_withholding(
    input_schema: &'static ::aws_smithy_schema::Schema,
    violations: &[Violation],
) -> Option<Refused> {
    let stated = in_floating_point_form(input_schema, violations);
    ::service_common::validation_message::aws_validation_refusal_withholding(
        &as_glue_states_it(&Refusal::Invalid(stated)),
        |_| true,
    )
}

/// `refusal` with every violation stated the way Glue states one ([`as_glue_clause`]).
fn as_glue_states_it(refusal: &Refusal) -> Refusal {
    match refusal {
        Refusal::Invalid(violations) => {
            Refusal::Invalid(violations.iter().map(as_glue_clause).collect())
        }
        other => other.clone(),
    }
}

/// One violation as Glue's own clause states it: at the member path Glue's API spells
/// ([`api_request_path`]), and with an enumerated type's admissible value set listed in the
/// order the layer in front of this service enumerates that set
/// (`service_common::validation_message::in_hashed_enum_order` — the order of its own hash
/// table over the value texts, which is neither the model's declaration order nor
/// alphabetical).
///
/// The clause, the requirement and the envelope stay the shared renderer's and the
/// framework's: only the member path and the ORDER of a value set move, so every Glue
/// refusal path words one constraint identically.
fn as_glue_clause(violation: &Violation) -> Violation {
    let stated = ::service_common::validation_message::in_hashed_enum_order(violation);
    Violation {
        path: api_request_path(&stated.path),
        ..stated
    }
}

/// `violations` with the value half of each clause about a FLOATING-POINT member of
/// `input_schema` written the way that type is written — a whole `150` as `150.0` — rather
/// than as the decimal expansion the framework's numeric rendering produces.
///
/// WHICH members are floating point is read off the MODEL
/// (`service_common::validation_message::floating_point_member_paths` over the operation's
/// own input shape), so a member added to the model reaches this with the model and nothing
/// is named by hand. The rendering itself is the shared one
/// (`service_common::validation_message::in_float_value_rendering`), so two services cannot
/// quote one double differently.
///
/// Read only when there is something to state, so a request that satisfies the model pays
/// nothing for it.
fn in_floating_point_form(
    input_schema: &'static ::aws_smithy_schema::Schema,
    violations: &[Violation],
) -> Vec<Violation> {
    let floating = ::service_common::validation_message::floating_point_member_paths(&[(
        "",
        input_schema,
    )]);
    violations
        .iter()
        .map(|violation| {
            ::service_common::validation_message::in_float_value_rendering(violation, |path| {
                floating.contains(path)
            })
        })
        .collect()
}

/// The suffix the WIRE spells a member carrying a resource definition with
/// (`CreateDatabaseRequest$DatabaseInput`, `CreateTableRequest$TableInput`), which Glue's
/// own request path does not carry.
const DEFINITION_MEMBER_SUFFIX: &str = "Input";

/// `path` as Glue's own API spells the member it names: a member that carries a RESOURCE
/// DEFINITION is named for the resource it carries, so `databaseInput.name` is stated as
/// `database.name` and the member a request addresses a resource by (`name`,
/// `databaseName`) is stated exactly as the framework's walk spells it.
///
/// A constraint message is the only place the server names the member a caller got wrong,
/// so the path is part of the rule rather than prose: a client that reads it to find its
/// own field has to be given a member its request model declares. Only the FIRST segment
/// is rewritten, because that is the one naming a member of the request itself; every
/// segment under it names a member of the definition, which both models spell the same
/// way.
fn api_request_path(path: &str) -> String {
    let mut segments = path.splitn(2, '.');
    let Some(member) = segments.next() else {
        return path.to_string();
    };
    let stated = member
        .strip_suffix(DEFINITION_MEMBER_SUFFIX)
        .filter(|resource| !resource.is_empty())
        .unwrap_or(member);
    match segments.next() {
        Some(under) => format!("{stated}.{under}"),
        None => stated.to_string(),
    }
}

// ---------------------------------------------------------------------------
// How the Data Catalog refuses an input value its own validation rejects
// ---------------------------------------------------------------------------
//
// Two layers refuse a Glue request, and the recordings say which one answers what. The
// cross-service constraint validator answers a value past a `@length` MAXIMUM or outside
// a `@range` — HTTP 400 `ValidationException`, whose lowercase `message` carries the
// "N validation error(s) detected: …" envelope — which this service states in AWS's own
// rendering of it ([`glue_constraint_refusal`]). Everything else below is GLUE's own input
// validation: a value outside an enum's value set, an ARN outside `GlueResourceArn`'s
// pattern, and a required name member supplied as null or empty. Each of those is the
// operation's own modelled `InvalidInputException` (HTTP 400, prose in the `Message`
// member the model declares), carrying the sentence this service words that check with.
//
// So the members Glue judges itself are taken off the centralized boundary walk
// (`skip_validation` on their input shapes in `src/ops/types/`) and judged by the
// operation that read them — except for the one refusal no handler can reach, which the
// wrapper at the end of this file answers.
//
// The SENTENCES live here, once, because several operations declare the same member: a
// refusal `GetTable` makes has to read exactly like the one `CreateTable` makes.

/// The sentence the catalog refuses a resource's OWN name member supplied as null or
/// empty with, for a database (`DatabaseInput$Name`, and the `Name` a database write
/// addresses).
///
/// RECORDED constant, and the whole message: it states both halves of the check at once
/// and names neither the operation nor a value — there is no value to name.
const DATABASE_NAME_NULL_OR_EMPTY: &str = "Database name cannot be equal to null or empty";

/// The same for a table's own name member (`TableInput$Name`, and the `Name` a table read
/// or write addresses). RECORDED constant, and the whole message.
const TABLE_NAME_NULL_OR_EMPTY: &str = "Table name cannot be equal to null or empty";

/// The sentence the catalog refuses a `DatabaseName` — the member that addresses the
/// database a table operation works IN, rather than a database's own name — supplied as
/// null or empty with.
///
/// RECORDED constant, and a DIFFERENT sentence from [`DATABASE_NAME_NULL_OR_EMPTY`]: the
/// catalog states the parent reference as a requirement and a resource's own name as a
/// value it will not take, so the two are not one shared string.
const DATABASE_NAME_REQUIRED: &str = "Database name is required";

/// The sentence the catalog refuses a `ResourceShareType` outside the enum its member
/// targets with.
///
/// RECORDED template: fixed text plus ONE variable part, the value the request sent,
/// echoed exactly as sent — so a client reading the refusal sees which of its own values
/// the service would not take.
pub fn resource_share_type_not_valid(submitted: &str) -> String {
    format!("Resource share type {submitted} is not valid")
}

/// The sentence the catalog refuses an `AttributesToGet` entry that is no member of the
/// attribute enum its list targets with.
///
/// RECORDED constant, and the whole message: it names neither the entry the request sent
/// nor the set it had to come from, so one wording answers every such entry.
pub const ATTRIBUTE_NOT_VALID_TO_GET: &str = "Attribute name is not a valid attribute to get.";

/// The sentence the catalog refuses an `AttributesToGet` that does not name `NAME` with.
///
/// RECORDED constant, and the whole message: it names neither the attributes the request
/// sent nor the combinations the operation admits, so ONE wording answers every such list.
/// Every projected listing of a catalog resource shares it, because `NAME` is the
/// attribute each of them documents as required ("The request must include the `NAME`").
pub const ATTRIBUTES_TO_GET_MUST_CONTAIN_NAME: &str = "AttributesToGet must contain NAME.";

/// The sentence the catalog refuses an `AttributesToGet` supplied as the empty list with —
/// the list the member "doesn't accept".
///
/// RECORDED constant, and the whole message: it echoes nothing of the request, and names
/// the member the way the catalog's own validator spells it, so one wording answers the
/// empty list of every projected read.
pub const ATTRIBUTES_TO_GET_CANNOT_BE_EMPTY: &str = "If provided, attributesToGet cannot be empty.";

/// Whether `attributes` carries an entry that is no member of `admitted` — the
/// generated, model-derived value set of the enum the list's member targets
/// (`crate::ops::types::TABLE_ATTRIBUTES_VALUES` and its siblings), so a value added to
/// the model reaches this check with the model.
pub fn attribute_outside_set(attributes: &[String], admitted: &[&str]) -> bool {
    attributes
        .iter()
        .any(|attribute| !admitted.contains(&attribute.as_str()))
}

// ---------------------------------------------------------------------------
// The `@pattern` the framework boundary cannot apply
// ---------------------------------------------------------------------------

/// The expression `HashString` and `NameString` — the shapes nearly every Glue id and name
/// member targets — declare, as the model writes it
/// (`smithy_query(query="shape",shape="HashString")`).
pub const HASH_STRING_PATTERN: &str =
    r"^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$";

/// Whether `value` satisfies [`HASH_STRING_PATTERN`].
///
/// The expression admits a tab and the three Unicode ranges it spells: U+0020..U+D7FF,
/// U+E000..U+FFFD, and the astral planes, which it writes as the UTF-16 surrogate pair
/// range `\uD800\uDC00-\uDBFF\uDFFF` and which a Rust `char` holds directly
/// (U+10000..U+10FFFF). So every other character is outside it: a control character below
/// U+0020 other than a tab — a newline, a carriage return — and the two non-characters
/// U+FFFE and U+FFFF.
///
/// It is checked HERE, in the operation that reads the member, because the boundary
/// validator cannot: the framework's regex engine does not compile an expression spelling a
/// surrogate range, and a pattern it cannot compile admits every value
/// (`vendor/smithy-schema-server/src/constraints.rs`).
#[must_use]
pub fn is_modelled_hash_string(value: &str) -> bool {
    value.chars().all(|character| {
        matches!(character,
            '\t' | '\u{0020}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
    })
}

/// The constraint refusal a value outside [`HASH_STRING_PATTERN`] earns at `member_path`,
/// or `None` for a value the expression admits.
///
/// AWS's own validation-error envelope over the one clause that fired, naming the value the
/// caller submitted, the member path it was sent at and the expression it had to satisfy —
/// built through [`glue_constraint_refusal`], so a clause this service states itself reads
/// exactly like one the boundary states for a pattern it CAN compile.
///
/// Which error a handler carries it in is the operation's own: the recordings show the
/// operations modelling `InvalidInputException` answering that, and the ones that do not
/// answering the framework's `ValidationException` channel.
#[must_use]
pub fn hash_string_pattern_refusal(member_path: &str, value: &str) -> Option<String> {
    if is_modelled_hash_string(value) {
        return None;
    }
    glue_constraint_refusal(&Refusal::Invalid(vec![Violation {
        path: member_path.to_string(),
        kind: ViolationKind::Pattern(HASH_STRING_PATTERN.to_string()),
        value: Some(value.to_string()),
        sensitive: false,
    }]))
    .map(|refused| refused.message)
}

// ---------------------------------------------------------------------------
// The members Glue states a requirement for in its own error
// ---------------------------------------------------------------------------
//
// The same layering as the catalog's name members above, over the OTHER members whose
// requirement Glue states itself: a request that carries no value at all for one of them —
// the member absent, or a collection member supplied EMPTY, which is the same "you gave me
// none" to the service — is the operation's own modelled `InvalidInputException` carrying
// Glue's sentence for that check, never the boundary's `ValidationException`.

/// The sentence `PutDataCatalogExportConfiguration` states its `ExportSetting` requirement
/// with.
///
/// RECORDED constant, and nothing in it comes from the request. The JSON envelope the
/// recording shows around it is not part of the sentence: that envelope is this operation's
/// own rendering of EVERY refusal of its input ([`JSON_ENVELOPE_OPERATIONS`]), so it is
/// applied in one place rather than baked into one message.
const EXPORT_SETTING_REQUIRED: &str = "ExportSetting is required.";

/// The sentence `StartDataQualityRulesetEvaluationRun` states its `RulesetNames`
/// requirement with.
///
/// RECORDED constant, and the whole message: it states the requirement as a count rather
/// than naming the member, so one wording answers both an absent list and an empty one —
/// which is exactly the pair of violations this check covers.
const AT_LEAST_ONE_RULESET_NAME: &str = "At least one ruleset name must be provided.";

/// The operations whose API states a missing required member as `<Member> is required for
/// <Operation>` ([`member_required_for`]): the TABLE OPTIMIZER family, every operation of
/// which addresses an optimizer by the same four `@required` members
/// (`CatalogId`, `DatabaseName`, `TableName`, `Type`).
///
/// The sentence names the member and the operation and nothing else, so it covers every
/// required member of every operation in the family rather than the one member a recording
/// happened to omit.
const MEMBER_REQUIRED_OPERATIONS: [&str; 5] = [
    "CreateTableOptimizer",
    "DeleteTableOptimizer",
    "GetTableOptimizer",
    "ListTableOptimizerRuns",
    "UpdateTableOptimizer",
];

/// The sentence [`MEMBER_REQUIRED_OPERATIONS`] state a requirement with: the member as the
/// MODEL spells it, then the operation that requires it.
///
/// RECORDED template: fixed text plus two variable parts, both read off the request's own
/// model — the member the request left out and the operation it called.
fn member_required_for(member: &str, operation: &str) -> String {
    format!("{member} is required for {operation}")
}

/// A TOP-LEVEL violation path as the MODEL spells the member it names: the path's own
/// spelling with its first letter upper-cased, which is the inverse of the lowerCamel casing
/// the framework's walk applies to a member name
/// (`aws_smithy_schema_server::constraints::path::member`).
///
/// `None` for a path naming a member INSIDE another member: every segment under the first
/// is a member of a definition shape, and re-casing only the first letter of a dotted path
/// would spell half of it as the model does and half as the walk does. A nested omission is
/// therefore left to the boundary, which spells the whole path itself.
fn as_modelled_member(path: &str) -> Option<String> {
    if path.contains('.') {
        return None;
    }
    let mut characters = path.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}

// ---------------------------------------------------------------------------
// The refusal the framework boundary would answer instead
// ---------------------------------------------------------------------------
//
// A member supplied as null or empty is GLUE's refusal, not the constraint validator's, but
// neither half of it can be made inside a handler: an ABSENT member is
// found while the request is being READ, so no input value ever exists to judge (the
// model makes such a member non-`Option` on the generated input struct), and an EMPTY one
// breaks the member's modelled `@length` minimum, which the boundary walk answers before
// any handler runs. Taking the member off that walk is not the answer either, because the
// SAME `@length` carries the maximum the validator does answer. So the wrapper below runs
// the framework's own boundary walk first, answers exactly those two violations in the
// operation's own error, and leaves every other violation — the `@length` maximum included
// — to the boundary.

/// `InvalidInputException`'s wire error code and its modelled `@httpError`
/// (`smithy_query(query="shape",shape="InvalidInputException")`), restated here because
/// this file renders that error itself rather than returning it from a handler.
const INVALID_INPUT_EXCEPTION: &str = "InvalidInputException";

/// See [`INVALID_INPUT_EXCEPTION`].
const INVALID_INPUT_EXCEPTION_STATUS: u16 = 400;

/// The status the constraint validator's refusal carries, which this file renders itself
/// for a wrapped operation — the same 400 the framework's own boundary answers with
/// (`crate::scaffolded::Scaffolded::ValidationException`).
const VALIDATION_EXCEPTION_STATUS: u16 = 400;

/// The operations whose own input validation Glue words itself, each wrapped so a member
/// the request supplied no value for is answered in the operation's own modelled error, and
/// so a constraint clause quotes the value the request carried in its modelled form.
///
/// Exactly the operations the sentences above word a refusal for: one added to that set is
/// wrapped by adding one line here, and one that judges no member of its own is served
/// straight from the generated routing table.
fn glue_input_validation_table<Eng: Send + Sync + 'static>(
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut refusals: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    register_input_validation::<crate::ops::create_database::CreateDatabase<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::update_database::UpdateDatabase<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::get_database::GetDatabase<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::delete_database::DeleteDatabase<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::create_table::CreateTable<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::update_table::UpdateTable<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::get_table::GetTable<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::get_tables::GetTables<Eng>>(&mut refusals);
    register_input_validation::<crate::ops::delete_table::DeleteTable<Eng>>(&mut refusals);
    // The TABLE OPTIMIZER family, whose API states every required member it was given none
    // of as `<Member> is required for <Operation>` ([`MEMBER_REQUIRED_OPERATIONS`]).
    register_input_validation::<crate::ops::create_table_optimizer::CreateTableOptimizer<Eng>>(
        &mut refusals,
    );
    register_input_validation::<crate::ops::delete_table_optimizer::DeleteTableOptimizer<Eng>>(
        &mut refusals,
    );
    register_input_validation::<crate::ops::get_table_optimizer::GetTableOptimizer<Eng>>(
        &mut refusals,
    );
    register_input_validation::<crate::ops::list_table_optimizer_runs::ListTableOptimizerRuns<Eng>>(
        &mut refusals,
    );
    register_input_validation::<crate::ops::update_table_optimizer::UpdateTableOptimizer<Eng>>(
        &mut refusals,
    );
    // The catalog's export configuration and the data quality run start, whose own
    // requirement sentences sit beside the family's above.
    register_input_validation::<
        crate::ops::put_data_catalog_export_configuration::PutDataCatalogExportConfiguration<Eng>,
    >(&mut refusals);
    register_input_validation::<
        crate::ops::start_data_quality_ruleset_evaluation_run::StartDataQualityRulesetEvaluationRun<
            Eng,
        >,
    >(&mut refusals);
    // Judges no member of its own: wrapped so a clause about its FLOATING-POINT member
    // quotes the submitted value in that member's modelled form
    // ([`in_floating_point_form`]), which the refusal seam cannot do.
    register_input_validation::<
        crate::ops::update_column_statistics_task_settings::UpdateColumnStatisticsTaskSettings<Eng>,
    >(&mut refusals);
    refusals
}

/// Register one operation's wrapper under the key the model names that operation by, read
/// off the operation itself so the table cannot be keyed by a hand-typed name that drifts
/// from the model.
///
/// The wrapper is composed inside the shared request-id report, so the refusal it words
/// names the request it answered exactly as the generated operation's own answers do.
fn register_input_validation<Op: Operation>(
    refusals: &mut HashMap<&'static str, Box<dyn ErasedOperation>>,
) {
    refusals.insert(
        Op::NAME,
        Box::new(RequestIdReportingOperation::header(
            GlueInputValidation::<Op>::new(),
        )),
    );
}

/// One modelled operation, served so that a request supplying no value for one of the
/// members whose requirement Glue states itself is answered in the operation's own modelled
/// `InvalidInputException` rather than in the framework's refusal body, which carries an
/// error code and a lowercase `message` and nothing else
/// (`smithy_server_runtime::Refused`) — and so that a constraint clause about any OTHER
/// member of it quotes the submitted value in that member's own modelled form.
///
/// Zero-sized: the wrapper holds no engine and no operation instance. It carries the
/// operation TYPE, which is all it needs — the input shape to read the request into and
/// the input `Schema` to read it against — and hands a request it does not refuse to the
/// one operation instance the generated routing table built from the shared engine. There
/// is therefore no second instance of any operation and no second store.
///
/// A request this wrapper does not refuse is READ TWICE, once here and once in the
/// operation's own sequence, because the framework hands no already-built input to an
/// operation. That cost is deliberate, bounded by the body limits the framework already
/// enforces, and paid only by the few operations the table above names.
struct GlueInputValidation<Op> {
    operation: PhantomData<fn() -> Op>,
}

impl<Op> GlueInputValidation<Op> {
    const fn new() -> Self {
        Self {
            operation: PhantomData,
        }
    }
}

impl<Op: Operation> ErasedOperation for GlueInputValidation<Op> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            // The protocol is read, used and dropped inside this one block:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the await
            // below would make this future non-`Send`.
            let refused = {
                let protocol: &dyn ServerHttpProtocol = svc.definition().protocol();
                // The framework's OWN boundary walk, collected through the shared half
                // every service that words its own pre-handler refusal uses — the
                // protocol's deserializer plus `constraints::validate` over the built
                // input — so this cannot find a different violation set from the sequence
                // that follows it. It additionally restores each clause's echoed value to
                // the bytes the caller sent, which only this side of the walk can do: the
                // framework's own rendering elides a long value, and AWS quotes it whole.
                let violations =
                    ::service_common::validation::refusing_violations::<Op>(protocol, req);
                match stated_refusal::<Op>(&violations) {
                    // Glue's own refusal, in the operation's own modelled error: a member
                    // it was given no value for, or — for an operation that reports every
                    // refusal of its input that way — the constraint envelope inside that
                    // operation's JSON envelope.
                    Some(StatedRefusal::Modelled(message)) => serialized_refusal::<Op>(
                        protocol,
                        &crate::ops::types::InvalidInputException {
                            message: Some(message),
                            from_federation_source: None,
                        },
                        &crate::ops::types::SCHEMA_INVALID_INPUT_EXCEPTION,
                        INVALID_INPUT_EXCEPTION,
                        INVALID_INPUT_EXCEPTION_STATUS,
                    ),
                    // Every other violation is the constraint validator's, and is stated
                    // the way AWS states one: the envelope and clauses the shared renderer
                    // owns, over the restored values above, at the member paths Glue's own
                    // API spells and in the member's own modelled form
                    // ([`glue_constraint_refusal_of`]).
                    Some(StatedRefusal::Constraint { code, message }) => {
                        serialized_refusal::<Op>(
                            protocol,
                            &crate::scaffolded::ValidationException::new(message),
                            &crate::scaffolded::VALIDATION_EXCEPTION_SCHEMA,
                            &code,
                            VALIDATION_EXCEPTION_STATUS,
                        )
                    }
                    None => None,
                }
            };
            if let Some(response) = refused {
                // The row the framework sequence would have recorded for this request, so
                // a refused one is not a hole in the telemetry series.
                record_input_refusal::<Op>(ctx, started.elapsed());
                return response;
            }
            // Nothing to state: the request is served by the generated operation, which
            // runs the whole framework sequence (deserialize, validate, handle, serialize)
            // plus its telemetry over the same bytes. Reached through the routing table
            // rather than through `override_operation`, which would route straight back
            // here.
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the key
                // the generated table registered. Answer the framework's own
                // no-such-operation response rather than inventing one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// `None` — the framework reads the schemes off the MODELLED operation the request's
    /// routing key names (the generated table's instance), which is what the model bakes
    /// them on, so wrapping an operation cannot change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

/// Serialize one refusal in the protocol the request is served in, or `None` when it will
/// not serialize — which leaves the request to the framework, refused in the framework's
/// own envelope. That is the very divergence this wrapper removes, so it is logged rather
/// than passed over silently.
fn serialized_refusal<Op: Operation>(
    protocol: &dyn ServerHttpProtocol,
    error: &dyn ::aws_smithy_schema::serde::SerializableStruct,
    error_schema: &::aws_smithy_schema::Schema,
    error_code: &str,
    http_status: u16,
) -> Option<aws_smithy_runtime_api::http::Response> {
    match protocol.serialize_error(error, error_schema, error_code, http_status) {
        Ok(response) => Some(response),
        Err(error) => {
            ::tracing::warn!(
                operation = Op::NAME,
                %error,
                "this service's own refusal would not serialize; it falls back to the \
                 framework's own envelope"
            );
            None
        }
    }
}

/// Glue's own sentence for a request that supplied NO VALUE for one of `operation`'s
/// members whose requirement Glue states itself, or `None` for every other violation set —
/// which keeps the constraint validator's envelope.
///
/// Only a member the request gave no value for is answered here: absent, or a string or
/// collection supplied empty, which the service reads as the same omission
/// ([`supplies_no_value`]). A violation of any other class — a value PAST a member's
/// modelled maximum, a value outside a `@range` or an enum's value set — is left to the
/// boundary, which answers it in the constraint envelope the recordings show the validator
/// answering.
fn supplies_no_value_message(operation: &str, violations: &[Violation]) -> Option<String> {
    // Every violation of the set has to be one of these, so a request that ALSO broke a
    // constraint the validator answers keeps the validator's envelope rather than losing
    // that clause to this sentence. The first one states the refusal, as a service that
    // checks its members one at a time states it.
    if violations.is_empty() || !violations.iter().all(supplies_no_value) {
        return None;
    }
    violations
        .iter()
        .find_map(|violation| required_member_message(operation, &violation.path))
}

/// The operations that state every refusal the PRE-HANDLER walk words for them in their own
/// modelled `InvalidInputException`, whose `Message` carries the refusal text inside a JSON
/// object of one member ([`json_enveloped`]) — and whose constraint clauses therefore
/// WITHHOLD the value the request submitted, which is what the recorded clauses of this
/// surface do.
///
/// RECORDED, per operation: the envelope is a property of the surface in front of the
/// operation and not of the constraint that fired, so a requirement this service states and
/// a modelled constraint the validator states reach a caller the same way.
const JSON_ENVELOPE_OPERATIONS: [&str; 1] = ["PutDataCatalogExportConfiguration"];

/// `text` inside the JSON object [`JSON_ENVELOPE_OPERATIONS`] carry their refusals in.
///
/// Built with the JSON serializer rather than by formatting braces, so a sentence carrying a
/// quote or a backslash — a clause quoting a caller's own value — is escaped as JSON
/// requires and the envelope stays one well-formed object.
fn json_enveloped(text: &str) -> String {
    ::serde_json::json!({ "Message": text }).to_string()
}

/// How this service states ONE pre-handler refusal: in which error, and with what text.
enum StatedRefusal {
    /// The operation's own modelled `InvalidInputException` (HTTP 400, prose in the
    /// `Message` member the model declares).
    Modelled(String),
    /// The constraint validator's envelope, under the error code the shared renderer states
    /// for it.
    Constraint { code: String, message: String },
}

/// How `Op` states the refusal `violations` carry, or `None` when there is nothing to state
/// — which leaves the request to the operation itself.
///
/// The one place the two layers are chosen between, so a wrapped operation cannot word one
/// of them differently from another: Glue's own requirement sentence when the request
/// supplied no value for a member Glue states one for, else the constraint envelope; and,
/// for an operation that reports every refusal of its input as its own error
/// ([`JSON_ENVELOPE_OPERATIONS`]), either of those inside that operation's JSON envelope.
fn stated_refusal<Op: Operation>(violations: &[Violation]) -> Option<StatedRefusal> {
    let own = supplies_no_value_message(Op::NAME, violations);
    if JSON_ENVELOPE_OPERATIONS.contains(&Op::NAME) {
        let text = match own {
            Some(sentence) => sentence,
            // The value half is dropped: this surface's recorded constraint clauses name
            // the member and the requirement and quote no value at all.
            None => {
                glue_constraint_refusal_withholding(Op::INPUT_SCHEMA, violations)?.message
            }
        };
        return Some(StatedRefusal::Modelled(json_enveloped(&text)));
    }
    match own {
        Some(sentence) => Some(StatedRefusal::Modelled(sentence)),
        None => {
            let stated = glue_constraint_refusal_of(Op::INPUT_SCHEMA, violations)?;
            Some(StatedRefusal::Constraint {
                code: stated.code,
                message: stated.message,
            })
        }
    }
}

/// Whether this violation says the request supplied NO value for the member: it carried
/// none at all, or carried one the member's modelled `@length` MINIMUM rules out as empty —
/// the empty string for a string member, and the empty list or map for a collection one,
/// which the framework's walk renders as `0 items` / `0 entries`
/// (`service_common::validation_message::list_item_count` and its sibling read those
/// renderings, so the two layers cannot disagree about which is which).
///
/// A value that merely breaks a different bound is not this refusal, so nothing else is
/// folded into it.
fn supplies_no_value(violation: &Violation) -> bool {
    match violation.kind {
        ViolationKind::MissingRequiredMember => true,
        ViolationKind::MinLength(_) => violation.value.as_deref().is_some_and(|stored| {
            stored.is_empty()
                || ::service_common::validation_message::list_item_count(stored) == Some(0)
                || ::service_common::validation_message::map_entry_count(stored) == Some(0)
        }),
        _ => false,
    }
}

/// Glue's sentence for the member at `path` of `operation` that the request supplied no
/// value for, or `None` when Glue states no requirement of its own for that member — which
/// leaves the refusal to the boundary.
///
/// The member paths are the ones the model declares, spelled as the framework spells a
/// violation path (lowerCamel, a nested member under its enclosing member).
fn required_member_message(operation: &str, path: &str) -> Option<String> {
    match (operation, path) {
        ("PutDataCatalogExportConfiguration", "exportSetting") => {
            Some(EXPORT_SETTING_REQUIRED.to_string())
        }
        ("StartDataQualityRulesetEvaluationRun", "rulesetNames") => {
            Some(AT_LEAST_ONE_RULESET_NAME.to_string())
        }
        _ if MEMBER_REQUIRED_OPERATIONS.contains(&operation) => {
            Some(member_required_for(&as_modelled_member(path)?, operation))
        }
        // The CATALOG's own name sentences, which several operations share and which are
        // asked last: an operation stating its own requirement above states that one, and
        // the catalog's `databaseName` is not every surface's `DatabaseName`.
        _ => name_refusal_message(operation, path).map(str::to_string),
    }
}

/// The catalog's sentence for the name member at `path` of `operation`, or `None` when
/// `path` is no name member of the catalog's databases and tables.
///
/// The member paths are the ones the model declares, spelled as the framework spells a
/// violation path (lowerCamel, a nested member under its enclosing member). Which sentence
/// a member takes is a property of what it names, not of the operation that carries it: a
/// resource's OWN name member is the one the catalog will not take null or empty, the
/// `DatabaseName` that addresses the database an operation works in is the one it states as
/// a requirement, and a bare `Name` is the own name of the resource that operation
/// addresses — which is what the operation's name says it is.
fn name_refusal_message(operation: &str, path: &str) -> Option<&'static str> {
    match path {
        "databaseName" => Some(DATABASE_NAME_REQUIRED),
        "databaseInput.name" => Some(DATABASE_NAME_NULL_OR_EMPTY),
        "tableInput.name" => Some(TABLE_NAME_NULL_OR_EMPTY),
        "name" if operation.ends_with("Database") => Some(DATABASE_NAME_NULL_OR_EMPTY),
        "name" if operation.ends_with("Table") => Some(TABLE_NAME_NULL_OR_EMPTY),
        _ => None,
    }
}

/// Record the refusal answered above as the operation row the framework sequence would
/// have recorded, so a request refused by the wrapper is not a hole in the telemetry
/// series: the same classification (from the refusal's status and error shape), the same
/// folded handle (this service's signing name and the operation's own name), and the same
/// already-sanitized request metadata. Everything is gated on a client collecting, exactly
/// as the framework gates it.
fn record_input_refusal<Op: Operation>(ctx: &OperationContext<'_>, latency: std::time::Duration) {
    if !emulator_telemetry::recording_active() {
        return;
    }
    let handle =
        emulator_telemetry::OperationHandle::folded(ctx.identity().signing_name_static(), Op::NAME);
    emulator_telemetry::record_generated_operation_with_handle(
        &handle,
        latency,
        Some(emulator_telemetry::OperationFailure::classify(
            INVALID_INPUT_EXCEPTION_STATUS,
            INVALID_INPUT_EXCEPTION,
        )),
        smithy_server_runtime::RequestTelemetry::from_headers(&ctx.parts().headers).into_metadata(),
    );
}
