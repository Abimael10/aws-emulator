// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Rekognition: the face COLLECTION an
//! `AWS::Rekognition::Collection` declares, the Custom Labels PROJECT an
//! `AWS::Rekognition::Project` declares, and the DATASET of such a project an
//! `AWS::Rekognition::Dataset` declares. One file of [`super`]'s per-type arms, dispatched to
//! by its `Provisioner`.
//!
//! ## What identifies each of them
//!
//! A COLLECTION is identified by the `CollectionId` the template states: `CreateCollection`
//! takes that id ("`@required`, `@length(1..255)`, `@pattern ^[a-zA-Z0-9_.\-]+$`",
//! `smithy_query(query="operation",operation="CreateCollection")`), every later call on a
//! collection takes the same id, and the type's `Ref` returns it — so the declared id is this
//! resource's physical id and CloudFormation generates no name for one
//! ([`super::super::template::naming_rules`] names the property). The collection's ARN is the
//! one that service reports for it, read off its own `DescribeCollection`.
//!
//! A PROJECT is identified by its `ProjectName` the same way, which is the convention the
//! naming rules already read (the type's last segment plus `Name`). Its ARN is NOT a name any
//! rule could compute: `ProjectArn` ends in a numeric segment Rekognition assigns
//! (`project\/[a-zA-Z0-9_.\-]{1,255}\/[0-9]+$`), so the ARN the create answered is what this
//! resource publishes, and it is the handle every later call on a project takes.
//!
//! A DATASET has no name of its own at all: `CreateDataset` takes the project it belongs to
//! and which half of that project's data it is, and answers the `DatasetArn` that service
//! minted (`project\/…\/dataset\/(train|test)\/[0-9]+$`). That ARN is the resource's physical
//! id and the one handle every later call on a dataset takes
//! ([`super::super::template::identity_is_service_assigned`] names the type).
//!
//! ## What each lifecycle is held open on
//!
//! A collection is created and deleted synchronously: `CreateCollection` answers the ARN of a
//! collection that already takes faces, and `DeleteCollection` removes it.
//!
//! A dataset is NOT: "creating a dataset takes a while to complete. Use `DescribeDataset` to
//! check the current status. The dataset created successfully if the value of `Status` is
//! `CREATE_COMPLETE`" (`smithy_query(query="doc",shape="CreateDataset")`), so the resource's
//! creation is held open until that service reports it `CREATE_COMPLETE`
//! ([`rekognition_dataset_readiness`]). Its teardown is asynchronous for the same reason —
//! "deleting a dataset might take while" — so the deletion finishes when that service no
//! longer holds the dataset ([`rekognition_dataset_removal`]).
//!
//! Deleting a PROJECT is asynchronous too: "the project is deleted when the project no longer
//! appears in the `DescribeProjects` response" (`DeleteProject`), which is what
//! [`rekognition_project_removal`] waits for.
//!
//! There is no UPDATE arm for any of the three: Rekognition publishes no call that changes a
//! collection, a project or a dataset that exists, so every property of each type is
//! create-only and a declaration restating one replaces the resource instead.

use super::*;

/// Amazon Rekognition's signing name and `X-Amz-Target` prefix, from its own model
/// (`sigv4Name: rekognition`, service shape `RekognitionService`, protocol `awsJson1_1`) — the
/// wire these calls are written on, as that service's own suites drive it.
const REKOGNITION_SIGNING_NAME: &str = "rekognition";
const REKOGNITION_TARGET_PREFIX: &str = "RekognitionService";

/// The three resource types these arms provision. Visible to the rest of this service's state
/// because the catalogue of declarable types, the naming rules and the return-value catalogue
/// each key an entry by one of them.
pub(in crate::state) const REKOGNITION_COLLECTION: &str = "AWS::Rekognition::Collection";
pub(in crate::state) const REKOGNITION_PROJECT: &str = "AWS::Rekognition::Project";
pub(in crate::state) const REKOGNITION_DATASET: &str = "AWS::Rekognition::Dataset";

/// The property an `AWS::Rekognition::Collection` NAMES itself with, which is also the member
/// `CreateCollection` and every later call on a collection take it as — and what that type's
/// `Ref` returns. The type's last segment is `Collection`, so neither `CollectionName` nor
/// `Name` finds it, which is why the naming rules carry an entry for this type.
pub(in crate::state) const COLLECTION_ID_PROPERTY: &str = "CollectionId";

/// The operations these arms call, as Rekognition's own model names them.
const CREATE_COLLECTION: &str = "CreateCollection";
const DESCRIBE_COLLECTION: &str = "DescribeCollection";
const DELETE_COLLECTION: &str = "DeleteCollection";
const CREATE_PROJECT: &str = "CreateProject";
const DESCRIBE_PROJECTS: &str = "DescribeProjects";
const DELETE_PROJECT: &str = "DeleteProject";
const CREATE_DATASET: &str = "CreateDataset";
const DESCRIBE_DATASET: &str = "DescribeDataset";
const DELETE_DATASET: &str = "DeleteDataset";

/// The error code Rekognition answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled refusal of a call whose "resource specified in
/// the request cannot be found"). Judged on the CODE and never on a message, exactly as every
/// other arm here judges one.
const REKOGNITION_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member `DescribeCollection` reports a collection's ARN in — spelled with the
/// upper-case `ARN`, which is how that service's own model spells that member.
const COLLECTION_ARN_READ_MEMBER: &str = "CollectionARN";

/// The members `CreateProject` and the calls on a project take, and the member a project's own
/// description reports its name-derived identity in.
const PROJECT_NAME_MEMBER: &str = "ProjectName";
const PROJECT_NAMES_MEMBER: &str = "ProjectNames";
const PROJECT_ARN_MEMBER: &str = "ProjectArn";
const PROJECT_DESCRIPTIONS_MEMBER: &str = "ProjectDescriptions";

/// The members `CreateDataset` takes and the member a dataset's own description is reported in.
const DATASET_TYPE_MEMBER: &str = "DatasetType";
const DATASET_SOURCE_MEMBER: &str = "DatasetSource";
const DATASET_ARN_MEMBER: &str = "DatasetArn";
const DATASET_DESCRIPTION_MEMBER: &str = "DatasetDescription";

/// The member a dataset's description reports its status in, and the two statuses this provider
/// reads it for: the one a dataset still being created is in and the one its creation finishes
/// in (modelled `DatasetStatus`, `smithy_query(query="shape",shape="DatasetStatus")`).
const STATUS_MEMBER: &str = "Status";
const DATASET_STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const DATASET_STATUS_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The member every one of these creates takes its tag set as: a MAP of tag key to value
/// (`TagMap`), which is the shape `CreateCollection` and `CreateDataset` each publish.
const TAGS_MEMBER: &str = "Tags";

/// The property the collection and the dataset types publish their tag set as.
const TAGS_PROPERTY: &str = "Tags";

/// Every property an `AWS::Rekognition::Collection` publishes, which is what a declaration may
/// state: a property no arm here maps onto `CreateCollection`'s request is refused rather than
/// dropped silently ([`refuse_unmapped_properties`]).
const COLLECTION_PROPERTIES: &[&str] = &[COLLECTION_ID_PROPERTY, TAGS_PROPERTY];

/// Every property an `AWS::Rekognition::Project` publishes. That type publishes no `Tags`
/// property at all, so no tag is propagated to a project.
const PROJECT_PROPERTIES: &[&str] = &[PROJECT_NAME_MEMBER];

/// Every property an `AWS::Rekognition::Dataset` publishes: the project it belongs to, which
/// half of that project's data it is, the source its entries come from, and its tags — the
/// members `CreateDataset` takes, spelled identically on both sides.
const DATASET_PROPERTIES: &[&str] = &[
    PROJECT_ARN_MEMBER,
    DATASET_TYPE_MEMBER,
    DATASET_SOURCE_MEMBER,
    TAGS_PROPERTY,
];

/// The `AWS::Rekognition::Dataset` properties that are members of `CreateDataset` under the
/// SAME name, as (property, member) — every one of them but its `Tags`, which that call takes
/// as a map ([`create_rekognition_dataset`]).
const DATASET_MEMBERS: &[(&str, &str)] = &[
    (PROJECT_ARN_MEMBER, PROJECT_ARN_MEMBER),
    (DATASET_TYPE_MEMBER, DATASET_TYPE_MEMBER),
    (DATASET_SOURCE_MEMBER, DATASET_SOURCE_MEMBER),
];

/// Create the collection an `AWS::Rekognition::Collection` declares, in Rekognition itself —
/// so a client that deployed the stack reaches the collection through that service's own
/// `DescribeCollection`, `ListCollections` and `IndexFaces` under the id the stack published,
/// rather than holding an id nothing created.
///
/// The collection is created under the `CollectionId` the declaration states, which is this
/// resource's physical id, carrying the tags the declaration asks for beside the ones
/// CloudFormation gives the resource ([`tags_of`]) as the `TagMap` that create takes.
/// Rekognition's own validation is what judges the id — one it refuses, and one it already
/// holds a collection under (`ResourceAlreadyExistsException`), leave the stack rolling back
/// rather than publishing an id for a collection nothing holds.
///
/// What the resource publishes is the ARN that service reports for the collection, read off its
/// own `DescribeCollection` ([`described_collection`]) rather than recomputed here.
pub(super) async fn create_rekognition_collection(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, COLLECTION_PROPERTIES)?;
    let collection_id = resource.physical_resource_id.clone();
    let mut request = Map::new();
    request.insert(
        COLLECTION_ID_PROPERTY.to_string(),
        Value::String(collection_id.clone()),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&tags)));
    }
    call_rekognition(ctx, region, CREATE_COLLECTION, &Value::Object(request)).await?;
    let described = described_collection(ctx, region, &collection_id).await?;
    let Some(arn) = described
        .as_ref()
        .and_then(|described| reported_text(described, COLLECTION_ARN_READ_MEMBER))
    else {
        // The create went through but that service reports no collection under the id, or
        // reports one without the ARN its read models. There would be nothing for the stack to
        // publish, so this is a failed create rather than a resource with an ARN this module
        // invented.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "Amazon Rekognition reports no {COLLECTION_ARN_READ_MEMBER} for the collection \
                 {collection_id}"
            ),
        });
    };
    Ok(Created::attributes(arn_attributes(
        REKOGNITION_COLLECTION,
        &arn,
    )))
}

/// Delete the collection a stack created, by the `CollectionId` it was created under — which
/// is this resource's physical id and the one handle `DeleteCollection` takes.
///
/// A collection Rekognition no longer holds (`ResourceNotFoundException`) is already in the
/// state the delete asks for.
pub(super) async fn delete_rekognition_collection(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    collection_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        COLLECTION_ID_PROPERTY.to_string(),
        Value::String(collection_id.to_string()),
    );
    match call_rekognition(ctx, region, DELETE_COLLECTION, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The collection's configuration as Rekognition reports it, for drift detection.
///
/// `DescribeCollection` carries no member of the collection's own declaration — it reports the
/// ARN, the face model version and the counts — so what a read establishes is the one declared
/// property that identifies it: the `CollectionId` the collection really answers to. A
/// collection that read no longer holds was deleted, which for a READ is what
/// [`Provisioner::actual_configuration`] reports as `DELETED`.
pub(super) async fn describe_rekognition_collection(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    collection_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(_) = described_collection(ctx, region, collection_id).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    reported.insert(
        COLLECTION_ID_PROPERTY.to_string(),
        Value::String(collection_id.to_string()),
    );
    Ok(Some(Value::Object(reported)))
}

/// What Rekognition reports for ONE collection, or `None` for one it does not hold.
async fn described_collection(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    collection_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        COLLECTION_ID_PROPERTY.to_string(),
        Value::String(collection_id.to_string()),
    );
    match call_rekognition(ctx, region, DESCRIBE_COLLECTION, &Value::Object(request)).await {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Create the Custom Labels project an `AWS::Rekognition::Project` declares, in Rekognition
/// itself — so a client that deployed the stack reaches the project through that service's own
/// `DescribeProjects` under the name the stack published, and can create its datasets and its
/// model versions in it.
///
/// The project is created under the `ProjectName` the declaration states, which is this
/// resource's physical id. What the resource publishes is the `ProjectArn` that service
/// MINTED: it ends in a numeric segment Rekognition assigns, so no naming rule could compute
/// it, and it is the handle every later call on a project takes.
pub(super) async fn create_rekognition_project(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, PROJECT_PROPERTIES)?;
    let mut request = Map::new();
    request.insert(
        PROJECT_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let answered = call_rekognition(ctx, region, CREATE_PROJECT, &Value::Object(request)).await?;
    let Some(arn) = reported_text(&answered, PROJECT_ARN_MEMBER) else {
        // Nothing addresses a project by an identity nothing minted, so this is reported as a
        // failure rather than recorded as a resource with an ARN this module invented.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!("{CREATE_PROJECT} answered no {PROJECT_ARN_MEMBER}"),
        });
    };
    Ok(Created::attributes(arn_attributes(
        REKOGNITION_PROJECT,
        &arn,
    )))
}

/// Delete the project a stack created, by the `ProjectArn` Rekognition minted for it — the one
/// handle `DeleteProject` takes, which this resource publishes ([`published_arn`]).
///
/// A project that service no longer holds (`ResourceNotFoundException`) is already in the state
/// the delete asks for, and so is a resource that never got an ARN to address.
pub(super) async fn delete_rekognition_project(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let Some(project_arn) = published_arn(resource) else {
        return Ok(());
    };
    let mut request = Map::new();
    request.insert(PROJECT_ARN_MEMBER.to_string(), Value::String(project_arn));
    match call_rekognition(ctx, region, DELETE_PROJECT, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Rekognition has finished removing the project a stack deleted.
///
/// `DeleteProject` "is an asynchronous operation": it answers the project `DELETING` and "the
/// project is deleted when the project no longer appears in the `DescribeProjects` response",
/// so the deletion is finished exactly when that service reports no such project.
pub(super) async fn rekognition_project_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match described_project(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(described) => Ok(Removal::Pending(
            reported_text(&described, STATUS_MEMBER).unwrap_or_default(),
        )),
    }
}

/// The project's configuration as Rekognition reports it, for drift detection: the one declared
/// property a read of a project establishes, which is the `ProjectName` it answers to. A
/// project that read no longer holds was deleted, which is what `None` reports.
pub(super) async fn describe_rekognition_project(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    project_name: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(_) = described_project(ctx, region, project_name).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    reported.insert(
        PROJECT_NAME_MEMBER.to_string(),
        Value::String(project_name.to_string()),
    );
    Ok(Some(Value::Object(reported)))
}

/// What Rekognition reports for ONE project, by the name it was created under, or `None` for a
/// name it holds no project under.
///
/// `DescribeProjects` narrowed by its own `ProjectNames` filter is that service's read of a
/// single project — "if you don't specify a value, the response includes descriptions for all
/// the projects in your AWS account" — so the narrowed list is a page holding the project or
/// no page at all.
async fn described_project(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    project_name: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        PROJECT_NAMES_MEMBER.to_string(),
        Value::Array(vec![Value::String(project_name.to_string())]),
    );
    let answered = match call_rekognition(ctx, region, DESCRIBE_PROJECTS, &Value::Object(request))
        .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(Value::Array(described)) = answered.get(PROJECT_DESCRIPTIONS_MEMBER) else {
        return Ok(None);
    };
    Ok(described.first().cloned())
}

/// Create the dataset an `AWS::Rekognition::Dataset` declares, in Rekognition itself — so a
/// client that deployed the stack reaches the dataset through that service's own
/// `DescribeDataset` under the ARN the stack published, and finds it among the owning project's
/// `DescribeProjects` datasets.
///
/// The declaration IS the request ([`DATASET_MEMBERS`]): the project the dataset belongs to,
/// which half of that project's data it is, and the source its entries come from are
/// `CreateDataset`'s own members under the same names, with the tags CloudFormation gives the
/// resource beside the declaration's own sent as the `TagMap` that create takes. Rekognition's
/// own validation is what judges them — a `ProjectArn` it holds no project under, a second
/// dataset of the same type, and a source it cannot read are its refusals, which fail the
/// resource and roll the stack back rather than publishing an ARN for a dataset nothing holds.
///
/// The identity is Rekognition's: the `DatasetArn` it minted is the one handle every dataset
/// API takes and what this type's `Ref` returns, so it replaces the stand-in the naming rules
/// computed.
pub(super) async fn create_rekognition_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, DATASET_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, DATASET_MEMBERS, &mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&tags)));
    }
    let answered = call_rekognition(ctx, region, CREATE_DATASET, &Value::Object(request)).await?;
    let Some(arn) = reported_text(&answered, DATASET_ARN_MEMBER) else {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!("{CREATE_DATASET} answered no {DATASET_ARN_MEMBER}"),
        });
    };
    let mut created = Created::attributes(arn_attributes(REKOGNITION_DATASET, &arn));
    created.physical_resource_id = Some(arn);
    Ok(created)
}

/// Whether Rekognition has finished creating the dataset a stack declared.
///
/// A created dataset is [`DATASET_STATUS_CREATE_IN_PROGRESS`] — "creating a dataset takes a
/// while to complete. Use `DescribeDataset` to check the current status. The dataset created
/// successfully if the value of `Status` is `CREATE_COMPLETE`" — so that status is the one the
/// resource's creation is finished in, and every other status is one no wait brings a NEW
/// dataset into: one whose creation failed, one being torn down out of this stack's reach. A
/// dataset that service no longer holds is in the same position, nothing being left to create
/// it.
pub(super) async fn rekognition_dataset_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    dataset_arn: &str,
) -> Result<Readiness, Refusal> {
    let Some(described) = described_dataset(ctx, region, dataset_arn).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon Rekognition holds no dataset {dataset_arn}"
        )));
    };
    let status = reported_text(&described, STATUS_MEMBER).unwrap_or_default();
    if status == DATASET_STATUS_CREATE_COMPLETE {
        return Ok(Readiness::Ready);
    }
    if status == DATASET_STATUS_CREATE_IN_PROGRESS {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon Rekognition reports the dataset {dataset_arn} with status {status}"
    )))
}

/// Whether Rekognition has finished removing the dataset a stack deleted.
///
/// `DeleteDataset` does not take the dataset away while the caller waits: "deleting a dataset
/// might take while. Use `DescribeDataset` to check the current status … If you try to access
/// the dataset after it is deleted, you get a `ResourceNotFoundException` exception" — so the
/// deletion is finished exactly when that service no longer holds the dataset.
pub(super) async fn rekognition_dataset_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    dataset_arn: &str,
) -> Result<Removal, Refusal> {
    match described_dataset(ctx, region, dataset_arn).await? {
        None => Ok(Removal::Gone),
        Some(described) => Ok(Removal::Pending(
            reported_text(&described, STATUS_MEMBER).unwrap_or_default(),
        )),
    }
}

/// Delete the dataset a stack created, by the `DatasetArn` Rekognition minted for it — the one
/// handle `DeleteDataset` takes, which is this resource's physical id.
///
/// A dataset that service no longer holds (`ResourceNotFoundException`) is already in the state
/// the delete asks for.
pub(super) async fn delete_rekognition_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    dataset_arn: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        DATASET_ARN_MEMBER.to_string(),
        Value::String(dataset_arn.to_string()),
    );
    match call_rekognition(ctx, region, DELETE_DATASET, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The dataset's configuration as Rekognition reports it, for drift detection.
///
/// `DescribeDataset` carries no member of the declaration itself — it reports the two instants,
/// the status and the statistics — so what a read establishes about the declaration is which
/// HALF of its project's data the dataset is, which that service recorded in the ARN it minted
/// (`…/dataset/(train|test)/<version>`) and reports back under this type's own `DatasetType`
/// property. A dataset that read no longer holds was deleted, which is what `None` reports.
pub(super) async fn describe_rekognition_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    dataset_arn: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(_) = described_dataset(ctx, region, dataset_arn).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    if let Some(dataset_type) = dataset_type_of(dataset_arn) {
        reported.insert(
            DATASET_TYPE_MEMBER.to_string(),
            Value::String(dataset_type),
        );
    }
    Ok(Some(Value::Object(reported)))
}

/// What Rekognition reports for ONE dataset, by the ARN it minted, or `None` for one it does
/// not hold.
async fn described_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    dataset_arn: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        DATASET_ARN_MEMBER.to_string(),
        Value::String(dataset_arn.to_string()),
    );
    match call_rekognition(ctx, region, DESCRIBE_DATASET, &Value::Object(request)).await {
        Ok(answered) => Ok(answered.get(DATASET_DESCRIPTION_MEMBER).cloned()),
        Err(refusal) if refusal.code == REKOGNITION_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The `DatasetType` a dataset ARN records, as the type's own property spells it: the
/// `(train|test)` segment of `project/<name>/dataset/(train|test)/<version>`, upper-cased, or
/// `None` for an identifier that is not such an ARN.
fn dataset_type_of(dataset_arn: &str) -> Option<String> {
    let (head, _) = dataset_arn.rsplit_once('/')?;
    let (_, segment) = head.rsplit_once('/')?;
    if segment.is_empty() {
        return None;
    }
    Some(segment.to_ascii_uppercase())
}

/// The ARN a resource of one of these types PUBLISHES, or `None` for one that never got one.
///
/// The attributes a provisioned resource carries are where the owning service's own answer
/// landed ([`template::arn_attributes`] is what put it under every spelling of the name), so a
/// call that takes a resource's ARN reads it there rather than recomputing it. A value that is
/// not an ARN is no handle at all and answers `None`.
fn published_arn(resource: &StoredResource) -> Option<String> {
    resource
        .attributes
        .get("Arn")
        .filter(|value| value.starts_with("arn:"))
        .cloned()
}

/// The ARN attributes a resource of `resource_type` publishes for `arn`, as the map a
/// [`Created`] carries: every spelling of the name a type's published return values use
/// ([`template::arn_attributes`]), so an `Fn::GetAtt` over the type's own `Arn` and over its
/// `<Type>Arn` answer the one ARN the owning service reported.
fn arn_attributes(resource_type: &str, arn: &str) -> BTreeMap<String, String> {
    template::arn_attributes(resource_type, arn)
        .into_iter()
        .collect()
}

/// Call one Amazon Rekognition operation in process, on the awsJson wire that service
/// publishes.
async fn call_rekognition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        REKOGNITION_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{REKOGNITION_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}
