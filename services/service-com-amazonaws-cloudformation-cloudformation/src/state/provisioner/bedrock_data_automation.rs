// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Bedrock Data Automation's BUILD-TIME control
//! plane: the blueprint, the data automation project and the data automation library a
//! template declares. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner::BedrockDataAutomation` variant, which carries which of the three the
//! declaration is ([`Resource`]) — beside the flow and knowledge-base arms of the Bedrock
//! Agent build-time plane ([`super::bedrock_flows`], [`super::bedrock_knowledge_bases`]) and
//! the guardrail arms of Amazon Bedrock's own control plane ([`super::bedrock_guardrails`]).
//!
//! ## Why each resource is really created
//!
//! A blueprint is the extraction schema an inference run is asked for by ARN, a project is
//! what `InvokeDataAutomationAsync` names as its `dataAutomationProjectArn`, and a library is
//! what a project's `dataAutomationLibraryConfiguration` points at. Each one's handle is the
//! ARN the owning service minted: `GetBlueprint`, `UpdateBlueprint`, `CopyBlueprintStage`,
//! `CreateBlueprintVersion`, `GetDataAutomationProject`, `UpdateDataAutomationProject`,
//! `GetDataAutomationLibrary` and every delete address one by it. A stack that reported one
//! created while nothing created it publishes an ARN that resolves to nothing, so the first
//! call a client makes on the deployed stack fails. Creating each resource where it belongs is
//! what makes the identity the stack publishes one the owning service answers for.
//!
//! ## The identifier is the service's
//!
//! None of the three creates takes an identifier for the resource: each takes the NAME the
//! declaration states and answers the ARN it minted, whose resource segment is 12 to 36
//! characters of `[a-zA-Z0-9-]` that service drew (`BlueprintArn`,
//! `DataAutomationProjectArn` and `DataAutomationLibraryArn` each fix that form). No name
//! CloudFormation decides is such an ARN, so the identity in force is the one that service
//! assigned: the ARN IS each resource's physical id — the type's primary identifier, which
//! its `Ref` returns — and the stand-in the naming rules computed is only what a reference was
//! resolved against until the create answered.
//!
//! ## What the declaration is
//!
//! Each property is the create's member of the same name with a lower-cased initial
//! (`ProjectDescription` is `projectDescription`, `StandardOutputConfiguration.Document` is
//! `standardOutputConfiguration.document`), so a declared configuration document IS the
//! request member once its names are re-cased ([`lower_camel_members`]). Two properties are
//! not that relation and are mapped on their own:
//!
//! * the ENCRYPTION pair. Each type declares the key and the context flat (`KmsKeyId`,
//!   `KmsEncryptionContext`) while every call takes them inside one
//!   `encryptionConfiguration` member — and the context is a map of the CALLER's own keys, so
//!   re-casing those would encrypt under a context the declaration did not describe.
//! * a blueprint's `Schema`, which `BlueprintSchema` declares as a STRING of up to 100000
//!   characters: a declaration that writes the schema as a JSON document reaches that member
//!   as the document's own text rather than as a structure that member cannot carry, through
//!   the one JSON-to-text rule this catalogue has ([`document_text`]).
//!
//! The declared `Tags` reach each create as the `TagList` of `{key, value}` structures its own
//! `tags` member takes, so they travel beside the ones CloudFormation gives the resource
//! ([`tags_of`]).
//!
//! No `clientToken` is sent. That member is an `@idempotencyToken`, so a token derived from
//! the resource would make the create of a REPLACEMENT resource replay the one the stack
//! already had instead of minting a new one.

use super::*;

/// Which of the three Amazon Bedrock Data Automation resources a declaration is, carried by
/// the one `Provisioner` variant these arms are reached through so the three share one set of
/// arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resource {
    /// `AWS::Bedrock::Blueprint`.
    Blueprint,
    /// `AWS::Bedrock::DataAutomationProject`.
    Project,
    /// `AWS::Bedrock::DataAutomationLibrary`.
    Library,
}

/// The resource-type names of the three, which the registry of recognised types, the
/// return-value catalogue and the primary-identifier table each key an entry by
/// ([`super::super::template`] re-exports them), re-exported here so an arm names the type it
/// serves by the same const.
pub(in crate::state) use super::super::template::{
    BEDROCK_BLUEPRINT, BEDROCK_DATA_AUTOMATION_LIBRARY, BEDROCK_DATA_AUTOMATION_PROJECT,
    BLUEPRINT_ARN_ATTRIBUTE, LIBRARY_ARN_ATTRIBUTE, PROJECT_ARN_ATTRIBUTE,
};

/// The collection each resource is created under and addressed by its ARN beneath, from each
/// operation's own `@http` binding (`PUT /blueprints/`,
/// `DELETE /blueprints/{blueprintArn}/`, and so on — the trailing slash included, as those
/// bindings spell it).
const BLUEPRINTS_URI: &str = "/blueprints/";
const PROJECTS_URI: &str = "/data-automation-projects/";
const LIBRARIES_URI: &str = "/data-automation-libraries/";

/// The operations these arms call, as Amazon Bedrock Data Automation's own model names them.
/// The name travels with each call ([`call_rest_json_named`]) because [`BEDROCK_SIGNING_NAME`]
/// is a signing name several services share, so the operation is also what says WHICH of them
/// was called (`super::provisioning::SDK_SHARED_NAME_SERVICES`).
const CREATE_BLUEPRINT: &str = "CreateBlueprint";
const GET_BLUEPRINT: &str = "GetBlueprint";
const UPDATE_BLUEPRINT: &str = "UpdateBlueprint";
const DELETE_BLUEPRINT: &str = "DeleteBlueprint";
const CREATE_PROJECT: &str = "CreateDataAutomationProject";
const GET_PROJECT: &str = "GetDataAutomationProject";
const UPDATE_PROJECT: &str = "UpdateDataAutomationProject";
const DELETE_PROJECT: &str = "DeleteDataAutomationProject";
const CREATE_LIBRARY: &str = "CreateDataAutomationLibrary";
const GET_LIBRARY: &str = "GetDataAutomationLibrary";
const UPDATE_LIBRARY: &str = "UpdateDataAutomationLibrary";
const DELETE_LIBRARY: &str = "DeleteDataAutomationLibrary";

/// The error code Amazon Bedrock Data Automation answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of every one of its reads and deletes).
const BDA_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member a READ answers the resource inside, where it answers one: `GetBlueprint` reports
/// a `blueprint`, `GetDataAutomationProject` a `project` and `GetDataAutomationLibrary` a
/// `library`. A create answers the project's and the library's own members flat and the
/// blueprint inside that same wrapper, which is why [`answered_detail`] reads the wrapper where
/// an answer carries one and the answer itself otherwise.
const BLUEPRINT_MEMBER: &str = "blueprint";
const PROJECT_MEMBER: &str = "project";
const LIBRARY_MEMBER: &str = "library";

/// The member each create takes the resource's NAME in, which is the one member
/// CloudFormation decides.
const BLUEPRINT_NAME_MEMBER: &str = "blueprintName";
const PROJECT_NAME_MEMBER: &str = "projectName";
const LIBRARY_NAME_MEMBER: &str = "libraryName";

/// The property each type states that name in.
const BLUEPRINT_NAME_PROPERTY: &str = "BlueprintName";
const PROJECT_NAME_PROPERTY: &str = "ProjectName";
const LIBRARY_NAME_PROPERTY: &str = "LibraryName";

/// The member the declared `Tags` list reaches each create as — its own `tags`, which no
/// update of the three takes.
const TAGS_MEMBER: &str = "tags";

/// The properties that state the key a resource's data is encrypted under, and the member
/// every call takes the pair inside: `EncryptionConfiguration$kmsKeyId` is required once that
/// member is present, so a declaration that states only a context sends none.
const KMS_KEY_ID_PROPERTY: &str = "KmsKeyId";
const KMS_ENCRYPTION_CONTEXT_PROPERTY: &str = "KmsEncryptionContext";
const ENCRYPTION_CONFIGURATION_MEMBER: &str = "encryptionConfiguration";
const KMS_KEY_ID_MEMBER: &str = "kmsKeyId";
const KMS_ENCRYPTION_CONTEXT_MEMBER: &str = "kmsEncryptionContext";

/// The property a declaration may already state the pair above as, which IS that member's own
/// shape with its names upper-cased: such a declaration is re-cased rather than rebuilt.
const ENCRYPTION_CONFIGURATION_PROPERTY: &str = "EncryptionConfiguration";

/// A blueprint's SCHEMA: the property, and the member `CreateBlueprint` and `UpdateBlueprint`
/// each take it in as a string.
const SCHEMA_PROPERTY: &str = "Schema";
const SCHEMA_MEMBER: &str = "schema";

/// The scalar properties of a BLUEPRINT that are the create's member of the same name with a
/// lower-cased initial, as (property, member).
const BLUEPRINT_MEMBERS: &[(&str, &str)] =
    &[(TYPE_PROPERTY, TYPE_MEMBER), ("BlueprintStage", "blueprintStage")];

/// The properties of a PROJECT that are the create's member of the same name with a
/// lower-cased initial, as (property, member): the scalars the declaration states about the
/// project itself.
const PROJECT_MEMBERS: &[(&str, &str)] = &[
    ("ProjectDescription", "projectDescription"),
    ("ProjectStage", "projectStage"),
    (PROJECT_TYPE_PROPERTY, PROJECT_TYPE_MEMBER),
];

/// The CONFIGURATION documents of a project, as (property, member): modelled structures all
/// the way down, so each one's declared document is the member once its names are re-cased
/// ([`lower_camel_members`]).
const PROJECT_CONFIGURATIONS: &[(&str, &str)] = &[
    (
        "StandardOutputConfiguration",
        "standardOutputConfiguration",
    ),
    ("CustomOutputConfiguration", "customOutputConfiguration"),
    ("OverrideConfiguration", "overrideConfiguration"),
    (
        "DataAutomationLibraryConfiguration",
        "dataAutomationLibraryConfiguration",
    ),
];

/// The properties of a LIBRARY that are the create's member of the same name with a
/// lower-cased initial, as (property, member).
const LIBRARY_MEMBERS: &[(&str, &str)] = &[("LibraryDescription", "libraryDescription")];

/// The member a library's update carries, which is the ONE member that call takes beside the
/// library's own identity.
const LIBRARY_DESCRIPTION_MEMBER: &str = "libraryDescription";

/// The member a blueprint's `Type` reaches its create as, which its UPDATE does not take.
const TYPE_MEMBER: &str = "type";

/// The property a blueprint declares that type in.
const TYPE_PROPERTY: &str = "Type";

/// A project's own TYPE: the property, and the member its create takes it in, which
/// `UpdateDataAutomationProject` does not take either.
const PROJECT_TYPE_PROPERTY: &str = "ProjectType";
const PROJECT_TYPE_MEMBER: &str = "projectType";

/// The property every one of the three declares its tags in, which no update of them carries.
const TAGS_PROPERTY: &str = "Tags";

/// The attributes a BLUEPRINT publishes, as (attribute, member): the ARN that service minted
/// and the two instants its clock keeps, read off the answer rather than recomputed here.
const BLUEPRINT_ATTRIBUTES: &[(&str, &str)] = &[
    (BLUEPRINT_ARN_ATTRIBUTE, "blueprintArn"),
    ("CreationTime", "creationTime"),
    ("LastModifiedTime", "lastModifiedTime"),
];

/// The attributes a PROJECT publishes, as (attribute, member). Its create answers the ARN, the
/// stage and the status alone; the two instants reach the resource off the read taken once that
/// service reports the project built ([`ready_attributes`]).
const PROJECT_ATTRIBUTES: &[(&str, &str)] = &[
    (PROJECT_ARN_ATTRIBUTE, "projectArn"),
    ("ProjectStage", "projectStage"),
    ("Status", "status"),
    ("CreationTime", "creationTime"),
    ("LastModifiedTime", "lastModifiedTime"),
];

/// The attributes a LIBRARY publishes, as (attribute, member): the ARN that service minted, the
/// status it reports the library in and the instant it recorded for it.
const LIBRARY_ATTRIBUTES: &[(&str, &str)] = &[
    (LIBRARY_ARN_ATTRIBUTE, "libraryArn"),
    ("Status", "status"),
    ("CreationTime", "creationTime"),
];

/// The `DataAutomationProjectStatus` a built project rests in ("COMPLETED"), the one it is
/// still being built in ("IN_PROGRESS") and the one no wait brings it out of ("FAILED") —
/// `smithy_query(query="shape",shape="DataAutomationProjectStatus")` lists exactly those three.
const PROJECT_COMPLETED: &str = "COMPLETED";
const PROJECT_IN_PROGRESS: &str = "IN_PROGRESS";

/// The `DataAutomationLibraryStatus` of a library being removed ("DELETING", beside "ACTIVE" —
/// `smithy_query(query="shape",shape="DataAutomationLibraryStatus")`), which is the status an
/// accepted delete leaves one in until that service has released it.
const LIBRARY_DELETING: &str = "DELETING";

/// How a name is derived for a resource whose declaration states none, and how many digits it
/// carries.
///
/// Each type's name is a required property, so this is the fallback for a declaration that
/// states none anyway: the name is derived from the resource's own computed identity (a
/// function of the stack and the resource's logical id), which keeps the same declaration of
/// the same stack asking for the same resource. The form satisfies all three `@pattern`s
/// (`^[a-zA-Z0-9-_]+$`) and stays well inside their `@length(1..128)`.
const DERIVED_NAME_PREFIX: &str = "cfn-";
const DERIVED_NAME_DIGITS: usize = 16;

/// Which of the three a resource type is, or `None` for a type that is none of them — which is
/// what leaves every other type to the answer [`super::provisioner_for`] already gave it.
pub(super) fn bedrock_data_automation_provisioner_for(resource_type: &str) -> Option<Resource> {
    match resource_type {
        BEDROCK_BLUEPRINT => Some(Resource::Blueprint),
        BEDROCK_DATA_AUTOMATION_PROJECT => Some(Resource::Project),
        BEDROCK_DATA_AUTOMATION_LIBRARY => Some(Resource::Library),
        _ => None,
    }
}

/// Create the Amazon Bedrock Data Automation resource a declaration of one of the three types
/// asks for, so a client that read the ARN out of the stack's outputs finds it in that
/// service's own reads — and so every hop off it (reading it, versioning a blueprint, invoking
/// a project, ingesting into a library) addresses a resource that exists.
///
/// The identity is that service's: the ARN it minted becomes this resource's physical id — the
/// primary identifier of the type — and the values it answered are the resource's attributes.
pub(super) async fn create_bedrock_data_automation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = declared_members(resource_kind, &resource.properties)?;
    request.insert(
        name_member(resource_kind).to_string(),
        Value::String(configured_name(resource_kind, resource)),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, LOWER_CASED_TAG),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        create_operation(resource_kind),
        "PUT",
        collection_uri(resource_kind),
        Some(&Value::Object(request)),
    )
    .await?;
    let (arn, attributes) = identified_resource(resource_kind, &answered)?;
    Ok(Created {
        // That service assigned the resource's identity, so the ARN it answered is what `Ref`
        // resolves to and what every call on the resource can be addressed by.
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Whether the resource this stack created has reached the state Amazon Bedrock Data Automation
/// reports it ready in.
///
/// Only a PROJECT has one to wait for: `CreateDataAutomationProject` answers the project
/// `IN_PROGRESS` — that service is still building it, and an inference run against it in that
/// state has nothing to run — so the resource is held open until the project reports
/// `COMPLETED`. Every other status is one no wait brings it out of (a build that `FAILED`), and
/// a project that service no longer holds is in the same position, nothing being left to create
/// it. A blueprint and a library are complete as their creates answer (a created library is
/// `ACTIVE`), so each is ready without asking anything.
pub(super) async fn bedrock_data_automation_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    if resource_kind != Resource::Project {
        return Ok(Readiness::Ready);
    }
    let Some(project) = read_resource(ctx, region, resource_kind, physical_resource_id).await?
    else {
        return Ok(Readiness::Failed(format!(
            "Amazon Bedrock Data Automation holds no data automation project \
             {physical_resource_id}"
        )));
    };
    let status = reported_text(&project, "status").unwrap_or_default();
    if status == PROJECT_COMPLETED {
        return Ok(Readiness::Ready);
    }
    if status == PROJECT_IN_PROGRESS {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon Bedrock Data Automation reports data automation project \
         {physical_resource_id} as {status}"
    )))
}

/// The attributes the owning service publishes for a resource it now reports up: its own read of
/// the resource, whose `status` and instants are not the ones a create answered.
///
/// Only a PROJECT has any, for the reason [`bedrock_data_automation_readiness`] records: the
/// other two are complete as their creates answer, so their records stay exactly what those
/// creates reported.
pub(super) async fn bedrock_data_automation_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    if resource_kind != Resource::Project {
        return Ok(BTreeMap::new());
    }
    Ok(
        read_resource(ctx, region, resource_kind, physical_resource_id)
            .await?
            .map(|project| reported_attributes(&project, PROJECT_ATTRIBUTES))
            .unwrap_or_default(),
    )
}

/// Apply a changed declaration to the resource the stack created, through that type's own
/// update — the one call that changes a resource that exists.
///
/// Each update takes the resource's ARN together with the configuration it is to carry, so a
/// member the declaration no longer states leaves the resource without it rather than keeping
/// the one an earlier deployment gave it. The resource keeps the ARN that service minted, so
/// the handle the stack publishes is unchanged.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it ([`unchangeable_properties`]): a changed `Tags`
/// list, which no update of the three carries — their tags are that service's own
/// `TagResource` and `UntagResource` — a changed blueprint `Type`, and a changed encryption
/// pair on a LIBRARY, whose update carries its description alone.
pub(super) async fn update_bedrock_data_automation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if unchangeable_properties(resource_kind)
        .iter()
        .any(|property| declared.get(property) != resource.properties.get(property))
    {
        return Ok(None);
    }
    let request = updated_members(resource_kind, declared)?;
    let answered = call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        update_operation(resource_kind),
        "PUT",
        &resource_uri(resource_kind, &resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Some(Created::attributes(
        identified_resource(resource_kind, &answered)?.1,
    )))
}

/// Delete the resource a stack created, by the identity it was created under — this resource's
/// physical id, which every one of those deletes takes as its ARN label. One the service no
/// longer holds (`ResourceNotFoundException`, judged on the CODE and never on a message that
/// carries the request's own values) is already in the state the delete asks for.
pub(super) async fn delete_bedrock_data_automation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        delete_operation(resource_kind),
        "DELETE",
        &resource_uri(resource_kind, physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BDA_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon Bedrock Data Automation still holds the resource a stack asked it to delete.
///
/// Only a LIBRARY has a removal to wait out: `DeleteDataAutomationLibrary` ACCEPTS the delete
/// and answers the library `DELETING` — that service still holds it, and a library it holds
/// still occupies the `libraryName` the declaration asked for — so the resource's deletion is
/// finished once that service no longer answers for it. A blueprint and a project are gone as
/// their deletes answer.
pub(super) async fn bedrock_data_automation_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    if resource_kind != Resource::Library {
        return Ok(Removal::Gone);
    }
    match read_resource(ctx, region, resource_kind, physical_resource_id).await? {
        Some(library) => Ok(Removal::Pending(
            reported_text(&library, "status").unwrap_or_else(|| LIBRARY_DELETING.to_string()),
        )),
        None => Ok(Removal::Gone),
    }
}

/// The resource's configuration as Amazon Bedrock Data Automation reports it, for drift
/// detection: that service's own read of the ARN that is this resource's physical id
/// ([`reported_properties`]). One it no longer holds (`ResourceNotFoundException`, judged on the
/// CODE) was deleted, which is what `None` reports.
pub(super) async fn describe_bedrock_data_automation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        read_resource(ctx, region, resource_kind, physical_resource_id)
            .await?
            .map(|detail| reported_as_properties(&detail, reported_properties(resource_kind))),
    )
}

/// The resource Amazon Bedrock Data Automation holds under `arn`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
///
/// Every read of the three is bound to `POST` on the resource's own URI with the rest of its
/// input in the body, so the request carries an empty document rather than no body at all.
async fn read_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    arn: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        read_operation(resource_kind),
        "POST",
        &resource_uri(resource_kind, arn),
        Some(&Value::Object(Map::new())),
    )
    .await
    {
        Ok(answer) => Ok(Some(answered_detail(resource_kind, &answer))),
        Err(refusal) if refusal.code == BDA_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The collection one of the three lives under, which is also the path one resource of that
/// kind is addressed beneath.
fn collection_uri(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => BLUEPRINTS_URI,
        Resource::Project => PROJECTS_URI,
        Resource::Library => LIBRARIES_URI,
    }
}

/// The URI one resource is addressed at: the collection plus the ARN `@httpLabel` and the
/// trailing slash those bindings carry, the ARN escaped as a single path segment
/// ([`percent_encoded`]) — it carries `:` and `/` — so a value carrying URI structure addresses
/// one resource rather than another path.
fn resource_uri(resource_kind: Resource, arn: &str) -> String {
    format!(
        "{}{}/",
        collection_uri(resource_kind),
        percent_encoded(arn)
    )
}

/// The operation that creates one of the three, as that service's own model names it.
fn create_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => CREATE_BLUEPRINT,
        Resource::Project => CREATE_PROJECT,
        Resource::Library => CREATE_LIBRARY,
    }
}

/// The operation that reads one of the three.
fn read_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => GET_BLUEPRINT,
        Resource::Project => GET_PROJECT,
        Resource::Library => GET_LIBRARY,
    }
}

/// The operation that changes one of the three.
fn update_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => UPDATE_BLUEPRINT,
        Resource::Project => UPDATE_PROJECT,
        Resource::Library => UPDATE_LIBRARY,
    }
}

/// The operation that deletes one of the three.
fn delete_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => DELETE_BLUEPRINT,
        Resource::Project => DELETE_PROJECT,
        Resource::Library => DELETE_LIBRARY,
    }
}

/// The member each create takes the resource's name in.
fn name_member(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => BLUEPRINT_NAME_MEMBER,
        Resource::Project => PROJECT_NAME_MEMBER,
        Resource::Library => LIBRARY_NAME_MEMBER,
    }
}

/// The property each type states that name in.
fn name_property(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => BLUEPRINT_NAME_PROPERTY,
        Resource::Project => PROJECT_NAME_PROPERTY,
        Resource::Library => LIBRARY_NAME_PROPERTY,
    }
}

/// The attribute each type publishes the ARN that service minted as, which is also this
/// resource's physical id.
fn arn_attribute(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Blueprint => BLUEPRINT_ARN_ATTRIBUTE,
        Resource::Project => PROJECT_ARN_ATTRIBUTE,
        Resource::Library => LIBRARY_ARN_ATTRIBUTE,
    }
}

/// The attributes one answered resource of this kind reports.
fn published_attributes(resource_kind: Resource) -> &'static [(&'static str, &'static str)] {
    match resource_kind {
        Resource::Blueprint => BLUEPRINT_ATTRIBUTES,
        Resource::Project => PROJECT_ATTRIBUTES,
        Resource::Library => LIBRARY_ATTRIBUTES,
    }
}

/// The members a read of one of the three reports under the names the declaring type spells them
/// with, for drift detection, as (property, member).
///
/// A blueprint's `Schema` and a project's CONFIGURATION documents are left out: the read reports
/// each in that service's own member spelling (and a schema as the text it stores), which is not
/// the form the declaration states, so comparing them would report drift on a resource nobody
/// changed. A property the read does not report is not evidence of anything
/// ([`super::drift`]).
fn reported_properties(resource_kind: Resource) -> &'static [(&'static str, &'static str)] {
    match resource_kind {
        Resource::Blueprint => &[
            ("BlueprintName", "blueprintName"),
            ("Type", "type"),
            ("BlueprintStage", "blueprintStage"),
            ("KmsKeyId", "kmsKeyId"),
            ("KmsEncryptionContext", "kmsEncryptionContext"),
        ],
        Resource::Project => &[
            ("ProjectName", "projectName"),
            ("ProjectDescription", "projectDescription"),
            ("ProjectStage", "projectStage"),
            ("ProjectType", "projectType"),
            ("KmsKeyId", "kmsKeyId"),
            ("KmsEncryptionContext", "kmsEncryptionContext"),
        ],
        Resource::Library => &[
            ("LibraryName", "libraryName"),
            ("LibraryDescription", "libraryDescription"),
            ("KmsKeyId", "kmsKeyId"),
            ("KmsEncryptionContext", "kmsEncryptionContext"),
        ],
    }
}

/// The resource inside one answer: the member a read reports it in where the answer carries that
/// wrapper, and the answer itself otherwise — a project's and a library's creates answer their
/// own members flat.
fn answered_detail(resource_kind: Resource, answered: &Value) -> Value {
    let member = match resource_kind {
        Resource::Blueprint => BLUEPRINT_MEMBER,
        Resource::Project => PROJECT_MEMBER,
        Resource::Library => LIBRARY_MEMBER,
    };
    answered
        .get(member)
        .filter(|detail| detail.is_object())
        .cloned()
        .unwrap_or_else(|| answered.clone())
}

/// The attributes one answered resource reports ([`published_attributes`]), beside the ARN that
/// service identified it by — the one value a resource of these types cannot be recorded
/// without.
///
/// An answer carrying no ARN is reported as a refusal rather than recorded as a created
/// resource: there would be no handle a client could call that service with. ONE statement of
/// that rule, so the create's identity and the update's attributes cannot disagree about it.
fn identified_resource(
    resource_kind: Resource,
    answered: &Value,
) -> Result<(String, BTreeMap<String, String>), Refusal> {
    let detail = answered_detail(resource_kind, answered);
    let attributes = reported_attributes(&detail, published_attributes(resource_kind));
    let Some(arn) = attributes.get(arn_attribute(resource_kind)).cloned() else {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "Amazon Bedrock Data Automation answered a created resource without an ARN"
                .to_string(),
        });
    };
    Ok((arn, attributes))
}

/// The NAME the resource is created under: the one the declaration asked for, and otherwise one
/// derived from the resource's own computed identity ([`DERIVED_NAME_PREFIX`]) — the physical id
/// the naming rules computed being a stand-in ARN, which no name member of this service admits.
fn configured_name(resource_kind: Resource, resource: &StoredResource) -> String {
    resource
        .properties
        .get(name_property(resource_kind))
        .and_then(Value::as_str)
        .filter(|declared| !declared.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{DERIVED_NAME_PREFIX}{}",
                crate::state::stable_identifier_digits(
                    &resource.physical_resource_id,
                    DERIVED_NAME_DIGITS
                )
            )
        })
}

/// One declared property document as the request members the CREATE of this kind takes, with the
/// name and the tags left out — the caller adds the name it decided and the tag list that call
/// takes, so one reader serves the create and the update.
fn declared_members(
    resource_kind: Resource,
    declared: &Value,
) -> Result<Map<String, Value>, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    match resource_kind {
        Resource::Blueprint => {
            copy_members(&properties, BLUEPRINT_MEMBERS, &mut request);
            if let Some(schema) = properties.get(SCHEMA_PROPERTY) {
                // `BlueprintSchema` is a STRING, so a declaration that writes the schema as a
                // JSON document travels as that document's own text, through the one
                // JSON-to-text rule this catalogue has. A value that is neither text nor a
                // document is sent AS IT CAME, so that operation's own validation refuses it
                // rather than this dropping the schema the template stated.
                request.insert(
                    SCHEMA_MEMBER.to_string(),
                    document_text(schema).map_or_else(|| schema.clone(), Value::String),
                );
            }
        }
        Resource::Project => {
            copy_members(&properties, PROJECT_MEMBERS, &mut request);
            copy_configurations(&properties, &mut request)?;
        }
        Resource::Library => copy_members(&properties, LIBRARY_MEMBERS, &mut request),
    }
    if let Some(encryption) = encryption_configuration(&properties) {
        request.insert(ENCRYPTION_CONFIGURATION_MEMBER.to_string(), encryption);
    }
    Ok(request)
}

/// The properties a change to which the update of this kind CANNOT carry, so a declaration
/// that states one of them differently is reported as NOT applied rather than as a success
/// that dropped it ([`update_bedrock_data_automation`]).
///
/// The tags are on every list: no update of the three takes a `tags` member at all, a
/// resource's tags being that service's own `TagResource` and `UntagResource`. Beside them, a
/// blueprint's `Type` is no member of `UpdateBlueprint`, a project's `ProjectType` is no member
/// of `UpdateDataAutomationProject`, and a library's encryption pair is no member of
/// `UpdateDataAutomationLibrary`, which carries its `libraryDescription` alone. (Each type's
/// NAME is one of its create-only properties, so a declaration that renames one asks for a
/// different resource and never reaches an update at all.)
fn unchangeable_properties(resource_kind: Resource) -> &'static [&'static str] {
    match resource_kind {
        Resource::Blueprint => &[TAGS_PROPERTY, TYPE_PROPERTY],
        Resource::Project => &[TAGS_PROPERTY, PROJECT_TYPE_PROPERTY],
        Resource::Library => &[
            TAGS_PROPERTY,
            KMS_KEY_ID_PROPERTY,
            KMS_ENCRYPTION_CONTEXT_PROPERTY,
            ENCRYPTION_CONFIGURATION_PROPERTY,
        ],
    }
}

/// One declared property document as the request members the UPDATE of this kind takes: the
/// create's own members minus the ones that call does not carry.
///
/// A library's update takes its `libraryDescription` alone, and neither a project's nor a
/// blueprint's takes the resource's name — each type's name is one of its create-only
/// properties, so a declaration that renames one asks for a different resource and never
/// reaches here. A CHANGE to any member dropped here is reported as not applied rather than
/// silently discarded ([`unchangeable_properties`]).
fn updated_members(
    resource_kind: Resource,
    declared: &Value,
) -> Result<Map<String, Value>, Refusal> {
    let mut request = declared_members(resource_kind, declared)?;
    if resource_kind == Resource::Blueprint {
        // `UpdateBlueprint` takes no `type`: a blueprint's `Type` is one of its create-only
        // properties, so a declaration stating another asks for a different blueprint.
        request.remove(TYPE_MEMBER);
    }
    if resource_kind == Resource::Project {
        // `UpdateDataAutomationProject` takes no `projectType` either, for the same reason: a
        // project's `ProjectType` is one of its create-only properties.
        request.remove(PROJECT_TYPE_MEMBER);
    }
    if resource_kind == Resource::Library {
        // `UpdateDataAutomationLibrary` carries the description alone, so nothing else the
        // declaration states is sent as a member that call does not take.
        request.retain(|member, _| member == LIBRARY_DESCRIPTION_MEMBER);
    }
    Ok(request)
}

/// Copy the project's declared CONFIGURATION documents onto the members they are, each with its
/// member names re-cased ([`lower_camel_members`]).
fn copy_configurations(
    properties: &Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), Refusal> {
    for (property, member) in PROJECT_CONFIGURATIONS {
        if let Some(stated) = properties.get(*property).filter(|value| !value.is_null()) {
            request.insert((*member).to_string(), lower_camel_members(stated, 0)?);
        }
    }
    Ok(())
}

/// The `encryptionConfiguration` member a declaration asks for, or `None` where it asks for
/// none.
///
/// Three spellings reach the one member: the type's own flat `KmsKeyId` and
/// `KmsEncryptionContext`, and an `EncryptionConfiguration` document already written as that
/// member with upper-cased names. The CONTEXT is a map of the CALLER's own keys, so it travels
/// as written rather than re-cased.
///
/// A declaration that states a context and NO key sends that context anyway: `kmsKeyId` is that
/// structure's required member, so what refuses such a declaration is the owning service's own
/// validation of the member it requires, rather than this dropping a value the template stated.
fn encryption_configuration(properties: &Map<String, Value>) -> Option<Value> {
    let mut encryption = Map::new();
    if let Some(stated) = properties
        .get(ENCRYPTION_CONFIGURATION_PROPERTY)
        .and_then(Value::as_object)
    {
        if let Some(key) = stated.get(KMS_KEY_ID_PROPERTY) {
            encryption.insert(KMS_KEY_ID_MEMBER.to_string(), key.clone());
        }
        if let Some(context) = stated.get(KMS_ENCRYPTION_CONTEXT_PROPERTY) {
            encryption.insert(KMS_ENCRYPTION_CONTEXT_MEMBER.to_string(), context.clone());
        }
    }
    if let Some(key) = properties
        .get(KMS_KEY_ID_PROPERTY)
        .filter(|value| !value.is_null())
    {
        encryption.insert(KMS_KEY_ID_MEMBER.to_string(), key.clone());
    }
    if let Some(context) = properties
        .get(KMS_ENCRYPTION_CONTEXT_PROPERTY)
        .filter(|value| !value.is_null())
    {
        encryption.insert(KMS_ENCRYPTION_CONTEXT_MEMBER.to_string(), context.clone());
    }
    (!encryption.is_empty()).then_some(Value::Object(encryption))
}
