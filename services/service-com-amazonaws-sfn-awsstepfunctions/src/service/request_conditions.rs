// SPDX-License-Identifier: CC0-1.0

//! What a request of this service FIXES beside the action and the resource: the condition
//! keys a statement about it can be scoped by, read off the request itself.
//!
//! A policy statement may narrow a grant with a `Condition`, and IAM decides such a clause
//! against the values the REQUEST carries for the keys it names. A service that hands the
//! evaluator no values therefore cannot decide any conditioned statement at all: the shared
//! evaluator reads an undecidable clause fail-closed, so an `Allow` carrying one never
//! grants — a caller whose whole grant is conditioned is refused every call, however
//! correctly its policy is written. The values are what this module reads, so the keys a
//! statement about a call of this service can be scoped by are decided from the request
//! rather than guessed at:
//!
//! * `states:StateMachineQualifier` — the ALIAS or VERSION a request reaches a state
//!   machine through. `StartExecution`'s own `stateMachineArn` "accepts one of the
//!   following inputs: an unqualified state machine ARN […] a state machine version ARN
//!   […] a state machine alias ARN"
//!   (`smithy_query(query="doc",shape="StartExecutionInput",member="stateMachineArn",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`),
//!   which are three ways of naming ONE state machine to run: the resource such a call is
//!   taken on is that state machine, and WHICH qualifier it was reached through is this
//!   key ([`authorized_resource`]). An alias request fixes the qualifiers it is ABOUT: the
//!   alias's own name and the version each entry of its routing configuration points at,
//!   so a grant scoped to a set of qualifiers covers exactly the aliases and versions it
//!   names;
//! * `aws:RequestTag/<key>` and `aws:TagKeys` — the tags a request WRITES, from the member
//!   the operation carries them in, which is what a tag-scoped grant of a create or a
//!   tagging call is decided against;
//! * `aws:ResourceTag/<key>` — the tags the resource the request ADDRESSES carries, read
//!   from this service's own store, which is what a tag-scoped grant over existing
//!   resources is decided against. Only a resource this account HOLDS has tags to read:
//!   for anything else the key is left unpublished, so a clause over it stays undecided
//!   rather than being decided from an assumed empty tag set.
//!
//! A key is PUBLISHED ([`RequestConditions::published`]) only where this request can answer
//! whether it carries a value for it at all — the operation models the member, or the
//! resource is one this store holds — because the shared evaluator reads a published key's
//! absence as "the request carries none" (which is what makes `…IfExists`, a negated
//! operator and `Null` decidable) and an unpublished key's absence as "this emulator does
//! not model it", which decides nothing. Nothing here is guessed: a member a request does
//! not carry, and a resource this account does not hold, publish nothing.

use ::serde_json::Value;

/// `states:StateMachineQualifier` — this service's own condition key for the alias name or
/// version number a request reaches a state machine through (see the module docs).
pub const STATE_MACHINE_QUALIFIER_KEY: &str = "states:StateMachineQualifier";

/// `aws:RequestTag/<key>` — IAM's key family for the value a request carries for one tag
/// key, written up to and including the separator its members are named after it by, which
/// is how a whole family is published.
const REQUEST_TAG_FAMILY: &str = "aws:RequestTag/";

/// `aws:ResourceTag/<key>` — the same family for the tags the resource the request
/// addresses already carries.
const RESOURCE_TAG_FAMILY: &str = "aws:ResourceTag/";

/// `aws:TagKeys` — the KEYS of the tags a request carries, which a request carries one
/// value per key for.
const TAG_KEYS_KEY: &str = "aws:TagKeys";

/// The member a request carries the tags it WRITES in, and the operations of this model
/// that declare it (`CreateStateMachineInput$tags`, `CreateActivityInput$tags`,
/// `TagResourceInput$tags`) — keyed by operation, so a value carried under a member an
/// operation does not model fixes nothing.
const TAGS_MEMBER: &str = "tags";
const TAGS_IN: &[&str] = &["CreateActivity", "CreateStateMachine", "TagResource"];

/// The member a request carries the tag KEYS it removes in, and the one operation of this
/// model that declares it (`UntagResourceInput$tagKeys`).
const TAG_KEYS_MEMBER: &str = "tagKeys";
const TAG_KEYS_IN: &[&str] = &["UntagResource"];

/// The member an alias request carries its routing configuration in, the member of one
/// entry of it that names the version the alias routes to, and the member a create names
/// the alias itself by (`CreateStateMachineAliasInput$routingConfiguration`,
/// `RoutingConfigurationListItem$stateMachineVersionArn`,
/// `CreateStateMachineAliasInput$name`).
const ROUTING_CONFIGURATION_MEMBER: &str = "routingConfiguration";
const ROUTED_VERSION_MEMBER: &str = "stateMachineVersionArn";
const NAME_MEMBER: &str = "name";

/// The operations whose request is ABOUT a set of qualifiers rather than about one: an
/// alias write names the alias it creates or repoints and every version it routes to, so
/// the call fixes a value of [`STATE_MACHINE_QUALIFIER_KEY`] for each of them and a grant
/// scoped to qualifiers has to cover them all.
///
/// Source: the two operations of this model whose input carries a `routingConfiguration`
/// (`smithy_query(query="operation",operation="CreateStateMachineAlias,UpdateStateMachineAlias",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
const ALIAS_ROUTING_OPERATIONS: &[&str] = &["CreateStateMachineAlias", "UpdateStateMachineAlias"];

/// The resource a call is taken on, given `addressed` — the ARN its request named for what
/// it is about ([`super::identity_authorization`]).
///
/// A call that reaches a state machine THROUGH a qualifier is taken on the STATE MACHINE
/// itself, whichever of the three forms its ARN was written in: a start names the state
/// machine to execute by an unqualified ARN, a version ARN or an alias ARN alike, and a
/// call about an alias or a version is about one REFERENCE of a state machine rather than
/// about a resource of its own. Which qualifier the call is about is carried by
/// [`STATE_MACHINE_QUALIFIER_KEY`] instead — the condition key AWS publishes for exactly
/// that — so a grant of the action on a state machine covers every way of naming it and a
/// grant scoped by qualifier covers the references it names.
///
/// For every other ARN it is the ARN the request carried: only a state machine ARN carries
/// a qualifier at all ([`state_machine_qualifier`]).
#[must_use]
pub fn authorized_resource(addressed: &str) -> String {
    unqualified_arn(addressed).to_string()
}

/// The condition context one request carries, and the keys it can answer for.
#[derive(Debug, Clone, Default)]
pub struct RequestConditions {
    /// The `(key, value)` pairs the request fixes, one pair per value of a key it carries
    /// several values for.
    pub context: Vec<(String, String)>,
    /// The keys this request MODELS, whether or not it carries a value for each — what
    /// makes a published key's absence mean "the request carries none" (see the module
    /// docs).
    pub published: Vec<&'static str>,
}

impl RequestConditions {
    /// The same keys with [`STATE_MACHINE_QUALIFIER_KEY`] carrying `qualifier` alone —
    /// `None` for no value at all.
    ///
    /// What ONE decision of a call about several references of a state machine carries: the
    /// decision is about that reference, so the qualifier it fixes is that reference's own.
    /// `shared` is this request's context WITHOUT any qualifier entry, built once by the
    /// caller and borrowed by every decision, so n decisions cost n entries rather than n
    /// copies of the whole context.
    #[must_use]
    fn about_qualifier(&self, shared: &[(String, String)], qualifier: Option<&str>) -> Self {
        let mut context: Vec<(String, String)> = shared.to_vec();
        if let Some(qualifier) = qualifier {
            context.push((
                STATE_MACHINE_QUALIFIER_KEY.to_string(),
                qualifier.to_string(),
            ));
        }
        let mut published = self.published.clone();
        if !published.contains(&STATE_MACHINE_QUALIFIER_KEY) {
            published.push(STATE_MACHINE_QUALIFIER_KEY);
        }
        Self { context, published }
    }

    /// This request's context without any value of [`STATE_MACHINE_QUALIFIER_KEY`] — what
    /// every decision of a call about several qualifiers shares.
    fn without_qualifiers(&self) -> Vec<(String, String)> {
        self.context
            .iter()
            .filter(|(key, _)| key != STATE_MACHINE_QUALIFIER_KEY)
            .cloned()
            .collect()
    }
}

/// ONE decision a request asks for: the resource it is taken on, and the condition keys
/// that decision carries.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The ARN the decision is taken on.
    pub resource: String,
    /// The condition keys the request fixes for it.
    pub conditions: RequestConditions,
}

/// Every decision the submitted input asks for, in the order they are decided — so the
/// FIRST one the documents in force do not allow refuses the call, and the refusal names
/// the resource and the action that decision was about.
///
/// A request is normally ONE decision: the action on the resource it addresses, carrying
/// the qualifier it reached that resource through. A request about SEVERAL qualifiers of
/// one state machine is one decision PER QUALIFIER — an alias write is about the alias it
/// writes and about every version its routing configuration points at — so a caller holds
/// the call only where its documents cover the alias name AND every version number, which
/// is what a grant scoped by [`STATE_MACHINE_QUALIFIER_KEY`] is written to express, and a
/// grant covering only some of them refuses the call on the first one it does not cover.
pub fn asked_for<Eng: Send + Sync + 'static>(
    engine: &Eng,
    submitted: Option<&Value>,
    operation: &str,
    addressed: &str,
) -> Vec<Asked> {
    let conditions = request_conditions(engine, submitted, operation, addressed);
    let resource = authorized_resource(addressed);
    let qualifiers = qualifier_values(submitted, operation, addressed).unwrap_or_default();
    if qualifiers.len() > 1 {
        // The qualifier set is bounded by the modelled routing list it is read from
        // ([`qualifier_values`]), so the decisions a request can ask for are bounded with
        // it; the context every decision shares is built ONCE and borrowed by each.
        let shared = conditions.without_qualifiers();
        return qualifiers
            .iter()
            .map(|qualifier| Asked {
                resource: resource.clone(),
                conditions: conditions.about_qualifier(&shared, Some(qualifier)),
            })
            .collect();
    }
    vec![Asked {
        resource,
        conditions,
    }]
}

/// The condition keys the submitted input `submitted` fixes for `operation`, addressing
/// `addressed` (see the module docs).
///
/// `engine` is read only for the tags of the resource the request addresses, through this
/// service's own store: a read, taken before the operation runs and changing nothing.
pub fn request_conditions<Eng: Send + Sync + 'static>(
    engine: &Eng,
    submitted: Option<&Value>,
    operation: &str,
    addressed: &str,
) -> RequestConditions {
    let mut conditions = RequestConditions::default();
    if let Some(qualifiers) = qualifier_values(submitted, operation, addressed) {
        conditions.published.push(STATE_MACHINE_QUALIFIER_KEY);
        for qualifier in qualifiers {
            conditions
                .context
                .push((STATE_MACHINE_QUALIFIER_KEY.to_string(), qualifier));
        }
    }
    if let Some(written) = written_tags(submitted, operation) {
        conditions.published.push(REQUEST_TAG_FAMILY);
        conditions.published.push(TAG_KEYS_KEY);
        for (key, value) in written {
            if let Some(value) = value {
                conditions
                    .context
                    .push((format!("{REQUEST_TAG_FAMILY}{key}"), value));
            }
            conditions
                .context
                .push((TAG_KEYS_KEY.to_string(), key.clone()));
        }
    }
    if let Some(carried) = resource_tags(engine, submitted, operation, addressed) {
        conditions.published.push(RESOURCE_TAG_FAMILY);
        for (key, value) in carried {
            conditions
                .context
                .push((format!("{RESOURCE_TAG_FAMILY}{key}"), value));
        }
    }
    conditions
}

/// The ARN of the alias a `CreateStateMachineAlias` would MINT, or `None` for every other
/// operation and for a request that names no version for the alias to route to.
///
/// Such a request carries no ARN of the alias: "the alias ARN is a combination of state
/// machine ARN and the alias name separated by a colon (:)"
/// (`StateMachineAliasListItem`), and the state machine is the one the versions its
/// `routingConfiguration` points at belong to — so the ARN the call addresses is a
/// function of the request alone, and nothing is looked up and nothing is created to
/// decide it.
#[must_use]
pub fn minted_alias_arn(submitted: Option<&Value>, operation: &str) -> Option<String> {
    if operation != MINTS_AN_ALIAS {
        return None;
    }
    let name = member_text(submitted, NAME_MEMBER)?;
    let version = routed_versions(submitted).into_iter().next()?;
    // A routed value that is not a version ARN of a state machine names no state machine
    // for the alias to belong to, which the operation refuses for itself.
    state_machine_qualifier(&version)?;
    Some(crate::state::state_machine_alias_arn(
        unqualified_arn(&version),
        &name,
    ))
}

/// The one operation of this model that mints an alias (`CreateStateMachineAlias`).
const MINTS_AN_ALIAS: &str = "CreateStateMachineAlias";

/// `arn` with any version or alias qualifier removed — the state machine a qualified state
/// machine ARN names — and `arn` itself for every other value.
#[must_use]
pub fn unqualified_arn(arn: &str) -> &str {
    match state_machine_qualifier(arn) {
        // The qualifier is the ARN's last colon-separated field, so the state machine's
        // own ARN is what precedes that field's separator.
        Some(qualifier) => &arn[..arn.len() - qualifier.len() - 1],
        None => arn,
    }
}

/// The version number or alias name the state machine ARN `arn` is qualified with, or
/// `None` for an unqualified one and for a value that is not a state machine ARN of this
/// service at all.
///
/// A map-run LABELED ARN (`stateMachine:<name>/<label>`) names a Distributed Map state
/// inside a state machine, so its trailing field is not a qualifier — the same reading
/// [`crate::state::ComAmazonawsSfnAwsstepfunctionsState`]'s own resolution of a qualified
/// ARN takes.
#[must_use]
pub fn state_machine_qualifier(arn: &str) -> Option<&str> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != crate::state::ARN_SERVICE {
        return None;
    }
    let mut fields = parsed.resource.splitn(3, ':');
    if fields.next()? != crate::state::STATE_MACHINE_RESOURCE {
        return None;
    }
    let name = fields.next().filter(|name| !name.is_empty())?;
    if name.contains('/') {
        return None;
    }
    fields.next().filter(|qualifier| !qualifier.is_empty())
}

/// Every value of [`STATE_MACHINE_QUALIFIER_KEY`] this request fixes, or `None` for a
/// request that models no qualifier at all — one about a resource that has none (an
/// activity, an execution) or about no resource of this service.
///
/// A request that CAN carry a qualifier and carries none fixes an empty set of values,
/// which is what lets a clause about its absence be decided: the key is published with no
/// value rather than left unknown.
///
/// BOUNDED by the modelled routing list ([`routed_versions`]) before anything is read, so
/// the set is at most one qualifier per entry that list admits plus the alias's own name
/// however long a list the request carries; the set is held as a `BTreeSet`, so collecting
/// it costs no scan per entry.
fn qualifier_values(
    submitted: Option<&Value>,
    operation: &str,
    addressed: &str,
) -> Option<Vec<String>> {
    let addresses_a_state_machine = addressed_state_machine(addressed);
    let routes = ALIAS_ROUTING_OPERATIONS.contains(&operation);
    if !addresses_a_state_machine && !routes {
        return None;
    }
    let mut values: Vec<String> = Vec::new();
    let mut seen: ::std::collections::BTreeSet<String> = ::std::collections::BTreeSet::new();
    let mut fix = |value: String| {
        if !value.is_empty() && seen.insert(value.clone()) {
            values.push(value);
        }
    };
    if let Some(qualifier) = state_machine_qualifier(addressed) {
        fix(qualifier.to_string());
    }
    if routes {
        // The alias a create names by `name` is a qualifier the call is about, exactly as
        // the one an update's alias ARN carries is.
        if let Some(name) = member_text(submitted, NAME_MEMBER) {
            fix(name);
        }
        for version in routed_versions(submitted) {
            if let Some(qualifier) = state_machine_qualifier(&version) {
                fix(qualifier.to_string());
            }
        }
    }
    Some(values)
}

/// Whether `addressed` is a state machine ARN of this service — qualified or not — which
/// is what makes [`STATE_MACHINE_QUALIFIER_KEY`] a key the request models.
fn addressed_state_machine(addressed: &str) -> bool {
    ::service_common::arn::parse(addressed).is_some_and(|parsed| {
        parsed.vendor == crate::state::ARN_SERVICE
            && parsed.resource_type() == crate::state::STATE_MACHINE_RESOURCE
    })
}

/// The version ARNs the submitted routing configuration points at, in the order it writes
/// them — empty for a request that carries none.
///
/// The LIST is caller-sized, so it is bounded BEFORE any of it is read: a request carrying
/// more entries than the modelled maximum ([`MAX_ROUTED_VERSIONS`]) is one the operation
/// itself refuses, so none of its entries reaches a condition context and the qualifier
/// key is fixed by nothing it carried — which leaves a clause over it undecided, the
/// fail-closed reading the shared evaluator takes.
fn routed_versions(submitted: Option<&Value>) -> Vec<String> {
    let entries = submitted
        .and_then(|body| body.get(ROUTING_CONFIGURATION_MEMBER))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if entries.len() > MAX_ROUTED_VERSIONS {
        return Vec::new();
    }
    entries
        .iter()
        .filter_map(|entry| entry.get(ROUTED_VERSION_MEMBER))
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The most versions an alias's routing configuration can point at: the modelled maximum
/// of `RoutingConfigurationList` (`smithy_query(query="shape",shape="RoutingConfigurationList",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`
/// — `length 1..2`), which is what bounds the qualifiers one request can be about.
const MAX_ROUTED_VERSIONS: usize = 2;

/// The tags this request WRITES, as `(key, value)` pairs — a key removed by an untagging
/// call carries no value — or `None` for an operation that writes none, which models
/// neither tag key family.
///
/// The LIST is caller-sized, so what is read out of it is bounded before any of it is: a
/// request carrying more entries than the modelled maximum
/// ([`crate::state::MAX_TAGS`]) is one the operation itself refuses, so none of its
/// entries is read into a condition context at all and the families stay unpublished —
/// which the shared evaluator reads fail-closed. The routing configuration above is
/// bounded the same way, by [`MAX_ROUTED_VERSIONS`].
fn written_tags(submitted: Option<&Value>, operation: &str) -> Option<Vec<(String, Option<String>)>> {
    if TAGS_IN.contains(&operation) {
        let entries = submitted
            .and_then(|body| body.get(TAGS_MEMBER))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if entries.len() > crate::state::MAX_TAGS {
            return None;
        }
        let tags = entries
            .iter()
            .filter_map(|entry| {
                let key = entry.get("key").and_then(Value::as_str)?;
                let value = entry.get("value").and_then(Value::as_str);
                Some((key.to_string(), value.map(str::to_string)))
            })
            .collect();
        return Some(tags);
    }
    if TAG_KEYS_IN.contains(&operation) {
        let entries = submitted
            .and_then(|body| body.get(TAG_KEYS_MEMBER))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if entries.len() > crate::state::MAX_TAGS {
            return None;
        }
        let keys = entries
            .iter()
            .filter_map(Value::as_str)
            .map(|key| (key.to_string(), None))
            .collect();
        return Some(keys);
    }
    None
}

/// The tags the resource `addressed` names carries, as `(key, value)` pairs, or `None`
/// when the tag set is not something this request can answer for at all — an ARN no
/// taggable resource of this account belongs to, and one whose resource this account does
/// not hold.
///
/// A CREATE is the one call whose resource does not exist yet, and its tag set is not
/// unknown: the resource it mints carries exactly the tags the request writes, which is
/// what a create is judged against — so for a create the tags of the resource ARE the
/// request's tags, and a grant conditioned on a resource tag covers creating a resource
/// that will carry it.
fn resource_tags<Eng: Send + Sync + 'static>(
    engine: &Eng,
    submitted: Option<&Value>,
    operation: &str,
    addressed: &str,
) -> Option<Vec<(String, String)>> {
    if MINTS_A_TAGGED_RESOURCE.contains(&operation) {
        return Some(
            written_tags(submitted, operation)?
                .into_iter()
                .map(|(key, value)| (key, value.unwrap_or_default()))
                .collect(),
        );
    }
    let owner = taggable_owner(addressed)?;
    let state = crate::state::service_state(engine)?;
    let store = state.lock();
    if !store.state_machines.contains_key(&owner) && !store.activities.contains_key(&owner) {
        return None;
    }
    Some(
        store
            .tags_of(&owner)
            .into_iter()
            .filter_map(|tag| Some((tag.key?, tag.value.unwrap_or_default())))
            .collect(),
    )
}

/// The operations that MINT a resource carrying the tags the request writes
/// (`CreateStateMachineInput$tags` "tags to be added when creating a state machine",
/// `CreateActivityInput$tags`).
const MINTS_A_TAGGED_RESOURCE: &[&str] = &["CreateActivity", "CreateStateMachine"];

/// The ARN of the TAGGABLE resource of this service `arn` belongs to: itself for a state
/// machine or an activity, and the state machine for everything a state machine owns.
///
/// "Tags to be added when creating a state machine" and a tagging call's "ARN for the Step
/// Functions state machine or activity" are the two taggable resource types, and a
/// resource of neither type belongs to one of them: an execution is a run OF a state
/// machine, a Map Run is a state of one, and a version or an alias is a reference to one —
/// which is why a tag-scoped policy covers "a tagged activity, a tagged state machine and
/// its executions". Read from the ARN alone, and only within the account this emulator
/// serves, since a tag set of another account's resource is not something this store can
/// answer for.
fn taggable_owner(arn: &str) -> Option<String> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != crate::state::ARN_SERVICE
        || parsed.account != ::engine::emulator_account_id()
    {
        return None;
    }
    let mut fields = parsed.resource.splitn(3, ':');
    let kind = fields.next()?;
    // A map-run ARN writes `<state machine name>/<label>` in the field a state machine ARN
    // writes the name in, so the owning state machine's name is what precedes the label.
    let name = fields
        .next()
        .and_then(|name| name.split('/').next())
        .filter(|name| !name.is_empty() && !name.contains('*'))?;
    match kind {
        crate::state::ACTIVITY_RESOURCE => Some(crate::state::activity_arn(parsed.region, name)),
        crate::state::STATE_MACHINE_RESOURCE
        | crate::state::EXECUTION_RESOURCE
        | crate::state::EXPRESS_EXECUTION_RESOURCE
        | crate::state::MAP_RUN_RESOURCE => {
            Some(crate::state::state_machine_arn(parsed.region, name))
        }
        _ => None,
    }
}

/// The text the submitted input carries under the member `name`, or `None` for a member it
/// does not carry, one carrying a value of another type, and one carrying no text at all.
fn member_text(submitted: Option<&Value>, name: &str) -> Option<String> {
    submitted
        .and_then(|body| body.get(name))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;

    /// One row of the qualifier table: the case, the operation, the request body, the ARN
    /// it addresses and the values of the qualifier key it fixes.
    type QualifierRow = (&'static str, &'static str, Value, String, Option<Vec<&'static str>>);

    /// One row of the written-tag table: the case, the operation, the request body and the
    /// `(key, value)` pairs it writes.
    type TagRow = (
        &'static str,
        &'static str,
        Value,
        Option<Vec<(&'static str, Option<&'static str>)>>,
    );

    /// The Region and the state machine the rows below are about.
    const REGION: &str = "us-west-2";

    /// RULE: a call that reaches a state machine through a qualifier is taken on the
    /// state machine itself, whichever of the three forms its ARN was written in — an
    /// alias and a version are references of a state machine rather than resources of
    /// their own — while every other ARN is the resource the call is taken on.
    ///
    /// The rows span the unqualified form, a version ARN, an alias ARN, a map-run labeled
    /// ARN (whose trailing field is no qualifier), an operation about an alias, an ARN of
    /// another resource type and the wildcard ARN of an API's own resource type.
    #[test]
    fn a_start_is_taken_on_the_state_machine_whichever_form_named_it() {
        let machine = crate::state::state_machine_arn(REGION, "runner");
        let version = crate::state::state_machine_version_arn(&machine, 3);
        let alias = crate::state::state_machine_alias_arn(&machine, "PROD");
        let labeled = crate::state::state_machine_arn(REGION, "runner/mapState");
        let execution = crate::state::execution_arn(REGION, "runner", "one");
        let every = crate::state::every_resource_of_type_arn(
            REGION,
            crate::state::STATE_MACHINE_RESOURCE,
        );
        for (case, addressed, expected) in [
            ("an unqualified start", &machine, &machine),
            ("a start by version ARN", &version, &machine),
            ("a start by alias ARN", &alias, &machine),
            (
                "a start of a Distributed Map state's labeled ARN",
                &labeled,
                &labeled,
            ),
            (
                "an alias ARN, which names a reference of a state machine",
                &alias,
                &machine,
            ),
            ("an execution's own ARN", &execution, &execution),
            ("the wildcard ARN of an API's resource type", &every, &every),
        ] {
            let taken_on = authorized_resource(addressed);
            assert_eq!(
                &taken_on, expected,
                "case {case:?}: expected the call to be taken on {expected:?}, got {taken_on:?}"
            );
        }
    }

    /// RULE: the qualifier key carries every qualifier the request is about — the one its
    /// ARN names, and for an alias write the alias's own name and every version it routes
    /// to — and is published, with no value, by a request that models one and carries
    /// none.
    #[test]
    fn the_qualifier_key_carries_every_qualifier_the_request_is_about() {
        let machine = crate::state::state_machine_arn(REGION, "runner");
        let alias = crate::state::state_machine_alias_arn(&machine, "PROD");
        let first = crate::state::state_machine_version_arn(&machine, 1);
        let second = crate::state::state_machine_version_arn(&machine, 2);
        let activity = crate::state::activity_arn(REGION, "worker");
        // (case, operation, the request body, the ARN it addresses, the values it fixes —
        // `None` where the request models no qualifier at all)
        let rows: Vec<QualifierRow> = vec![
            (
                "a start by alias ARN",
                "StartExecution",
                json!({ "stateMachineArn": alias }),
                alias.clone(),
                Some(vec!["PROD"]),
            ),
            (
                "an unqualified start, which fixes no qualifier",
                "StartExecution",
                json!({ "stateMachineArn": machine }),
                machine.clone(),
                Some(vec![]),
            ),
            (
                "an alias create naming two versions",
                "CreateStateMachineAlias",
                json!({
                    "name": "PROD",
                    "routingConfiguration": [
                        { "stateMachineVersionArn": first, "weight": 60 },
                        { "stateMachineVersionArn": second, "weight": 40 },
                    ],
                }),
                machine.clone(),
                Some(vec!["PROD", "1", "2"]),
            ),
            (
                "an alias update, whose own ARN names the alias",
                "UpdateStateMachineAlias",
                json!({
                    "stateMachineAliasArn": alias,
                    "routingConfiguration": [{ "stateMachineVersionArn": second, "weight": 100 }],
                }),
                alias.clone(),
                Some(vec!["PROD", "2"]),
            ),
            (
                "a call about a resource that carries no qualifier",
                "DescribeActivity",
                json!({ "activityArn": activity }),
                activity.clone(),
                None,
            ),
        ];
        for (case, operation, body, addressed, expected) in rows {
            let fixed = qualifier_values(Some(&body), operation, &addressed);
            let expected = expected.map(|values| {
                values
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            });
            assert_eq!(
                fixed, expected,
                "case {case:?}: expected the qualifier values {expected:?}, got {fixed:?}"
            );
        }
    }

    /// RULE: the tags a request writes are fixed under the request-tag family and under
    /// the tag-keys key, one value per key; an operation that writes no tags models
    /// neither.
    #[test]
    fn the_tags_a_request_writes_are_fixed_under_the_request_tag_keys() {
        // (case, operation, the request body, the pairs it fixes)
        let rows: Vec<TagRow> = vec![
            (
                "a create carrying two tags",
                "CreateStateMachine",
                json!({ "name": "runner", "tags": [
                    { "key": "team", "value": "payments" },
                    { "key": "env", "value": "prod" },
                ] }),
                Some(vec![
                    ("team", Some("payments")),
                    ("env", Some("prod")),
                ]),
            ),
            (
                "a create carrying no tags at all",
                "CreateStateMachine",
                json!({ "name": "runner" }),
                Some(vec![]),
            ),
            (
                "an untagging call, which carries keys and no values",
                "UntagResource",
                json!({ "resourceArn": "arn", "tagKeys": ["team"] }),
                Some(vec![("team", None)]),
            ),
            (
                "an operation that writes no tags",
                "StartExecution",
                json!({ "stateMachineArn": "arn", "tags": [{ "key": "team", "value": "x" }] }),
                None,
            ),
        ];
        for (case, operation, body, expected) in rows {
            let written = written_tags(Some(&body), operation);
            let expected: Option<Vec<(String, Option<String>)>> = expected.map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
                    .collect()
            });
            assert_eq!(
                written, expected,
                "case {case:?}: expected the written tags {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: the taggable resource an ARN belongs to is the state machine or the activity
    /// itself, and the owning state machine for everything a state machine owns.
    #[test]
    fn the_taggable_resource_of_an_arn_is_the_resource_that_owns_it() {
        let machine = crate::state::state_machine_arn(REGION, "runner");
        let activity = crate::state::activity_arn(REGION, "worker");
        for (case, arn, expected) in [
            ("a state machine", machine.clone(), Some(machine.clone())),
            ("an activity", activity.clone(), Some(activity)),
            (
                "an execution of a state machine",
                crate::state::execution_arn(REGION, "runner", "one"),
                Some(machine.clone()),
            ),
            (
                "a synchronous Express execution",
                crate::state::express_execution_arn(REGION, "runner", "one", "id"),
                Some(machine.clone()),
            ),
            (
                "a Map Run of a state machine",
                crate::state::map_run_arn(REGION, "runner", "mapState", "id"),
                Some(machine.clone()),
            ),
            (
                "a version of a state machine",
                crate::state::state_machine_version_arn(&machine, 2),
                Some(machine.clone()),
            ),
            (
                "an alias of a state machine",
                crate::state::state_machine_alias_arn(&machine, "PROD"),
                Some(machine),
            ),
            (
                "every resource of this service",
                crate::state::every_resource_arn(REGION),
                None,
            ),
            (
                "a resource of another service",
                "arn:aws:sqs:us-west-2:000000000000:queue".to_string(),
                None,
            ),
        ] {
            let owner = taggable_owner(&arn);
            assert_eq!(
                owner, expected,
                "case {case:?}: expected the tags of {expected:?} to decide it, got {owner:?}"
            );
        }
    }
}
