// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Connect Customer Profiles' own control plane: the
//! domain an `AWS::CustomerProfiles::Domain` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! A domain is "a container for all customer data, such as customer profile attributes, object
//! types, profile keys, and encryption keys" (`CreateDomain`), and it is NAMED: `CreateDomain`
//! takes the domain's name as the `@httpLabel` of `POST /domains/{DomainName}`, `UpdateDomain`
//! reconfigures it under that same name (`PUT`), `GetDomain` reads it (`GET`) and
//! `DeleteDomain` removes it (`DELETE`). So the name is the handle every call on a domain
//! takes, which is the physical id the naming rules already compute for the type, and no
//! identifier is minted anywhere.
//!
//! Every value the type publishes is that service's own rather than anything a template states
//! — the two instants it recorded for the domain, the status its rule-based matching
//! configuration is in, and the usage statistics it counts over the domain's data — so each is
//! read off `GetDomain`'s own report of the domain rather than computed here.

use super::*;

/// Amazon Connect Customer Profiles' signing name, from its model (`sigv4Name: profile`), which
/// speaks `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares, naming the operation so a refusal is reported the way
/// that service's own SDK raises it.
const CUSTOMER_PROFILES_SIGNING_NAME: &str = "profile";

/// The collection one domain is addressed under: `/domains/{DomainName}`, which all four calls
/// below are bound to, each with its own method.
const DOMAINS_URI: &str = "/domains";

/// The operations this module calls, as that service's own model names them.
const CREATE_DOMAIN: &str = "CreateDomain";
const UPDATE_DOMAIN: &str = "UpdateDomain";
const GET_DOMAIN: &str = "GetDomain";
const DELETE_DOMAIN: &str = "DeleteDomain";

/// The error code that service answers for a domain it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of every operation above).
const CUSTOMER_PROFILES_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member the tag set travels as on both the create and the update: a `TagMap`, which is
/// the MAP spelling [`tags_as_map`] builds.
const TAGS_MEMBER: &str = "Tags";

/// The properties of the type that are members of `CreateDomain`'s request BODY, each under the
/// member of the same name: `restJson1` carries a member under its own name unless the model
/// renames it, and this operation renames none.
///
/// The domain's NAME is not among them: it is the `@httpLabel` of the URI, which is where
/// [`domain_uri`] puts it.
const DOMAIN_MEMBERS: &[(&str, &str)] = &[
    ("DeadLetterQueueUrl", "DeadLetterQueueUrl"),
    ("DefaultEncryptionKey", "DefaultEncryptionKey"),
    ("DefaultExpirationDays", "DefaultExpirationDays"),
    ("Matching", "Matching"),
    ("RuleBasedMatching", "RuleBasedMatching"),
];

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it
/// ([`refuse_unmapped_properties`]): the name the domain is created under, the body members
/// above, and the tag set.
const DOMAIN_MAPPED_PROPERTIES: &[&str] = &[
    "DomainName",
    "DeadLetterQueueUrl",
    "DefaultEncryptionKey",
    "DefaultExpirationDays",
    "Matching",
    "RuleBasedMatching",
    "Tags",
];

/// The members of the request that are NOT text on the wire, each as the path of member names
/// that reaches it and the kind it is carried as.
///
/// A template states a value either in JSON's own spelling or as the TEXT a parameter reference
/// resolves to — `Ref` on a parameter answers that parameter's value, which a stack operation
/// carries as a string whatever the parameter's declared type — so a declaration that reaches
/// the owning service unconverted would be refused by that member's own validation for being
/// the wrong kind. Every non-text member of this type's request is therefore listed here and
/// read in the one spelling the member takes ([`carried_as`]).
///
/// Source: that service's own model for the shapes the create takes —
/// `expirationDaysInteger` for the window, `MatchingRequest`/`AutoMerging` and
/// `RuleBasedMatchingRequest` for the flags and levels of the two matching configurations
/// (`smithy_query(query="shape", shape="MatchingRequest,RuleBasedMatchingRequest,AutoMerging")`).
const DOMAIN_TYPED_MEMBERS: &[(&[&str], Kind)] = &[
    (&["DefaultExpirationDays"], Kind::Number),
    (&["Matching", "Enabled"], Kind::Flag),
    (&["Matching", "AutoMerging", "Enabled"], Kind::Flag),
    (
        &["Matching", "AutoMerging", "MinAllowedConfidenceScoreForMerging"],
        Kind::Number,
    ),
    (&["RuleBasedMatching", "Enabled"], Kind::Flag),
    (
        &["RuleBasedMatching", "MaxAllowedRuleLevelForMerging"],
        Kind::Number,
    ),
    (
        &["RuleBasedMatching", "MaxAllowedRuleLevelForMatching"],
        Kind::Number,
    ),
];

/// Which kind a member of [`DOMAIN_TYPED_MEMBERS`] is carried as.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A number: an integer member takes the integral spelling of the value
    /// ([`declared_number`]).
    Number,
    /// A boolean, read in either spelling a template gives one ([`declared_flag`]).
    Flag,
}

/// The two instants the type publishes, each under the member `GetDomain` reports it as — a
/// timestamp on the wire, so each is read as the instant [`instant_text`] renders one as.
const DOMAIN_INSTANT_ATTRIBUTES: &[(&str, &str)] = &[
    ("CreatedAt", "CreatedAt"),
    ("LastUpdatedAt", "LastUpdatedAt"),
];

/// The attributes the type publishes as a path THROUGH one of `GetDomain`'s structure members,
/// as `(attribute, member, nested member)`: the status the rule-based matching configuration is
/// in (`RuleBasedMatchingResponse$Status`) and the four counts that service keeps over the
/// domain's data (`DomainStats`: `MeteringProfileCount`, `ObjectCount`, `ProfileCount`,
/// `TotalSize`).
const DOMAIN_NESTED_ATTRIBUTES: &[(&str, &str, &str)] = &[
    ("RuleBasedMatching.Status", "RuleBasedMatching", "Status"),
    ("Stats.MeteringProfileCount", "Stats", "MeteringProfileCount"),
    ("Stats.ObjectCount", "Stats", "ObjectCount"),
    ("Stats.ProfileCount", "Stats", "ProfileCount"),
    ("Stats.TotalSize", "Stats", "TotalSize"),
];

/// The scalar properties `GetDomain` reports back under the names the type declares, for drift
/// detection: the domain's name, its expiration window, its encryption key and its dead-letter
/// queue. A property that is a STRUCTURE on the wire (either matching configuration) is left
/// out, which is the position every arm whose property is not a scalar is in and what [`drift`]
/// documents.
const DOMAIN_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("DeadLetterQueueUrl", "DeadLetterQueueUrl"),
    ("DefaultEncryptionKey", "DefaultEncryptionKey"),
    ("DefaultExpirationDays", "DefaultExpirationDays"),
    ("DomainName", "DomainName"),
];

/// Create the domain an `AWS::CustomerProfiles::Domain` declares, in Amazon Connect Customer
/// Profiles — so a client that deployed the stack reads the domain back through that service's
/// own `GetDomain` for the name the stack published, and can drive its data plane (a profile
/// created in it, a search over it) rather than getting a `ResourceNotFoundException` for a
/// domain nothing created.
///
/// The declaration IS the request ([`DOMAIN_MEMBERS`]), under the name that is this resource's
/// physical id, with the declared tags beside the ones CloudFormation gives the resource
/// ([`tags_of`]) as the `TagMap` that call's own `Tags` member takes.
///
/// What that service answers is what the stack reports: the create carries the domain's two
/// instants but none of the statistics, so every published value is read off its own report of
/// the domain it has just recorded ([`reported_domain_attributes`]).
pub(super) async fn create_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, DOMAIN_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, DOMAIN_MEMBERS, &mut request);
    carried_as(&mut request);
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    call_rest_json_named(
        ctx,
        CUSTOMER_PROFILES_SIGNING_NAME,
        region,
        CREATE_DOMAIN,
        "POST",
        &domain_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let reported =
        read_customer_profiles_domain(ctx, region, &resource.physical_resource_id).await?;
    Ok(Created::attributes(reported_domain_attributes(
        reported.as_ref(),
    )))
}

/// Apply a changed declaration to the domain the stack created, through `UpdateDomain` — the
/// one call that reconfigures a domain that exists.
///
/// That call takes every property of the type but the domain's NAME, which is the handle it is
/// addressed by: a declaration naming another domain is another resource, which the naming
/// rules decide before this arm is reached. So the request is the declaration under the same
/// member names the create uses.
///
/// The tag set is NOT among them. That call's own `Tags` member REPLACES a domain's whole tag
/// map, and the set a resource is to end up carrying is the declaration's beside the ones
/// CloudFormation gives it — which this call is not handed — so the tags are reconciled in
/// [`retag_customer_profiles_domain`] instead, exactly as every other type whose tags change
/// independently of its declaration does it.
pub(super) async fn update_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, DOMAIN_MEMBERS, &mut request);
    carried_as(&mut request);
    call_rest_json_named(
        ctx,
        CUSTOMER_PROFILES_SIGNING_NAME,
        region,
        UPDATE_DOMAIN,
        "PUT",
        &domain_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // What the resource now reports is read off that service's own report of the domain, the
    // one reading the create uses — so the statistics and the instants cannot differ by which
    // call last looked at it.
    let reported =
        read_customer_profiles_domain(ctx, region, &resource.physical_resource_id).await?;
    Ok(Created::attributes(reported_domain_attributes(
        reported.as_ref(),
    )))
}

/// Bring the tags of the domain a resource record names to the ones this deployment gives it:
/// the declaration's own `Tags` beside the ones CloudFormation propagates ([`tags_of`]) — the
/// same set the CREATE applied — so a key outside it is one this deployment no longer gives the
/// resource and a key in it with another value is re-valued.
///
/// `UpdateDomain`'s own `Tags` member is what carries a domain's tag set: it REPLACES the map
/// the domain holds, so the whole desired set is sent at once rather than a delta. Where the
/// two already agree nothing is called at all, which is what makes this safe to ask on every
/// deployment; a domain that service no longer holds has no tags to reconcile.
pub(super) async fn retag_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(domain) =
        read_customer_profiles_domain(ctx, region, &resource.physical_resource_id).await?
    else {
        return Ok(());
    };
    // That service answers a domain's tags as the `TagMap` a declaration's `Tags` property is
    // also written as, so one reader serves both sides.
    let held = tags_as_map(&declared_tags(&domain));
    let desired = tags_as_map(&tags_of(declared, propagated));
    if held == desired || desired.is_empty() {
        // An empty set is no request that call can carry — its `Tags` member is a `TagMap`
        // with a minimum length of one — so there is nothing to ask of that service.
        return Ok(());
    }
    call_rest_json_named(
        ctx,
        CUSTOMER_PROFILES_SIGNING_NAME,
        region,
        UPDATE_DOMAIN,
        "PUT",
        &domain_uri(&resource.physical_resource_id),
        Some(&Value::Object(
            [(TAGS_MEMBER.to_string(), Value::Object(desired))]
                .into_iter()
                .collect(),
        )),
    )
    .await?;
    Ok(())
}

/// Delete the domain a stack created, addressed by the name that service knows it by — this
/// resource's physical id. A domain it no longer holds (`ResourceNotFoundException`, judged on
/// the CODE and never on a message that carries the request's own name) is already in the state
/// the delete asks for.
pub(super) async fn delete_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        CUSTOMER_PROFILES_SIGNING_NAME,
        region,
        DELETE_DOMAIN,
        "DELETE",
        &domain_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == CUSTOMER_PROFILES_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The domain's configuration as Amazon Connect Customer Profiles reports it, for drift
/// detection: `GetDomain`'s own record, read by the name the stack published. One that service
/// no longer holds was deleted.
pub(super) async fn describe_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        read_customer_profiles_domain(ctx, region, physical_resource_id)
            .await?
            .map(|domain| reported_as_properties(&domain, DOMAIN_REPORTED_PROPERTIES)),
    )
}

/// The domain record that service holds under `name`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_customer_profiles_domain(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        CUSTOMER_PROFILES_SIGNING_NAME,
        region,
        GET_DOMAIN,
        "GET",
        &domain_uri(name),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == CUSTOMER_PROFILES_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes one reported domain gives the resource: its two instants, the status of its
/// rule-based matching configuration and the four usage statistics that service counts.
///
/// A domain that service reports nothing about gives none, and a value it does not report
/// leaves that attribute as the naming rules computed it — a domain with no rule-based matching
/// configured is reported without one, there being no status for it.
fn reported_domain_attributes(domain: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(domain) = domain else {
        return attributes;
    };
    for (attribute, member) in DOMAIN_INSTANT_ATTRIBUTES {
        if let Some(instant) = domain.get(*member).and_then(instant_text) {
            attributes.insert((*attribute).to_string(), instant);
        }
    }
    for (attribute, member, nested) in DOMAIN_NESTED_ATTRIBUTES {
        let reported = domain
            .get(*member)
            .and_then(|structure| structure.get(*nested))
            .and_then(scalar_text);
        if let Some(value) = reported.filter(|value| !value.is_empty()) {
            attributes.insert((*attribute).to_string(), value);
        }
    }
    attributes
}

/// The URI one domain is addressed at: the collection and the `DomainName` `@httpLabel` escaped
/// as a single path segment ([`percent_encoded`]), so a declared name outside that member's own
/// pattern is refused by that service's own validation rather than addressing another path.
fn domain_uri(name: &str) -> String {
    format!("{DOMAINS_URI}/{}", percent_encoded(name))
}

/// Put every non-text member of `request` into the spelling that member is carried in
/// ([`DOMAIN_TYPED_MEMBERS`]).
///
/// A value already in that spelling is left exactly as it came, and so is one that is neither
/// spelling of it: the owning operation's own validation is what refuses a value that is no
/// number or flag at all, rather than this dropping it.
///
/// Each kind is read by the ONE reading this provisioner has of it — [`declared_number`] for a
/// number and [`declared_flag`] for a boolean — so no arm reads a template's spelling of a
/// value its own way.
fn carried_as(request: &mut Map<String, Value>) {
    for (path, kind) in DOMAIN_TYPED_MEMBERS {
        let Some((holder, member)) = member_holder(request, path) else {
            continue;
        };
        let converted = match kind {
            Kind::Number => holder.get(member).and_then(declared_number),
            Kind::Flag => declared_flag(holder, member).map(Value::Bool),
        };
        if let Some(converted) = converted {
            holder.insert(member.to_string(), converted);
        }
    }
}

/// The structure one `path` of member names ends in, and the name of its last member: the
/// member itself is `holder[member]`.
///
/// `None` where the request carries no structure on the way — a path that walks on through a
/// member the declaration did not state, or through one that is a scalar rather than a
/// structure, reaches nothing.
fn member_holder<'r, 'p>(
    request: &'r mut Map<String, Value>,
    path: &'p [&str],
) -> Option<(&'r mut Map<String, Value>, &'p str)> {
    let (member, enclosing) = path.split_last()?;
    let mut holder = request;
    for step in enclosing {
        holder = holder.get_mut(*step)?.as_object_mut()?;
    }
    holder.contains_key(*member).then_some((holder, member))
}

