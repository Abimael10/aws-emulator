// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-ecr-amazonec2containerregistry-v20150921` service: how it claims requests and how it words the
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
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
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
//! ## The layer download plane
//!
//! `GetDownloadUrlForLayer` hands out a pre-signed download URL, and such a URL is a
//! promise that whoever holds it can FETCH the layer: it names the endpoint the CALLER
//! reached this emulator on (`crate::state::layer_download_url_host`), under the path
//! space `crate::state::LAYER_DOWNLOAD_PATH_PREFIX` owns, which names no modelled
//! operation — every operation of this service is `POST /` on awsJson1_1. Two seams
//! carry it, the same pair any emulated data plane uses:
//!
//! - [`Service::can_handle`] also claims a request to that path space, so a fetch that
//!   carries no `Authorization` header and no host naming a service — which is how a
//!   pre-signed URL is followed — is not answered `UnknownService`, whether or not the
//!   credential scope in its query string was read;
//! - [`Service::override_routing_error`] serves it. A download names no modelled
//!   operation by construction, so the router reaches exactly this seam for it, and
//!   reaches it BEFORE authentication — which is what lets the URL's OWN signature be
//!   what authorises the fetch, rather than a credential of the holder's.
//!
//! What it serves is the blob the repository HOLDS (`crate::state::LayerBlob::content`),
//! read at the moment of the fetch: a download is a pure read, so following one URL
//! twice answers the same bytes and changes nothing a later caller sees.
//!
//! What AUTHORISES the fetch is the URL's own query-string signature
//! (`crate::state::layer_download_signature`), which is what a pre-signed URL presents
//! instead of a credential: it is verified BEFORE the store is read, so an address this
//! registry did not sign — a rewritten signing field, a window extended by hand, another
//! layer's address, another endpoint host — learns nothing about what the registry holds.
//! Past the signature, the capability is the grant the control plane minted
//! (`crate::state::LayerDownloadGrant`), addressed by the token the URL carries: a token
//! whose grant this registry no longer holds resolves to no download, a grant whose
//! window has closed is refused as an expired request, and a grant whose layer or
//! repository this registry no longer holds has nothing to serve.
//!
//! ## The Docker Registry v2 data plane
//!
//! The other data plane this service carries is the registry API itself — the endpoint a
//! `docker pull` talks to — served by `service/registry_v2.rs` through the SAME pair of
//! seams, for the same reason: its paths (`/v2/<name>/manifests/<reference>`,
//! `/v2/<name>/blobs/<digest>`) name no modelled operation, and a request to one presents a
//! Basic credential rather than a SigV4 signature, so without [`Service::can_handle`]
//! claiming that path space the front door can attribute it to no service at all. What
//! authorises it is the credential `GetAuthorizationToken` minted, and what it answers is
//! what the repository holds. See that module's own docs.

/// The Docker Registry v2 data plane this registry serves beside its API, in
/// `service/registry_v2.rs`: the endpoint a `docker pull` talks to, claimed by
/// [`Service::can_handle`] and served by [`Service::override_routing_error`] the same way
/// the layer download plane is.
mod registry_v2;

/// This service's request-deserialization layer, in `service/unreadable_body.rs`: a body
/// whose top level is not a JSON structure never becomes an input, so it is refused with
/// the protocol's own `SerializationException` rather than with the constraint refusal the
/// framework would render. Reached from [`Service::override_operation`], the one seam a
/// service has before dispatch.
mod unreadable_body;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service, Violation, ViolationKind,
};
use std::sync::Arc;

/// The `com-amazonaws-ecr-amazonec2containerregistry-v20150921` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The SAME shared engine the generated entry threads into every operation.
    ///
    /// Held here because the layer download plane is served from
    /// [`Service::override_routing_error`], which is not an operation and so has no
    /// `self.engine` of its own; it must read the one store the operations write, not a
    /// second copy.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            engine,
        }
    }
}

/// The clause ECR states for one refused input member: the parameter at fault and the
/// constraint it failed.
///
/// ECR words every such clause through [`crate::state::parameter_constraint`] and fills
/// both halves from the request and the model, so they are reproduced from the violation
/// here rather than written out per member. The two halves are decided INDEPENDENTLY, and
/// the recorded corpus shows each of them taking either form:
///
/// * WHICH parameter the clause names follows from WHERE in the request the value sits. A
///   parameter the request CARRIES ([`carried_parameter`]) is named by its API label
///   ([`member_at_fault`]) — a top-level member, or an element of one of its collections,
///   which is one VALUE of that same parameter and is named by it in the singular. A
///   member of a structure NESTED in the request has no API label of its own, so it is
///   named by the path the validating walk descended to it, collection steps included
///   (`… at 'signingConfiguration.rules.1.member.repositoryFilters.1.member.filter' …`).
/// * HOW the constraint reads follows from WHOSE RULE the value broke. Every constraint
///   but one is stated in the `Member must …` wording the model-driven walk states a
///   modelled constraint in, whatever the member's position — a `@length` on a top-level
///   member included. The exception is a `@pattern`, which ECR words its own way for the
///   value classes it polices in its OWN code ([`EXPRESSIONS_ECR_POLICES_ITSELF`]): the
///   expression as ECR publishes it, with no `Member` in front of it (`… at
///   'repositoryName' … 'must satisfy regular expression '[a-z0-9]+…''`). Every other
///   class keeps the walk's `Member must satisfy regular expression pattern: <expression>`
///   ([`pattern_requirement`]) over the expression ECR publishes for that class
///   ([`policed`]).
///
/// Two refusals are worded by neither rule. An ARN of a class ECR words itself — the
/// signing profile a signing rule names ([`ARN_EXPRESSIONS_ECR_WORDS_IN_PROSE`]) — is
/// stated wherever the request carries it in prose naming the kind of ARN and the value
/// that is not one ([`arn_constraint`]) rather than by quoting an expression. And a LIST
/// ECR reads no value out of ([`element_constraint`]) has no one value to refuse, so its
/// clause names the list itself and nests what each element must satisfy.
///
/// A violation kind neither wording knows (the enum is `#[non_exhaustive]`) keeps the
/// framework's own clause, which states the same two facts in its own words.
fn clause(violation: &Violation) -> String {
    let member = member_at_fault(&violation.path);
    // An ARN of a class ECR words in its own prose is stated that way wherever the request
    // carries it, so this branch is taken before the position of the member is even looked
    // at.
    if let ViolationKind::Pattern(pattern) = &violation.kind {
        if let Some(prose) = arn_constraint(&member, pattern, violation.value.as_deref()) {
            return stated(&member, &prose);
        }
    }
    // A list ECR acts on no value of is refused as a WHOLE, so this is settled before the
    // position of the member is looked at too; see [`element_constraint`].
    if let Some((collection, requirement)) = element_constraint(violation) {
        return stated(collection, &requirement);
    }
    let Some(requirement) = requirement(&violation.kind) else {
        return violation.message();
    };
    let parameter = if carried_parameter(&violation.path) {
        member
    } else {
        ::std::borrow::Cow::Borrowed(violation.path.as_str())
    };
    stated(&parameter, &requirement)
}

/// The clause itself, in the one place this service words a refused parameter into it
/// ([`crate::state::parameter_constraint`]), so the boundary walk's refusal and a
/// handler's refusal of a rule the model cannot express read as one form.
fn stated(parameter: &str, constraint: &str) -> String {
    crate::state::parameter_constraint(parameter, constraint)
}

/// The constraint half of a clause: the `Member must …` wording the model-driven walk
/// states a modelled constraint in, except for a `@pattern` of a value class ECR polices
/// itself ([`polices_itself`]), which is stated as the expression ECR publishes
/// ([`stated_expression`]). `None` for a kind neither states, so the caller can keep the
/// framework's whole clause.
fn requirement(kind: &ViolationKind) -> Option<String> {
    Some(match kind {
        ViolationKind::MissingRequiredMember => "Member must not be null".to_string(),
        ViolationKind::MinLength(min) => {
            format!("Member must have length greater than or equal to {min}")
        }
        ViolationKind::MaxLength(max) => {
            format!("Member must have length less than or equal to {max}")
        }
        ViolationKind::MinValue(min) => {
            format!("Member must have value greater than or equal to {min}")
        }
        ViolationKind::MaxValue(max) => {
            format!("Member must have value less than or equal to {max}")
        }
        // The one requirement NOT worded "Member must …": a value class ECR polices in
        // its own code has its expression stated the way ECR publishes it. Deliberate —
        // do not "align" it with the arms around it.
        ViolationKind::Pattern(pattern) if polices_itself(pattern) => stated_expression(pattern),
        ViolationKind::Pattern(pattern) => pattern_requirement(policed(pattern)),
        // The value set is stated in the order the admissible values' own HASH TABLE is
        // walked rather than the order the model declares them in: the front door this
        // service answers behind holds them keyed on each value's own text and enumerates
        // that table. The order follows from the values themselves
        // (`::service_common::validation_message::hashed_enum_order`, the tree's one
        // derivation of it), so it belongs to the enumerated TYPE: every enum of this
        // model is stated through it and none is written out anywhere.
        ViolationKind::EnumValue(allowed) => {
            format!(
                "Member must satisfy enum value set: [{}]",
                ::service_common::validation_message::hashed_enum_order(allowed).join(", ")
            )
        }
        ViolationKind::UniqueItems => "Member must have unique values".to_string(),
        _ => return None,
    })
}

/// The requirement a clause states for a `@pattern` in the `Member must …` wording, over
/// `expression`: the one spelling of that sentence, so the clause the model-driven walk
/// states and the one nested inside a collection's ([`element_constraint`]) cannot word it
/// differently.
fn pattern_requirement(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The expressions of the value classes ECR polices in its OWN code, as THE MODEL spells
/// them: the classes whose `@pattern` a refusal states in ECR's own words rather than in
/// the model-driven walk's.
///
/// These are the two values ECR has to READ before a call can do its work — the name it
/// resolves a repository by and the account id it resolves a registry by — and the
/// recorded refusals state each of them as the expression ECR publishes for the class
/// (`… 'must satisfy regular expression '[a-z0-9]+…''`,
/// `… 'must satisfy regular expression [0-9]{12}'`). Every other class the recorded
/// refusals show — a namespace prefix, a layer digest, a repository filter, an ARN the
/// registry only records — is stated in the walk's wording with the expression ECR
/// publishes for it, whether the member sits at the top level of the request or inside a
/// nested structure.
///
/// Keyed on the EXPRESSION, exactly as [`policed`] is, so a class is stated the same way
/// for every member targeting it and on every operation that carries one.
/// `smithy_query(query="shape", shape="RepositoryName")` and `shape="RegistryId"` give the
/// two spellings listed here.
const EXPRESSIONS_ECR_POLICES_ITSELF: [&str; 2] = [
    r"^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$",
    "^[0-9]{12}$",
];

/// Whether `pattern` constrains a value class ECR polices in its own code
/// ([`EXPRESSIONS_ECR_POLICES_ITSELF`]).
fn polices_itself(pattern: &str) -> bool {
    EXPRESSIONS_ECR_POLICES_ITSELF.contains(&pattern)
}

/// The expressions ECR states BARE — the expression alone inside the clause's own quotes,
/// with no inner quoting.
///
/// ECR's own validation states the expression it polices for a value class
/// ([`as_stated`]), and whether that statement DELIMITS the expression is a property of
/// the value CLASS rather than of the member, the operation or the constraint: the
/// recorded refusals show the repository-name classes' expression delimited in single
/// quotes (`… 'must satisfy regular expression '[a-z0-9]+…''`) and the ACCOUNT-ID class's
/// — the twelve digits a registry id is — stated with nothing around it. Keyed on the
/// expression, exactly as [`policed`] is, so it holds for every member targeting that
/// shape: `registryId` is a parameter most operations of this service carry, and the
/// clause reads the same on each.
///
/// `smithy_query(query="shape", shape="RegistryId")` gives `^[0-9]{12}$`, which
/// [`as_stated`] publishes as the expression listed here.
const EXPRESSIONS_STATED_BARE: [&str; 1] = ["[0-9]{12}"];

/// The requirement ECR'S OWN validation states for a `@pattern`: the expression as ECR
/// publishes it ([`as_stated`]), delimited unless the value class is one ECR states bare
/// ([`EXPRESSIONS_STATED_BARE`]).
fn stated_expression(pattern: &str) -> String {
    let expression = as_stated(policed(pattern));
    if EXPRESSIONS_STATED_BARE.contains(&expression.as_str()) {
        return format!("must satisfy regular expression {expression}");
    }
    format!("must satisfy regular expression '{expression}'")
}

/// The list members this model marks `@deprecated`, which is ECR saying it acts on no
/// value of them.
///
/// `smithy_query(query="operation", operation="GetAuthorizationToken")` shows
/// `registryIds` carrying `@deprecated` — "This field is deprecated. The returned
/// authorization token can be used to access any Amazon ECR registry that the IAM
/// principal has access to, specifying a registry ID doesn't change the permissions scope
/// of the authorization token." — and it is the only such list this model declares.
const LISTS_ECR_ACTS_ON_NO_VALUE_OF: [&str; 1] = ["registryIds"];

/// The clause's two halves for a violation at an ELEMENT of a list ECR acts on no value of
/// ([`LISTS_ECR_ACTS_ON_NO_VALUE_OF`]): the LIST member, under the plural name it is
/// declared with, and the element's requirement nested in the collection form
/// `Member must satisfy constraint: [ … ]`.
///
/// Which half of a clause names WHAT follows from what ECR does with the value. A value
/// ECR resolves to a resource is refused one value at a time, so the clause names that one
/// value's parameter ([`member_at_fault`], the singular of the collection's name). A list
/// ECR reads no value out of has no one value to refuse: the LIST is what it checks the
/// shape of, so the clause names the list and states what each element has to satisfy
/// inside it.
///
/// The nesting is [`::service_common::validation_message::element_set_requirement`], the
/// tree's one spelling of that wrapper, so this cannot word it differently from the
/// services that hand the whole refusal to that module. Inside it the requirement is the
/// element's, in the `Member must …` wording ([`pattern_requirement`]) over the expression
/// ECR publishes for the value class ([`as_stated`]) — ECR's own validation is still what
/// caught the value, so the expression is ECR's published one rather than the model's
/// source text.
///
/// `None` for every other violation — one at an element of a list ECR DOES resolve, one on
/// a collection ITSELF (its `@length`, its `@uniqueItems`), one on a scalar member — each
/// of which keeps its own clause.
fn element_constraint(violation: &Violation) -> Option<(&str, String)> {
    let collection = ::service_common::validation::list_of_element(&violation.path)?;
    if !LISTS_ECR_ACTS_ON_NO_VALUE_OF.contains(&collection) {
        return None;
    }
    let requirement = match &violation.kind {
        ViolationKind::Pattern(pattern) => pattern_requirement(&as_stated(policed(pattern))),
        kind => requirement(kind)?,
    };
    Some((
        collection,
        ::service_common::validation_message::element_set_requirement(&[requirement]),
    ))
}

/// Whether `path` names a parameter the request CARRIES, which is what the API gives a
/// label of its own: a top-level member (`repositoryName`), or an element of a top-level
/// collection (`repositoryNames.2.member`), which is one value of that same parameter.
/// A member of a structure NESTED in the request is not — the API names no such member on
/// its own, and the model-driven walk reports it under the path it descended.
fn carried_parameter(path: &str) -> bool {
    !named_member(path).contains('.')
}

/// The member a refusal names when the API gives the value a label of its own: the member
/// at fault ITSELF, not the path the validating walk descended to reach it
/// (`signingProfileArn`, not `signingConfiguration.rules.1.member.signingProfileArn`).
///
/// The steps from a collection into an element are dropped off the end first
/// ([`named_member`]), and what remains is the last named member. A refused value that was
/// an ELEMENT is one value of the collection's parameter rather than the collection
/// itself, and ECR names the value's own parameter: the collection is named by the plural
/// of it, so the value is named by [`one_value_of`] that name (`repositoryNames.2.member`
/// -> `repositoryName`).
fn member_at_fault(path: &str) -> ::std::borrow::Cow<'_, str> {
    let remaining = named_member(path);
    let member = remaining.rsplit('.').next().unwrap_or(remaining);
    if remaining.len() == path.len() {
        return ::std::borrow::Cow::Borrowed(member);
    }
    one_value_of(member)
}

/// The parameter ONE VALUE of the collection parameter `collection` belongs to: a
/// collection of a parameter's values is named by the PLURAL of that parameter, so a value
/// of it is named by the singular — `repositoryNames` -> `repositoryName`, `imageIds` ->
/// `imageId`, `ecrRepositoryPrefixes` -> `ecrRepositoryPrefix`.
///
/// The plural is undone the way English forms it, which is what a member name in this model
/// is written in: a name ending in a sibilant takes `-es` (`prefix` -> `prefixes`), a name
/// ending in a consonant and `y` has it replaced by `-ies` (`policy` -> `policies`), and
/// every other name takes a plain `-s`. A name that is not a plural has nothing to drop and
/// keeps its own spelling.
fn one_value_of(collection: &str) -> ::std::borrow::Cow<'_, str> {
    /// The endings an `-es` plural follows: `s`, `x` and `z`, plus the digraphs `ch` and
    /// `sh`, are the ones that take the syllable rather than a bare `s`.
    const SIBILANT_ENDINGS: [&str; 5] = ["s", "x", "z", "ch", "sh"];

    if let Some(stem) = collection.strip_suffix("ies").filter(|s| !s.is_empty()) {
        return ::std::borrow::Cow::Owned(format!("{stem}y"));
    }
    if let Some(stem) = collection.strip_suffix("es").filter(|s| !s.is_empty()) {
        if SIBILANT_ENDINGS.iter().any(|ending| stem.ends_with(ending)) {
            return ::std::borrow::Cow::Borrowed(stem);
        }
    }
    collection
        .strip_suffix('s')
        .filter(|value| !value.is_empty())
        .map_or(::std::borrow::Cow::Borrowed(collection), |value| {
            ::std::borrow::Cow::Borrowed(value)
        })
}

/// `path` with the steps from a collection into an element dropped off the end, so what
/// is left ends in a MEMBER name: `repositoryNames.2.member` -> `repositoryNames`, and
/// `signingConfiguration.rules.1.member.filter` -> itself.
///
/// The element step is recognised through [`::service_common::validation::list_of_element`],
/// the one place in the tree that spells it, so this cannot disagree with the walk about
/// the shape of a path. The other step that spelling covers, a map entry, cannot arise
/// here: this service's model declares no map shape at all, and a map key is a value the
/// CALLER chose rather than a member name.
///
/// A path with no named member left in it keeps its own spelling: there is nothing in it
/// to prefer.
fn named_member(path: &str) -> &str {
    let mut remaining = path;
    while let Some(collection) = ::service_common::validation::list_of_element(remaining) {
        remaining = collection;
    }
    remaining
}

/// The expression ECR POLICES for a repository-name GLOB — the value a repository filter
/// carries.
///
/// This model spells that one value class twice:
/// `smithy_query(query="shape", shape="SigningRepositoryFilterValue")` gives this
/// expression, while `ScanningRepositoryFilterValue` gives
/// [`SEPARATOR_CLASS_GLOB_SPELLING`] — two writings of the same language (a run of
/// `[a-z0-9*]` with single `.`, `_`, `-` or `/` separators between runs), differing only
/// in whether the separators are one character class or a group. ECR validates a
/// repository filter against the expression it publishes, so a refusal quotes this
/// spelling for either member.
const REPOSITORY_GLOB_EXPRESSION: &str =
    "^(?:[a-z0-9*]+(?:[._-][a-z0-9*]+)*/)*[a-z0-9*]+(?:[._-][a-z0-9*]+)*$";

/// The other spelling of [`REPOSITORY_GLOB_EXPRESSION`] this model carries, which ECR's
/// validator does not state back to a caller.
const SEPARATOR_CLASS_GLOB_SPELLING: &str = r"^[a-z0-9*](?:[._\-/a-z0-9*]?[a-z0-9*]+)*$";

/// The expression ECR PUBLISHES for a LAYER DIGEST — the `<algorithm>:<hex>` form a layer
/// blob, and the manifest of an image, is addressed by.
///
/// `smithy_query(query="shape", shape="LayerDigest")` spells the class ANCHORED
/// (`^[a-zA-Z0-9-_+.]+:[a-fA-F0-9]+$`); the expression ECR's own validator holds for it,
/// and quotes back to a caller, carries no anchors. Both spellings admit the same values
/// under a total match, so the expression a refusal quotes is the published one.
const LAYER_DIGEST_EXPRESSION: &str = "[a-zA-Z0-9-_+.]+:[a-fA-F0-9]+";

/// The spelling of the layer-digest class this model carries, which ECR's validator does
/// not state back to a caller.
const ANCHORED_LAYER_DIGEST_SPELLING: &str = "^[a-zA-Z0-9-_+.]+:[a-fA-F0-9]+$";

/// The expression ECR PUBLISHES for a REPLICATION repository filter — the value a
/// replication rule's `repositoryFilters` entry carries.
///
/// A `PREFIX_MATCH` filter may carry the `*` wildcard, and the expression ECR publishes
/// admits it in every character class, with the length the value may run to as a leading
/// lookahead. `smithy_query(query="shape", shape="RepositoryFilterValue")` spells the
/// class without the wildcard and states its length as a separate `@length(2..256)`, so
/// the model and the service disagree about this one class, and the published expression
/// is the one a refusal quotes.
///
/// What this emulator CHECKS a filter against is still the model's spelling: the boundary
/// walk is driven by the model's own constraints, and the lookahead this expression opens
/// with is not one the walk's regular-expression engine compiles. The two spellings
/// therefore disagree about the values carrying `*`, which are narrower here than at ECR;
/// that acceptance gap is carried as its own issue
/// (`20261118T120000Z-ecr-putreplicationconfiguration-refuses-a-wildcard-repository-filter-aws-accepts`),
/// while a value either spelling refuses is refused with the published one quoted, which
/// is what the recorded refusal carries.
const REPLICATION_FILTER_EXPRESSION: &str =
    "^(?=.{1,256}$)(?:[a-z0-9*]+(?:[._-][a-z0-9*]*)*/)*[a-z0-9*]*(?:[._-][a-z0-9*]*)*$";

/// The spelling of the replication-filter class this model carries, which ECR's validator
/// does not state back to a caller.
const MODELLED_REPLICATION_FILTER_SPELLING: &str =
    "^(?:[a-z0-9]+(?:[._-][a-z0-9]*)*/)*[a-z0-9]*(?:[._-][a-z0-9]*)*$";

/// The value classes whose expression ECR PUBLISHES differently from how this model spells
/// it, as the pair (the model's spelling, the one ECR states).
///
/// Keyed on the EXPRESSION rather than on a member, an operation or a constraint, so the
/// expression a refusal quotes belongs to the value class: every member targeting the
/// class is refused with the same one. A class absent from this table is published exactly
/// as the model spells it, which is what the recorded refusals of this service's other
/// classes show.
const PUBLISHED_EXPRESSIONS: [(&str, &str); 3] = [
    (SEPARATOR_CLASS_GLOB_SPELLING, REPOSITORY_GLOB_EXPRESSION),
    (ANCHORED_LAYER_DIGEST_SPELLING, LAYER_DIGEST_EXPRESSION),
    (
        MODELLED_REPLICATION_FILTER_SPELLING,
        REPLICATION_FILTER_EXPRESSION,
    ),
];

/// The expression ECR polices for the value class `pattern` constrains: `pattern` itself
/// for every class the model spells the way ECR publishes it, and the published spelling
/// for the classes it does not ([`PUBLISHED_EXPRESSIONS`]).
fn policed(pattern: &str) -> &str {
    PUBLISHED_EXPRESSIONS
        .iter()
        .find(|(modelled, _)| *modelled == pattern)
        .map_or(pattern, |(_, published)| *published)
}

/// A `@pattern` expression as ECR'S OWN validation states it, which differs from how the
/// model spells it in two ways — both of them about spelling the same expression, neither
/// about what it matches:
///
/// * the `^` / `$` the model anchors with are dropped ([`unanchored`]): AWS states the
///   expression a value must satisfy, not the anchoring that makes the match total;
/// * a `/` carries no backslash. A slash is not a regular-expression metacharacter, so
///   `\/` and `/` match the same character and AWS states the bare one. Every other
///   escape is kept — `\.`, `\w`, and `\\` included — because dropping one WOULD change
///   what the expression matches.
fn as_stated(pattern: &str) -> String {
    let body = unanchored(pattern);
    let mut stated = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            stated.push(character);
            continue;
        }
        match characters.next() {
            // The one escape that is not part of the expression AWS states.
            Some('/') => stated.push('/'),
            // Every other escape is: keep the pair exactly as the model spells it.
            Some(escaped) => {
                stated.push('\\');
                stated.push(escaped);
            }
            // A trailing backslash escapes nothing; it stands as it is.
            None => stated.push('\\'),
        }
    }
    stated
}

/// The suffix the name of a member holding an Amazon Resource Name ends in.
const ARN_MEMBER_SUFFIX: &str = "Arn";

/// The expressions of the ARN value classes whose `@pattern` refusal ECR words in PROSE
/// rather than by quoting the expression, as THE MODEL spells them.
///
/// The recorded refusals show exactly one such class: the Amazon Web Services Signer
/// signing profile ARN a signing rule carries, which ECR refuses with `The signing profile
/// ARN '<value>' must be well formed`. Every OTHER ARN class this model carries is stated
/// in the model-driven walk's wording over the expression the model spells — both the
/// Secrets Manager secret a pull through cache rule's credential is read from and the IAM
/// principal a pull time update exclusion is recorded for are recorded that way — so an ARN
/// class absent from here keeps the walk's wording, whether or not ECR resolves the value.
///
/// Keyed on the EXPRESSION, exactly as [`EXPRESSIONS_ECR_POLICES_ITSELF`] and
/// [`PUBLISHED_EXPRESSIONS`] are, so a class is worded the same way for every member
/// targeting it and on every operation that carries one.
/// `smithy_query(query="shape", shape="SigningProfileArn")` gives the spelling listed here.
const ARN_EXPRESSIONS_ECR_WORDS_IN_PROSE: [&str; 1] =
    [r"^arn:aws(-[a-z]+)*:signer:[a-z0-9-]+:[0-9]{12}:\/signing-profiles\/[a-zA-Z0-9_]{2,}$"];

/// The constraint ECR states for a member holding an ARN of a class it words itself,
/// refused against its `@pattern`: prose naming the kind of ARN and the value that is not
/// one, rather than the expression.
///
/// For such an ARN ECR does not quote the member's expression back at the caller — it says
/// the ARN is not well formed: `The signing profile ARN '<value>' must be well formed`.
/// The kind of ARN comes from the member's own name with its [`ARN_MEMBER_SUFFIX`] removed
/// and its lowerCamel words separated (`signingProfileArn` -> `signing profile`), so every
/// such member this service models is worded by the one rule and none is written out here.
///
/// `None` — so the expression itself is stated — for the expression of every other value
/// class ([`ARN_EXPRESSIONS_ECR_WORDS_IN_PROSE`]), when the member is not ARN-valued (its
/// name does not end in `Arn`), and when the violation carries no value, which a
/// `@pattern` failure always does (only an absent member has none) but which is not this
/// function's to assume.
fn arn_constraint(member: &str, pattern: &str, value: Option<&str>) -> Option<String> {
    if !ARN_EXPRESSIONS_ECR_WORDS_IN_PROSE.contains(&pattern) {
        return None;
    }
    let kind = member
        .strip_suffix(ARN_MEMBER_SUFFIX)
        .filter(|kind| !kind.is_empty())?;
    let value = value?;
    Some(crate::state::malformed_arn_constraint(
        &in_words(kind),
        value,
    ))
}

/// A lowerCamel member name as the words it is made of: `signingProfile` -> `signing
/// profile`, `credential` -> `credential`.
///
/// Every member this service models is lowerCamel with no acronym RUN (`smithy_query`
/// digest), so each capital opens a word. A name carrying a run (`sourceKMSKey`) would
/// be worded letter by letter; it is spelled out here rather than guessed at, because a
/// run-aware split would have to decide where the run ends with nothing in the model to
/// decide it from.
fn in_words(name: &str) -> String {
    let mut words = String::with_capacity(name.len() + name.len() / 4);
    for character in name.chars() {
        if character.is_uppercase() && !words.is_empty() {
            words.push(' ');
        }
        words.extend(character.to_lowercase());
    }
    words
}

/// A `@pattern` expression without the anchors a model may spell it with: AWS states
/// the expression a value must satisfy, not the anchoring the model uses to make the
/// match total. A `$` escaped by a backslash is a literal dollar sign inside the
/// expression, not the end anchor, so it stays. (Same rule, and the same reasoning, as
/// `services/service-com-amazonaws-controlcatalog-controlcatalog/src/service.rs`'s own
/// `unanchored`; neither service's private helper is reachable from the other.)
fn unanchored(pattern: &str) -> &str {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    match body.strip_suffix('$') {
        Some(trimmed) => {
            let backslashes = trimmed.len() - trimmed.trim_end_matches('\\').len();
            if backslashes % 2 == 0 {
                trimmed
            } else {
                body
            }
        }
        None => body,
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        if smithy_server_runtime::resolve_route_key(parts)
            .is_some_and(|key| key == self.entry.identity().signing_name())
        {
            return true;
        }
        // A layer download carries no credential and no `X-Amz-Target`, and when a client
        // addresses the emulator directly rather than by endpoint host its host names no
        // service either, so the path this registry publishes downloads at is the only
        // thing left to identify it. Without this the router answers `UnknownService` for
        // a URL this service announced. Every method is claimed, not only the two that
        // are served: a `POST` to a path this service publishes is this service's to
        // refuse as a method. See the module docs.
        if crate::state::parse_layer_download_path(parts.uri.path()).is_some() {
            return true;
        }
        // A Docker Registry v2 request — a `docker pull` — carries a Basic credential and
        // no `X-Amz-Target`, so nothing in it resolves a route key and the registry
        // endpoint it addresses is what identifies it. Without this the router answers
        // `UnknownService` for the one endpoint every container client pulls through. See
        // `service/registry_v2.rs` for how narrowly the claim is drawn.
        registry_v2::claims_registry_v2(parts)
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // A body the protocol cannot FRAME is refused FIRST, because deserialization
        // precedes every later layer: the framework would otherwise report a framing
        // failure as a constraint violation, which is a different error class and a
        // different remediation for the caller ([`unreadable_body`]). This is NOT a
        // re-route — the refusal answers for the operation the model's own routing key
        // names, and a key the model does not name resolves to `None` here exactly as it
        // does in the generated table, so the router's routing-error path is unchanged.
        if unreadable_body::refuses(self.definition(), req) {
            return Some(&unreadable_body::UNREADABLE_BODY);
        }
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal — an absent
        // required member, or a value outside a `@length`, `@range`, `@pattern`,
        // `@enum` or `@uniqueItems` the model declares — before any handler runs.
        //
        // ECR states such an input as `InvalidParameterException` ("The specified
        // parameter is invalid. Review the available parameters for the API request."):
        // 57 of this model's 58 operations declare it, and it is the only client-input
        // error `CreateRepository`, `DescribeRepositories`, `ListImages`, `DescribeImages`
        // and `PutImage` declare at all. The framework's default code,
        // `ValidationException`, is not in those operations' error sets — the model binds
        // its own `ValidationException` ("There was an exception validating this request")
        // only to other operations — so a client branching on ECR's documented code sees
        // an error its SDK cannot even name. This seam is per SERVICE, not per operation,
        // so the code chosen here is the one nearly every operation models.
        //
        // The status stays with the protocol: awsJson1_1 renders a refusal as HTTP 400,
        // which is `InvalidParameterException`'s modelled status.
        match refusal {
            // Every operation but `DeleteSigningConfiguration` declares this error; that
            // one declares ECR's own `ValidationException` instead, and this seam cannot
            // name the operation, so it answers the code the other 57 model rather than
            // the framework's synthetic one, which none of them models.
            Refusal::Invalid(violations) => Some(Refused {
                code: "InvalidParameterException".to_string(),
                // Every member at fault, so a caller fixes the request from the message
                // alone; one refused member is the single-clause case of the same rule.
                // A refusal that named no violation still answers the modelled code,
                // with the error shape's own documented sentence: the alternative is
                // falling through to a code no operation here models.
                message: if violations.is_empty() {
                    "The specified parameter is invalid. Review the available parameters \
                     for the API request."
                        .to_string()
                } else {
                    violations
                        .iter()
                        .map(clause)
                        .collect::<Vec<String>>()
                        .join("; ")
                },
            }),
            // Authentication is not this service's concern, and a refusal kind added
            // later keeps the framework's wording until it is worded here.
            _ => None,
        }
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501).
        //
        // A request to a layer download URL this registry handed out is exactly that
        // case by construction — the path is not in the model — so this is where such a
        // URL is served. Anything else keeps the framework's default.
        let path = ::service_common::request_path(req.uri());
        if let Some(target) = crate::state::parse_layer_download_path(path) {
            return Some(self.serve_layer_download(req, &target));
        }
        // A registry request names no modelled operation either — the registry API's paths
        // are not in the model — so this is where a `docker pull` is served.
        let target = registry_v2::registry_v2_target(path)?;
        Some(self.serve_registry_v2(req, &target))
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

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Serve one request to a published layer download URL.
    ///
    /// A download is a pure READ: it answers the bytes the repository holds for the layer
    /// the grant names and writes nothing, so following one URL twice answers the same
    /// layer and changes nothing a later caller sees.
    ///
    /// The URL's own SIGNATURE decides whether the download is authorised at all, and it
    /// is checked before the store is read: this plane stands in for the object store a
    /// real pre-signed download is served from, and that store serves what it signed.
    ///
    /// Past it, the grant the URL's token addresses is what decides it, in this order: a
    /// token whose grant this registry no longer holds names no download at all; a grant
    /// whose window has closed is an expired request, which is a different answer from one
    /// that never existed; a path whose digest is not the one the grant serves names no
    /// download either; and finally the repository's own record of the blob is read, so a
    /// repository or a layer this registry no longer holds has nothing to serve.
    fn serve_layer_download(
        &self,
        req: &RtRequest,
        target: &crate::state::LayerDownloadTarget,
    ) -> RtResponse {
        if !matches!(req.method(), "GET" | "HEAD") {
            return method_not_allowed();
        }
        match crate::state::layer_download_signature(
            request_authority(req),
            request_target(req.uri()),
            ::std::time::SystemTime::now(),
        ) {
            crate::state::LayerDownloadSignature::Honoured => {}
            // The wording each condition answers with is the one the object store this
            // plane stands in for gives for it.
            crate::state::LayerDownloadSignature::Absent => {
                return download_error(403, "AccessDenied", DOWNLOAD_UNSIGNED_MESSAGE)
            }
            crate::state::LayerDownloadSignature::Expired => {
                return download_error(403, "AccessDenied", DOWNLOAD_EXPIRED_MESSAGE)
            }
            crate::state::LayerDownloadSignature::Invalid => {
                return download_error(403, "SignatureDoesNotMatch", SIGNATURE_MISMATCH_MESSAGE)
            }
        }
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return download_error(
                500,
                "InternalError",
                "the ecr repository store is unavailable",
            );
        };
        let store = state.lock();

        let Some(grant) = store.layer_download_grant(&target.token) else {
            return download_error(404, "NoSuchKey", DOWNLOAD_NOT_FOUND_MESSAGE);
        };
        if grant.has_expired(::std::time::SystemTime::now()) {
            return download_error(403, "AccessDenied", DOWNLOAD_EXPIRED_MESSAGE);
        }
        if grant.layer_digest != target.layer_digest {
            return download_error(404, "NoSuchKey", DOWNLOAD_NOT_FOUND_MESSAGE);
        }
        let held = store
            .repository(&grant.region, &grant.repository_name)
            .ok()
            .and_then(|repo| repo.layer_blobs.get(&grant.layer_digest));
        let Some(blob) = held else {
            return download_error(404, "NoSuchKey", DOWNLOAD_NOT_FOUND_MESSAGE);
        };

        // A `GET` carries the layer's bytes and a `HEAD` reports its length without them.
        layer_response(&blob.content, req.method() == "GET")
    }
}

/// The body of a download refusal that found no layer: one sentence carrying nothing from
/// the request, so a caller cannot probe for layers it was not handed a URL for.
const DOWNLOAD_NOT_FOUND_MESSAGE: &str = "The specified key does not exist.";

/// The body of a download refusal whose URL is past its validity window.
const DOWNLOAD_EXPIRED_MESSAGE: &str = "Request has expired";

/// The body of a download refusal that presented no signature at all: an anonymous request
/// for content only a pre-signed address reaches.
const DOWNLOAD_UNSIGNED_MESSAGE: &str = "Access Denied";

/// The body of a download refusal whose URL presents a signature that was not computed
/// over the address it was presented on — the fixed sentence the object store this plane
/// stands in for answers a signature mismatch with.
const SIGNATURE_MISMATCH_MESSAGE: &str = "The request signature we calculated does not \
     match the signature you provided. Check your key and signing method.";

/// The read methods the layer download plane serves: `GET` carries the bytes and `HEAD`
/// reports the metadata without them.
const DOWNLOAD_READ_METHODS: &str = "GET, HEAD";

/// The refusal the download plane answers a method it does not serve with. RFC 9110
/// requires a `405` to state which methods the target does support, so the refusal names
/// them in the `Allow` header as well as in its message.
fn method_not_allowed() -> RtResponse {
    let mut response = download_error(
        405,
        "MethodNotAllowed",
        "A layer download is served by GET or HEAD",
    );
    response
        .headers_mut()
        .insert("allow", DOWNLOAD_READ_METHODS);
    response
}

/// One layer download: the blob's own bytes, under the generic binary media type and its
/// own length.
///
/// `with_body` is false for a `HEAD`, which reports the layer's length without its bytes.
fn layer_response(content: &[u8], with_body: bool) -> RtResponse {
    let body = if with_body {
        SdkBody::from(content.to_vec())
    } else {
        SdkBody::empty()
    };
    let mut response = RtResponse::new(status(200), body);
    response
        .headers_mut()
        .insert("content-type", crate::state::LAYER_DOWNLOAD_CONTENT_TYPE);
    response
        .headers_mut()
        .insert("content-length", content.len().to_string());
    response
}

/// One download-plane error response: the status, the code in `x-amzn-errortype`, and the
/// `<Error>` document the object store this plane stands in for answers a refused fetch
/// with — its `<Code>`, its `<Message>` and the id of the request it refused.
///
/// The download plane is a pre-signed object download rather than a modelled operation, so
/// it answers in that store's own envelope instead of this service's awsJson1_1 one. Every
/// code and message is a constant of this module, never built from the request, and each
/// value is still output-encoded on the way into the document, through the one XML escape
/// this tree's response encoders use.
fn download_error(status_code: u16, code: &str, message: &str) -> RtResponse {
    let escape = ::aws_smithy_schema_server::xml_escape;
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{}</Code>\
         <Message>{}</Message><RequestId>{}</RequestId></Error>",
        escape(code),
        escape(message),
        crate::state::mint_hex(DOWNLOAD_REQUEST_ID_BYTES).to_ascii_uppercase()
    );
    let mut response = RtResponse::new(status(status_code), SdkBody::from(body));
    response
        .headers_mut()
        .insert("content-type", "application/xml");
    response
        .headers_mut()
        .insert("x-amzn-errortype", code.to_string());
    response
}

/// How many bytes of a minted id the `<RequestId>` of a refused fetch carries: eight, the
/// 16 hex characters an object store's request id is written in.
const DOWNLOAD_REQUEST_ID_BYTES: usize = 8;

/// The endpoint a request reached this emulator on: its `Host` header, or the authority an
/// HTTP/2 request carries on the URI instead of one. It is the host a published URL was
/// signed over, so a URL is verified against the endpoint it is followed through.
fn request_authority(req: &RtRequest) -> &str {
    req.headers()
        .get(::http::header::HOST.as_str())
        .or_else(|| {
            req.uri()
                .split_once("://")
                .map(|(_scheme, rest)| match rest.find(['/', '?', '#']) {
                    Some(end) => &rest[..end],
                    None => rest,
                })
        })
        .unwrap_or_default()
}

/// The request TARGET of a request URI: its path and its query, with any scheme and
/// authority removed — the two halves of the address a published URL's signature covers.
///
/// `::service_common::request_path` reads the path alone, which is what the routing seams
/// above need; a signature covers the query too, so this reads both. (The same split, for
/// the same reason, as
/// `services/service-com-amazonaws-serverlessapplicationrepository-serverlessapplicationrepository/src/service.rs`'s
/// own `request_target`; neither service's private helper is reachable from the other, and
/// the shared home for it, if a third needs one, is `services/common`.)
fn request_target(uri: &str) -> &str {
    // The scheme is looked for only BEFORE the query: a query VALUE may carry `://`, which
    // a scan over the whole URI would mistake for the authority.
    let query_at = uri.find('?').unwrap_or(uri.len());
    let path_at = match uri[..query_at].find("://") {
        Some(scheme_end) => uri[scheme_end + 3..query_at]
            .find('/')
            .map_or(query_at, |slash| scheme_end + 3 + slash),
        None => 0,
    };
    &uri[path_at..]
}

/// A status this plane answers with. Every call site passes a constant of this module, so
/// the conversion cannot fail.
fn status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("the download plane's statuses are valid constants")
}
