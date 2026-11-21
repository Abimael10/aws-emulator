// SPDX-License-Identifier: CC0-1.0

//! How this service states a request it refuses BEFORE any handler runs.
//!
//! A request that breaks a rule the MODEL declares — a `@required` member absent, a value
//! outside a declared `@length`, `@range`, `@pattern` or enum value set — is refused at the
//! boundary, by the framework's own deserializer and constraint walk, before the
//! operation's `handle` is reached. `ValidationException` is a common error of EVERY
//! operation of this service (`smithy_query(query="digest")`), and the model gives it a
//! required `message` plus a `fieldList` of `ValidationExceptionField {path, message}` —
//! documented as "a list of specific failures encountered while validating the input",
//! whose `path` is "a JSONPointer expression to the structure member whose value failed to
//! satisfy the modeled constraints". The framework's own refusal body carries an error code
//! and a message and nothing else ([`smithy_server_runtime::Refused`]), so a refusal
//! rendered there can never carry that list.
//!
//! So every modelled operation is served behind [`ModelledValidation`], which collects the
//! violations of a request with the framework's own two steps
//! ([`::service_common::validation::refusing_violations`]) and answers them in this
//! service's modelled shape, and hands every request it does not refuse to the generated
//! operation unchanged.
//!
//! # How this service words one
//!
//! One envelope sentence over one clause per violation:
//!
//! ```text
//! 1 validation error detected. Value at '/registrationId' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[A-Za-z0-9_:/-]+$
//! ```
//!
//! The fixed text is `<N> validation error(s) detected. ` followed by the per-field clause,
//! `Value at '<path>' failed to satisfy constraint: <requirement>`. Its variable parts are
//! the violation count, the path — the JSON pointer of the member in the request document —
//! and the requirement the member failed. No submitted value is echoed anywhere in it, so a
//! caller's input never comes back inside a summary message. The same clause, without the
//! envelope, is what each `fieldList` entry states about its own member.
//!
//! A clause about a `@length` states one number of the value besides: the LENGTH the check
//! counted, in front of the member —
//!
//! ```text
//! 1 validation error detected. Value with length 11 at '/attributes' failed to satisfy constraint: Member must have length between 1 and 10, inclusive
//! ```
//!
//! — which is the one part of its own input a caller cannot read off the request without
//! re-measuring it, and the one number that says by how much the value overran. `@length`
//! counts a string's characters, a collection's elements and a blob's bytes, and a
//! [`Violation`] carries the BOUND rather than the size that broke it, so the number is read
//! off the value ([`::service_common::validation_message::stated_length`], which owns that
//! reading for every service). Every other constraint class names the member and the
//! requirement alone.
//!
//! Every requirement sentence is the framework's own
//! ([`::service_common::validation_message::requirement`]) except one: a `@range` or
//! `@length` the model bounds on BOTH sides is stated as that whole interval — `Member must
//! be between <min> and <max>, inclusive` — rather than as the single bound the value fell
//! outside, which is all a [`Violation`] carries. The window is therefore read off the
//! MODEL ([`window_of`]).

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::LazyLock;

use ::service_common::validation::SubmittedEcho;
use aws_smithy_schema::{Schema, ShapeId, TraitMap};
use aws_smithy_schema_server::constraints::{
    self, ElementConstraintsTrait, LengthTrait, RangeTrait,
};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, ServerHttpProtocol, Service,
    Violation, ViolationKind,
};

use crate::ops::types::{ValidationException, ValidationExceptionField};

/// The wire error code this service refuses such a request with: its modelled
/// `ValidationException`, which is also the code the framework's own refusal channel picks
/// for this restJson1 service. Restated here because this module renders the whole body.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `ValidationException`'s modelled `@httpError`
/// (`smithy_query(query="shape",shape="ValidationException")`).
const VALIDATION_EXCEPTION_STATUS: u16 = 400;

/// The text that separates a clause's subject from the requirement it failed.
const CLAUSE_MARKER: &str = "failed to satisfy constraint: ";

/// The framework's name for a step from a collection into one of its elements.
const COLLECTION_STEP: &str = "member";

/// The path a clause and a `fieldList` entry name the member by: the JSON pointer of that
/// member in the request document, which is what `ValidationExceptionField$path` documents.
///
/// The framework spells a violation's path as the dotted, lowerCamel trail of member names
/// it found the value under (`aws_smithy_schema_server::constraints::path`), which is the
/// same spelling every member of this model's input shapes takes on the wire; the
/// translation from that path to the pointer (the collection step, an element's base, the
/// characters a segment escapes) is [`::service_common::json_pointer::json_pointer`]'s,
/// which owns that rule for every service.
fn pointer(path: &str) -> String {
    ::service_common::json_pointer::json_pointer(path)
}

/// The clause one violation states: the member's pointer, the requirement it failed, and —
/// for a `@length` — the length the check counted ([`subject`]).
///
/// It names the member and the rule, and never the value that broke it: the value is the
/// caller's own input — some of this service's members are `@sensitive` — and this service's
/// clause carries no echo of it.
fn clause(operation: &Operated<'_>, violation: &Violation, echo: &SubmittedEcho) -> String {
    format!(
        "{}at '{}' {CLAUSE_MARKER}{}",
        subject(violation, echo),
        pointer(&violation.path),
        requirement(operation, violation)
    )
}

/// What a clause names before the member: the value, and for a `@length` violation the
/// LENGTH that check counted.
///
/// The number is a measurement of the caller's value and not the value itself, so it is the
/// one thing of the input a clause here states; it is read by
/// [`::service_common::validation_message::stated_length`], the one reading of the
/// validating walk's own renderings, and a value nothing at hand measures states no length
/// rather than a number nothing counted.
fn subject(violation: &Violation, echo: &SubmittedEcho) -> String {
    match ::service_common::validation_message::stated_length(violation, echo) {
        Some(length) => format!("Value with length {length} "),
        None => "Value ".to_string(),
    }
}

/// The requirement half of one violation's clause.
///
/// Every sentence is the framework's own, read back out of the clause it renders for that
/// violation class, so a requirement this service states and one the framework states are
/// the same sentence and a class added upstream is worded rather than unhandled. The one
/// exception is a `@range` or `@length` the model bounds on both sides, which this service
/// states as the whole interval — see [`value_requirement`] and [`length_requirement`].
fn requirement(operation: &Operated<'_>, violation: &Violation) -> String {
    match &violation.kind {
        ViolationKind::MinValue(_) | ViolationKind::MaxValue(_) => {
            value_requirement(window_of(operation, &violation.path).range, &violation.kind)
        }
        ViolationKind::MinLength(_) | ViolationKind::MaxLength(_) => {
            length_requirement(window_of(operation, &violation.path).length, &violation.kind)
        }
        other => shared_requirement(other),
    }
}

/// The operation a refused request named: its modelled name, and the schema of its input.
///
/// Both are needed to read a window off the model — the schema declares the operation's own
/// members, and the name is what the nested shapes below them are keyed by
/// ([`NESTED_WINDOWS`]) — so they travel together rather than as two arguments threaded
/// through every step.
#[derive(Debug, Clone, Copy)]
struct Operated<'schema> {
    name: &'static str,
    input_schema: &'schema Schema,
}

/// The requirement the framework words for `kind`, which is the sentence this service states
/// for every class but the two bounded ones.
fn shared_requirement(kind: &ViolationKind) -> String {
    ::service_common::validation_message::requirement(kind.clone())
}

/// A `@range` violation as this service states it: the declared interval when the model
/// bounds the member on both sides, and otherwise the framework's own sentence for the one
/// bound that failed.
///
/// The two-sided sentence is what the recorded refusal of a `codeLength` outside its
/// declared `4..8` states, and it is a statement about the MODEL rather than about the
/// request: the same interval whichever side the value fell outside.
fn value_requirement(range: Option<RangeTrait>, kind: &ViolationKind) -> String {
    if let Some(RangeTrait {
        min: Some(min),
        max: Some(max),
    }) = range
    {
        return format!(
            "Member must be between {} and {}, inclusive",
            render_bound(min),
            render_bound(max)
        );
    }
    shared_requirement(kind)
}

/// A `@length` violation on the same rule as [`value_requirement`]: the declared interval
/// when the model bounds the member on both sides, else the framework's own sentence.
///
/// The interval sentence is this renderer's companion to the `@range` one above — the same
/// refusal renderer states both — so a member bounded on both sides states its window
/// whether the bound is on a length or on a value.
fn length_requirement(length: Option<LengthTrait>, kind: &ViolationKind) -> String {
    if let Some(LengthTrait {
        min: Some(min),
        max: Some(max),
    }) = length
    {
        return format!("Member must have length between {min} and {max}, inclusive");
    }
    shared_requirement(kind)
}

/// A `@range` bound as a requirement displays it: an integral bound without a trailing
/// `.0`, a fractional one with its decimals — the rendering the framework's own clause uses
/// for the bound it carries, so a bound read off the model here and one read off a violation
/// agree.
///
/// `{bound:.0}` rather than a cast through `i64`: a narrowing cast SATURATES, so a bound
/// whose magnitude is past `i64`'s would advertise `i64::MAX` instead of what the model
/// declares.
fn render_bound(bound: f64) -> String {
    if bound.fract() == 0.0 && bound.is_finite() {
        return format!("{bound:.0}");
    }
    bound.to_string()
}

/// The modelled `ValidationException` for a set of boundary violations: the envelope
/// sentence over every clause, and one `fieldList` entry per violated member.
///
/// The count is followed by a PERIOD, where the framework's own default follows it with a
/// colon; the count word and the clause separator for several violations are the framework's
/// own ([`::aws_smithy_schema_server::constraints::envelope`] pluralises `validation error`
/// and joins the clauses with `; `), so a refusal of two members is stated the way a refusal
/// of one is rather than in a format invented here.
///
/// `None` when there is nothing to state, i.e. no violation.
fn boundary_exception(
    operation: &Operated<'_>,
    violations: &[(Violation, SubmittedEcho)],
) -> Option<ValidationException> {
    if violations.is_empty() {
        return None;
    }
    let clauses: Vec<String> = violations
        .iter()
        .map(|(violation, echo)| clause(operation, violation, echo))
        .collect();
    let message = match clauses.len() {
        1 => format!("1 validation error detected. {}", clauses[0]),
        n => format!("{n} validation errors detected. {}", clauses.join("; ")),
    };
    let field_list = violations
        .iter()
        .zip(clauses)
        .map(|((violation, _), clause)| ValidationExceptionField {
            path: pointer(&violation.path),
            message: clause,
        })
        .collect();
    Some(ValidationException {
        message,
        field_list: Some(field_list),
    })
}

/// The `@range` and `@length` windows the model declares at one member.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Windows {
    range: Option<RangeTrait>,
    length: Option<LengthTrait>,
}

/// Whether this member declares a window at all.
impl Windows {
    fn declared(&self) -> bool {
        self.range.is_some() || self.length.is_some()
    }
}

/// The window the model declares at the member a violation at `path` is about, for a request
/// of `operation`.
///
/// Three places a member can be, and each is answered from the model rather than from a list
/// of names: a member of the operation's own input, which its input schema declares; an
/// ELEMENT of one of those members, whose own constraints codegen attaches to the collection
/// member as an [`ElementConstraintsTrait`]; and a member of a nested structure, which a
/// member schema carries no link to ([`::aws_smithy_schema::Schema::member_name`] carries
/// the member's own name and nothing of the shape it targets), so those are read off the
/// nested shapes' own schemas ([`NESTED_WINDOWS`]).
///
/// An empty window is not a wrong clause: it states the single bound the value fell outside,
/// which is the framework's own sentence.
fn window_of(operation: &Operated<'_>, path: &str) -> Windows {
    let input_schema = operation.input_schema;
    if let Some(member) = member_named(input_schema, path) {
        if let Some(windows) = member.traits().map(windows_in) {
            return windows;
        }
    }
    if let Some(element) = collection_of_element(path)
        .and_then(|collection| member_named(input_schema, collection))
        .and_then(Schema::traits)
        .and_then(|traits| {
            constraint_trait::<ElementConstraintsTrait>(traits, &ElementConstraintsTrait::TRAIT_ID)
        })
    {
        return windows_in(element.inner());
    }
    NESTED_WINDOWS
        .get(&(operation.name, nested_key(path)))
        .copied()
        .unwrap_or_default()
}

/// The key a nested member is catalogued under: the path the walk reports it at, with every
/// element POSITION replaced by [`ANY_POSITION`].
///
/// A position is a property of the request, not of the model — the same member of the third
/// element of a list declares the same bounds as the first — so it is normalised away, and
/// the catalog can be keyed by one path per member.
fn nested_key(path: &str) -> String {
    path.split('.')
        .map(|segment| {
            if !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()) {
                ANY_POSITION
            } else {
                segment
            }
        })
        .collect::<Vec<&str>>()
        .join(".")
}

/// The segment [`nested_key`] puts in place of an element's position.
const ANY_POSITION: &str = "*";

/// The member of `schema` the walk reports under `path`, or `None` when `path` names no
/// member of it (a nested member, or an element's).
fn member_named<'schema>(schema: &'schema Schema, path: &str) -> Option<&'schema Schema> {
    schema
        .members()
        .iter()
        .copied()
        .find(|member| {
            member
                .member_name()
                .is_some_and(|name| constraints::path::member(name) == path)
        })
}

/// The collection member a path names an ELEMENT of (`tagKeys.2.member` -> `tagKeys`), or
/// `None` for a path that names no element.
fn collection_of_element(path: &str) -> Option<&str> {
    let (head, position) = path
        .strip_suffix(COLLECTION_STEP)?
        .strip_suffix('.')?
        .rsplit_once('.')?;
    position
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(head)
}

/// The windows a trait map declares.
fn windows_in(traits: &TraitMap) -> Windows {
    Windows {
        range: constraint_trait::<RangeTrait>(traits, &RangeTrait::TRAIT_ID).copied(),
        length: constraint_trait::<LengthTrait>(traits, &LengthTrait::TRAIT_ID).copied(),
    }
}

/// One constraint trait off a trait map, downcast to the type that carries its bounds.
fn constraint_trait<'map, T: 'static>(traits: &'map TraitMap, id: &ShapeId) -> Option<&'map T> {
    traits.get(id)?.as_any().downcast_ref::<T>()
}

/// Every bounded member of a structure NESTED under one of this service's operation inputs,
/// keyed by the operation that reaches it and the path the framework's walk reports it under
/// ([`nested_key`]).
///
/// Built once off the generated schemas, so a bound that changes in the model changes here
/// with it rather than being restated. Keyed by the OPERATION as well as the path because
/// this model declares an update-only twin of each parameter structure, bounded differently
/// — `UpdateTextParameters.inlineTemplateBody` admits the empty string that clears the stored
/// value, where `TextParameters.inlineTemplateBody` requires one character — and both sit at
/// the same path under their own operation's input. A path two listed shapes of ONE operation
/// declared with different windows would be left out: no window is a smaller statement than
/// the bound that failed, never a wrong one, and
/// [`tests::every_bounded_member_of_a_nested_shape_keeps_its_window`] pins that none is.
static NESTED_WINDOWS: LazyLock<HashMap<(&'static str, String), Windows>> = LazyLock::new(|| {
    let mut declared: HashMap<(&'static str, String), Windows> = HashMap::new();
    let mut ambiguous: HashSet<(&'static str, String)> = HashSet::new();
    for (operation, prefix, schema) in nested_shapes() {
        for member in schema.members() {
            let (Some(name), Some(traits)) = (member.member_name(), member.traits()) else {
                continue;
            };
            let windows = windows_in(traits);
            if !windows.declared() {
                continue;
            }
            let key = (
                operation,
                format!("{prefix}.{}", constraints::path::member(name)),
            );
            match declared.get(&key) {
                Some(already) if *already != windows => {
                    ambiguous.insert(key);
                }
                _ => {
                    declared.insert(key, windows);
                }
            }
        }
    }
    declared.retain(|key, _| !ambiguous.contains(key));
    declared
});

/// Every structure an operation input of this service reaches BELOW its own members: the
/// operation that reaches it, the path prefix the walk reports that structure's members
/// under, and the generated schema that declares them.
///
/// ENUMERATED rather than walked: codegen attaches a member's own constraint traits to its
/// member schema, but a member schema carries no link to the schema of the shape it TARGETS,
/// so the closure cannot be reached at runtime from the input schemas. An operation absent
/// from this list is not a wrong clause, only a clause that states the bound that failed
/// rather than the whole window — and
/// [`tests::every_bounded_member_of_a_nested_shape_keeps_its_window`] pins that every bounded
/// member of the shapes listed here reaches the catalog.
///
/// Source: the nested structures of this model's operation inputs, from
/// `smithy_query(query="shape", shape="<each input>")` — the tag pair every create and the
/// tagging write carries, the passcode-policy and per-channel parameter structures of the
/// notify-code operations (with their update-only twins), and the attribute entry of
/// `CreateBrandProfileAttributes`. Every other input of this service declares no structure
/// member at all.
fn nested_shapes() -> Vec<(&'static str, &'static str, &'static Schema)> {
    use crate::ops::types as t;
    /// Where a `TagList` element's own members sit: the list member, the element's position
    /// (normalised by [`nested_key`]) and the collection step.
    const TAGS: &str = "tags.*.member";
    vec![
        ("CreateBrandProfile", TAGS, &t::Tag::SCHEMA),
        ("CreateBrandProfileFromRegistration", TAGS, &t::Tag::SCHEMA),
        ("CreateNotifyCodeConfiguration", TAGS, &t::Tag::SCHEMA),
        ("TagResource", TAGS, &t::Tag::SCHEMA),
        (
            "CreateBrandProfileAttributes",
            "attributes.*.member",
            &t::BrandProfileAttributeInput::SCHEMA,
        ),
        // The passcode-policy and per-channel parameters, which three operations reach: the
        // create under the shapes a request supplies, the send under its own per-request
        // override members, and the update under the update-only twins of them.
        (
            "CreateNotifyCodeConfiguration",
            "codeConfigurationParameters",
            &t::CodeConfigurationParameters::SCHEMA,
        ),
        (
            "CreateNotifyCodeConfiguration",
            "channelParameters",
            &t::ChannelParameters::SCHEMA,
        ),
        (
            "CreateNotifyCodeConfiguration",
            "channelParameters.text",
            &t::TextParameters::SCHEMA,
        ),
        (
            "CreateNotifyCodeConfiguration",
            "channelParameters.voice",
            &t::VoiceParameters::SCHEMA,
        ),
        (
            "CreateNotifyCodeConfiguration",
            "channelParameters.notify",
            &t::NotifyParameters::SCHEMA,
        ),
        (
            "CreateNotifyCodeConfiguration",
            "channelParameters.whatsApp",
            &t::WhatsAppParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideCodeConfigurationParameters",
            &t::CodeConfigurationParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideChannelParameters",
            &t::ChannelParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideChannelParameters.text",
            &t::TextParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideChannelParameters.voice",
            &t::VoiceParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideChannelParameters.notify",
            &t::NotifyParameters::SCHEMA,
        ),
        (
            "SendNotifyCodeVerification",
            "overrideChannelParameters.whatsApp",
            &t::WhatsAppParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "codeConfigurationParameters",
            &t::UpdateCodeConfigurationParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "channelParameters",
            &t::UpdateChannelParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "channelParameters.text",
            &t::UpdateTextParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "channelParameters.voice",
            &t::UpdateVoiceParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "channelParameters.notify",
            &t::UpdateNotifyParameters::SCHEMA,
        ),
        (
            "UpdateNotifyCodeConfiguration",
            "channelParameters.whatsApp",
            &t::UpdateWhatsAppParameters::SCHEMA,
        ),
    ]
}

/// The modelled-refusal wrapper for EVERY operation this service serves.
///
/// `ValidationException` is a COMMON error of this service — the model declares it on all 27
/// operations — so every operation states a request refused before its handler ran the same
/// way, and there is no operation whose boundary refusal may answer in the framework's
/// envelope instead. An operation added to the model later is wrapped by adding one line
/// here.
pub(super) fn modelled_validation_table<Eng: Send + Sync + 'static>(
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut refusals: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    register::<crate::ops::create_brand_profile::CreateBrandProfile<Eng>>(&mut refusals);
    register::<crate::ops::create_brand_profile_attributes::CreateBrandProfileAttributes<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::create_brand_profile_from_registration::CreateBrandProfileFromRegistration<Eng>,
    >(&mut refusals);
    register::<crate::ops::create_notify_code_configuration::CreateNotifyCodeConfiguration<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::create_registrations_from_brand_profile::CreateRegistrationsFromBrandProfile<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_brand_profile::DeleteBrandProfile<Eng>>(&mut refusals);
    register::<crate::ops::delete_brand_profile_attribute::DeleteBrandProfileAttribute<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::delete_notify_code_configuration::DeleteNotifyCodeConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::get_brand_profile::GetBrandProfile<Eng>>(&mut refusals);
    register::<crate::ops::get_brand_profile_attribute::GetBrandProfileAttribute<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::get_job::GetJob<Eng>>(&mut refusals);
    register::<crate::ops::get_notify_code_configuration::GetNotifyCodeConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::list_brand_profile_attributes::ListBrandProfileAttributes<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::list_brand_profiles::ListBrandProfiles<Eng>>(&mut refusals);
    register::<crate::ops::list_jobs::ListJobs<Eng>>(&mut refusals);
    register::<crate::ops::list_notify_code_configurations::ListNotifyCodeConfigurations<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::list_registrations_from_brand_profile::ListRegistrationsFromBrandProfile<Eng>,
    >(&mut refusals);
    register::<crate::ops::list_tags_for_resource::ListTagsForResource<Eng>>(&mut refusals);
    register::<crate::ops::send_notify_code_verification::SendNotifyCodeVerification<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::tag_resource::TagResource<Eng>>(&mut refusals);
    register::<crate::ops::untag_resource::UntagResource<Eng>>(&mut refusals);
    register::<crate::ops::update_brand_profile::UpdateBrandProfile<Eng>>(&mut refusals);
    register::<crate::ops::update_brand_profile_attribute::UpdateBrandProfileAttribute<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::update_brand_profile_from_registration::UpdateBrandProfileFromRegistration<Eng>,
    >(&mut refusals);
    register::<crate::ops::update_notify_code_configuration::UpdateNotifyCodeConfiguration<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::update_registrations_from_brand_profile::UpdateRegistrationsFromBrandProfile<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::validate_notify_code_verification::ValidateNotifyCodeVerification<Eng>>(
        &mut refusals,
    );
    refusals
}

/// Register one operation's modelled-refusal wrapper under the key the model names that
/// operation by, read off the operation itself so the table cannot be keyed by a hand-typed
/// name that drifts from the model.
fn register<Op: Operation>(refusals: &mut HashMap<&'static str, Box<dyn ErasedOperation>>) {
    refusals.insert(Op::NAME, Box::new(ModelledValidation::<Op>::new()));
}

/// One modelled operation, served so that a request refused BEFORE its handler runs is
/// answered in this service's modelled `ValidationException` — with the `fieldList` the
/// model declares — rather than in the framework's refusal body, which carries an error code
/// and a `message` and nothing else.
///
/// Zero-sized: the wrapper holds no engine and no operation instance. It carries the
/// operation TYPE, which is all it needs — the input shape to read the request into and the
/// input `Schema` to read it against — and hands a request the model accepts to the one
/// operation instance the generated routing table built from the shared engine. There is
/// therefore no second instance of any operation and no second store.
///
/// A request this wrapper does not refuse is READ TWICE, once here and once in the
/// operation's own sequence, because the framework hands no already-built input to an
/// operation. That cost is deliberate and bounded by the body limits the framework already
/// enforces; it is what buys the refusal a shape only the handler side could otherwise have.
struct ModelledValidation<Op> {
    operation: PhantomData<fn() -> Op>,
}

impl<Op> ModelledValidation<Op> {
    const fn new() -> Self {
        Self {
            operation: PhantomData,
        }
    }
}

impl<Op: Operation> ErasedOperation for ModelledValidation<Op> {
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
                // The framework's own two steps, in its order: read the request into the
                // operation's input shape, then walk the constraint traits over what was
                // read. A failure that is not a refusal — a body the codec cannot read at
                // all — yields no violation and keeps the framework's own rendering. Each
                // violation comes paired with how a clause can state the value it is about,
                // which is where a `@length` clause reads the length it states from.
                let violations =
                    ::service_common::validation::refusing_violations_with_echoes::<Op>(
                        protocol, req,
                    );
                let operated = Operated {
                    name: Op::NAME,
                    input_schema: Op::INPUT_SCHEMA,
                };
                match boundary_exception(&operated, &violations) {
                    Some(modelled) => match protocol.serialize_error(
                        &modelled,
                        &crate::ops::types::SCHEMA_VALIDATION_EXCEPTION,
                        VALIDATION_EXCEPTION,
                        VALIDATION_EXCEPTION_STATUS,
                    ) {
                        Ok(response) => Some(response),
                        // A refusal that will not serialize leaves the request to the
                        // framework, which refuses it in its own envelope — the very
                        // divergence this wrapper removes, so it is logged rather than
                        // passed over silently.
                        Err(error) => {
                            tracing::warn!(
                                operation = Op::NAME,
                                %error,
                                "this service's modelled refusal would not serialize; it \
                                 falls back to the framework's own envelope"
                            );
                            None
                        }
                    },
                    None => None,
                }
            };
            if let Some(response) = refused {
                // The row the framework sequence would have recorded for this request, so a
                // refused one is not a hole in the telemetry series.
                record_refusal::<Op>(ctx, started.elapsed());
                return response;
            }
            // Nothing to state: the request is served by the generated operation, which runs
            // the whole framework sequence (deserialize, validate, handle, serialize) plus
            // its telemetry over the same bytes. Reached through the routing table rather
            // than through `override_operation`, which would route straight back here.
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the key the
                // generated table registered. Answer the framework's own no-such-operation
                // response rather than inventing one.
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

/// Record the refusal answered above as the operation row the framework sequence would have
/// recorded, so a request refused by the wrapper is not a hole in the telemetry series: the
/// same classification (from the refusal's status and error shape), the same folded handle
/// (this service's signing name and the operation's own name), and the same already-sanitized
/// request metadata. Everything is gated on a client collecting, exactly as the framework
/// gates it.
fn record_refusal<Op: Operation>(ctx: &OperationContext<'_>, latency: std::time::Duration) {
    if !emulator_telemetry::recording_active() {
        return;
    }
    let handle =
        emulator_telemetry::OperationHandle::folded(ctx.identity().signing_name_static(), Op::NAME);
    emulator_telemetry::record_generated_operation_with_handle(
        &handle,
        latency,
        Some(emulator_telemetry::OperationFailure::classify(
            VALIDATION_EXCEPTION_STATUS,
            VALIDATION_EXCEPTION,
        )),
        smithy_server_runtime::RequestTelemetry::from_headers(&ctx.parts().headers).into_metadata(),
    );
}

#[cfg(test)]
mod tests {
    //! The parts of this renderer that need no request: the clause one violation states, the
    //! envelope over several, and the window catalog this model declares. The end-to-end
    //! half — which body a refused request receives over the wire — is
    //! `tests/all/brand_profile_refusal_bodies.rs`.

    use super::*;

    /// `CreateNotifyCodeConfiguration` as the renderer names it: the operation whose input
    /// declares both a top-level bounded member and the nested `codeConfigurationParameters`
    /// the window catalog is read for.
    fn notify_create() -> Operated<'static> {
        Operated {
            name: <crate::ops::create_notify_code_configuration::CreateNotifyCodeConfiguration<
                ::engine::InMemoryEngine,
            > as Operation>::NAME,
            input_schema:
                <crate::ops::create_notify_code_configuration::CreateNotifyCodeConfiguration<
                    ::engine::InMemoryEngine,
                > as Operation>::INPUT_SCHEMA,
        }
    }

    /// `UpdateNotifyCodeConfiguration`, whose nested parameter structures sit at the SAME
    /// paths as the create's and are bounded differently.
    fn notify_update() -> Operated<'static> {
        Operated {
            name: <crate::ops::update_notify_code_configuration::UpdateNotifyCodeConfiguration<
                ::engine::InMemoryEngine,
            > as Operation>::NAME,
            input_schema:
                <crate::ops::update_notify_code_configuration::UpdateNotifyCodeConfiguration<
                    ::engine::InMemoryEngine,
                > as Operation>::INPUT_SCHEMA,
        }
    }

    /// A clause names the member as the JSON pointer into the request body, states the
    /// requirement, and carries no value of the caller's — only, for a `@length`, the length
    /// that check counted.
    #[test]
    fn a_clause_addresses_the_member_as_a_pointer_and_echoes_no_value() {
        // The rows differ along what the rule generalises over: the depth of the member, the
        // kind of step into it, the class of constraint it broke, and — for the length class
        // — which rendering the length is read from.
        let rows: [(&str, &str, ViolationKind, SubmittedEcho, &str); 7] = [
            (
                "a top-level member outside its pattern",
                "registrationId",
                ViolationKind::Pattern("^[A-Za-z0-9_:/-]+$".to_string()),
                SubmittedEcho::Rendered("a value the caller sent".to_string()),
                "Value at '/registrationId' failed to satisfy constraint: Member must satisfy \
                 regular expression pattern: ^[A-Za-z0-9_:/-]+$",
            ),
            (
                "an absent required member",
                "brandProfileName",
                ViolationKind::MissingRequiredMember,
                SubmittedEcho::Absent,
                "Value at '/brandProfileName' failed to satisfy constraint: Member must not be \
                 null",
            ),
            (
                "a member of a nested structure",
                "codeConfigurationParameters.codeLength",
                ViolationKind::MaxValue("8".to_string()),
                SubmittedEcho::Rendered("9".to_string()),
                "Value at '/codeConfigurationParameters/codeLength' failed to satisfy \
                 constraint: Member must be between 4 and 8, inclusive",
            ),
            (
                "a member of a list element",
                "tags.2.member.key",
                ViolationKind::MissingRequiredMember,
                SubmittedEcho::Absent,
                "Value at '/tags/1/key' failed to satisfy constraint: Member must not be null",
            ),
            (
                "a string past its length maximum: the length it was measured at",
                "notifyCodeConfigurationName",
                ViolationKind::MaxLength(64),
                SubmittedEcho::Rendered("n".repeat(65)),
                "Value with length 65 at '/notifyCodeConfigurationName' failed to satisfy \
                 constraint: Member must have length between 1 and 64, inclusive",
            ),
            (
                "a nested string below its length minimum",
                "channelParameters.text.inlineTemplateBody",
                ViolationKind::MinLength(1),
                SubmittedEcho::Rendered(String::new()),
                "Value with length 0 at '/channelParameters/text/inlineTemplateBody' failed to \
                 satisfy constraint: Member must have length between 1 and 6000, inclusive",
            ),
            (
                "a value no rendering at hand measures: no length is stated",
                "notifyCodeConfigurationName",
                ViolationKind::MaxLength(64),
                SubmittedEcho::Unrendered,
                "Value at '/notifyCodeConfigurationName' failed to satisfy constraint: Member \
                 must have length between 1 and 64, inclusive",
            ),
        ];

        for (case, path, kind, echo, expected) in rows {
            let violation = Violation {
                path: path.to_string(),
                kind,
                // A value the walk recorded is never stated: the clause is about the member,
                // the rule, and at most how long the value was.
                value: None,
                sensitive: false,
            };
            let actual = clause(&notify_create(), &violation, &echo);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A member the model bounds on both sides states the whole interval, whichever side the
    /// value fell outside; one bounded on a single side keeps the framework's own sentence.
    #[test]
    fn a_bounded_member_states_the_window_the_model_declares() {
        // Sources: `smithy_query(query="shape",shape="CodeConfigurationParameters,\
        // TextParameters,UpdateTextParameters,CreateNotifyCodeConfigurationInput")`. The rows
        // differ along where the member sits, which side of its window the value fell outside,
        // whether the bound is on a value or a length, and whether the model bounds it at all.
        let rows: [(&str, Operated<'static>, &str, ViolationKind, &str); 7] = [
            (
                "a nested member past its maximum",
                notify_create(),
                "codeConfigurationParameters.codeLength",
                ViolationKind::MaxValue("8".to_string()),
                "Member must be between 4 and 8, inclusive",
            ),
            (
                "the same member below its minimum: the same window",
                notify_create(),
                "codeConfigurationParameters.codeLength",
                ViolationKind::MinValue("4".to_string()),
                "Member must be between 4 and 8, inclusive",
            ),
            (
                "another bounded member of the same nested shape",
                notify_create(),
                "codeConfigurationParameters.validityPeriodMinutes",
                ViolationKind::MaxValue("60".to_string()),
                "Member must be between 1 and 60, inclusive",
            ),
            (
                "a member two structures deep",
                notify_create(),
                "channelParameters.text.inlineTemplateBody",
                ViolationKind::MinLength(1),
                "Member must have length between 1 and 6000, inclusive",
            ),
            (
                "the same path under the update, whose own twin shape clears the value",
                notify_update(),
                "channelParameters.text.inlineTemplateBody",
                ViolationKind::MaxLength(6000),
                "Member must have length between 0 and 6000, inclusive",
            ),
            (
                "a top-level member of this operation's own input",
                notify_create(),
                "notifyCodeConfigurationName",
                ViolationKind::MaxLength(64),
                "Member must have length between 1 and 64, inclusive",
            ),
            (
                "a member the model declares no window for",
                notify_create(),
                "somethingUnbounded",
                ViolationKind::MaxValue("8".to_string()),
                "Member must have value less than or equal to 8",
            ),
        ];

        for (case, operated, path, kind, expected) in rows {
            let violation = Violation {
                path: path.to_string(),
                kind,
                value: None,
                sensitive: false,
            };
            let actual = requirement(&operated, &violation);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The envelope counts the clauses, closes the count with a period, and states each
    /// clause once in `message` and once as its own `fieldList` entry.
    #[test]
    fn the_envelope_counts_its_clauses_and_lists_every_field() {
        assert!(
            boundary_exception(&notify_create(), &[]).is_none(),
            "case \"no violation\": there is nothing to state"
        );
        let violations: Vec<(Violation, SubmittedEcho)> =
            ["notifyCodeConfigurationName", "clientToken"]
                .into_iter()
                .map(|path| {
                    (
                        Violation {
                            path: path.to_string(),
                            kind: ViolationKind::MissingRequiredMember,
                            value: None,
                            sensitive: false,
                        },
                        SubmittedEcho::Absent,
                    )
                })
                .collect();

        let one = boundary_exception(&notify_create(), &violations[..1])
            .expect("one violation words a refusal");
        assert_eq!(
            one.message,
            "1 validation error detected. Value at '/notifyCodeConfigurationName' failed to \
             satisfy constraint: Member must not be null",
            "case \"one violation\": got {:?}",
            one.message
        );
        let listed = one.field_list.expect("one violation lists its field");
        assert_eq!(
            listed.len(),
            1,
            "case \"one violation\": expected one entry, got {listed:?}"
        );
        assert_eq!(
            (listed[0].path.as_str(), listed[0].message.as_str()),
            (
                "/notifyCodeConfigurationName",
                "Value at '/notifyCodeConfigurationName' failed to satisfy constraint: Member \
                 must not be null"
            ),
            "case \"one violation\": got {:?}",
            listed[0]
        );

        let two = boundary_exception(&notify_create(), &violations)
            .expect("two violations word a refusal");
        assert!(
            two.message.starts_with("2 validation errors detected. ") && two.message.contains("; "),
            "case \"two violations\": the count pluralises and the clauses join, got {:?}",
            two.message
        );
        let listed = two.field_list.expect("two violations list both fields");
        assert_eq!(
            listed
                .iter()
                .map(|field| field.path.as_str())
                .collect::<Vec<&str>>(),
            vec!["/notifyCodeConfigurationName", "/clientToken"],
            "case \"two violations\": every violated member is listed, got {listed:?}"
        );
    }

    /// Every bounded member of a listed nested shape keeps its window: no path of one
    /// operation is declared twice with DIFFERENT bounds, which [`NESTED_WINDOWS`] would drop
    /// as ambiguous.
    ///
    /// This is what makes a window clause reachable for every bounded nested member rather
    /// than only for the ones a suite happens to name. A model change that reused a path under
    /// one operation with other bounds would fail here instead of silently degrading that
    /// member's clause to the single bound that failed.
    #[test]
    fn every_bounded_member_of_a_nested_shape_keeps_its_window() {
        let mut bounded: Vec<(&'static str, String)> = Vec::new();
        for (operation, prefix, schema) in nested_shapes() {
            for member in schema.members() {
                let (Some(name), Some(traits)) = (member.member_name(), member.traits()) else {
                    continue;
                };
                if windows_in(traits).declared() {
                    bounded.push((
                        operation,
                        format!("{prefix}.{}", constraints::path::member(name)),
                    ));
                }
            }
        }
        assert!(
            !bounded.is_empty(),
            "case \"the nested shapes\": the listed shapes declare bounded members, got none"
        );
        let dropped: Vec<&(&'static str, String)> = bounded
            .iter()
            .filter(|key| !NESTED_WINDOWS.contains_key(*key))
            .collect();
        assert!(
            dropped.is_empty(),
            "case \"an ambiguous member path\": every bounded member of the nested shapes keeps \
             its window, yet {dropped:?} declare one path with different bounds"
        );
    }
}
