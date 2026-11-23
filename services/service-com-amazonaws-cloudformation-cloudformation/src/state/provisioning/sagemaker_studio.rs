// SPDX-License-Identifier: CC0-1.0

//! `AWS::SageMaker::Domain`, `AWS::SageMaker::UserProfile` and `AWS::SageMaker::Space`: the
//! Studio estate a stack declares, created in the Amazon SageMaker AI control plane itself.
//!
//! This is the per-type half of provisioning for three resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::efs_resources`] for the reason that module records — the
//! catalogue file is far past the length a change to this repository may touch — and is reached
//! through the same seam ([`super::Owner`]).
//!
//! ## Why these resources are really created
//!
//! Each of the three is a first-class resource of that control plane, and the handle the stack
//! publishes for it is the one every later call takes: `DescribeDomain`, `UpdateDomain` and
//! `DeleteDomain` take the `DomainId` the create minted, and `DescribeUserProfile`,
//! `DescribeSpace` and their deletes take the PAIR of that domain and the member's own name
//! (`smithy_query(query="operation",operation="CreateDomain,CreateUserProfile,CreateSpace")` on
//! that service's model). A stack that reported one of them created while nothing created it
//! publishes, through `Ref` and its outputs, an identifier that resolves to nothing: the first
//! `CreatePresignedDomainUrl` or `CreateApp` a client makes with the deployed stack is refused,
//! and the stack's own deletion has nothing to delete. So all three are provisioned here rather
//! than left declared-only — and the refusals that control plane makes about a declaration it
//! will not take (an `AuthMode` outside its own enum among them) are what fail the resource and
//! roll the stack back, rather than being passed off as a stack that completed.
//!
//! ## Whose the identity is
//!
//! A DOMAIN's is SageMaker's: `CreateDomain` takes no identifier and answers the `d-…`
//! `DomainId` it assigned, which is what that type's `Ref` returns and what every call on the
//! domain takes — so the arm answers that id as the resource's physical id
//! ([`crate::state::template::identity_is_service_assigned`] names the type) and the
//! `DomainName` the template asked for is only what the create was made under.
//!
//! A USER PROFILE's and a SPACE's are the PAIR their own API addresses them by — the member's
//! own name and the domain that holds it, in the order that type's schema states its primary
//! identifier in — which is the composite identity
//! [`crate::state::template::composite_identity_properties`] records for each type. Both parts
//! are values the declaration states, so the identity is CloudFormation's own to compose, and a
//! declaration naming the domain by a reference carries the id SageMaker minted for it once the
//! deployment has re-pointed that reference ([`super::repointing`]).
//!
//! ## What the declarations are
//!
//! Every property these three types publish is a member of the owning create under the SAME
//! name, so the resolved property document IS the request, with the members CloudFormation
//! decides set on the way out: the domain's NAME (the declared `DomainName`, or the one the
//! naming rules generated), and each member's own domain and name, which are the parts of its
//! identity. A declared property outside those catalogues is REFUSED
//! ([`refuse_unapplied_properties`]) rather than dropped: a resource reported created with less
//! than its declaration asked for is the shallow success these arms exist to avoid.
//!
//! Each of the three creates takes a `Tags` list, so every one of these resources carries the
//! tags CloudFormation gives it beside the declared ones, as the `[{Key, Value}]` list that
//! service's own `Tag` shape spells.
//!
//! ## What the stack waits for, and what it deletes
//!
//! None of the three exists usably when its create answers: SageMaker reports a new domain,
//! user profile and space `Pending` and admits a member only once the domain holding it is
//! `InService`, so each resource's creation is held open until that service reports it
//! `InService` ([`readiness`]) and one it reports failed fails the resource with the
//! `FailureReason` that service gave. Each delete is asynchronous the same way — the resource
//! is reported `Deleting` until that service has let it go — so a removal is finished only once
//! SageMaker no longer holds the resource ([`removal`]), which is also what keeps a stack's
//! teardown from asking for the domain while a member of it is still on its way out.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    call_json_rpc, key_value_tag_list, storable_tags, Created, Readiness, Refusal, Removal,
    AWS_JSON_1_1,
};
use crate::state::provisioner::{SAGEMAKER_SIGNING_NAME, SAGEMAKER_TARGET_PREFIX};
use crate::state::StoredResource;

/// The resource types this module provisions. Visible to the rest of this service's state
/// because more than one module answers about them: the identity in force is the one the owning
/// service minted or the pair the declaration states, the return-value catalogue publishes each
/// type's attributes, and the model validation names the properties each one requires.
pub(in crate::state) const SAGEMAKER_DOMAIN: &str = "AWS::SageMaker::Domain";
/// See [`SAGEMAKER_DOMAIN`].
pub(in crate::state) const SAGEMAKER_USER_PROFILE: &str = "AWS::SageMaker::UserProfile";
/// See [`SAGEMAKER_DOMAIN`].
pub(in crate::state) const SAGEMAKER_SPACE: &str = "AWS::SageMaker::Space";

/// The operations this module asks for, by name — stated rather than spelled at each call so the
/// target a refused resource reports and the request that was made cannot drift apart.
const CREATE_DOMAIN: &str = "CreateDomain";
const DESCRIBE_DOMAIN: &str = "DescribeDomain";
const DELETE_DOMAIN: &str = "DeleteDomain";
const CREATE_USER_PROFILE: &str = "CreateUserProfile";
const DESCRIBE_USER_PROFILE: &str = "DescribeUserProfile";
const DELETE_USER_PROFILE: &str = "DeleteUserProfile";
const CREATE_SPACE: &str = "CreateSpace";
const DESCRIBE_SPACE: &str = "DescribeSpace";
const DELETE_SPACE: &str = "DeleteSpace";

/// The error code SageMaker answers for a Studio resource it does not hold: the modelled
/// `ResourceNotFound` of every read and delete of the three
/// (`smithy_query(query="operation",operation="DescribeDomain,DescribeUserProfile,\
/// DescribeSpace")`), which is what says a delete has nothing left to remove and what a drift
/// read reads as the resource being gone.
const NOT_FOUND: &str = "ResourceNotFound";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the same
/// one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that names no resource at all — one whose
/// identifying properties are not there to address a call with.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code a create that answered 2xx without the identifier it minted is reported under.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// The status in which SageMaker serves one of these resources: `InService`, the one every
/// `…Status` of the three settles in and the one a domain has to be in before it admits a user
/// profile or a space (`smithy_query(query="shape",
/// shape="DomainStatus,UserProfileStatus,SpaceStatus")`).
const IN_SERVICE: &str = "InService";

/// The statuses a NEW one of these resources is still coming up in: the `Pending` its create
/// answers, and the `Updating` a change leaves it in — the two a wait brings a resource out of.
const COMING_UP: &[&str] = &["Pending", "Updating"];

/// The members of a resource's description this module reads: its status, the reason that
/// service gives for one it could not bring up, and the URL a domain and a space publish.
const STATUS: &str = "Status";
const FAILURE_REASON: &str = "FailureReason";
const URL: &str = "Url";

/// The members a create is addressed by and answers with, each the name both sides spell it.
const DOMAIN_ID: &str = "DomainId";
const DOMAIN_NAME: &str = "DomainName";
const DOMAIN_ARN: &str = "DomainArn";
const USER_PROFILE_NAME: &str = "UserProfileName";
const USER_PROFILE_ARN: &str = "UserProfileArn";
const SPACE_NAME: &str = "SpaceName";
const SPACE_ARN: &str = "SpaceArn";

/// The member a declared tag list reaches each create as.
const TAGS: &str = "Tags";

/// Which of the three types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::SageMaker::Domain`.
    Domain,
    /// `AWS::SageMaker::UserProfile`.
    UserProfile,
    /// `AWS::SageMaker::Space`.
    Space,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        SAGEMAKER_DOMAIN => Some(Kind::Domain),
        SAGEMAKER_USER_PROFILE => Some(Kind::UserProfile),
        SAGEMAKER_SPACE => Some(Kind::Space),
        _ => None,
    }
}

/// Every property `AWS::SageMaker::Domain` publishes, which this arm APPLIES: how the domain
/// reaches the network, the mode its members authenticate with, the defaults a user profile and
/// a shared space are created with, its name, its settings, the key its volumes are encrypted
/// with, whether a home file system is created for it and under which key, the subnets and the
/// VPC it communicates through, its tags and whether those tags propagate.
///
/// These are `CreateDomain`'s OWN members, each under the same name on both sides
/// (`smithy_query(query="operation",operation="CreateDomain")`), so a declaration stating any
/// of them reaches SageMaker as the template wrote it and that service's own validation judges
/// it. Taking the create's member set is what keeps a property the type publishes from being
/// dropped or refused as the API grows: the two EFS members
/// (`HomeEfsFileSystemCreation`, the key its volume is encrypted with) are members of that call
/// under those names, so a declaration carrying one is applied rather than failed, while a
/// property no call of this module carries is still refused
/// ([`refuse_unapplied_properties`]).
const DOMAIN_PROPERTIES: &[&str] = &[
    "AppNetworkAccessType",
    "AppSecurityGroupManagement",
    "AuthMode",
    "DefaultSpaceSettings",
    "DefaultUserSettings",
    DOMAIN_NAME,
    "DomainSettings",
    "HomeEfsFileSystemCreation",
    "HomeEfsFileSystemKmsKeyId",
    "KmsKeyId",
    "SubnetIds",
    "TagPropagation",
    TAGS,
    "VpcId",
];

/// Every property `AWS::SageMaker::UserProfile` publishes, each a `CreateUserProfile` member of
/// the same name.
const USER_PROFILE_PROPERTIES: &[&str] = &[
    DOMAIN_ID,
    "SingleSignOnUserIdentifier",
    "SingleSignOnUserValue",
    TAGS,
    USER_PROFILE_NAME,
    "UserSettings",
];

/// Every property `AWS::SageMaker::Space` publishes, each a `CreateSpace` member of the same
/// name.
const SPACE_PROPERTIES: &[&str] = &[
    DOMAIN_ID,
    "OwnershipSettings",
    "SpaceDisplayName",
    SPACE_NAME,
    "SpaceSettings",
    "SpaceSharingSettings",
    TAGS,
];

/// The properties of a domain's description this emulator compares a declaration against, which
/// are the members `DescribeDomain` reports under the names the type publishes them as.
const DOMAIN_REPORTED_PROPERTIES: &[&str] = &[
    "AppNetworkAccessType",
    "AppSecurityGroupManagement",
    "AuthMode",
    "DefaultSpaceSettings",
    "DefaultUserSettings",
    DOMAIN_NAME,
    "DomainSettings",
    "KmsKeyId",
    "SubnetIds",
    "TagPropagation",
    "VpcId",
];

/// The same for a user profile, read off `DescribeUserProfile`.
const USER_PROFILE_REPORTED_PROPERTIES: &[&str] = &[
    DOMAIN_ID,
    "SingleSignOnUserIdentifier",
    "SingleSignOnUserValue",
    USER_PROFILE_NAME,
    "UserSettings",
];

/// The same for a space, read off `DescribeSpace`.
const SPACE_REPORTED_PROPERTIES: &[&str] = &[
    DOMAIN_ID,
    "OwnershipSettings",
    "SpaceDisplayName",
    SPACE_NAME,
    "SpaceSettings",
    "SpaceSharingSettings",
];

/// Create the resource one of the three declarations asks for, in Amazon SageMaker AI itself —
/// so a client that deployed the stack reads every one of them back with that service's own
/// reads, and the identifier the stack publishes is the one that service answers for.
///
/// `Err` is that service's own refusal — a value outside one of its enums, a domain that holds
/// nothing by the declared id, a domain whose provisioning has not settled — which is what the
/// resource fails with, and what rolls the stack back.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type, kind)?;
    let mut request = declared_members(&resource.properties, properties_of(kind));
    let tags = storable_tags(&resource.properties, propagated);
    if tags.is_empty() {
        request.remove(TAGS);
    } else {
        request.insert(TAGS.to_string(), key_value_tag_list(&tags));
    }
    match kind {
        Kind::Domain => {
            // The name the create is made under is CloudFormation's: the declared `DomainName`
            // where the template states one, and the generated name the naming rules computed
            // otherwise — which is the identity the resource carries until SageMaker answers
            // with the one it minted.
            let name = text_member(&resource.properties, DOMAIN_NAME)
                .unwrap_or_else(|| resource.physical_resource_id.clone());
            request.insert(DOMAIN_NAME.to_string(), Value::String(name));
            let answered = call(ctx, region, CREATE_DOMAIN, &Value::Object(request)).await?;
            let domain_id = answered_member(&answered, DOMAIN_ID, CREATE_DOMAIN)?;
            let mut attributes = BTreeMap::new();
            attributes.insert(DOMAIN_ID.to_string(), domain_id.clone());
            for member in [DOMAIN_ARN, URL] {
                if let Some(value) = text_member(&answered, member) {
                    attributes.insert(member.to_string(), value);
                }
            }
            Ok(Created {
                // The id that service assigned REPLACES the name the naming rules computed: it
                // is what `Ref` resolves to and what every call on the domain takes.
                physical_resource_id: Some(domain_id),
                attributes,
            })
        }
        Kind::UserProfile | Kind::Space => {
            let (domain_id, name) = identity_of(kind, resource)?;
            request.insert(DOMAIN_ID.to_string(), Value::String(domain_id));
            request.insert(name_member(kind).to_string(), Value::String(name));
            let operation = create_operation(kind);
            let answered = call(ctx, region, operation, &Value::Object(request)).await?;
            let arn_member = arn_member(kind);
            let mut attributes = BTreeMap::new();
            attributes.insert(
                arn_member.to_string(),
                answered_member(&answered, arn_member, operation)?,
            );
            // The identity stays the PAIR the declaration states, which is what every call on
            // the member takes and what the stack's own deletion addresses it by.
            Ok(Created::attributes(attributes))
        }
    }
}

/// Whether SageMaker reports the created resource ready.
///
/// Each create answers the resource `Pending`, in which it serves nothing — a domain in it
/// admits no user profile and no space, and neither member can be launched — so this asks rather
/// than assumes: `InService` is ready, the statuses a new resource comes out of are a wait
/// ([`COMING_UP`]), and every other status is one no wait brings it out of, which fails the
/// resource with the reason that service gives.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(described) = describe(ctx, region, kind, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "SageMaker holds no {} {physical_resource_id}",
            resource_noun(kind)
        )));
    };
    let status = text_member(&described, STATUS).unwrap_or_default();
    if status == IN_SERVICE {
        return Ok(Readiness::Ready);
    }
    if COMING_UP.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    // The resource is in a status no wait brings it out of. What that service says about it is
    // the account of why, where it gives one.
    Ok(Readiness::Failed(
        text_member(&described, FAILURE_REASON).unwrap_or_else(|| {
            format!(
                "SageMaker reports the {} {physical_resource_id} in status {status}",
                resource_noun(kind)
            )
        }),
    ))
}

/// The attributes SageMaker publishes for a resource it now reports `InService`.
///
/// A domain's are the ones its own create answered, so nothing is read again for one. A SPACE's
/// `Url` is no member of `CreateSpace`'s answer at all — that service reports the address the
/// space is served at through `DescribeSpace` — so it is read off the space that service now
/// holds, which is the one value a client reaching the stack's `Fn::GetAtt Url` can use.
pub(super) async fn ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut attributes = BTreeMap::new();
    if kind != Kind::Space {
        return Ok(attributes);
    }
    let Some(described) = describe(ctx, region, kind, physical_resource_id).await? else {
        return Ok(attributes);
    };
    if let Some(url) = text_member(&described, URL) {
        attributes.insert(URL.to_string(), url);
    }
    Ok(attributes)
}

/// Whether SageMaker has finished removing a deleted resource.
///
/// Every one of the three deletes is asynchronous: that service reports the resource `Deleting`
/// and goes on answering reads of it until the removal is through, so the removal is finished
/// once SageMaker no longer holds it. This is also what holds a stack's teardown back from
/// asking for the domain while a member of it is still on its way out — SageMaker refuses to
/// delete a domain whose members are still there.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let Some(described) = describe(ctx, region, kind, physical_resource_id).await? else {
        return Ok(Removal::Gone);
    };
    Ok(Removal::Pending(
        text_member(&described, STATUS).unwrap_or_default(),
    ))
}

/// Delete the resource a stack created, addressed by the identifier it publishes: the domain's
/// own `DomainId`, and the pair that identifies a member of one.
///
/// A resource SageMaker no longer holds is already in the state the delete asks for, judged on
/// the CODE its deletes answer for one they cannot find and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let request = addressed(kind, &resource.physical_resource_id)?;
    match call(ctx, region, delete_operation(kind), &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as SageMaker reports it, for drift detection.
///
/// Each description answers the create's own members under those same names, so the answer maps
/// onto the property vocabulary one for one. A resource that service no longer holds was
/// deleted, which is what `None` says ([`super::ActualConfiguration::read`]).
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(described) = describe(ctx, region, kind, &resource.physical_resource_id).await?
    else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    for property in reported_properties(kind) {
        if let Some(value) = described.get(*property).filter(|value| !value.is_null()) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(configuration)))
}

/// Every property the type of `kind` publishes, for the one check that refuses a declared
/// property no call this module makes carries.
fn properties_of(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Domain => DOMAIN_PROPERTIES,
        Kind::UserProfile => USER_PROFILE_PROPERTIES,
        Kind::Space => SPACE_PROPERTIES,
    }
}

/// The properties a description of one of the three reports back, read in the type's own
/// property vocabulary.
fn reported_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Domain => DOMAIN_REPORTED_PROPERTIES,
        Kind::UserProfile => USER_PROFILE_REPORTED_PROPERTIES,
        Kind::Space => SPACE_REPORTED_PROPERTIES,
    }
}

/// The create of `kind`, as that operation's own model names it.
fn create_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => CREATE_DOMAIN,
        Kind::UserProfile => CREATE_USER_PROFILE,
        Kind::Space => CREATE_SPACE,
    }
}

/// The read of `kind`. See [`create_operation`].
fn describe_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => DESCRIBE_DOMAIN,
        Kind::UserProfile => DESCRIBE_USER_PROFILE,
        Kind::Space => DESCRIBE_SPACE,
    }
}

/// The delete of `kind`. See [`create_operation`].
fn delete_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => DELETE_DOMAIN,
        Kind::UserProfile => DELETE_USER_PROFILE,
        Kind::Space => DELETE_SPACE,
    }
}

/// The member a MEMBER of a domain carries its own name in.
fn name_member(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => DOMAIN_NAME,
        Kind::UserProfile => USER_PROFILE_NAME,
        Kind::Space => SPACE_NAME,
    }
}

/// The member the create of a member of a domain answers its ARN in.
fn arn_member(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => DOMAIN_ARN,
        Kind::UserProfile => USER_PROFILE_ARN,
        Kind::Space => SPACE_ARN,
    }
}

/// What one of the three resources is CALLED in a sentence about it, for the reports a readiness
/// answer words.
fn resource_noun(kind: Kind) -> &'static str {
    match kind {
        Kind::Domain => "domain",
        Kind::UserProfile => "user profile",
        Kind::Space => "space",
    }
}

/// The request that addresses the ONE resource `physical_resource_id` identifies: the `DomainId`
/// of a domain, and the domain and the name of a member of one — the pair that type's composite
/// primary identifier is made of ([`identity_parts`]).
///
/// The one place this module turns a published identity back into a request, so a read and a
/// delete cannot come to address different resources.
fn addressed(kind: Kind, physical_resource_id: &str) -> Result<Value, Refusal> {
    if kind == Kind::Domain {
        return Ok(serde_json::json!({ DOMAIN_ID: physical_resource_id }));
    }
    let Some((domain_id, name)) = identity_parts(kind, physical_resource_id) else {
        return Err(unidentified(kind));
    };
    Ok(serde_json::json!({
        DOMAIN_ID: domain_id,
        name_member(kind): name,
    }))
}

/// The domain and the name the identity of a member of a domain is made of, or `None` for an
/// identifier that is not such a pair.
///
/// A pair is the two values its type's composite primary identifier is made of, in THAT type's
/// own order ([`crate::state::template::composite_identity_properties`]: a user profile's own
/// name leads, a space's domain does), joined with the one separator CloudFormation composes a
/// composite physical resource id with
/// ([`crate::state::template::COMPOSITE_IDENTITY_SEPARATOR`]). Neither half can carry that
/// separator: both `DomainId` and the member's own name are modelled with patterns that admit
/// letters, digits and hyphens alone.
fn identity_parts(kind: Kind, physical_resource_id: &str) -> Option<(String, String)> {
    let (first, second) = physical_resource_id
        .split_once(crate::state::template::COMPOSITE_IDENTITY_SEPARATOR)?;
    if first.is_empty() || second.is_empty() {
        return None;
    }
    let (domain_id, name) = match kind {
        Kind::UserProfile => (second, first),
        _ => (first, second),
    };
    Some((domain_id.to_string(), name.to_string()))
}

/// The domain and the name a member of a domain is CREATED under: the parts of the identity
/// CloudFormation composed for it, and the properties the declaration states them in where that
/// identity is not a pair at all (a declaration missing one of them, which its own model
/// validation is what refuses).
fn identity_of(kind: Kind, resource: &StoredResource) -> Result<(String, String), Refusal> {
    if let Some(pair) = identity_parts(kind, &resource.physical_resource_id) {
        return Ok(pair);
    }
    let domain_id = text_member(&resource.properties, DOMAIN_ID);
    let name = text_member(&resource.properties, name_member(kind));
    match (domain_id, name) {
        (Some(domain_id), Some(name)) => Ok((domain_id, name)),
        _ => Err(unidentified(kind)),
    }
}

/// The refusal for a declaration that names no resource of `kind` at all: the two properties
/// every call on one takes are what identify it, and a declaration stating neither addresses
/// nothing — so nothing is asked of SageMaker rather than a call being made about a resource
/// nobody named.
fn unidentified(kind: Kind) -> Refusal {
    Refusal {
        code: INVALID_REQUEST.to_string(),
        message: format!(
            "a SageMaker {} is identified by its {DOMAIN_ID} and its {}, and the declaration \
             states neither",
            resource_noun(kind),
            name_member(kind)
        ),
    }
}

/// The resource as SageMaker's own read of it reports it, or `None` for one that service no
/// longer holds.
async fn describe(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = addressed(kind, physical_resource_id)?;
    match call(ctx, region, describe_operation(kind), &request).await {
        Ok(described) => Ok(Some(described)),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// One call to Amazon SageMaker AI on the awsJson1_1 wire its model declares.
///
/// The wire identity is the catalogue's own reading of it
/// ([`crate::state::provisioner::SAGEMAKER_SIGNING_NAME`], that model's `sigv4Name` and service
/// shape), shared with the arms that call the same control plane, so the two cannot drift
/// apart.
///
/// The refusal travels exactly as that service worded it: SageMaker's resource providers report
/// its own message ([`super::Owner::handler_message`]), so nothing is added around it here.
async fn call(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        SAGEMAKER_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{SAGEMAKER_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// The members of `members` a declaration states, as the request carries them: the property and
/// the member are the same name on both sides, so every value goes through as the template wrote
/// it and that service's own validation judges what it would judge for a client's own call. A
/// property declared as `null` states no value at all, so it is left out rather than sent as a
/// null member.
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    for member in members {
        if let Some(value) = properties.get(*member).filter(|value| !value.is_null()) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    request
}

/// One text member of a document, or `None` where it carries none: a property a declaration
/// states, and a member an answer reports, are read the same way — the member has to be there
/// and carry characters to be a value at all.
fn text_member(document: &Value, member: &str) -> Option<String> {
    document
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The member SageMaker answered a create with, which is the handle a client addresses the
/// resource by.
///
/// An answer carrying none would leave no handle a client could call that service with and
/// nothing for the stack to publish, so it is a failed create rather than a created resource.
fn answered_member(
    answered: &Value,
    member: &str,
    operation: &str,
) -> Result<String, Refusal> {
    text_member(answered, member).ok_or_else(|| Refusal {
        code: UNIDENTIFIED_FAILURE.to_string(),
        message: format!("SageMaker answered {operation} without a {member}"),
    })
}

/// The refusal for a declared property this module does not apply, naming which property it was:
/// refusing is what keeps a resource from being reported as created with less than its
/// declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    kind: Kind,
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    let published = properties_of(kind);
    let unapplied = properties
        .keys()
        .map(String::as_str)
        .find(|property| !published.contains(property));
    match unapplied {
        Some(property) => Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: each of the three type names this module owns resolves to its own kind, and a type
    /// it does not own resolves to none.
    #[test]
    fn each_type_resolves_to_its_own_kind() {
        let rows = [
            (SAGEMAKER_DOMAIN, Some(Kind::Domain)),
            (SAGEMAKER_USER_PROFILE, Some(Kind::UserProfile)),
            (SAGEMAKER_SPACE, Some(Kind::Space)),
            ("AWS::SageMaker::Model", None),
            ("AWS::NotAService::NotAType", None),
        ];
        for (resource_type, expected) in rows {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, expected,
                "case {resource_type:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the identity of a MEMBER of a domain is the pair its own API addresses it by, so a
    /// composed identity addresses that pair and anything that is not a pair addresses nothing.
    #[test]
    fn a_members_identity_addresses_the_pair_it_is_made_of() {
        let space = addressed(Kind::Space, "d-abcdefghijkl|team-space")
            .expect("a pair addresses a space");
        assert_eq!(
            space,
            serde_json::json!({ "DomainId": "d-abcdefghijkl", "SpaceName": "team-space" }),
            "case \"a composed pair\": expected both halves of the pair, got {space}"
        );
        let profile = addressed(Kind::UserProfile, "analyst|d-abcdefghijkl")
            .expect("a pair addresses a user profile");
        assert_eq!(
            profile,
            serde_json::json!({ "DomainId": "d-abcdefghijkl", "UserProfileName": "analyst" }),
            "case \"a composed pair, user profile\": expected both halves, got {profile}"
        );
        let domain = addressed(Kind::Domain, "d-abcdefghijkl").expect("an id addresses a domain");
        assert_eq!(
            domain,
            serde_json::json!({ "DomainId": "d-abcdefghijkl" }),
            "case \"a domain id\": expected the domain's own id, got {domain}"
        );
        for unpaired in ["", "d-abcdefghijkl", "|team-space", "d-abcdefghijkl|"] {
            let refused = addressed(Kind::Space, unpaired);
            assert!(
                refused.is_err(),
                "case {unpaired:?}: expected no resource to be addressed, got {refused:?}"
            );
        }
    }

    /// RULE: a declared property no call this module makes carries is refused, naming it; every
    /// property the type publishes is applied.
    #[test]
    fn a_property_no_call_carries_is_refused() {
        let declared = serde_json::json!({
            "DomainName": "studio",
            "AuthMode": "IAM",
            "NotAProperty": "x",
        });
        let refused = refuse_unapplied_properties(&declared, SAGEMAKER_DOMAIN, Kind::Domain)
            .expect_err("an unpublished property is refused");
        assert!(
            refused.message.contains("NotAProperty"),
            "case \"an unpublished property\": expected the refusal to name it, got {refused:?}"
        );
        let applied = serde_json::json!({
            "DomainName": "studio",
            "AuthMode": "IAM",
            "VpcId": "vpc-00000000000000001",
            "SubnetIds": ["subnet-00000000000000001"],
            "DefaultUserSettings": { "ExecutionRole": "arn:aws:iam::000000000000:role/studio" },
            "Tags": [{ "Key": "team", "Value": "ml" }],
        });
        let accepted = refuse_unapplied_properties(&applied, SAGEMAKER_DOMAIN, Kind::Domain);
        assert!(
            accepted.is_ok(),
            "case \"every published property\": expected acceptance, got {accepted:?}"
        );
    }
}
