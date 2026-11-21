// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Data Lifecycle Manager's own control plane: the
//! lifecycle policy an `AWS::DLM::LifecyclePolicy` declares. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! A lifecycle policy declares no name of its own: `CreateLifecyclePolicy` takes the role the
//! policy runs under, its activation state and the configuration it runs, takes NO identifier at
//! all, and answers the `PolicyId` Amazon Data Lifecycle Manager minted
//! (`^policy-[a-f0-9]+$`). That id is the type's primary identifier — what its `Ref` returns —
//! so it becomes the resource's physical id, and it is the `PolicyId` label
//! `GetLifecyclePolicy`, `UpdateLifecyclePolicy` and `DeleteLifecyclePolicy` each address one
//! by.
//!
//! The type's properties ARE `CreateLifecyclePolicy`'s own members
//! ([`DLM_LIFECYCLE_POLICY_MEMBERS`]): `restJson1` carries a member under its own name unless the
//! model renames it, and that operation renames none. The one property that is not a member of
//! the create as the template spells it is the tag set: the type publishes `Tags` as the
//! `[{Key, Value}]` list most types do, while that operation's own `Tags` member is a `TagMap`,
//! so the set is carried as the map it takes.
//!
//! What a policy REPORTS is that service's — the id it minted and the `PolicyArn` it built
//! around it — so each published value is read off its own report of the policy rather than
//! computed here.

use super::*;

/// Amazon Data Lifecycle Manager's signing name, from its model (`sigv4Name: dlm`), which speaks
/// `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares, naming the operation so a refusal is reported the way
/// that service's own SDK raises it.
pub(super) const DLM_SIGNING_NAME: &str = "dlm";

/// The collection `CreateLifecyclePolicy` is bound to (`POST /policies`), and the collection one
/// policy is addressed under by the id that service minted (`GET`/`PATCH`/`DELETE
/// /policies/{PolicyId}`).
const POLICIES_URI: &str = "/policies";

/// The collection a policy's TAGS are addressed under, by the policy's own ARN
/// (`/tags/{ResourceArn}`): `ListTagsForResource` reads them, `TagResource` writes them and
/// `UntagResource` drops them.
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as Amazon Data Lifecycle Manager's own model names them.
const CREATE_LIFECYCLE_POLICY: &str = "CreateLifecyclePolicy";
const GET_LIFECYCLE_POLICY: &str = "GetLifecyclePolicy";
const UPDATE_LIFECYCLE_POLICY: &str = "UpdateLifecyclePolicy";
const DELETE_LIFECYCLE_POLICY: &str = "DeleteLifecyclePolicy";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code that service answers for a policy it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read, update and delete).
const DLM_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member the tag set travels as on the create and on `TagResource` alike: the `TagMap` of
/// tag key to value.
const TAGS_MEMBER: &str = "Tags";

/// The member `GetLifecyclePolicy` reports the policy under, and the two members of that record
/// this arm reads: the id that service minted and the ARN it built around it.
const POLICY_MEMBER: &str = "Policy";
const POLICY_ID_MEMBER: &str = "PolicyId";
const POLICY_ARN_MEMBER: &str = "PolicyArn";

/// The attribute the type publishes, which is the ARN that service holds the policy under.
const ARN_ATTRIBUTE: &str = "Arn";

/// The properties of the type that are members of `CreateLifecyclePolicy`'s request body, each
/// under the member of the same name.
///
/// The tag set is not among them: it is merged with the tags CloudFormation propagates and
/// carried as the `TagMap` that operation takes before it is written ([`tags_of`]).
const DLM_LIFECYCLE_POLICY_MEMBERS: &[(&str, &str)] = &[
    ("CopyTags", "CopyTags"),
    ("CreateInterval", "CreateInterval"),
    ("CrossRegionCopyTargets", "CrossRegionCopyTargets"),
    ("DefaultPolicy", "DefaultPolicy"),
    ("Description", "Description"),
    ("Exclusions", "Exclusions"),
    ("ExecutionRoleArn", "ExecutionRoleArn"),
    ("ExtendDeletion", "ExtendDeletion"),
    ("PolicyDetails", "PolicyDetails"),
    ("RetainInterval", "RetainInterval"),
    ("State", "State"),
];

/// The members `UpdateLifecyclePolicy` takes of those: every one but the KIND of default policy,
/// which that operation does not carry at all — `DefaultPolicy` is documented on the create as
/// "the type of default policy to create" — which is why it is this type's one replacement
/// property ([`super::template::replacement_properties`]) rather than a member of this request.
const DLM_LIFECYCLE_POLICY_UPDATABLE_MEMBERS: &[(&str, &str)] = &[
    ("CopyTags", "CopyTags"),
    ("CreateInterval", "CreateInterval"),
    ("CrossRegionCopyTargets", "CrossRegionCopyTargets"),
    ("Description", "Description"),
    ("Exclusions", "Exclusions"),
    ("ExecutionRoleArn", "ExecutionRoleArn"),
    ("ExtendDeletion", "ExtendDeletion"),
    ("PolicyDetails", "PolicyDetails"),
    ("RetainInterval", "RetainInterval"),
    ("State", "State"),
];

/// Every property of the type this arm carries onto the owning API, so a declaration stating one
/// outside the set is REFUSED rather than reported as created without it
/// ([`refuse_unmapped_properties`]): the create's own members above, and the tag set.
const DLM_LIFECYCLE_POLICY_MAPPED_PROPERTIES: &[&str] = &[
    "CopyTags",
    "CreateInterval",
    "CrossRegionCopyTargets",
    "DefaultPolicy",
    "Description",
    "Exclusions",
    "ExecutionRoleArn",
    "ExtendDeletion",
    "PolicyDetails",
    "RetainInterval",
    "State",
    "Tags",
];

/// The members of the request that are NUMBERS on the wire: the creation frequency and the
/// retention period of a default policy (`CreateInterval` and `RetainInterval`, both
/// `@range(min: 1)` in that service's own model).
///
/// A template states a value either in JSON's own spelling or as the TEXT a parameter reference
/// resolves to — `Ref` on a parameter answers that parameter's value, which a stack operation
/// carries as a string whatever the parameter's declared type — so each is read in the one
/// spelling the member takes ([`declared_number`]) before the request is sent. A value already in
/// that spelling is left exactly as it came, and so is one that is neither spelling of a number:
/// the owning operation's own validation is what refuses a value that is no number at all, rather
/// than this dropping it.
const DLM_INTERVAL_MEMBERS: &[&str] = &["CreateInterval", "RetainInterval"];

/// The scalar properties `GetLifecyclePolicy` reports back under the names the type declares, for
/// drift detection: the policy's description, its activation state and the role it runs under. A
/// property that is a STRUCTURE or a LIST on the wire (the configuration, either default-policy
/// selection) is left out, which is the position every arm whose property is not a scalar is in
/// and what [`drift`] documents.
const DLM_LIFECYCLE_POLICY_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("Description", "Description"),
    ("ExecutionRoleArn", "ExecutionRoleArn"),
    ("State", "State"),
];

/// Create the lifecycle policy an `AWS::DLM::LifecyclePolicy` declares, in Amazon Data Lifecycle
/// Manager — so a client that deployed the stack reads the policy back through that service's own
/// `GetLifecyclePolicy` and `GetLifecyclePolicies` for the identity the stack published.
///
/// The declaration IS the request ([`DLM_LIFECYCLE_POLICY_MEMBERS`]), with the declared tags
/// beside the ones CloudFormation gives the resource ([`tags_of`]) as the `TagMap` that call's own
/// `Tags` member takes.
///
/// The identity is that service's: the `PolicyId` it answered becomes this resource's physical
/// id — the type's primary identifier, which its `Ref` returns — and the `PolicyArn` it holds the
/// policy under is the one attribute the type publishes, read off its own report of the created
/// policy rather than built here.
pub(super) async fn create_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, DLM_LIFECYCLE_POLICY_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, DLM_LIFECYCLE_POLICY_MEMBERS, &mut request);
    carried_as(&mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&tags)));
    }
    let answered = call_rest_json_named(
        ctx,
        DLM_SIGNING_NAME,
        region,
        CREATE_LIFECYCLE_POLICY,
        "POST",
        POLICIES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let policy_id = reported_text(&answered, POLICY_ID_MEMBER).unwrap_or_default();
    if policy_id.is_empty() {
        // There would be no handle a client could call Amazon Data Lifecycle Manager with, and
        // nothing for the stack to publish, so this is a failed create rather than a created
        // resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Amazon Data Lifecycle Manager control plane answered a created \
                      lifecycle policy without a policy id"
                .to_string(),
        });
    }
    // The ARN is that service's to build, and the create answers only the id, so the one
    // attribute the type publishes is read off that service's own report of the policy it just
    // made.
    let reported = read_dlm_lifecycle_policy(ctx, region, &policy_id).await?;
    let arn = reported
        .as_ref()
        .and_then(|policy| reported_text(policy, POLICY_ARN_MEMBER))
        .unwrap_or_default();
    if arn.is_empty() {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the Amazon Data Lifecycle Manager control plane reports no {POLICY_ARN_MEMBER} \
                 for the lifecycle policy {policy_id} it created"
            ),
        });
    }
    Ok(Created {
        physical_resource_id: Some(policy_id),
        attributes: [(ARN_ATTRIBUTE.to_string(), arn)].into_iter().collect(),
    })
}

/// Apply a changed declaration to the policy the stack created, through `UpdateLifecyclePolicy` —
/// the one call that reconfigures a policy that exists.
///
/// That call takes the policy's description, its activation state, the role it runs under and its
/// whole configuration ([`DLM_LIFECYCLE_POLICY_UPDATABLE_MEMBERS`]), addressed by the id that
/// service minted. The KIND of default policy is not one of them, which is why a declaration
/// restating it replaces the policy instead of reaching here
/// ([`super::template::replacement_properties`]). The tags travel separately
/// ([`retag_dlm_lifecycle_policy`]).
pub(super) async fn update_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(
        &properties,
        DLM_LIFECYCLE_POLICY_UPDATABLE_MEMBERS,
        &mut request,
    );
    carried_as(&mut request);
    call_rest_json_named(
        ctx,
        DLM_SIGNING_NAME,
        region,
        UPDATE_LIFECYCLE_POLICY,
        "PATCH",
        &policy_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // The policy keeps the identity that service minted, and the ARN built around it, so the
    // attribute the type publishes is re-read from that service rather than assumed unchanged.
    let arn = read_dlm_lifecycle_policy(ctx, region, &resource.physical_resource_id)
        .await?
        .and_then(|policy| reported_text(&policy, POLICY_ARN_MEMBER));
    Ok(Created::attributes(
        arn.map(|arn| (ARN_ATTRIBUTE.to_string(), arn))
            .into_iter()
            .collect(),
    ))
}

/// Bring the tags of the policy a resource record names to the ones this deployment gives it,
/// through that service's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment no
/// longer gives the resource and is removed, and one in it with another value is re-valued. Where
/// the two already agree nothing is called at all, which is what makes this safe to ask on every
/// deployment; a policy that service no longer holds has no tags to reconcile.
///
/// Those calls address a policy by its ARN rather than by the id this resource's physical id is,
/// so the ARN is read off that service's own report of the policy.
pub(super) async fn retag_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(policy) =
        read_dlm_lifecycle_policy(ctx, region, &resource.physical_resource_id).await?
    else {
        return Ok(());
    };
    let Some(arn) = reported_text(&policy, POLICY_ARN_MEMBER) else {
        return Ok(());
    };
    let uri = format!("{TAGS_URI}/{}", percent_encoded(&arn));
    let answered =
        match call_rest_json_named(ctx, DLM_SIGNING_NAME, region, LIST_TAGS_FOR_RESOURCE, "GET", &uri, None)
            .await
        {
            Ok(answered) => answered,
            Err(refusal) if refusal.code == DLM_RESOURCE_NOT_FOUND => return Ok(()),
            Err(refusal) => return Err(refusal),
        };
    // That service answers a policy's tags as the MAP of key to value a declaration's `Tags`
    // property may also be written as, so one reader serves both sides.
    let held = declared_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            DLM_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "POST",
            &uri,
            Some(&Value::Object(
                [(
                    TAGS_MEMBER.to_string(),
                    Value::Object(tags_as_map(&added)),
                )]
                .into_iter()
                .collect(),
            )),
        )
        .await?;
    }
    if !removed.is_empty() {
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per key
        // rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            DLM_SIGNING_NAME,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{uri}?{keys}"),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the lifecycle policy a stack created, addressed by the id Amazon Data Lifecycle Manager
/// minted for it — this resource's physical id. A policy it no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message, which carries the
/// request's own id) is already in the state the delete asks for.
pub(super) async fn delete_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        DLM_SIGNING_NAME,
        region,
        DELETE_LIFECYCLE_POLICY,
        "DELETE",
        &policy_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DLM_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The policy's configuration as Amazon Data Lifecycle Manager reports it, for drift detection:
/// `GetLifecyclePolicy`'s own record, read by the id the stack published. One that service no
/// longer holds was deleted.
pub(super) async fn describe_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_dlm_lifecycle_policy(ctx, region, physical_resource_id)
        .await?
        .map(|policy| reported_as_properties(&policy, DLM_LIFECYCLE_POLICY_REPORTED_PROPERTIES)))
}

/// The policy record that service holds under `policy_id`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
///
/// `GetLifecyclePolicy` reports the record under its own `Policy` member, so what comes back is
/// that record rather than the envelope around it.
async fn read_dlm_lifecycle_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    policy_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        DLM_SIGNING_NAME,
        region,
        GET_LIFECYCLE_POLICY,
        "GET",
        &policy_uri(policy_id),
        None,
    )
    .await
    {
        Ok(answered) => Ok(answered.get(POLICY_MEMBER).cloned()),
        Err(refusal) if refusal.code == DLM_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one policy is addressed at: the collection and the `PolicyId` `@httpLabel` escaped as a
/// single path segment ([`percent_encoded`]), so a value outside that member's own pattern is
/// refused by that service's own validation rather than addressing another path.
fn policy_uri(policy_id: &str) -> String {
    format!("{POLICIES_URI}/{}", percent_encoded(policy_id))
}

/// Put the numeric members of the request into the spelling they are carried in
/// ([`DLM_INTERVAL_MEMBERS`]).
fn carried_as(request: &mut Map<String, Value>) {
    for member in DLM_INTERVAL_MEMBERS {
        let Some(converted) = request.get(*member).and_then(declared_number) else {
            continue;
        };
        request.insert((*member).to_string(), converted);
    }
}
