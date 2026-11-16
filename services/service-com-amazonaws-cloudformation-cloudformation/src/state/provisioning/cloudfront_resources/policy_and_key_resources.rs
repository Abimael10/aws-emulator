// SPDX-License-Identifier: CC0-1.0

//! The CloudFront resources a stack declares as ONE configuration and CloudFront identifies
//! by an id it mints: `AWS::CloudFront::CachePolicy`, `AWS::CloudFront::OriginRequestPolicy`,
//! `AWS::CloudFront::PublicKey`, `AWS::CloudFront::KeyGroup` and
//! `AWS::CloudFront::CloudFrontOriginAccessIdentity`, created in Amazon CloudFront itself.
//!
//! ## Why they are really created
//!
//! A stack that reported one of these created while nothing created it publishes an id that
//! resolves to nothing: `ListCachePolicies(Type=custom)` reports no policy, `ListPublicKeys`
//! no key and `GetKeyGroup` no group, so the IaC and the API views of the same account
//! disagree — and a distribution that names the policy by `!Ref` is refused by CloudFront for
//! an id nothing holds. The id a stack publishes is one CloudFront really minted exactly
//! because the deployment calls that service's own create, which is what this module does.
//!
//! ## The one shape all of them share
//!
//! Each type publishes a SINGLE property that is the very structure its create takes as the
//! request payload (`CachePolicyConfig`, `PublicKeyConfig`, …), each create is
//! `POST /2020-05-31/<collection>` answering the resource with the `Id` CloudFront minted, and
//! each resource is versioned by the `ETag` its configuration read reports — the version its
//! update and its delete take back as `If-Match`. So they differ only in the entries of
//! [`shape`], and every call below is driven from that one lookup rather than written once per
//! type.
//!
//! Which names are judged is the two levels [`super`] judges them at, for the same reason:
//! a property the TYPE does not publish and a member the CONFIGURATION does not publish are
//! REFUSED rather than dropped, since those are the levels where a template and the API part
//! company; below them a member travels under the name the template wrote, which is the name
//! the API takes, and the OWNING service's own deserialization is what judges it.
//!
//! Two members are the template's and the API's spellings of one value, and both are written
//! here rather than in the catalogue above:
//!
//! * `PublicKeyConfig$CallerReference` is REQUIRED by the API and by the type alike, so a
//!   declaration that states one sends it; one that states none sends the reference
//!   CloudFormation supplies for a create that requires one ([`super::caller_reference`]);
//! * `KeyGroupConfig$Items` is a list of public key ids whose element the model names
//!   `PublicKey` (`PublicKeyIdList`'s member `xmlName`), which is not a container of
//!   `{Quantity, Items}` like the lists the catalogue above wraps, so it is rendered here.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Created, Refusal, Removal, StoredResource};

/// The resource types this module provisions.
pub(crate) const CLOUDFRONT_CACHE_POLICY: &str = crate::state::template::CLOUDFRONT_CACHE_POLICY;
pub(crate) const CLOUDFRONT_PUBLIC_KEY: &str = crate::state::template::CLOUDFRONT_PUBLIC_KEY;
pub(crate) const CLOUDFRONT_KEY_GROUP: &str = crate::state::template::CLOUDFRONT_KEY_GROUP;
pub(crate) const CLOUDFRONT_ORIGIN_REQUEST_POLICY: &str =
    crate::state::template::CLOUDFRONT_ORIGIN_REQUEST_POLICY;
pub(crate) const CLOUDFRONT_ORIGIN_ACCESS_IDENTITY: &str =
    crate::state::template::CLOUDFRONT_ORIGIN_ACCESS_IDENTITY;

/// Which of them a resource is. Resolved once, so every call below is about that same
/// resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `AWS::CloudFront::CachePolicy`.
    CachePolicy,
    /// `AWS::CloudFront::PublicKey`.
    PublicKey,
    /// `AWS::CloudFront::KeyGroup`.
    KeyGroup,
    /// `AWS::CloudFront::OriginRequestPolicy`.
    OriginRequestPolicy,
    /// `AWS::CloudFront::CloudFrontOriginAccessIdentity`.
    OriginAccessIdentity,
}

/// The kind of resource `resource_type` is, or `None` for a type this module does not own.
pub(crate) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        CLOUDFRONT_CACHE_POLICY => Some(Kind::CachePolicy),
        CLOUDFRONT_PUBLIC_KEY => Some(Kind::PublicKey),
        CLOUDFRONT_KEY_GROUP => Some(Kind::KeyGroup),
        CLOUDFRONT_ORIGIN_REQUEST_POLICY => Some(Kind::OriginRequestPolicy),
        CLOUDFRONT_ORIGIN_ACCESS_IDENTITY => Some(Kind::OriginAccessIdentity),
        _ => None,
    }
}

/// Everything that differs between these types: where their collection lives, what their
/// one configuration property is called, which members it publishes, and the code CloudFront
/// answers for a resource it does not hold.
struct Shape {
    /// The property the type publishes, which is also the element the create's payload is.
    config: &'static str,
    /// The members that configuration publishes, in the order its shape declares them
    /// (`smithy_query(query="shape",shape="CachePolicyConfig,PublicKeyConfig,…")`).
    /// A declared member this list does not carry is refused rather than dropped.
    members: &'static [&'static str],
    /// The collection the create posts to, from the operation's own HTTP binding
    /// (`POST /2020-05-31/cache-policy`).
    collection: &'static str,
    /// The element the answer reports the created resource under, which is the payload
    /// member's own shape (`CreateCachePolicyResult$CachePolicy`).
    element: &'static str,
    /// The create, named in the refusal for an answer that identifies nothing.
    create: &'static str,
    /// The error code CloudFront answers for a resource it does not hold, which is what says
    /// a delete has nothing left to remove.
    not_found: &'static str,
    /// Whether the UPDATE is bound to the resource's CONFIG path rather than to the resource
    /// itself: `UpdatePublicKey` is `PUT /public-key/{Id}/config`, while `UpdateCachePolicy`
    /// and `UpdateKeyGroup` are bound to `/cache-policy/{Id}` and `/key-group/{Id}`.
    update_on_config_path: bool,
    /// The members of the ANSWER that carry the return values the type publishes beside its
    /// id, each of which only CloudFront can state: `PublicKey$CreatedTime` ("when the public
    /// key was uploaded"), the `LastModifiedTime` of a policy or a key group ("when it was
    /// last modified"), and the `S3CanonicalUserId` of an origin access identity (the id
    /// Amazon S3 knows the identity by). Read off what the call answered.
    reported: &'static [&'static str],
    /// The members the API REQUIRES that a template may leave out, which are sent empty rather
    /// than left out of a document the service would refuse.
    required_empty: &'static [&'static str],
}

/// The entry every call below is driven from, one per [`Kind`] — a `match` rather than a table
/// walk, so the compiler is what says every kind has one and no lookup can fail.
fn shape(kind: Kind) -> &'static Shape {
    match kind {
        Kind::CachePolicy => &CACHE_POLICY,
        Kind::PublicKey => &PUBLIC_KEY,
        Kind::KeyGroup => &KEY_GROUP,
        Kind::OriginRequestPolicy => &ORIGIN_REQUEST_POLICY,
        Kind::OriginAccessIdentity => &ORIGIN_ACCESS_IDENTITY,
    }
}

const CACHE_POLICY: Shape = Shape {
    config: "CachePolicyConfig",
    members: &[
        "Comment",
        "Name",
        "DefaultTTL",
        "MaxTTL",
        "MinTTL",
        "ParametersInCacheKeyAndForwardedToOrigin",
    ],
    collection: "cache-policy",
    element: "CachePolicy",
    create: "CreateCachePolicy",
    not_found: "NoSuchCachePolicy",
    update_on_config_path: false,
    reported: &["LastModifiedTime"],
    required_empty: &[],
};

const PUBLIC_KEY: Shape = Shape {
    config: "PublicKeyConfig",
    members: &["CallerReference", "Name", "EncodedKey", "Comment"],
    collection: "public-key",
    element: "PublicKey",
    create: "CreatePublicKey",
    not_found: "NoSuchPublicKey",
    update_on_config_path: true,
    reported: &["CreatedTime"],
    required_empty: &[],
};

const KEY_GROUP: Shape = Shape {
    config: "KeyGroupConfig",
    members: &["Name", "Items", "Comment"],
    collection: "key-group",
    element: "KeyGroup",
    create: "CreateKeyGroup",
    // "NoSuchResource: a resource that was specified is not valid" is the code every key-group
    // operation answers for a group this account does not hold.
    not_found: "NoSuchResource",
    update_on_config_path: false,
    reported: &["LastModifiedTime"],
    required_empty: &[],
};

const ORIGIN_REQUEST_POLICY: Shape = Shape {
    config: "OriginRequestPolicyConfig",
    members: &[
        "Comment",
        "Name",
        "HeadersConfig",
        "CookiesConfig",
        "QueryStringsConfig",
    ],
    collection: "origin-request-policy",
    element: "OriginRequestPolicy",
    create: "CreateOriginRequestPolicy",
    not_found: "NoSuchOriginRequestPolicy",
    update_on_config_path: false,
    reported: &["LastModifiedTime"],
    required_empty: &[],
};

const ORIGIN_ACCESS_IDENTITY: Shape = Shape {
    config: "CloudFrontOriginAccessIdentityConfig",
    members: &["CallerReference", "Comment"],
    // The legacy identity's collection is TWO segments
    // (`POST /2020-05-31/origin-access-identity/cloudfront`), which the paths below build on
    // exactly as they build on a one-segment collection.
    collection: "origin-access-identity/cloudfront",
    element: "CloudFrontOriginAccessIdentity",
    create: "CreateCloudFrontOriginAccessIdentity",
    not_found: "NoSuchCloudFrontOriginAccessIdentity",
    update_on_config_path: true,
    reported: &["S3CanonicalUserId"],
    // `CloudFrontOriginAccessIdentityConfig$Comment` is REQUIRED by the API and optional in a
    // template.
    required_empty: &["Comment"],
};

/// Create the resource one of these declarations describes, in Amazon CloudFront itself — so
/// a client that deployed the stack can pass `!Ref <policy>` straight to a cache behaviour's
/// `CachePolicyId`, and `!Ref <key>` to a key group's `Items`.
///
/// `Err` is CloudFront's own refusal of the create, which is what the resource fails with: a
/// configuration it will not take (a `MinTTL` above the `MaxTTL`, an encoded key that is no
/// public key), a name another resource of the kind already has, a public key the group names
/// that does not exist.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let shape = shape(kind);
    let body = config_document(kind, &resource.properties, resource, propagated)?;
    let answered = super::call_cloudfront(
        ctx,
        region,
        "POST",
        &format!("/{}", shape.collection),
        Some(&body),
        None,
    )
    .await?;
    let id = super::answered_identifier(&answered, shape.element, shape.create)?;
    let mut attributes = reported(&answered, shape);
    attributes.insert("Id".to_string(), id.clone());
    Ok(Created {
        physical_resource_id: Some(id),
        attributes,
    })
}

/// The attributes one of these answers reports for the resource: the INSTANT the type
/// publishes beside its id, which is CloudFront's to state. A member the answer does not
/// carry contributes no attribute rather than an empty one.
fn reported(answered: &str, shape: &Shape) -> BTreeMap<String, String> {
    let members = super::xml_scalar_children(answered, shape.element);
    let mut attributes = BTreeMap::new();
    for attribute in shape.reported {
        if let Some(value) = members
            .get(*attribute)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert(
                (*attribute).to_string(),
                super::published_attribute(attribute, value),
            );
        }
    }
    attributes
}

/// Apply a CHANGED declaration to the resource the stack already has, through CloudFront's
/// own update — the call that takes the resource's id, so it keeps its identity and nothing is
/// replaced.
///
/// Each update takes the whole configuration and the version the caller read, so it is the
/// same document the create sent plus the `If-Match` that version answered.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<Option<Created>, Refusal> {
    let shape = shape(kind);
    let id = &resource.physical_resource_id;
    // The DECLARED document, not the record's: the resource carries the properties it was
    // CREATED with, so a property the new declaration introduces is only in `declared`.
    let body = config_document(kind, declared, resource, propagated)?;
    let (_, version) = super::read_version(ctx, region, &config_path(kind, id)).await?;
    let path = if shape.update_on_config_path {
        config_path(kind, id)
    } else {
        resource_path(kind, id)
    };
    let answered =
        super::call_cloudfront(ctx, region, "PUT", &path, Some(&body), version.as_deref()).await?;
    // The instant the update left on the resource, which is what the type publishes beside its
    // id: the change moved it, so the record carries what the call answered.
    Ok(Some(Created::attributes(reported(&answered, shape))))
}

/// Delete the resource a stack created, addressed by the id CloudFront minted for it.
///
/// A resource CloudFront no longer holds is already in the state the delete asks for, judged
/// on the CODE and never on a message. Each delete is conditional on the version the
/// configuration read reports, so that version is read first.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let shape = shape(kind);
    let id = &resource.physical_resource_id;
    let version = match super::read_version(ctx, region, &config_path(kind, id)).await {
        Ok((_, version)) => version,
        Err(refusal) if refusal.code == shape.not_found => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    match super::call_cloudfront(
        ctx,
        region,
        "DELETE",
        &resource_path(kind, id),
        None,
        version.as_deref(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == shape.not_found => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether CloudFront has finished removing a deleted resource.
///
/// Each of these resources is gone as its delete answers: none of them carries a status at
/// all, and no configuration of theirs propagates to the edge locations the way a
/// distribution's does.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// The configuration document a create or an update sends: the declared configuration under
/// the root element the operation's payload is bound to.
fn config_document(
    kind: Kind,
    properties: &Value,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<String, Refusal> {
    let shape = shape(kind);
    super::refuse_unapplied_properties(properties, &resource.resource_type, &[shape.config])?;
    let declared = properties
        .get(shape.config)
        .and_then(Value::as_object)
        .ok_or_else(|| Refusal {
            code: super::MISSING_PROPERTY.to_string(),
            message: format!(
                "Property {} cannot be empty for resource of type {}",
                shape.config, resource.resource_type
            ),
        })?;
    super::refuse_unapplied_members(declared, shape.config, shape.members)?;
    super::refuse_unrenderable_members(properties, shape.config, 0)?;
    let mut rendered = String::new();
    for member in shape.members {
        match declared.get(*member).filter(|value| !value.is_null()) {
            Some(value) => rendered.push_str(&rendered_config_member(
                kind, member, value, declared, shape.config,
            )),
            // `CallerReference` is required by the API, and a declaration that states none
            // leaves CloudFormation to supply one.
            None if *member == "CallerReference" => {
                rendered.push_str(&super::xml_leaf(
                    "CallerReference",
                    &super::caller_reference(resource, propagated),
                ));
            }
            // A member the API requires and the template may omit is sent EMPTY rather than
            // left out of a document the service would refuse.
            None if shape.required_empty.contains(member) => {
                rendered.push_str(&super::xml_leaf(member, ""));
            }
            None => {}
        }
    }
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    let config = shape.config;
    Ok(format!(
        "<{config} xmlns=\"{namespace}\">{rendered}</{config}>"
    ))
}

/// One declared member of one of these configurations, as the request document carries it.
///
/// Everything but a key group's `Items` is rendered exactly as the catalogue above renders a
/// distribution's members ([`super::rendered_member`]), which is what keeps ONE spelling rule
/// for the whole service: the `{Quantity, Items}` containers a cache policy's headers, cookies
/// and query strings are wrapped in are entries of that catalogue.
fn rendered_config_member(
    kind: Kind,
    member: &str,
    value: &Value,
    siblings: &Map<String, Value>,
    owner: &str,
) -> String {
    // A key group's `Items` is the list of public key ids the group holds, whose element the
    // model names `PublicKey`: it is no `{Quantity, Items}` container, so the generic
    // rendering — which reads the element off the OWNER's catalogue entry — cannot name it.
    if kind == Kind::KeyGroup && member == "Items" {
        let items = value.as_array().map(Vec::as_slice).unwrap_or_default();
        return format!(
            "<Items>{}</Items>",
            super::rendered_items("PublicKey", items, "Items")
        );
    }
    super::rendered_member(member, value, siblings, owner)
}

/// The URI the per-resource operations are bound to, with the id written as ONE path segment
/// so an id outside the modelled form is refused by that operation's validation instead of
/// addressing a different resource.
fn resource_path(kind: Kind, id: &str) -> String {
    format!(
        "/{}/{}",
        shape(kind).collection,
        super::percent_encoded(id)
    )
}

/// The URI of one resource's CONFIGURATION, which is what reports the version every mutation
/// of it is conditional on.
fn config_path(kind: Kind, id: &str) -> String {
    format!("{}/config", resource_path(kind, id))
}
