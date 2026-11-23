// SPDX-License-Identifier: CC0-1.0

//! @op PutParameter
//! Create or update a parameter in Parameter Store — the write half of this slice. This
//! is the ONLY file to touch for this operation.
//!
//! What it persists is read back by `GetParameter`; both reach the one store in
//! [`crate::state`]. A put against a name that already exists is refused unless
//! `Overwrite` is set, and an overwrite APPENDS a version rather than replacing the
//! stored one, so an earlier value stays reachable through a `name:version` selector
//! until the documented 100-version retention displaces it.
//!
//! Implementing this operation also closes `issues/.../ssm-putparameter-stub.md`, which
//! asks for exactly this handler; `GetParameter` needs it to have anything to read.
//!
//! ## A `SecureString` is stored encrypted
//!
//! A `SecureString`'s `Value` is "encrypt[ed] ... with the KMS ID that you want to use"
//! before it is persisted, so what this store holds for such a version is the KMS
//! ciphertext and not the value the request carried: the sealing is
//! [`crate::state::secure_string::seal`], under the key `KeyId` names (the Amazon Web
//! Services managed key of this service when it names none) and bound to this parameter
//! by the encryption context. A `KeyId` that names no key at this endpoint therefore
//! refuses the put with `InvalidKeyId` rather than storing the value in the clear, and so
//! does one naming a key whose KEY STATE runs no cryptographic operation — a key that is
//! disabled or awaiting deletion or key material seals nothing, so the put is refused with
//! the refusal KMS answered and the parameter keeps the versions it had.
//!
//! ## Parameter policies
//!
//! A `Policies` document is validated against the supported policy types, their
//! documented limits and the sets they may form (an `ExpirationNotification` stands only
//! beside the `Expiration` whose time it notifies about) and is persisted with the
//! version, but no policy EFFECT is materialised: an `Expiration` whose timestamp has
//! passed does not delete the parameter here, and no `ExpirationNotification` event is
//! raised. Those are the parameter-policy behaviours, and nothing in this emulator
//! fabricates an event for work that did not run — so an accepted policy means
//! "recorded", not "acting".
//!
//! The document belongs to the PARAMETER, not to one request: "All existing policies are
//! preserved until you send new policies or an empty policy" (the `Policies` doc), so an
//! update that sends none carries the stored document onto the new version and only one
//! that sends an array replaces it. `KeyId` is the same: an update of a `SecureString`
//! that names none seals the new value under the key the parameter already carries.
//!
//! ## Tags belong to the parameter
//!
//! A parameter's tag set is the PARAMETER's, not a version's: nothing in the model ties one
//! to a version, and "to add tags to an existing Systems Manager parameter, use the
//! AddTagsToResource operation" (the `Tags` doc). So a put writes tags only where it CREATES
//! the parameter, and a put that supplies none — the member absent, or present and carrying
//! nothing, which is what "optional metadata" supplies — leaves the set exactly as it was.
//! A put that supplies tags beside `Overwrite` is refused outright
//! ([`TAGS_WITH_OVERWRITE_REJECTION`]), before anything is read or written, so neither an
//! existing parameter's value, versions and tags nor a new parameter come of it.
//!
//! ## A write is announced on the account's event bus
//!
//! Parameter Store is an event source as well as a store: a put that creates a parameter or
//! stores another version of one publishes the change it made onto the account's default
//! EventBridge bus, as `aws.ssm` / "Parameter Store Change"
//! ([`crate::state::parameter_events`]), so a rule scoped to that source and detail-type
//! reaches its targets with the parameter's name, type, the operation performed and its
//! description. The event is published from what the write PERSISTED, after the store's lock
//! is released, and a put this operation refused changes nothing and publishes nothing.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `PutParameter::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `PutParameter`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! Create or update a parameter in Parameter Store. Parameter Store throughput defines the
//! number of API transactions per second (TPS) that Systems Manager can process. This
//! applies to GetParameter, GetParameters, and PutParameter API calls for your Amazon Web
//! Services account and Amazon Web Services Region. By default, Parameter Store is
//! configured with a standard throughput quota suitable for low- to moderate-volume
//! workloads. Applications that retrieve configuration data infrequently or operate at
//! smaller scale can use this default setting without additional cost. For higher-volume
//! workloads, you can enable higher throughput. This increases the maximum number of
//! supported transactions per second for your account and Region. Increased throughput
//! supports applications and workloads that need concurrent access to multiple parameters.
//! If you experience ThrottlingException: Rate exceeded errors, enable higher throughput.
//! For more information, see Changing Parameter Store throughput.
//!
//! ## Input
//!
//! * `AllowedPattern` (AllowedPattern, len 0..1024) — A regular expression used to validate
//!   the parameter value. For example, for String types with values restricted to numbers,
//!   you can specify the following: AllowedPattern=^\d+$
//! * `DataType` (ParameterDataType, len 0..128) — The data type for a String parameter.
//!   Supported data types include plain text and Amazon Machine Image (AMI) IDs. The
//!   following data type values are supported. text aws:ec2:image aws:ssm:integration When
//!   you create a String parameter and specify aws:ec2:image, Amazon Web Services Systems
//!   Manager validates the parameter value is in the required format, such as
//!   ami-12345abcdeEXAMPLE, and that the specified AMI is available in your Amazon Web
//!   Services account. If the action is successful, the service sends back an HTTP 200
//!   response which indicates a successful PutParameter call for all cases except for data
//!   type aws:ec2:image. If you call PutParameter with aws:ec2:image data type, a successful
//!   …
//! * `Description` (ParameterDescription, len 0..1024) — Information about the parameter that
//!   you want to add to the system. Optional but recommended. Don't enter personally
//!   identifiable information in this field.
//! * `KeyId` (ParameterKeyId, len 1..256, pattern ^([a-zA-Z0-9:/_-]+)$) — The Key Management
//!   Service (KMS) ID that you want to use to encrypt a parameter. Use a custom key for
//!   better security. Required for parameters that use the SecureString data type. If you
//!   don't specify a key ID, the system uses the default key associated with your Amazon Web
//!   Services account, which is not as secure as using a custom key. To use a custom KMS key,
//!   choose the SecureString data type with the Key ID parameter.
//! * `Name` (PSParameterName, len 1..2048) **required** — The fully qualified name of the
//!   parameter that you want to create or update. You can't enter the Amazon Resource Name
//!   (ARN) for a parameter, only the parameter name itself. The fully qualified name includes
//!   the complete hierarchy of the parameter path and name. For parameters in a hierarchy,
//!   you must include a leading forward slash character (/) when you create or reference a
//!   parameter. For example: /Dev/DBServer/MySQL/db-string13 Naming Constraints: Parameter
//!   names are case sensitive. A parameter name must be unique within an Amazon Web Services
//!   Region A parameter name can't be prefixed with "aws" or "ssm" (case-insensitive).
//!   Parameter names can include only the following symbols and …
//! * `Overwrite` (Boolean, default false) — Overwrite an existing parameter. The default
//!   value is false.
//! * `Policies` (ParameterPolicies, len 1..4096) — One or more policies to apply to a
//!   parameter. This operation takes a JSON array. Parameter Store, a tool in Amazon Web
//!   Services Systems Manager supports the following policy types: Expiration: This policy
//!   deletes the parameter after it expires. When you create the policy, you specify the
//!   expiration date. You can update the expiration date and time by updating the policy.
//!   Updating the parameter doesn't affect the expiration date and time. When the expiration
//!   time is reached, Parameter Store deletes the parameter. ExpirationNotification: This
//!   policy initiates an event in Amazon CloudWatch Events that notifies you about the
//!   expiration. By using this policy, you can receive notification before …
//! * `Tags` (TagList, len 0..1000) — Optional metadata that you assign to a resource. Tags
//!   enable you to categorize a resource in different ways, such as by purpose, owner, or
//!   environment. For example, you might want to tag a Systems Manager parameter to identify
//!   the type of resource to which it applies, the environment, or the type of configuration
//!   data referenced by the parameter. In this case, you could specify the following
//!   key-value pairs: Key=Resource,Value=S3bucket Key=OS,Value=Windows
//!   Key=ParameterType,Value=LicenseKey To add tags to an existing Systems Manager parameter,
//!   use the AddTagsToResource operation.
//! * `Tier` (ParameterTier, one of: Advanced | Intelligent-Tiering | Standard) — The
//!   parameter tier to assign to a parameter. Parameter Store offers a standard tier and an
//!   advanced tier for parameters. Standard parameters have a content size limit of 4 KB and
//!   can't be configured to use parameter policies. You can create a maximum of 10,000
//!   standard parameters for each Region in an Amazon Web Services account. Standard
//!   parameters are offered at no additional cost. Advanced parameters have a content size
//!   limit of 8 KB and can be configured to use parameter policies. You can create a maximum
//!   of 100,000 advanced parameters for each Region in an Amazon Web Services account.
//!   Advanced parameters incur a charge. For more information, see Managing parameter tiers
//!   in the Amazon …
//! * `Type` (ParameterType, one of: SecureString | String | StringList) — The type of
//!   parameter that you want to create. SecureString isn't currently supported for
//!   CloudFormation templates. Items in a StringList must be separated by a comma (,). You
//!   can't use other punctuation or special character to escape items in the list. If you
//!   have a parameter value that requires a comma, then use the String data type. Specifying
//!   a parameter type isn't required when updating a parameter. You must specify a parameter
//!   type when creating a parameter.
//! * `Value` (PSParameterValue) **required** — The parameter value that you want to add to
//!   the system. Standard parameters have a value limit of 4 KB. Advanced parameters have a
//!   value limit of 8 KB. Parameters can't be referenced or nested in the values of other
//!   parameters. You can't include values wrapped in double brackets {{}} or
//!   {{ssm:parameter-name}} in a parameter value.
//!
//! ## Output
//!
//! * `Tier` (ParameterTier, one of: Advanced | Intelligent-Tiering | Standard) — The tier
//!   assigned to the parameter.
//! * `Version` (PSParameterVersion, default 0) **required** — The new version number of a
//!   parameter. If you edit a parameter value, Parameter Store automatically creates a new
//!   version and assigns this new version a unique ID. You can reference a parameter version
//!   ID in API operations or in Systems Manager documents (SSM documents). By default, if you
//!   don't specify a specific version, the system returns the latest parameter value when a
//!   parameter is called.
//!
//! ## Errors
//!
//! * `HierarchyLevelLimitExceededException` (HTTP 400) — A hierarchy can have a maximum of 15
//!   levels. For more information, see Requirements and constraints for parameter names in
//!   the Amazon Web Services Systems Manager User Guide.
//! * `HierarchyTypeMismatchException` (HTTP 400) — Parameter Store doesn't support changing a
//!   parameter type in a hierarchy. For example, you can't change a parameter from a String
//!   type to a SecureString type. You must create a new, unique parameter.
//! * `IncompatiblePolicyException` (HTTP 400) — There is a conflict in the policies specified
//!   for this parameter. You can't, for example, specify two Expiration policies for a
//!   parameter. Review your policies, and try again.
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `InvalidAllowedPatternException` (HTTP 400) — The request doesn't meet the regular
//!   expression requirement.
//! * `InvalidKeyId` (HTTP 400) — The query key ID isn't valid.
//! * `InvalidPolicyAttributeException` (HTTP 400) — A policy attribute or its value is
//!   invalid.
//! * `InvalidPolicyTypeException` (HTTP 400) — The policy type isn't supported. Parameter
//!   Store supports the following policy types: Expiration, ExpirationNotification, and
//!   NoChangeNotification.
//! * `ParameterAlreadyExists` (HTTP 400) — The parameter already exists. You can't create
//!   duplicate parameters.
//! * `ParameterLimitExceeded` (HTTP 400) — You have exceeded the number of parameters for
//!   this Amazon Web Services account. Delete one or more parameters and try again.
//! * `ParameterMaxVersionLimitExceeded` (HTTP 400) — Parameter Store retains the 100 most
//!   recently created versions of a parameter. After this number of versions has been
//!   created, Parameter Store deletes the oldest version when a new one is created. However,
//!   if the oldest version has a label attached to it, Parameter Store won't delete the
//!   version and instead presents this error message: An error occurred
//!   (ParameterMaxVersionLimitExceeded) when calling the PutParameter operation: You
//!   attempted to create a new version of parameter-name by calling the PutParameter API with
//!   the overwrite flag. Version version-number, the oldest version, can't be deleted because
//!   it has a label associated with it. Move the label to another version of the …
//! * `ParameterPatternMismatchException` (HTTP 400) — The parameter name isn't valid.
//! * `PoliciesLimitExceededException` (HTTP 400) — You specified more than the maximum number
//!   of allowed policies for the parameter. The maximum is 10.
//! * `TooManyUpdates` (HTTP 400) — There are concurrent updates for a resource that supports
//!   one update at a time.
//! * `UnsupportedParameterType` (HTTP 400) — The parameter type isn't supported.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `PutParameterError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `PutParameterError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
// `AccessDeniedException` is widened in through `extra_errors`: this SERVICE models it
// ("The requester doesn't have permissions to perform the requested operation") and the
// other calls that CHANGE a parameter declare it, and it is the refusal a write addressed
// to the hierarchy AWS reserves for its own published parameters answers with — the same
// denial `DeleteParameter` and the label calls give for a name in it.
#[smithy_macros::operation_error(
    "com.amazonaws.ssm#PutParameter",
    extra_errors = ["AccessDeniedException"]
)]
#[derive(Debug)]
pub enum PutParameterErrorModeled {
    AccessDeniedException(AccessDeniedException),
    HierarchyLevelLimitExceededException(HierarchyLevelLimitExceededException),
    HierarchyTypeMismatchException(HierarchyTypeMismatchException),
    IncompatiblePolicyException(IncompatiblePolicyException),
    InternalServerError(InternalServerError),
    InvalidAllowedPatternException(InvalidAllowedPatternException),
    InvalidKeyId(InvalidKeyId),
    InvalidPolicyAttributeException(InvalidPolicyAttributeException),
    InvalidPolicyTypeException(InvalidPolicyTypeException),
    ParameterAlreadyExists(ParameterAlreadyExists),
    ParameterLimitExceeded(ParameterLimitExceeded),
    ParameterMaxVersionLimitExceeded(ParameterMaxVersionLimitExceeded),
    ParameterPatternMismatchException(ParameterPatternMismatchException),
    PoliciesLimitExceededException(PoliciesLimitExceededException),
    TooManyUpdates(TooManyUpdates),
    UnsupportedParameterType(UnsupportedParameterType),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::put_parameter::PutParameterError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type PutParameterError = crate::scaffolded::Scaffolded<PutParameterErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<PutParameterErrorModeled> for PutParameterError {
    fn from(modeled: PutParameterErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// What the store already holds for the name a put names, read once so the refusals
/// below need no second lookup.
#[derive(Debug)]
struct Existing {
    /// The stored parameter's current `Type` — what an update that names none inherits.
    type_: Option<String>,
    /// Its current `Tier`, which an update may raise but not lower.
    tier: Option<String>,
    /// The `Policies` document its newest version holds — what an update that sends none
    /// inherits: "All existing policies are preserved until you send new policies or an
    /// empty policy" (`PutParameter`'s `Policies` doc).
    policies: Option<String>,
    /// Its current `DataType`, `Description` and `AllowedPattern`: the attributes an
    /// update that names none inherits, exactly as it inherits the type above.
    data_type: Option<String>,
    description: Option<String>,
    allowed_pattern: Option<String>,
    /// The KMS key its newest version is encrypted under — what an update of a
    /// `SecureString` that names no `KeyId` keeps writing under, so a value stored under a
    /// customer managed key is not re-keyed to the service default by a value-only update.
    key_id: Option<String>,
    /// How many versions are retained right now.
    retained: usize,
    /// The oldest retained version's number when that version carries a label, which
    /// stops it from being displaced — and is the version the refusal names.
    labelled_oldest: Option<i64>,
}

/// How large an `AllowedPattern` may compile to, in bytes of compiled program, and how
/// large the lazy-DFA cache it may use.
///
/// Emulator resource guards: the expression comes from the request, so the automaton it
/// builds and the cache it matches through are bounded explicitly rather than left to
/// the crate's defaults — a cache that cannot be thrashed into the slower engine. A
/// pattern past the first is refused as one this service cannot evaluate. The haystack
/// is bounded too: `PutParameter` refuses a `Value` larger than the content size limit
/// of the tier it resolved before this runs, so neither factor of the matching cost is
/// caller-unbounded.
const COMPILED_PATTERN_LIMIT: usize = 64 * 1024;

/// See [`COMPILED_PATTERN_LIMIT`].
const PATTERN_DFA_LIMIT: usize = 64 * 1024;

/// What a put that carries both `Overwrite` and tags is refused with: entirely fixed text,
/// with no part of it taken from the request.
///
/// `PutParameter` models no error for the conflict, so this is the service-common
/// `ValidationException`. The sentence names the two remedies the service offers — drop the
/// flag to create a parameter with tags, or change an existing parameter's tags through the
/// tag plane — which is the same reading of the `Tags` doc the refusal is taken on ("To add
/// tags to an existing Systems Manager parameter, use the AddTagsToResource operation").
const TAGS_WITH_OVERWRITE_REJECTION: &str = "Invalid request: tags and overwrite can't be used \
                                             together. To create a parameter with tags, please \
                                             remove overwrite flag. To update tags for an \
                                             existing parameter, please use AddTagsToResource or \
                                             RemoveTagsFromResource.";

/// Why the `Policies` document is one Parameter Store will not take, named by the
/// modelled error that reports it.
#[derive(Debug)]
enum PolicyRejection {
    /// Not a policy document at all: a `ValidationException`, since the model has no
    /// error for a malformed document.
    Shape(String),
    /// `InvalidPolicyTypeException`.
    Type(String),
    /// `InvalidPolicyAttributeException`.
    Attribute(String),
    /// `IncompatiblePolicyException`.
    Incompatible(String),
    /// `PoliciesLimitExceededException`.
    Limit(String),
}

/// How many policies the `Policies` document carries, or why it is refused.
///
/// `Policies` is "one or more policies to apply to a parameter. This operation takes a
/// JSON array", and the rules applied here are the ones the modelled policy errors
/// state: the supported policy types (`InvalidPolicyTypeException`), at most
/// [`crate::state::MAX_POLICIES`] of them (`PoliciesLimitExceededException`), and an
/// `Attributes` value that is a policy attribute map
/// (`InvalidPolicyAttributeException`) — plus the two ways a SET of policies conflicts
/// with itself, which `IncompatiblePolicyException` reports: a type that configures ONE
/// thing about the parameter stated twice ("You can't, for example, specify two Expiration
/// policies for a parameter", [`crate::state::policy_type_may_repeat`] — a notification
/// type repeats, because a parameter may be notified about one event more than once), and a
/// notification about an expiration with no expiration to notify about — an
/// `ExpirationNotification` is "notification before or after the expiration time is
/// reached", and only an `Expiration` policy sets that time, so the two stand or fall
/// together and a document carrying the notification alone configures a notification that
/// can never fire.
///
/// What each policy then DOES — deleting the parameter when an `Expiration` comes due,
/// raising the notification events — is not implemented: those are the parameter-policy
/// behaviours, not this operation's, and nothing here fabricates an event for work that
/// did not run. The document is validated and persisted with the version.
fn policy_count(policies: Option<&str>) -> Result<usize, PolicyRejection> {
    let Some(policies) = policies.filter(|policies| !policies.trim().is_empty()) else {
        return Ok(0);
    };
    let parsed: ::serde_json::Value = ::serde_json::from_str(policies)
        .map_err(|why| PolicyRejection::Shape(format!("Policies isn't a JSON document: {why}")))?;
    let Some(entries) = parsed.as_array() else {
        return Err(PolicyRejection::Shape(
            "Policies must be a JSON array of policy objects".to_string(),
        ));
    };
    if entries.len() > crate::state::MAX_POLICIES {
        return Err(PolicyRejection::Limit(
            crate::state::policies_limit_message(),
        ));
    }
    let mut seen: Vec<&str> = Vec::with_capacity(entries.len());
    let mut carried = 0usize;
    for entry in entries {
        let Some(policy) = entry.as_object() else {
            return Err(PolicyRejection::Shape(
                "every entry of Policies must be a policy object".to_string(),
            ));
        };
        // "All existing policies are preserved until you send new policies or an empty
        // policy": an EMPTY policy object states no policy at all, so it is one of the two
        // ways a document asks for the parameter to carry none — the other being an empty
        // array — rather than a policy missing its type.
        if policy.is_empty() {
            continue;
        }
        let Some(policy_type) = policy.get("Type").and_then(::serde_json::Value::as_str) else {
            return Err(PolicyRejection::Type(
                "a policy carries no Type".to_string(),
            ));
        };
        if !crate::state::POLICY_TYPES.contains(&policy_type) {
            return Err(PolicyRejection::Type(
                crate::state::unsupported_policy_type_message(policy_type),
            ));
        }
        if policy
            .get("Attributes")
            .is_some_and(|attributes| !attributes.is_object())
        {
            return Err(PolicyRejection::Attribute(format!(
                "the Attributes of the {policy_type} policy is not a policy attribute map"
            )));
        }
        // A type that may not repeat configures ONE thing about the parameter, so a second
        // policy of it configures that thing twice over ("You can't, for example, specify two
        // Expiration policies for a parameter"); a notification type may repeat, because a
        // parameter may be notified about one event more than once
        // ([`crate::state::policy_type_may_repeat`]).
        if !crate::state::policy_type_may_repeat(policy_type) && seen.contains(&policy_type) {
            return Err(PolicyRejection::Incompatible(
                crate::state::duplicate_policy_message(policy_type),
            ));
        }
        seen.push(policy_type);
        carried += 1;
    }
    // A notification about an expiration needs the expiration it is measured from, so the
    // two are one configuration: a document carrying the notification without the
    // expiration asks for an event nothing can ever raise, and the whole put is refused —
    // no version is created and no parameter is.
    if seen.contains(&crate::state::POLICY_TYPE_EXPIRATION_NOTIFICATION)
        && !seen.contains(&crate::state::POLICY_TYPE_EXPIRATION)
    {
        return Err(PolicyRejection::Incompatible(
            crate::state::notification_requires_expiration_message(),
        ));
    }
    Ok(carried)
}

/// Why a value is not one the request's `AllowedPattern` accepts, named by the modelled
/// error that reports it.
///
/// The two mean opposite things, so they are never reported interchangeably: one says
/// the caller's VALUE is wrong, the other that the caller's PATTERN is.
#[derive(Debug)]
enum PatternRejection {
    /// The supplied `AllowedPattern` is not a regular expression at all:
    /// `InvalidAllowedPatternException` ("The request doesn't meet the regular
    /// expression requirement").
    Pattern(String),
    /// The value does not satisfy a well-formed `AllowedPattern`:
    /// `ParameterPatternMismatchException`.
    Value(String),
}

/// Why `value` does not satisfy `pattern`, or `None` when it does (or when the request
/// named no pattern).
///
/// `AllowedPattern` is "a regular expression used to validate the parameter value", so
/// the value is matched against it in full — the whole value, not a substring, the way
/// a regular-expression constraint on a member reads. A `StringList`'s value is not one
/// value but a list ("Items in a StringList must be separated by a comma (,)"), so the
/// constraint holds of each ITEM of it rather than of the comma-joined whole; every
/// other type, `SecureString` included, is matched as the one value it is — and a
/// `SecureString` is matched on the plaintext the request carried, before it is sealed.
///
/// The expression comes from the request, so it is evaluated by the `regex` crate, whose
/// matching is linear in the input and has no backtracking blowup, under that crate's
/// own compiled-size limit; the modelled `AllowedPattern` length (0..1024) bounds what
/// is compiled.
fn allowed_pattern_rejection(
    pattern: Option<&str>,
    type_: &str,
    value: &str,
) -> Option<PatternRejection> {
    let pattern = pattern.filter(|pattern| !pattern.is_empty())?;
    let anchored = format!("^(?:{pattern})$");
    // An explicit compiled-size budget, so a pattern inside the modelled 1024-character
    // `AllowedPattern` length still cannot compile into an arbitrarily large automaton.
    let expression = match ::regex::RegexBuilder::new(&anchored)
        .size_limit(COMPILED_PATTERN_LIMIT)
        .dfa_size_limit(PATTERN_DFA_LIMIT)
        .build()
    {
        Err(why) => {
            return Some(PatternRejection::Pattern(format!(
                "AllowedPattern '{pattern}' is not a regular expression this service can \
                 evaluate: {why}"
            )))
        }
        Ok(expression) => expression,
    };
    let satisfied = if type_ == crate::state::TYPE_STRING_LIST {
        value
            .split(crate::state::STRING_LIST_SEPARATOR)
            .all(|item| expression.is_match(item))
    } else {
        expression.is_match(value)
    };
    (!satisfied).then(|| {
        PatternRejection::Value(crate::state::allowed_pattern_mismatch_message(pattern))
    })
}

/// The `PutParameter` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct PutParameter<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> PutParameter<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for PutParameter<Eng>
{
    type Input = PutParameterInput;
    type Output = PutParameterOutput;
    type Error = PutParameterError;

    const NAME: &'static str = "PutParameter";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::PUT_PARAMETER_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::PUT_PARAMETER_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // `Name`'s length and `KeyId`'s pattern are MODELLED constraints, so the
        // centralized walk has already refused anything outside them. `Value`
        // (`PSParameterValue`) carries NO modelled length, so its size is bounded here,
        // against the limit of the tier this request resolves to — before anything
        // matches a caller-supplied pattern against it. What is checked beyond that is
        // what the model cannot say: every naming constraint, tier and quota rule the
        // operation's own documentation states.
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            PutParameterErrorModeled::InternalServerError(InternalServerError {
                message: Some("the parameter store is unavailable".to_string()),
            })
        })?;

        let name = crate::state::trimmed_name(&input.name)
            .map_err(crate::scaffolded::Scaffolded::validation)?;
        // The `/aws` hierarchy holds AWS's own published parameters and no parameter of
        // this account ([`crate::state::public_parameters`]), so a put addressing a name in
        // it asks to write data that is not the account's: the refusal is a DENIAL, as it
        // is for every other call that would change a name in that hierarchy, and which
        // sentence it carries turns on where in that hierarchy the name lies rather than on
        // this being the write ([`crate::state::public_parameters::reserved_name_refusal_message`]).
        // Decided before the naming constraints are read off the name, because what is
        // wrong with it is whose hierarchy it is in.
        if crate::state::public_parameters::is_published_name(name) {
            return Err(PutParameterErrorModeled::AccessDeniedException(
                AccessDeniedException {
                    message: crate::state::public_parameters::reserved_name_refusal_message(
                        &*self.engine,
                        &crate::state::request_region(ctx.parts()),
                        name,
                    ),
                },
            )
            .into());
        }
        match crate::state::name_rejection(name) {
            Some(crate::state::NameRejection::Pattern(why)) => {
                return Err(PutParameterErrorModeled::ParameterPatternMismatchException(
                    ParameterPatternMismatchException { message: Some(why) },
                )
                .into())
            }
            // A name carrying a level with no name in it is refused as a malformed
            // request value, before any naming constraint is read off it.
            Some(crate::state::NameRejection::Malformed(why)) => {
                return Err(crate::scaffolded::Scaffolded::validation(why))
            }
            Some(crate::state::NameRejection::Depth(why)) => {
                return Err(
                    PutParameterErrorModeled::HierarchyLevelLimitExceededException(
                        HierarchyLevelLimitExceededException { message: Some(why) },
                    )
                    .into(),
                )
            }
            None => {}
        }

        // `Type` and `Tier` are modelled enums, so the centralized walk has already
        // refused any value outside their value sets with a 400 `ValidationException`
        // before this body runs. `Tier` is re-checked here all the same, because the
        // tier decides the content-size limit below and a value the model does not
        // declare must not be served as if it were the standard tier. `DataType` is a
        // plain string, so its catalog is enforced here too.
        if let Some(requested) = input.tier.as_deref() {
            if !PARAMETER_TIER_VALUES.contains(&requested) {
                return Err(crate::scaffolded::Scaffolded::validation(format!(
                    "Tier '{requested}' isn't one of the parameter tiers {}",
                    PARAMETER_TIER_VALUES.join(", ")
                )));
            }
        }
        let data_type = input
            .data_type
            .clone()
            .filter(|data_type| !data_type.is_empty());
        if let Some(data_type) = data_type.as_deref() {
            if !crate::state::PARAMETER_DATA_TYPES.contains(&data_type) {
                return Err(crate::scaffolded::Scaffolded::validation(format!(
                    "DataType '{data_type}' isn't supported; the supported data type values are {}",
                    crate::state::PARAMETER_DATA_TYPES.join(" ")
                )));
            }
        }
        // The `aws:ec2:image` data type's own validation — "that the specified AMI is
        // available in your Amazon Web Services account" — is the one the doc describes: "a
        // successful HTTP 200 response does not guarantee that your parameter was
        // successfully created or updated … the asynchronous validation will fail and the
        // parameter will not be created or updated". So it is made below, once the data type
        // in force is resolved, and it gates the WRITE rather than the answer: the put
        // reports 200 with the version it allocated, and a value that fails it leaves the
        // parameter exactly as this put found it
        // ([`crate::state::parameter_image_validation`]).
        //
        // "Parameters can't be referenced or nested in the values of other parameters.
        // You can't include values wrapped in double brackets {{}} or
        // {{ssm:parameter-name}} in a parameter value."
        if input.value.contains("{{") {
            return Err(crate::scaffolded::Scaffolded::validation(
                "Value carries a value wrapped in double brackets: a parameter value can't \
                 reference or nest another parameter"
                    .to_string(),
            ));
        }

        let region = crate::state::request_region(ctx.parts());
        let now = ::std::time::SystemTime::now();
        // Who is writing this version. Resolved from the credential the request was
        // signed with, through the same reading `sts:GetCallerIdentity` answers from, so
        // the writer a metadata read reports back is the identity the caller holds — a
        // role session names that session, a user's own key names that user. Taken
        // before the store guard, so no guard of this service is held while another
        // service's store is read.
        let writer = crate::state::identity_authorization::caller_arn(&*self.engine, ctx);
        let overwrite = input.overwrite.unwrap_or(false);
        // The tags this request SUPPLIES. `Tags` is "optional metadata that you assign to a
        // resource", so a request that supplies none — the member absent, or present and
        // carrying nothing — asks for no tag to be written, and the parameter's own tag set
        // is left exactly as it was: the set belongs to the parameter rather than to one of
        // its versions, and `AddTagsToResource`/`RemoveTagsFromResource` are what change it.
        // Read once here, so the refusal below and the write further down cannot disagree
        // about what "supplied tags" is.
        let supplied_tags = input.tags.filter(|tags| !tags.is_empty());
        // Tags belong to the parameter, and `AddTagsToResource` is how an existing one
        // is tagged: "To add tags to an existing Systems Manager parameter, use the
        // AddTagsToResource operation." So a request may create a parameter with tags
        // or update one's value, never both at once — an update that carried tags would
        // otherwise silently replace the set. Refused before anything is read or written,
        // so an existing name keeps its value, its versions and its tags and a new one is
        // not created.
        if overwrite && supplied_tags.is_some() {
            return Err(crate::scaffolded::Scaffolded::validation(
                TAGS_WITH_OVERWRITE_REJECTION.to_string(),
            ));
        }
        let requested_policies = match policy_count(input.policies.as_deref()) {
            Ok(count) => count,
            Err(PolicyRejection::Shape(why)) => {
                return Err(crate::scaffolded::Scaffolded::validation(why))
            }
            Err(PolicyRejection::Type(why)) => {
                return Err(PutParameterErrorModeled::InvalidPolicyTypeException(
                    InvalidPolicyTypeException { message: Some(why) },
                )
                .into())
            }
            Err(PolicyRejection::Attribute(why)) => {
                return Err(PutParameterErrorModeled::InvalidPolicyAttributeException(
                    InvalidPolicyAttributeException { message: Some(why) },
                )
                .into())
            }
            Err(PolicyRejection::Incompatible(why)) => {
                return Err(PutParameterErrorModeled::IncompatiblePolicyException(
                    IncompatiblePolicyException { message: Some(why) },
                )
                .into())
            }
            Err(PolicyRejection::Limit(why)) => {
                return Err(PutParameterErrorModeled::PoliciesLimitExceededException(
                    PoliciesLimitExceededException { message: Some(why) },
                )
                .into())
            }
        };

        // What the store already holds, read under the guard and then released: a
        // `SecureString` is sealed under its KMS key below, and no state of this service
        // is held while KMS seals it.
        let (parameters_in_region, parameters_total, room_for_another, existing) = {
            let store = state.lock();
            let existing = store.parameter(&region, name).map(|record| Existing {
                type_: record.latest().map(|latest| latest.type_.clone()),
                tier: record.latest().map(|latest| latest.tier.clone()),
                policies: record.latest().and_then(|latest| latest.policies.clone()),
                data_type: record.latest().map(|latest| latest.data_type.clone()),
                description: record.latest().and_then(|latest| latest.description.clone()),
                allowed_pattern: record
                    .latest()
                    .and_then(|latest| latest.allowed_pattern.clone()),
                key_id: record.latest().and_then(|latest| latest.key_id.clone()),
                retained: record.versions.len(),
                labelled_oldest: record
                    .oldest()
                    .filter(|oldest| !oldest.labels.is_empty())
                    .map(|oldest| oldest.version),
            });
            (
                store.parameters_in_region(&region),
                store.parameters.len(),
                store.can_hold_another_parameter(),
                existing,
            )
        };

        // "All existing policies are preserved until you send new policies or an empty
        // policy" (`PutParameter`'s `Policies` doc): the member is applied only when the
        // request CARRIES it, so an update that sends none keeps the document the
        // parameter already holds and only one that sends an array — an empty array
        // included — replaces it. The inherited document was validated by the put that
        // stored it, so its policies are counted rather than judged again.
        let (stored_policies, policies) = match input.policies.clone() {
            Some(sent) => (Some(sent), requested_policies),
            None => {
                let inherited = existing.as_ref().and_then(|held| held.policies.clone());
                let carried = crate::state::inline_policies(inherited.as_deref()).len();
                (inherited, carried)
            }
        };
        let has_policies = policies > 0;

        // "The parameter already exists. You can't create duplicate parameters" —
        // unless the request asked to overwrite it. The sentence names no parameter: it
        // names the request member the caller has to set.
        if existing.is_some() && !overwrite {
            return Err(PutParameterErrorModeled::ParameterAlreadyExists(
                ParameterAlreadyExists {
                    message: Some(crate::state::PARAMETER_ALREADY_EXISTS_MESSAGE.to_string()),
                },
            )
            .into());
        }
        // "Specifying a parameter type isn't required when updating a parameter. You
        // must specify a parameter type when creating a parameter."
        let type_ = match (
            input.r#type.clone(),
            existing.as_ref().and_then(|held| held.type_.clone()),
        ) {
            // "Parameter Store doesn't support changing a parameter type in a
            // hierarchy. For example, you can't change a parameter from a String type
            // to a SecureString type. You must create a new, unique parameter."
            (Some(requested), Some(stored)) if requested != stored => {
                return Err(PutParameterErrorModeled::HierarchyTypeMismatchException(
                    HierarchyTypeMismatchException {
                        message: Some(format!(
                            "Parameter {name} is a {stored} parameter: Parameter Store doesn't \
                             support changing a parameter type to {requested}"
                        )),
                    },
                )
                .into())
            }
            (Some(requested), _) => requested,
            (None, Some(stored)) => stored,
            (None, None) => {
                return Err(crate::scaffolded::Scaffolded::validation(
                    "Type is required when creating a parameter".to_string(),
                ))
            }
        };
        // ...and so is every other attribute of the parameter the request does not carry:
        // a put that OVERWRITES one changes what it states, and what it states nothing about
        // is the parameter's already — the same inheritance the type above goes through
        // ("specifying a parameter type isn't required when updating a parameter"), which a
        // parameter's `Tier` is held to as well ([`crate::state::resolve_tier`]). A CREATE
        // has nothing to inherit, so it takes the documented default of each.
        let data_type = data_type
            .or_else(|| existing.as_ref().and_then(|held| held.data_type.clone()))
            .unwrap_or_else(|| crate::state::DEFAULT_DATA_TYPE.to_string());
        let description = input
            .description
            .clone()
            .or_else(|| existing.as_ref().and_then(|held| held.description.clone()));
        let allowed_pattern = input.allowed_pattern.clone().or_else(|| {
            existing
                .as_ref()
                .and_then(|held| held.allowed_pattern.clone())
        });
        // Whether the value this put carries passes the validation its data type is held to:
        // for `aws:ec2:image`, whether the AMI id "is available in your Amazon Web Services
        // account". The answer decides whether the write below LANDS — "the asynchronous
        // validation will fail and the parameter will not be created or updated" — and
        // nothing else: the put answers 200 with the version it allocated either way, which
        // is the whole of what the documented asynchrony makes observable
        // ([`crate::state::parameter_image_validation`]). Asked before this store's guard is
        // taken, because the catalogue it is decided against is Amazon EC2's own.
        let validated = crate::state::parameter_image_validation::validates(
            &*self.engine,
            &data_type,
            &input.value,
        );

        // A `KeyId` only means anything for a `SecureString`: "To use a custom KMS key,
        // choose the SecureString data type with the Key ID parameter." The type under
        // test is the resolved one, so an update that inherits `SecureString` may still
        // name a key.
        let secure = type_ == crate::state::TYPE_SECURE_STRING;
        if input.key_id.is_some() && !secure {
            return Err(PutParameterErrorModeled::InvalidKeyId(InvalidKeyId {
                message: Some(format!(
                    "KeyId applies only to the {} parameter type",
                    crate::state::TYPE_SECURE_STRING
                )),
            })
            .into());
        }

        let tier = crate::state::resolve_tier(
            input.tier.as_deref(),
            existing.as_ref().and_then(|held| held.tier.as_deref()),
            input.value.len(),
            has_policies,
            parameters_in_region,
        );
        // "You can change a standard parameter to an advanced parameter any time. But
        // you can't revert an advanced parameter to a standard parameter." An update
        // that names no tier stays in the one it has (see `resolve_tier`), so this
        // fires only for a request that asks for the lower tier.
        if existing
            .as_ref()
            .and_then(|held| held.tier.as_deref())
            .is_some_and(|stored| stored == crate::state::TIER_ADVANCED)
            && tier != crate::state::TIER_ADVANCED
        {
            return Err(crate::scaffolded::Scaffolded::validation(
                crate::state::tier_downgrade_message(),
            ));
        }
        let limit = crate::state::value_limit(&tier);
        if input.value.len() > limit {
            return Err(crate::scaffolded::Scaffolded::validation(
                crate::state::value_limit_message(&tier),
            ));
        }
        // The value is matched against the `AllowedPattern` IN FORCE — the request's, else
        // the one the parameter already carries — only now that it is known to be within the
        // tier's content size limit, so the haystack a caller-supplied expression is matched
        // against is bounded. A value that fails a well-formed pattern is the VALUE's fault;
        // a pattern that is no regular expression is the PATTERN's, and the two are reported
        // as the different refusals they are.
        match allowed_pattern_rejection(allowed_pattern.as_deref(), &type_, &input.value) {
            None => {}
            Some(PatternRejection::Pattern(why)) => {
                return Err(PutParameterErrorModeled::InvalidAllowedPatternException(
                    InvalidAllowedPatternException { message: Some(why) },
                )
                .into())
            }
            Some(PatternRejection::Value(why)) => {
                return Err(PutParameterErrorModeled::ParameterPatternMismatchException(
                    ParameterPatternMismatchException { message: Some(why) },
                )
                .into())
            }
        }
        // "Standard parameters ... can't be configured to use parameter policies."
        if has_policies && tier != crate::state::TIER_ADVANCED {
            return Err(crate::scaffolded::Scaffolded::validation(
                crate::state::policies_require_advanced_message(),
            ));
        }
        // Retention displaces the oldest version — but "if the oldest version has a
        // label attached to it, Parameter Store won't delete the version and instead
        // presents this error message", which names the version whose label is in the
        // way as the store holds it now.
        if let Some(oldest) = existing
            .as_ref()
            .filter(|held| held.retained >= crate::state::MAX_VERSIONS_RETAINED)
            .and_then(|held| held.labelled_oldest)
        {
            return Err(PutParameterErrorModeled::ParameterMaxVersionLimitExceeded(
                ParameterMaxVersionLimitExceeded {
                    message: Some(crate::state::max_version_limit_message(name, oldest)),
                },
            )
            .into());
        }
        // A create is refused once either bound is reached: the per-Region quota, and
        // the store's own total — the Region is a caller-chosen partition of the key
        // space, so only the total keeps this process finite.
        if existing.is_none() {
            let full = (parameters_in_region >= crate::state::MAX_PARAMETERS_PER_REGION)
                .then_some(("Region", parameters_in_region, crate::state::MAX_PARAMETERS_PER_REGION))
                .or_else(|| {
                    (!room_for_another).then_some((
                        "store",
                        parameters_total,
                        crate::state::MAX_PARAMETERS_TOTAL,
                    ))
                });
            if let Some((scope, held, maximum)) = full {
                return Err(PutParameterErrorModeled::ParameterLimitExceeded(
                    ParameterLimitExceeded {
                        message: Some(format!(
                            "this {scope} already holds {held} parameters, the maximum of {maximum}"
                        )),
                    },
                )
                .into());
            }
        }

        // "If you don't specify a key ID, the system uses the default key associated with
        // your Amazon Web Services account", which Parameter Store reaches through this
        // alias — for a parameter that carries no key yet. An UPDATE that names none keeps
        // writing under the key the parameter already carries, as it keeps every other
        // attribute it states nothing about: `KeyId` is "the KMS ID that you want to use to
        // encrypt a parameter", a property of the parameter rather than of one request, so a
        // value-only update of a `SecureString` held under a customer managed key is sealed
        // under that same key instead of being re-keyed to the service default. A type that
        // is not encrypted carries none.
        let key_id = match (secure, input.key_id) {
            (true, Some(key_id)) => Some(key_id),
            (true, None) => Some(
                existing
                    .as_ref()
                    .and_then(|held| held.key_id.clone())
                    .unwrap_or_else(|| {
                        crate::state::DEFAULT_SECURE_STRING_KEY_ID.to_string()
                    }),
            ),
            (false, _) => None,
        };
        // A `SecureString` is "the KMS ID that you want to use to encrypt a parameter"
        // applied: the value is sealed under that key, bound to this parameter by the
        // encryption context, and what this store holds for it is the CIPHERTEXT. Every
        // other type is not encrypted, so its value is stored as the request supplied it.
        let stored_value = match key_id.as_deref() {
            None => input.value,
            Some(key_id) => {
                let parameter_arn = crate::state::parameter_arn(&region, name);
                match crate::state::secure_string::seal(
                    &*self.engine,
                    &region,
                    key_id,
                    &parameter_arn,
                    &input.value,
                ) {
                    Ok(sealed) => sealed,
                    // "The query key ID isn't valid": the request named a key this
                    // endpoint has none of, so there is nothing to encrypt the value
                    // under and the put is refused rather than stored in the clear.
                    Err(crate::state::secure_string::SealRefusal::UnknownKey(why)) => {
                        return Err(PutParameterErrorModeled::InvalidKeyId(InvalidKeyId {
                            message: Some(why),
                        })
                        .into())
                    }
                    // The same "query key ID isn't valid" for a key that IS there and
                    // whose key state runs no cryptographic operation: KMS seals nothing
                    // under it, so the put is refused — carrying the refusal KMS answered
                    // — and no version of the parameter is created, the store below never
                    // being reached.
                    Err(crate::state::secure_string::SealRefusal::UnusableKey(why)) => {
                        return Err(PutParameterErrorModeled::InvalidKeyId(InvalidKeyId {
                            message: Some(why),
                        })
                        .into())
                    }
                    Err(crate::state::secure_string::SealRefusal::Unavailable(why)) => {
                        return Err(PutParameterErrorModeled::InternalServerError(
                            InternalServerError { message: Some(why) },
                        )
                        .into())
                    }
                }
            }
        };

        // The store is written under its lock, which this block holds for its own body
        // alone: the guard is not `Send`, and the publication that follows is awaited.
        let (version, change) = {
            let mut store = state.lock();
            // The version this put creates, read from the store it is written to: the
            // number follows the versions the parameter holds now. It is the number the put
            // ANSWERS with whether or not the write lands, because a put whose value fails
            // its data type's validation "will return success" all the same.
            let version = store
                .parameter(&region, name)
                .map_or(1, crate::state::ParameterRecord::next_version);
            if !validated {
                // "the asynchronous validation will fail and the parameter will not be
                // created or updated": nothing of this put is written, so a create leaves no
                // parameter behind, an overwrite leaves the versions, the value and the
                // attributes the parameter already had, and no change is published because
                // none was made.
                (version, None)
            } else {
                let stored_version = crate::state::ParameterVersionRecord {
                    version,
                    value: stored_value,
                    type_,
                    data_type,
                    tier: tier.clone(),
                    description,
                    allowed_pattern,
                    key_id,
                    policies: stored_policies,
                    last_modified_date: now,
                    // "ARN of the Amazon Web Services user who last changed the parameter":
                    // this put is that change, so the version records the principal it was
                    // signed for.
                    last_modified_user: Some(writer),
                    // Only `LabelParameterVersion` attaches a label, so a version is created
                    // carrying none.
                    labels: ::std::collections::BTreeSet::new(),
                };

                // Which change this put MADE, which is what the event it publishes reports: a
                // put that found the parameter already there stored another version of it, and
                // one that did not created it.
                let operation = match store.parameter_mut(&region, name) {
                    Some(record) => {
                        record.push_version(stored_version);
                        // Tags are a property of the parameter rather than of one version, and
                        // a put that supplies tags beside `Overwrite` is refused above, so an
                        // update never rewrites the set: the parameter keeps its tags.
                        crate::state::parameter_events::OPERATION_UPDATE
                    }
                    None => {
                        store.insert_parameter(crate::state::ParameterRecord {
                            // The name is rendered the one way the service renders a parameter
                            // name before it is persisted ([`crate::state::reported_name`]), so
                            // every read of this parameter reports the name it is addressed and
                            // ARN'd by rather than the spelling one create happened to use.
                            name: crate::state::reported_name(name).to_string(),
                            region: region.clone(),
                            tags: crate::state::to_btree_map(supplied_tags),
                            versions: vec![stored_version],
                        });
                        crate::state::parameter_events::OPERATION_CREATE
                    }
                };
                // What the change is REPORTED as is read off the record the write left behind,
                // so the event states the parameter a read of the same instant states and never
                // a second, invented copy of it.
                let change = store.parameter(&region, name).and_then(|record| {
                    record
                        .latest()
                        .map(|latest| crate::state::parameter_events::ParameterChange {
                            name: record.name.clone(),
                            type_: latest.type_.clone(),
                            operation,
                            description: latest.description.clone(),
                            at: latest.last_modified_date,
                        })
                });
                (version, change)
            }
        };
        // The put CHANGED a parameter, so Parameter Store publishes the change it made,
        // with the store's lock released — EventBridge delivers the event to every rule
        // that matches it, and a target of one may call back into this service.
        if let Some(change) = change {
            crate::state::parameter_events::publish(ctx.callback(), &region, &[change]).await;
        }

        Ok(PutParameterOutput {
            version,
            tier: Some(tier),
        })
    }
}
