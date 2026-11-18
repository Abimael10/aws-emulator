// SPDX-License-Identifier: CC0-1.0

//! `AWS::SSM::Parameter`, as it lives in the Systems Manager service's own store.
//!
//! A parameter is a resource of the Systems Manager service: `ssm:PutParameter` creates and
//! overwrites it, `ssm:GetParameter` and `ssm:DescribeParameters` report it,
//! `ssm:AddTagsToResource` tags it and `ssm:DeleteParameter` removes it, and the record of
//! it lives in that service's state in the same shared engine. So, exactly as for a log
//! group ([`super::log_groups`]) or a topic ([`super::topics`]), this provider holds a
//! handle to THAT store and nothing of its own: a parameter this emulator holds is one
//! Cloud Control API resolves, whichever API created it, and a parameter Systems Manager no
//! longer holds is one Cloud Control API reports as not found.
//!
//! Parameters are regional — the owning store holds one namespace per Region — so every
//! answer here is about the Region the call was made in.
//!
//! The resource model is the type's registry schema. Catalog source: the
//! `AWS::SSM::Parameter` resource provider schema's `properties`, whose `primaryIdentifier`
//! and whose one `createOnlyProperties` entry are both `/properties/Name`:
//!
//! * `Name`, the parameter's own name, which is its primary identifier;
//! * `Type` and `Value`, the two the schema declares `required`;
//! * `Description`, `Tier` and `DataType`, each a member `PutParameter` takes under the
//!   same name and each carried by the version the owning store holds — of which
//!   `Description` and `Tier` are among the properties the schema declares write-only, so
//!   the model this API REPORTS carries neither ([`crate::state::reported_model`]): they
//!   take effect on the parameter, and the owning service's own reads are what report
//!   them;
//! * `Tags`, which this type publishes as a MAP of tag key to tag value rather than as the
//!   `[{ Key, Value }]` list most types declare — so the tag set is read and reported here
//!   rather than through the shared list reading the other providers use;
//! * the read-only `Arn`, the parameter's own ARN, which this type's READ handler is
//!   recorded reporting through this API beside the properties a caller supplies — the one
//!   the OWNING service builds from the Region and the name ([`ssm::parameter_arn`]), so the
//!   ARN Cloud Control API reports is the ARN that service's own reads report.
//!
//! The schema's remaining properties are ones THIS path cannot apply, so a model that asks
//! for one is refused rather than accepted and dropped — a property a caller sets has to be
//! one the owning service then holds:
//!
//! * `AllowedPattern`, because it is "a regular expression used to validate the parameter
//!   value" that the owning service evaluates against the value of every write it accepts,
//!   and this path, applying the work the clock owes under the store's guard, runs no
//!   caller-supplied expression;
//! * `Policies`, because a policy document is one the owning service validates, admits only
//!   on an advanced parameter and then enforces on a schedule of its own.
//!
//! A model may still CARRY either of them with the value the parameter already holds — the
//! owning service's own read reports them, so a model assembled from there carries them —
//! which is what lets an update patch something else on a parameter that has one. Carrying
//! one unchanged asks for nothing, and a parameter keeps the pattern and the policies it
//! holds across every write here ([`DesiredModel::version`]), so a model that names
//! neither — which is every model this API reports — asks for nothing either.
//!
//! Two further rules of the owning service hold of every answer here:
//!
//! * the value a read reports is the value AT REST, which is what the owning service's own
//!   read answers when it asks for no decryption;
//! * the parameters Systems Manager itself publishes in the `/aws` hierarchy belong to no
//!   account, so none of them is a resource of this account to resolve, list or remove —
//!   and no create can reach that hierarchy either, the owning service keeping the `aws`
//!   and `ssm` name prefixes for itself ([`ssm::name_rejection`]).

use ::std::collections::BTreeMap;
use ::std::ops::Bound;
use ::std::time::SystemTime;

use ::serde_json::{Map, Value};
use ::service_com_amazonaws_ssm_amazonssm::ops::types::PARAMETER_TIER_VALUES;
use ::service_com_amazonaws_ssm_amazonssm::state as ssm;

use super::{require_held, require_value, text, NativeRefusal, NativeResourceProvider, SsmState};

/// The `TypeName` this provider stands behind.
pub const TYPE_NAME: &str = "AWS::SSM::Parameter";

/// The property that carries the type's primary identifier, which the schema's
/// `primaryIdentifier` points at (`/properties/Name`).
const IDENTIFIER_PROPERTY: &str = "Name";

/// The parameter's own ARN (`Arn`), which the owning service builds from the Region and the
/// name and which no caller supplies: the type's one read-only property, reported by every
/// read of a parameter.
const ARN: &str = "Arn";

/// The type of the parameter (`Type`), one of [`SUPPORTED_TYPES`].
const TYPE: &str = "Type";

/// The parameter's value (`Value`).
const VALUE: &str = "Value";

/// The parameter's description (`Description`).
const DESCRIPTION: &str = "Description";

/// The tier the parameter is served in (`Tier`).
const TIER: &str = "Tier";

/// The data type of the parameter's value (`DataType`).
const DATA_TYPE: &str = "DataType";

/// The regular expression the owning service validates the value against
/// (`AllowedPattern`) — one of the schema's write-only properties, and applied by nothing
/// here (see this module's header).
const ALLOWED_PATTERN: &str = "AllowedPattern";

/// The parameter policies the owning service holds on the parameter (`Policies`), applied
/// by nothing here.
const POLICIES: &str = "Policies";

/// The parameter's tag set (`Tags`), which this type publishes as a MAP of tag key to tag
/// value ([`tag_map`], [`tag_object`]).
const TAGS: &str = "Tags";

/// The `Type` values this resource type admits.
///
/// Catalog source: the `AWS::SSM::Parameter` schema's own `Type` enum, which declares the
/// two unencrypted types and not the encrypted one — "AWS CloudFormation doesn't support
/// creating a SecureString parameter type" (the type's own documentation). A parameter the
/// OWNING service's API created as that type is still read and listed here: a read reports
/// the parameter as it is, and only a write is held to what this type may ask for.
const SUPPORTED_TYPES: &[&str] = &[ssm::TYPE_STRING, ssm::TYPE_STRING_LIST];

/// The Systems Manager parameters of one engine, as Cloud Control API resources.
pub struct ParameterProvider {
    /// The owning service's store. A handle, so every answer is read from the live store.
    state: ::engine::ServiceState<SsmState>,
}

impl ParameterProvider {
    /// The provider for the Systems Manager store THIS engine carries, or `None` when it
    /// carries none.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        ssm::service_state(engine).map(|state| Self { state })
    }
}

impl NativeResourceProvider for ParameterProvider {
    fn type_name(&self) -> &'static str {
        TYPE_NAME
    }

    fn identifier_property(&self) -> &'static str {
        IDENTIFIER_PROPERTY
    }

    /// The name the parameter a create describes will answer to, for a model that names
    /// none.
    ///
    /// The type's primary identifier is the parameter's own name, which a desired state
    /// normally carries — so this answers `None` for such a model and leaves the identifier
    /// to the one rule every other type follows. A model that names NO name is a resource
    /// whose name the provider picks ([`generated_name`]), the same way a template that
    /// declares none leaves the name to CloudFormation.
    fn assigned_identifier(&self, region: &str, properties: &Value) -> Option<String> {
        // One name per Region is still one name: nothing about the Region decides it.
        let _ = region;
        names_no_name(properties).then(generated_name)
    }

    fn read(&self, region: &str, identifier: &str) -> Option<Value> {
        // A record in the store always carries a version, so a parameter that resolves has
        // a model; one whose versions were somehow gone would be reported as no resource
        // rather than as an empty one.
        self.state
            .lock()
            .parameter(region, identifier)
            .and_then(model)
    }

    fn list(&self, region: &str, after: Option<&str>, limit: usize) -> Vec<(String, Value)> {
        let store = self.state.lock();
        window(&store, region, after, limit)
            .filter_map(|record| model(record).map(|model| (record.name.clone(), model)))
            .collect()
    }

    fn create(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        // The name a parameter is created under is the OWNING service's to accept: the
        // naming rules its own `PutParameter` holds a name to, read through that service's
        // own reading of them, so nothing is created here under a name its API would
        // refuse.
        let name = accepted_name(identifier)?;
        let desired = DesiredModel::read(
            name,
            properties,
            // No parameter holds a version yet: this create is what settles every one of
            // these, and a pattern or a policy is one this path cannot apply at all. The
            // ARN is the owning service's own function of the Region and the name, so the
            // parameter this create describes already has one.
            &Held::fresh(region, name),
        )?;
        let mut store = self.state.lock();
        // The parameter is created in the owning service's store under its own rules: a
        // name it already holds is that service's own already-exists verdict, and its
        // per-Region quota and this emulator's own total both bound what a create may add.
        if store.parameter(region, name).is_some() {
            return Err(NativeRefusal::AlreadyExists);
        }
        let in_region = store.parameters_in_region(region);
        if in_region >= ssm::MAX_PARAMETERS_PER_REGION || !store.can_hold_another_parameter() {
            return Err(NativeRefusal::AtCapacity(parameters_quota_message()));
        }
        let version = desired.version(None, in_region, at)?;
        let record = ssm::ParameterRecord {
            // The name is rendered the one way the owning service renders a parameter name,
            // so every read of this parameter — through either API — reports the name it is
            // addressed and ARN'd by.
            name: ssm::reported_name(name).to_string(),
            region: region.to_string(),
            tags: desired.tags,
            versions: vec![version],
        };
        let created = model(&record).ok_or_else(unreportable)?;
        store.insert_parameter(record);
        Ok(created)
    }

    fn update(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let mut store = self.state.lock();
        let held = Held::of(
            store
                .parameter(region, identifier)
                .ok_or(NativeRefusal::NotFound)?,
        );
        let desired = DesiredModel::read(identifier, properties, &held)?;
        let in_region = store.parameters_in_region(region);
        // A write to a parameter the owning service holds is a new VERSION of it — "create
        // or update a parameter in Parameter Store", the overwrite half of the one call
        // that does both — so the model is applied by appending a version rather than by
        // editing the one a reader may already have seen.
        let version = desired.version(Some(&held), in_region, at)?;
        let record = store
            .parameter_mut(region, identifier)
            .ok_or(NativeRefusal::NotFound)?;
        record.push_version(version);
        // Tags belong to the parameter rather than to one of its versions, which is why the
        // owning service tags an existing one through `AddTagsToResource`: the set a model
        // carries is the set the parameter is left with, so a model that names none leaves
        // it untagged.
        record.tags = desired.tags;
        model(record).ok_or_else(unreportable)
    }

    fn delete(&self, region: &str, identifier: &str, at: SystemTime) -> Result<(), NativeRefusal> {
        // A parameter's record carries no instant a removal is stamped with.
        let _ = at;
        let mut store = self.state.lock();
        // The key the owning store holds a parameter under: the Region it lives in and the
        // one rendering of its name that store keys by, which is how its own
        // `DeleteParameter` addresses the record it removes.
        let key = (
            region.to_string(),
            ssm::canonical_name(identifier).to_string(),
        );
        store
            .parameters
            .remove(&key)
            .map(|_| ())
            .ok_or(NativeRefusal::NotFound)
    }
}

/// The window `(after, limit)` names of one Region's parameters: the records whose name
/// sorts AFTER `after`, in the owning store's own name order, at most `limit` of them.
///
/// The owning store keys its parameters by `(Region, name)`, so its own ordered range over
/// that key IS the window a listing pages over — nothing is sorted, and nothing outside the
/// window is read. The cursor a page resumes at is an identifier this provider reported,
/// which is the parameter's own name, so it is compared under the same rendering the store
/// keys by ([`ssm::canonical_name`]).
fn window<'a>(
    store: &'a SsmState,
    region: &'a str,
    after: Option<&str>,
    limit: usize,
) -> impl Iterator<Item = &'a ssm::ParameterRecord> + 'a {
    let start = match after {
        Some(after) => Bound::Excluded((
            region.to_string(),
            ssm::canonical_name(after).to_string(),
        )),
        None => Bound::Included((region.to_string(), String::new())),
    };
    store
        .parameters
        .range((start, Bound::Unbounded))
        .take_while(move |((held, _), _)| held == region)
        .take(limit)
        .map(|(_, record)| record)
}

/// The name the owning service would accept `identifier` as, or why it would not.
///
/// Every rule is that service's own: the spaces rule its reading of a name applies, and the
/// naming constraints its `PutParameter` states — the character set, the hierarchy's leading
/// separator, the prefixes it keeps for itself and the depth it allows.
fn accepted_name(identifier: &str) -> Result<&str, NativeRefusal> {
    let name = ssm::trimmed_name(identifier).map_err(NativeRefusal::Invalid)?;
    match ssm::name_rejection(name) {
        Some(
            ssm::NameRejection::Pattern(why)
            | ssm::NameRejection::Malformed(why)
            | ssm::NameRejection::Depth(why),
        ) => Err(NativeRefusal::Invalid(why)),
        None => Ok(name),
    }
}

/// What a create past the owning service's parameter quota is refused with: that service's
/// own published per-Region maximum.
fn parameters_quota_message() -> String {
    format!(
        "this account already holds the maximum of {} parameters for this Region",
        ssm::MAX_PARAMETERS_PER_REGION
    )
}

/// The resource model one parameter renders as — the schema's properties the owning record
/// carries, and only those it HAS: a parameter with no description, no pattern, no policy
/// and no tags carries none of those properties, the same way the owning service's own
/// reader reports none.
///
/// This is the model of the parameter AS IT IS, which is what a patch is applied to; what
/// a client is REPORTED is this model projected as the type's schema serialises it, so the
/// properties the schema declares write-only reach no response
/// ([`crate::state::reported_model`]).
///
/// `None` for a record carrying no version at all, which the owning store never holds: a
/// parameter IS its versions, so there would be no value, type or tier to report.
fn model(record: &ssm::ParameterRecord) -> Option<Value> {
    let latest = record.latest()?;
    let mut model = Map::new();
    model.insert(
        IDENTIFIER_PROPERTY.to_string(),
        Value::from(record.name.clone()),
    );
    model.insert(ARN.to_string(), Value::from(record.arn()));
    model.insert(TYPE.to_string(), Value::from(latest.type_.clone()));
    // The value AT REST, which is what the owning service's own read reports for a read
    // that asks for no decryption.
    model.insert(VALUE.to_string(), Value::from(latest.value.clone()));
    model.insert(TIER.to_string(), Value::from(latest.tier.clone()));
    model.insert(DATA_TYPE.to_string(), Value::from(latest.data_type.clone()));
    if let Some(description) = &latest.description {
        model.insert(DESCRIPTION.to_string(), Value::from(description.clone()));
    }
    if let Some(pattern) = &latest.allowed_pattern {
        model.insert(ALLOWED_PATTERN.to_string(), Value::from(pattern.clone()));
    }
    if let Some(policies) = &latest.policies {
        model.insert(POLICIES.to_string(), Value::from(policies.clone()));
    }
    if !record.tags.is_empty() {
        model.insert(TAGS.to_string(), tag_object(&record.tags));
    }
    Some(Value::Object(model))
}

/// The tag set one parameter reports as: this type's own MAP of tag key to tag value, in key
/// order, so two reads of one parameter report its tags alike.
fn tag_object(tags: &BTreeMap<String, String>) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| (key.clone(), Value::from(value.clone())))
            .collect(),
    )
}

/// The tag set `value` carries, read as the MAP this type publishes — or why it is not one.
///
/// Bounded by the owning service's own per-resource tag quota, judged on the map the model
/// carried.
fn tag_map(value: &Value) -> Result<BTreeMap<String, String>, NativeRefusal> {
    let Some(entries) = value.as_object() else {
        return Err(NativeRefusal::Invalid(format!(
            "{TAGS} must be an object of tag keys and their values"
        )));
    };
    let mut tags = BTreeMap::new();
    for (key, tag_value) in entries {
        let Some(tag_value) = tag_value.as_str() else {
            return Err(NativeRefusal::Invalid(format!(
                "every {TAGS} value must be a string"
            )));
        };
        tags.insert(key.clone(), tag_value.to_string());
    }
    let maximum = ssm::tagging::max_tags_for(ssm::tagging::TAGGING_TYPE_PARAMETER);
    if tags.len() > maximum {
        return Err(NativeRefusal::Invalid(format!(
            "{TAGS} carries {} tags: a resource can have a maximum of {maximum} tags",
            tags.len()
        )));
    }
    Ok(tags)
}

/// The name a create picks for a parameter whose model names none — the shared rule. A
/// parameter has no second spelling its name must end in, so the name carries no suffix.
fn generated_name() -> String {
    super::generated_name(false, "")
}

/// Whether `properties` names NO name at all — the property that carries this type's
/// primary identifier, matched the way every other reading of an identifier property
/// matches it: ignoring ASCII case.
fn names_no_name(properties: &Value) -> bool {
    properties.as_object().is_none_or(|model| {
        !model
            .keys()
            .any(|property| property.eq_ignore_ascii_case(IDENTIFIER_PROPERTY))
    })
}

/// The refusal for a model whose own record could not be rendered, which the owning store
/// holds no record for: a parameter always carries a version ([`model`]).
fn unreportable() -> NativeRefusal {
    NativeRefusal::Rejected(format!("{TYPE_NAME} carries no version to report"))
}

/// What the owning service already HOLDS for a parameter a model is read against: the
/// values the read-only and the unapplied properties may be carried with, and the state the
/// owning service's own write rules are evaluated against.
#[derive(Debug)]
struct Held {
    /// The parameter's own ARN, which every read reports.
    arn: String,
    /// The type the parameter is of — `None` where no parameter holds one yet, which is the
    /// create that settles it.
    type_: Option<String>,
    /// The tier it is served in, under the same rule.
    tier: Option<String>,
    /// The pattern the owning service validates its value against, if any.
    allowed_pattern: Option<String>,
    /// The policy document it carries, if any.
    policies: Option<String>,
    /// The number the next version of it carries.
    next_version: i64,
}

impl Held {
    /// What a parameter of `name` that `region` does not hold yet already has: its ARN,
    /// which the owning service builds from the two of them.
    fn fresh(region: &str, name: &str) -> Self {
        Self {
            arn: ssm::parameter_arn(region, name),
            type_: None,
            tier: None,
            allowed_pattern: None,
            policies: None,
            next_version: 1,
        }
    }

    /// What `record`'s newest version holds.
    fn of(record: &ssm::ParameterRecord) -> Self {
        let latest = record.latest();
        Self {
            arn: record.arn(),
            type_: latest.map(|latest| latest.type_.clone()),
            tier: latest.map(|latest| latest.tier.clone()),
            allowed_pattern: latest.and_then(|latest| latest.allowed_pattern.clone()),
            policies: latest.and_then(|latest| latest.policies.clone()),
            next_version: record.next_version(),
        }
    }
}

/// What a resource model asks a parameter to be: the properties this path APPLIES, having
/// been checked against the owning service's own rules.
struct DesiredModel {
    /// The type the parameter is to be of, or `None` to keep the one it has.
    type_: Option<String>,
    /// The value it is to hold.
    value: Option<String>,
    /// Its description, or `None` for a model that names none.
    description: Option<String>,
    /// The tier the model ASKS for, which the owning service's own rule resolves against
    /// the tier the parameter is in ([`ssm::resolve_tier`]).
    tier: Option<String>,
    /// The data type of its value, or `None` for a model that names none — which is the
    /// owning service's own default.
    data_type: Option<String>,
    /// The tag set it is to carry.
    tags: BTreeMap<String, String>,
}

impl DesiredModel {
    /// Read `properties` as the model of the parameter `name`, which the owning service
    /// holds `held` for — or state why that service would not take it.
    ///
    /// The identity and every property this path does not apply may be CARRIED (the
    /// parameter's own reads report them, so a model assembled from there carries them) but
    /// only with the value the parameter already holds: a value accepted here and never
    /// applied would be one nothing ever reports.
    fn read(name: &str, properties: &Value, held: &Held) -> Result<Self, NativeRefusal> {
        let Some(model) = properties.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self {
            type_: None,
            value: None,
            description: None,
            tier: None,
            data_type: None,
            tags: BTreeMap::new(),
        };
        for (property, value) in model {
            match property.as_str() {
                IDENTIFIER_PROPERTY => {
                    // The name identifies the resource, so a model carries the one the
                    // parameter answers to — under the one rendering the owning service
                    // gives a name, so `/Top` and `Top` name the parameter they both are.
                    let carried = text(property, value)?;
                    if ssm::canonical_name(&carried) != ssm::canonical_name(name) {
                        return Err(NativeRefusal::Invalid(format!(
                            "{property} of this resource is {}",
                            ssm::reported_name(name)
                        )));
                    }
                }
                // The ARN is the owning service's, not a caller's: a model may carry it
                // (every read reports it) only with the one the parameter answers to.
                ARN => require_value(property, value, &held.arn)?,
                TYPE => match &held.type_ {
                    // "Parameter Store doesn't support changing a parameter type … You
                    // must create a new, unique parameter", so the type is settled when
                    // the parameter is created. Whether the type it is settled at is one
                    // this type may WRITE is judged where the version is built
                    // ([`Self::version`]).
                    Some(held) => require_value(property, value, held)?,
                    None => desired.type_ = Some(text(property, value)?),
                },
                VALUE => desired.value = Some(text(property, value)?),
                DESCRIPTION => desired.description = Some(text(property, value)?),
                TIER => {
                    let requested = text(property, value)?;
                    if !PARAMETER_TIER_VALUES.contains(&requested.as_str()) {
                        return Err(NativeRefusal::Invalid(format!(
                            "{property} must be one of {}",
                            PARAMETER_TIER_VALUES.join(", ")
                        )));
                    }
                    desired.tier = Some(requested);
                }
                DATA_TYPE => {
                    let requested = text(property, value)?;
                    if !ssm::PARAMETER_DATA_TYPES.contains(&requested.as_str()) {
                        return Err(NativeRefusal::Invalid(format!(
                            "{property} must be one of {}",
                            ssm::PARAMETER_DATA_TYPES.join(" ")
                        )));
                    }
                    desired.data_type = Some(requested);
                }
                // Properties the OWNING service judges and enforces itself, which this path
                // does not; see this module's header.
                ALLOWED_PATTERN => {
                    require_held(TYPE_NAME, property, value, held.allowed_pattern.as_deref())?;
                }
                POLICIES => {
                    require_held(TYPE_NAME, property, value, held.policies.as_deref())?;
                }
                TAGS => desired.tags = tag_map(value)?,
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        Ok(desired)
    }

    /// The version this model stores, as of `at` — the record the owning service's own
    /// write would persist for it, or why that service would not persist one.
    ///
    /// `held` is what the parameter holds today, so an update inherits the type and the
    /// tier it is not asked to change; `in_region` is how many parameters the Region holds,
    /// which is one of the three options the `Intelligent-Tiering` tier is evaluated
    /// against.
    fn version(
        &self,
        held: Option<&Held>,
        in_region: usize,
        at: SystemTime,
    ) -> Result<ssm::ParameterVersionRecord, NativeRefusal> {
        // "Specifying a parameter type isn't required when updating a parameter. You must
        // specify a parameter type when creating a parameter."
        let type_ = self
            .type_
            .clone()
            .or_else(|| held.and_then(|held| held.type_.clone()))
            .ok_or_else(|| NativeRefusal::Invalid(format!("{TYPE} is required by {TYPE_NAME}")))?;
        // A write through this type may only ask for a type its own schema declares
        // ([`SUPPORTED_TYPES`]) — which is also why no write here settles a key: the
        // encrypted type is the owning service's own `PutParameter` to store, under the key
        // that request names. A parameter the owning API created as that type is still read
        // and listed; only a write through this type is refused.
        if !SUPPORTED_TYPES.contains(&type_.as_str()) {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE} must be one of {}",
                SUPPORTED_TYPES.join(", ")
            )));
        }
        // The schema declares `Value` required, and a parameter IS the value it holds:
        // there is nothing to store for a model that names none.
        let value = self
            .value
            .clone()
            .ok_or_else(|| NativeRefusal::Invalid(format!("{VALUE} is required by {TYPE_NAME}")))?;
        let held_tier = held.and_then(|held| held.tier.as_deref());
        // The tier is the owning service's own to resolve: a model that names one is served
        // in it, one that names none keeps the tier the parameter has, and
        // `Intelligent-Tiering` is evaluated against the options that service's own doc
        // names.
        let tier = ssm::resolve_tier(
            self.tier.as_deref(),
            held_tier,
            value.len(),
            // A policy document is one no model here carries beyond the one the parameter
            // already holds, so nothing this path applies asks for the advanced tier.
            false,
            in_region,
        );
        // "You can change a standard parameter to an advanced parameter any time. But you
        // can't revert an advanced parameter to a standard parameter."
        if held_tier == Some(ssm::TIER_ADVANCED) && tier != ssm::TIER_ADVANCED {
            return Err(NativeRefusal::Invalid(format!(
                "this parameter is an {} parameter and can't be reverted to the {tier} tier",
                ssm::TIER_ADVANCED
            )));
        }
        // The content size limit of the tier the write resolved to.
        if value.len() > ssm::value_limit(&tier) {
            return Err(NativeRefusal::Invalid(ssm::value_limit_message(&tier)));
        }
        let data_type = self
            .data_type
            .clone()
            .unwrap_or_else(|| ssm::DEFAULT_DATA_TYPE.to_string());
        Ok(ssm::ParameterVersionRecord {
            version: held.map_or(1, |held| held.next_version),
            value,
            type_,
            // "The aws:ec2:image value is validated asynchronously, and the PutParameter
            // call returns before the validation is complete": a version written under that
            // data type owes the owning service's own check of it, whichever API wrote the
            // version (`ssm::parameter_image_validation`).
            image_validation_due_at: ssm::parameter_image_validation::validation_due_at(
                &data_type,
            ),
            data_type,
            tier,
            description: self.description.clone(),
            // Carried by the parameter rather than applied here: the version keeps what the
            // parameter already holds, which is the only value a model may carry for it.
            allowed_pattern: held.and_then(|held| held.allowed_pattern.clone()),
            policies: held.and_then(|held| held.policies.clone()),
            // Only a `SecureString` is stored under a key, and this type creates none.
            key_id: None,
            last_modified_date: at,
            // The work a resource operation owes is applied when the CLOCK makes it due,
            // under the owning store's guard and with no request credential in hand, so
            // this version names no principal as having written it.
            last_modified_user: None,
            // Only `LabelParameterVersion` attaches a label, so a version is created
            // carrying none.
            labels: ::std::collections::BTreeSet::new(),
        })
    }
}
