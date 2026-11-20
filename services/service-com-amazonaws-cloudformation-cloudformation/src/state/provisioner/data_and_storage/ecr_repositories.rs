// SPDX-License-Identifier: CC0-1.0

//! The `AWS::ECR::Repository` provider: a repository of the PRIVATE Amazon ECR registry,
//! created, re-configured, re-tagged and deleted through Amazon ECR's own API.
//!
//! The declaration's settings reach that registry through the call ECR publishes for each
//! one: the members `CreateRepository` itself takes travel on the create, and each of the
//! others — the repository's resource-based policy, its lifecycle policy, its tag
//! mutability and its scanning configuration — through the call that writes it, both when
//! the stack creates the repository and when a later deployment states it differently.

use super::*;

/// Amazon ECR's signing name and `X-Amz-Target` prefix, from its model
/// (`sigv4Name: ecr`, service shape `AmazonEC2ContainerRegistry_V20150921`), which
/// speaks `awsJson1_1`.
pub(super) const ECR_SIGNING_NAME: &str = "ecr";
pub(super) const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// What an `AWS::ECR::Repository`'s declaration states, as `CreateRepository`'s own
/// members spell it: the property name against the member it is sent under.
///
/// Source: AWS's CloudFormation resource reference for the type, member by member
/// against `CreateRepositoryRequest` (`smithy_query(query="operation",
/// operation="CreateRepository")`). The nested documents go through as the template
/// spells them, because each one's members are spelled the same on both sides but for
/// their first letter — `ImageScanningConfiguration.ScanOnPush` against
/// `imageScanningConfiguration.scanOnPush`, `EncryptionConfiguration.EncryptionType` /
/// `.KmsKey` against `encryptionConfiguration.encryptionType` / `.kmsKey` — so they are
/// mapped member by member rather than passed through
/// ([`REPOSITORY_DOCUMENT_MEMBERS`]).
const REPOSITORY_SCALAR_MEMBERS: &[(&str, &str)] = &[("ImageTagMutability", "imageTagMutability")];

/// One nested document of an `AWS::ECR::Repository`: the property it is declared under,
/// the member `CreateRepository` takes it as, and the property-to-member mapping of its
/// own members.
type RepositoryDocument = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

/// The nested documents of an `AWS::ECR::Repository`, as the member each is sent under
/// and the property-to-member mapping inside it. See [`REPOSITORY_SCALAR_MEMBERS`].
const REPOSITORY_DOCUMENT_MEMBERS: &[RepositoryDocument] = &[
    (
        "ImageScanningConfiguration",
        "imageScanningConfiguration",
        &[("ScanOnPush", "scanOnPush")],
    ),
    (
        "EncryptionConfiguration",
        "encryptionConfiguration",
        &[("EncryptionType", "encryptionType"), ("KmsKey", "kmsKey")],
    ),
];

/// Create the repository an `AWS::ECR::Repository` declares, so a client that pushes to
/// or reads the registry the stack declared finds it in Amazon ECR itself.
///
/// The repository's ARN and URI are read back off the answer: they are the registry's
/// real identity for this repository, so they replace the ones the naming rules computed.
/// A declared `RepositoryPolicyText` is the repository's resource-based policy, and a
/// declared `LifecyclePolicy` its lifecycle policy — neither is a member of
/// `CreateRepository`, so each is installed on the repository this call just created
/// through the call Amazon ECR publishes for it, exactly as the settings documents that
/// ARE members of the create travel with it.
pub(crate) async fn create_repository(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = match &resource.properties {
        Value::Object(object) => object.clone(),
        _ => Map::new(),
    };
    let mut request = Map::new();
    // The lifecycle policy is read out of the declaration BEFORE the repository is
    // created, so a declaration this provider cannot build a `PutLifecyclePolicy` from
    // leaves no repository behind, exactly as a create Amazon ECR refuses does.
    let lifecycle = lifecycle_policy_request(&properties, &resource.physical_resource_id)?;
    // The repository's name is this resource's physical id — "Ref returns the repository
    // name" — already resolved against the stack's parameters.
    request.insert(
        "repositoryName".to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    for (property, member) in REPOSITORY_SCALAR_MEMBERS {
        if let Some(value) = properties.get(*property) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    for (property, member, mapping) in REPOSITORY_DOCUMENT_MEMBERS {
        let Some(Value::Object(declared)) = properties.get(*property) else {
            continue;
        };
        let mut document = Map::new();
        for (inner_property, inner_member) in *mapping {
            if let Some(value) = declared.get(*inner_property) {
                document.insert((*inner_member).to_string(), value.clone());
            }
        }
        request.insert((*member).to_string(), Value::Object(document));
    }
    // `Tags` is a list of `{Key, Value}` on both sides: ECR's own `Tag` shape spells its
    // members `Key` and `Value` too, so the declared list goes through as declared,
    // carrying the tags CloudFormation gives the repository beside them.
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("tags".to_string(), tags_as_list(&tags, UPPER_CASED_TAG));
    }
    let reported = call_ecr(ctx, region, "CreateRepository", &Value::Object(request)).await?;
    if let Some(policy) = repository_policy_text(&properties) {
        call_ecr(
            ctx,
            region,
            "SetRepositoryPolicy",
            &serde_json::json!({
                "repositoryName": resource.physical_resource_id,
                "policyText": policy,
            }),
        )
        .await?;
    }
    // "LifecyclePolicy — creates or updates a lifecycle policy" (this type's own
    // property): the policy it carries is what `PutLifecyclePolicy` ("creates or updates
    // the lifecycle policy for the specified repository") takes, so a repository whose
    // declaration states one answers a read of it with that policy rather than reporting
    // none.
    if let Some(lifecycle_request) = lifecycle {
        call_ecr(ctx, region, "PutLifecyclePolicy", &lifecycle_request).await?;
    }
    let repository = reported.get("repository");
    let mut attributes = BTreeMap::new();
    for (member, attribute) in [("repositoryArn", "Arn"), ("repositoryUri", "RepositoryUri")] {
        if let Some(value) = repository
            .and_then(|repository| repository.get(member))
            .and_then(Value::as_str)
        {
            attributes.insert(attribute.to_string(), value.to_string());
        }
    }
    // The repository was created under the name CloudFormation decided.
    Ok(Created::attributes(attributes))
}

/// The repository policy an `AWS::ECR::Repository` declares, as the policy TEXT
/// `SetRepositoryPolicy` takes: the one spelling rule a declared policy document reaches
/// its owning service under ([`document_text`]).
fn repository_policy_text(properties: &Map<String, Value>) -> Option<String> {
    properties.get("RepositoryPolicyText").and_then(document_text)
}

/// The property an `AWS::ECR::Repository` states its lifecycle policy under, and the two
/// members of that property: the policy document and the registry the repository belongs
/// to, as AWS's resource reference for the type spells them.
const REPOSITORY_LIFECYCLE_POLICY: &str = "LifecyclePolicy";
const REPOSITORY_LIFECYCLE_POLICY_TEXT: &str = "LifecyclePolicyText";
const REPOSITORY_LIFECYCLE_REGISTRY_ID: &str = "RegistryId";

/// The lifecycle policy an `AWS::ECR::Repository` declares, as the request
/// `PutLifecyclePolicy` takes: the `LifecyclePolicy` property's `LifecyclePolicyText` is
/// the policy document itself, and its `RegistryId` the registry the repository belongs to
/// — the two members of that call besides the repository's name, which is this resource's
/// physical id. The document reaches that call under the same spelling rule every declared
/// policy document does ([`document_text`]).
///
/// `Ok(None)` is a declaration that states no lifecycle policy at all. A declaration that
/// states the property but carries no document in it is one no `PutLifecyclePolicy` can be
/// built from: that is a refusal, so the resource fails rather than completing with a
/// declared part silently dropped.
fn lifecycle_policy_request(
    properties: &Map<String, Value>,
    repository_name: &str,
) -> Result<Option<Value>, Refusal> {
    let malformed = |what: &str| Refusal {
        code: INVALID_PROPERTY_CODE.to_string(),
        message: format!("the {REPOSITORY_LIFECYCLE_POLICY} of {ECR_REPOSITORY} {what}"),
    };
    let declared = match properties.get(REPOSITORY_LIFECYCLE_POLICY) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(declared)) => declared,
        Some(_) => return Err(malformed("is not a settings document")),
    };
    let text = declared
        .get(REPOSITORY_LIFECYCLE_POLICY_TEXT)
        .and_then(document_text)
        .ok_or_else(|| malformed(&format!("declares no {REPOSITORY_LIFECYCLE_POLICY_TEXT}")))?;
    let mut request = Map::new();
    request.insert(
        "repositoryName".to_string(),
        Value::String(repository_name.to_string()),
    );
    request.insert("lifecyclePolicyText".to_string(), Value::String(text));
    // A registry id is a scalar the declaration may spell as text or as the 12-digit
    // number it is, both of which reach the API member as its one textual spelling
    // ([`scalar_text`]); anything else is no registry id, which is refused rather than
    // dropped.
    match declared.get(REPOSITORY_LIFECYCLE_REGISTRY_ID) {
        None | Some(Value::Null) => {}
        Some(registry) => {
            let id = scalar_text(registry).ok_or_else(|| {
                malformed(&format!(
                    "declares a {REPOSITORY_LIFECYCLE_REGISTRY_ID} that is no registry id"
                ))
            })?;
            request.insert("registryId".to_string(), Value::String(id));
        }
    }
    Ok(Some(Value::Object(request)))
}

/// One Amazon ECR call, over the awsJson1.1 wire that registry speaks.
async fn call_ecr(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        ECR_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECR_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// The error codes Amazon ECR answers for a repository that carries no such configuration,
/// which is the state a deployment that STOPS declaring one is asking the repository into.
const LIFECYCLE_POLICY_NOT_FOUND: &str = "LifecyclePolicyNotFoundException";
const REPOSITORY_POLICY_NOT_FOUND: &str = "RepositoryPolicyNotFoundException";

/// The setting a repository goes back to when a deployment stops declaring
/// `ImageTagMutability`: "if this parameter is omitted, the default setting of `MUTABLE` will
/// be used which will allow image tags to be overwritten"
/// (`smithy_query(query="doc",shape="CreateRepositoryRequest",member="imageTagMutability")`).
const DEFAULT_IMAGE_TAG_MUTABILITY: &str = "MUTABLE";

/// The member of an `ImageScanningConfiguration`, and what a repository goes back to when a
/// deployment stops declaring the document: "if this parameter is not specified, it will
/// default to `false` and images will not be scanned"
/// (`smithy_query(query="doc",shape="ImageScanningConfiguration",member="scanOnPush")`).
const SCAN_ON_PUSH: &str = "scanOnPush";
const DEFAULT_SCAN_ON_PUSH: bool = false;

/// The properties of an `AWS::ECR::Repository` a repository can take a new value for while
/// it goes on existing — the type's no-interruption properties — each against the Amazon ECR
/// call that writes it on a repository that already exists.
///
/// The repository therefore keeps its name, and so its ARN, its URI and everything that
/// pushes to or pulls from it. `Tags` is one of them, applied in [`retag_repository`] rather
/// than in [`update_repository`], where every tag the repository is to carry is reconciled
/// at once; `EmptyOnDelete` is one too and reaches no call at all, because it is not state
/// the registry holds — it steers how the repository is DELETED, which the stack's own
/// record of the declaration decides.
///
/// Two properties are deliberately NOT here, so a deployment that changes one is reported as
/// not applied rather than as a success that dropped it:
///
/// * `EncryptionConfiguration`, which Amazon ECR publishes no call to change on a repository
///   that exists — `encryptionConfiguration` is a member of `CreateRepository` and of no
///   other call;
/// * `ImageTagMutabilityExclusionFilters`, which the create does not carry either
///   ([`REPOSITORY_SCALAR_MEMBERS`]), so nothing here could bring a repository into step with
///   a declaration that states it.
///
/// `RepositoryName` is in neither list: the name IS this resource's physical id, so a changed
/// one is a different resource and never reaches [`update_repository`] at all.
const REPOSITORY_IN_PLACE_PROPERTIES: &[&str] = &[
    "EmptyOnDelete",
    "ImageScanningConfiguration",
    "ImageTagMutability",
    REPOSITORY_LIFECYCLE_POLICY,
    "RepositoryPolicyText",
    "Tags",
];

/// Apply a CHANGED `AWS::ECR::Repository` declaration to the repository the stack already
/// has, through the Amazon ECR call that writes each setting on a repository that exists —
/// its lifecycle policy with `PutLifecyclePolicy` ("creates or updates the lifecycle policy
/// for the specified repository") and `DeleteLifecyclePolicy`, its resource-based policy with
/// `SetRepositoryPolicy` and `DeleteRepositoryPolicy`, its tag mutability with
/// `PutImageTagMutability`, and its scanning configuration with
/// `PutImageScanningConfiguration`.
///
/// The repository therefore keeps its identity — the same name, ARN and URI — so a read of
/// the configuration after the deployment answers what the template now declares, exactly as
/// it answers what the template declared after the create ([`create_repository`]). Only the
/// properties this deployment states differently are sent, so a setting neither revision
/// changed reaches that registry not at all; a property the revision no longer declares is
/// put back to the registry's own documented default, or removed where the configuration is
/// one a repository may carry none of, rather than silently kept.
///
/// `Ok(None)` is a change no Amazon ECR call makes to a repository that exists — a property
/// outside [`REPOSITORY_IN_PLACE_PROPERTIES`] — which the deployment then reports as not
/// applied rather than as a success that dropped the caller's change.
pub(crate) async fn update_repository(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if changed
        .iter()
        .any(|property| !REPOSITORY_IN_PLACE_PROPERTIES.contains(&property.as_str()))
    {
        return Ok(None);
    }
    let name = &resource.physical_resource_id;
    let is_changed = |property: &str| changed.iter().any(|changed| changed == property);
    let properties = match declared {
        Value::Object(object) => object.clone(),
        _ => Map::new(),
    };

    // "LifecyclePolicy — creates or updates a lifecycle policy": the declared document
    // REPLACES the one the repository holds, and a deployment that stops declaring one takes
    // the policy off the repository, which is the state a repository that never carried one
    // is in. A policy that is already gone is in the state that call asks for.
    if is_changed(REPOSITORY_LIFECYCLE_POLICY) {
        match lifecycle_policy_request(&properties, name)? {
            Some(request) => {
                call_ecr(ctx, region, "PutLifecyclePolicy", &request).await?;
            }
            None => {
                let request = serde_json::json!({ "repositoryName": name });
                match call_ecr(ctx, region, "DeleteLifecyclePolicy", &request).await {
                    Ok(_) => {}
                    Err(refusal) if refusal.code == LIFECYCLE_POLICY_NOT_FOUND => {}
                    Err(refusal) => return Err(refusal),
                }
            }
        }
    }

    // The repository's resource-based policy: the declared document replaces the one it
    // holds ("applies a repository policy to the specified repository to control access
    // permissions"), and a deployment that stops declaring one deletes it off the repository.
    if is_changed("RepositoryPolicyText") {
        match repository_policy_text(&properties) {
            Some(policy) => {
                let request = serde_json::json!({
                    "repositoryName": name,
                    "policyText": policy,
                });
                call_ecr(ctx, region, "SetRepositoryPolicy", &request).await?;
            }
            None => {
                let request = serde_json::json!({ "repositoryName": name });
                match call_ecr(ctx, region, "DeleteRepositoryPolicy", &request).await {
                    Ok(_) => {}
                    Err(refusal) if refusal.code == REPOSITORY_POLICY_NOT_FOUND => {}
                    Err(refusal) => return Err(refusal),
                }
            }
        }
    }

    // "PutImageTagMutability — updates the image tag mutability settings for the specified
    // repository": the setting the declaration now states, or the registry's own default
    // where it states none ([`DEFAULT_IMAGE_TAG_MUTABILITY`]).
    if is_changed("ImageTagMutability") {
        let setting = properties
            .get("ImageTagMutability")
            .and_then(scalar_text)
            .unwrap_or_else(|| DEFAULT_IMAGE_TAG_MUTABILITY.to_string());
        let request = serde_json::json!({
            "repositoryName": name,
            "imageTagMutability": setting,
        });
        call_ecr(ctx, region, "PutImageTagMutability", &request).await?;
    }

    // "PutImageScanningConfiguration — updates the image scanning configuration for the
    // specified repository": the document the declaration now states, mapped member by
    // member as the create maps it ([`REPOSITORY_DOCUMENT_MEMBERS`]), or the registry's own
    // default where it states none ([`DEFAULT_SCAN_ON_PUSH`]).
    if is_changed("ImageScanningConfiguration") {
        let mut document = Map::new();
        document.insert(SCAN_ON_PUSH.to_string(), Value::Bool(DEFAULT_SCAN_ON_PUSH));
        if let Some(Value::Object(stated)) = properties.get("ImageScanningConfiguration") {
            if let Some(value) = stated.get("ScanOnPush") {
                document.insert(SCAN_ON_PUSH.to_string(), value.clone());
            }
        }
        let request = serde_json::json!({
            "repositoryName": name,
            "imageScanningConfiguration": Value::Object(document),
        });
        call_ecr(ctx, region, "PutImageScanningConfiguration", &request).await?;
    }

    // The repository is the one the stack created, under the same name: its ARN and its URI
    // are the ones the create already recorded, so this reports no attribute of its own.
    Ok(Some(Created::default()))
}

/// Bring the tags of the repository a resource record names to the ones this deployment gives
/// it, through Amazon ECR's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set [`create_repository`] sent on the create, whose `tags` member
/// applies to the repository that call creates and to no repository that already exists — so
/// a key outside it is one this deployment no longer gives the repository and is removed, and
/// one in it with another value is re-valued. Where the two sets already agree nothing is
/// called at all, which is what makes this safe to ask on every deployment.
///
/// Those two calls address a repository by its ARN, which is what the registry reports for
/// the repository this record names; a repository Amazon ECR no longer holds has no tags to
/// reconcile.
pub(crate) async fn retag_repository(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(held_repository) =
        describe_repository(ctx, region, &resource.physical_resource_id).await?
    else {
        return Ok(());
    };
    let Some(arn) = held_repository
        .get("repositoryArn")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };
    let desired = tags_of(declared, propagated);
    let listed = call_ecr(
        ctx,
        region,
        "ListTagsForResource",
        &serde_json::json!({ "resourceArn": &arn }),
    )
    .await?;
    // That registry answers a repository's tags as a `tags` list of `{Key, Value}` structures
    // — the member spelled as this service's model spells it, the structures as a
    // declaration's `Tags` property spells them — so the one reader of that list serves both
    // sides once the list is handed to it under the property name it reads
    // ([`declared_tags`]).
    let held = declared_tags(&serde_json::json!({ "Tags": listed.get("tags") }));
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_ecr(
            ctx,
            region,
            "TagResource",
            &serde_json::json!({
                "resourceArn": &arn,
                "tags": tags_as_list(&added, UPPER_CASED_TAG),
            }),
        )
        .await?;
    }
    if !removed.is_empty() {
        call_ecr(
            ctx,
            region,
            "UntagResource",
            &serde_json::json!({
                "resourceArn": &arn,
                "tagKeys": tag_keys_as_list(&removed),
            }),
        )
        .await?;
    }
    Ok(())
}

/// Delete the repository a stack created. A repository ECR no longer holds
/// (`RepositoryNotFoundException`) is already in the state the delete asks for; one that
/// still holds images is NOT deleted unless the declaration asked for that
/// (`EmptyOnDelete`: "if true, deleting the repository force deletes the contents of the
/// repository"), and ECR's own refusal is what the stack reports.
pub(crate) async fn delete_repository(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let force = resource
        .properties
        .get("EmptyOnDelete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request = serde_json::json!({
        "repositoryName": resource.physical_resource_id,
        "force": force,
    });
    match call_ecr(ctx, region, "DeleteRepository", &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REPOSITORY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The repository's configuration as Amazon ECR reports it, for drift detection. A
/// repository it no longer holds (`RepositoryNotFoundException`, and an answer carrying
/// no repository at all) was deleted.
pub(crate) async fn describe_repository(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ "repositoryNames": [physical_resource_id] });
    match call_ecr(ctx, region, "DescribeRepositories", &request).await {
        Ok(answer) => Ok(first_listed(&answer, "repositories")),
        Err(refusal) if refusal.code == REPOSITORY_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}
