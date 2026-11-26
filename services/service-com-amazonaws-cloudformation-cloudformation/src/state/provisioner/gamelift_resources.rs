// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon GameLift Servers' own control plane: the custom
//! game BUILD an `AWS::GameLift::Build` declares and the Realtime SCRIPT an
//! `AWS::GameLift::Script` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`.
//!
//! Both types ship game code the same way: the declaration names the Amazon S3 location the
//! zip file is already at — its bucket, its key and the ARN of the role "that allows Amazon
//! GameLift Servers to access your Amazon S3 bucket" — and every property of either type is a
//! member of its create under the SAME name, so the evaluated property document IS the
//! request, with the tag set lifted out of it (`Tags`, which each create takes as its own
//! `TagList` of `{Key, Value}` pairs).
//!
//! The identity is that service's: neither `CreateBuild` nor `CreateScript` takes an
//! identifier, and each answers the `build-…`/`script-…` id it minted with the ARN built
//! around it — the id being what that type's `Ref` returns and the handle every later call on
//! the resource takes — so the id it answered is the resource's physical id and the ARN it
//! answered is the `Arn` the resource publishes, both read off that service's own record
//! rather than recomputed here
//! ([`super::super::template::identity_is_service_assigned`] names both types).
//!
//! A created build is `INITIALIZED` and becomes `READY` once that service has read the files
//! at the location the declaration named, which is Amazon GameLift Servers' own transition
//! and not a wait on the creation: the build the create answered exists and takes the calls
//! made on it, exactly as the DB instance an `AWS::RDS::DBInstance` creates is reported
//! created while Amazon RDS still has it `creating`. A build whose files nobody uploaded rests
//! `INITIALIZED` — the state "a new build has been defined, but no files have been uploaded" —
//! so there is no state here for a wait to end in.
//!
//! What a changed declaration can reach is what each `Update…` takes: a build's name and
//! version, and a script's name, version and content source. Every other property is fixed
//! when the resource is made ([`super::super::template::replacement_properties`]).

use super::*;

/// Amazon GameLift Servers' signing name and `X-Amz-Target` prefix, from its model
/// (`sigv4Name: gamelift`, service shape `GameLift`, protocol `awsJson1_1`) — the wire these
/// calls are written on, as that service's own suites drive it.
const GAMELIFT_SIGNING_NAME: &str = "gamelift";
const GAMELIFT_TARGET_PREFIX: &str = "GameLift";

/// The operations this module calls, as Amazon GameLift Servers' own model names them.
const CREATE_BUILD: &str = "CreateBuild";
const UPDATE_BUILD: &str = "UpdateBuild";
const DELETE_BUILD: &str = "DeleteBuild";
const DESCRIBE_BUILD: &str = "DescribeBuild";
const CREATE_SCRIPT: &str = "CreateScript";
const UPDATE_SCRIPT: &str = "UpdateScript";
const DELETE_SCRIPT: &str = "DeleteScript";
const DESCRIBE_SCRIPT: &str = "DescribeScript";

/// The error code that service answers for a build or a script it does not hold
/// (`NotFoundException`, the modelled refusal of every call that names one: "the requested
/// resource was not found").
const GAMELIFT_NOT_FOUND: &str = "NotFoundException";

/// The properties `CreateBuild` takes as the member of the SAME name, which is every property
/// of `AWS::GameLift::Build` but its `Tags` ([`tags_of`] reads those). `StorageLocation`'s own
/// members are the `S3Location` members spelled identically on both sides (`Bucket`, `Key`,
/// `RoleArn`, `ObjectVersion`), so the declared structure travels as written.
const BUILD_MEMBERS: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("OperatingSystem", "OperatingSystem"),
    ("ServerSdkVersion", "ServerSdkVersion"),
    ("StorageLocation", "StorageLocation"),
    ("Version", "Version"),
];

/// Every property `AWS::GameLift::Build` publishes, which is what a declaration may state: a
/// property no arm here maps onto that create's request is refused rather than dropped
/// silently ([`refuse_unmapped_properties`]).
const BUILD_PROPERTIES: &[&str] = &[
    "Name",
    "OperatingSystem",
    "ServerSdkVersion",
    "StorageLocation",
    TAGS_PROPERTY,
    "Version",
];

/// The properties `CreateScript` takes as the member of the same name, and every property
/// `AWS::GameLift::Script` publishes, read exactly as a build's are.
const SCRIPT_MEMBERS: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("NodeJsVersion", "NodeJsVersion"),
    ("StorageLocation", "StorageLocation"),
    ("Version", "Version"),
];
const SCRIPT_PROPERTIES: &[&str] = &[
    "Name",
    "NodeJsVersion",
    "StorageLocation",
    TAGS_PROPERTY,
    "Version",
];

/// The members each `Update…` takes beside the resource's own identifier: a build's name and
/// version, and a script's name, version and content source — the whole of what a changed
/// declaration can reach. (`UpdateScript`'s `ZipFile` is no property of the type: a template
/// ships the code from Amazon S3.)
const BUILD_UPDATE_MEMBERS: &[(&str, &str)] = &[("Name", "Name"), ("Version", "Version")];
const SCRIPT_UPDATE_MEMBERS: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("StorageLocation", "StorageLocation"),
    ("Version", "Version"),
];

/// The property either type publishes its tag set as, and the member each create takes it as:
/// a list of `Tag` structures whose members are spelled `Key`/`Value` ([`UPPER_CASED_TAG`]).
const TAGS_PROPERTY: &str = "Tags";
const TAGS_MEMBER: &str = "Tags";

/// The member each call about one of these resources carries the record in, and the member
/// each call addresses one by.
const BUILD_MEMBER: &str = "Build";
const SCRIPT_MEMBER: &str = "Script";
const BUILD_ID_MEMBER: &str = "BuildId";
const SCRIPT_ID_MEMBER: &str = "ScriptId";

/// The member each record carries the ARN that service built around the minted id in.
const BUILD_ARN_MEMBER: &str = "BuildArn";
const SCRIPT_ARN_MEMBER: &str = "ScriptArn";

/// The scalar properties a reported build and a reported script carry, as `(property,
/// member)` pairs: that model spells each of them exactly as the type's property is spelled.
/// Neither record carries its `StorageLocation` as a declaration states it (a build reports
/// none at all, and a script the location the service resolved) and neither carries its tag
/// set, which is the position every non-scalar property is in ([`super::drift`]).
const REPORTED_BUILD_PROPERTIES: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("OperatingSystem", "OperatingSystem"),
    ("ServerSdkVersion", "ServerSdkVersion"),
    ("Version", "Version"),
];
const REPORTED_SCRIPT_PROPERTIES: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("NodeJsVersion", "NodeJsVersion"),
    ("Version", "Version"),
];

/// Create the custom game build an `AWS::GameLift::Build` declares, in Amazon GameLift
/// Servers — so a client that deployed the stack finds the build in that service's own
/// `DescribeBuild` under the id the stack published and can create a fleet from it, rather
/// than holding an identifier nothing created.
///
/// The declaration IS the request ([`BUILD_MEMBERS`]), with the tag set CloudFormation gives
/// the resource beside the declaration's own sent as the `Tags` list that create takes. The
/// build is recorded in the service that OWNS it, so everything the declaration states is
/// resolved there exactly as it is for a client's own call: a storage location naming no
/// bucket, key or role, an operating system outside the modelled enum and a server SDK
/// version outside its pattern are that service's refusals, which fail the resource and roll
/// the stack back rather than publishing an identifier for a build nothing holds.
pub(super) async fn create_gamelift_build(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, BUILD_PROPERTIES)?;
    let request = create_request(resource, BUILD_MEMBERS, propagated);
    let answered = call_gamelift(ctx, region, CREATE_BUILD, &request).await?;
    let created = answered.get(BUILD_MEMBER).cloned().unwrap_or(Value::Null);
    created_resource(
        &created,
        BUILD_ID_MEMBER,
        BUILD_ARN_MEMBER,
        template::GAMELIFT_BUILD_ID_ATTRIBUTE,
        "build",
    )
}

/// Create the Realtime script an `AWS::GameLift::Script` declares, in the same service and
/// for the same reason: `CreateScript` records the script for the zip file at the declared
/// storage location, and a fleet deploys it from there.
///
/// Its runtime version is the declaration's own and is sent as the member that create takes
/// it as — so a declaration that names none is refused by the OWNING service, which is what
/// fails the resource and rolls the stack back.
pub(super) async fn create_gamelift_script(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SCRIPT_PROPERTIES)?;
    let request = create_request(resource, SCRIPT_MEMBERS, propagated);
    let answered = call_gamelift(ctx, region, CREATE_SCRIPT, &request).await?;
    let created = answered.get(SCRIPT_MEMBER).cloned().unwrap_or(Value::Null);
    created_resource(
        &created,
        SCRIPT_ID_MEMBER,
        SCRIPT_ARN_MEMBER,
        template::GAMELIFT_ID_ATTRIBUTE,
        "script",
    )
}

/// Apply a changed declaration to the build the stack created, through `UpdateBuild` — the
/// one call that changes a build that exists, which takes the build's id with a new name and
/// version and nothing else.
pub(super) async fn update_gamelift_build(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let answered = call_update(
        ctx,
        region,
        UPDATE_BUILD,
        BUILD_ID_MEMBER,
        &resource.physical_resource_id,
        BUILD_UPDATE_MEMBERS,
        declared,
    )
    .await?;
    let updated = answered.get(BUILD_MEMBER).cloned().unwrap_or(Value::Null);
    Ok(Created::attributes(reported_attributes(
        &updated,
        BUILD_ID_MEMBER,
        BUILD_ARN_MEMBER,
        template::GAMELIFT_BUILD_ID_ATTRIBUTE,
    )))
}

/// The same for the Realtime script, through `UpdateScript`: a script's name, version and
/// content source are what that call takes beside the script's id.
pub(super) async fn update_gamelift_script(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let answered = call_update(
        ctx,
        region,
        UPDATE_SCRIPT,
        SCRIPT_ID_MEMBER,
        &resource.physical_resource_id,
        SCRIPT_UPDATE_MEMBERS,
        declared,
    )
    .await?;
    let updated = answered.get(SCRIPT_MEMBER).cloned().unwrap_or(Value::Null);
    Ok(Created::attributes(reported_attributes(
        &updated,
        SCRIPT_ID_MEMBER,
        SCRIPT_ARN_MEMBER,
        template::GAMELIFT_ID_ATTRIBUTE,
    )))
}

/// Delete the build a stack created, by the id Amazon GameLift Servers minted for it — this
/// resource's physical id. `DeleteBuild` "permanently deletes the build resource and any
/// uploaded build files", so a later `DescribeBuild` of that id answers that service's own
/// `NotFoundException`; a build it no longer holds (judged on the CODE and never on a message
/// that carries the request's own values) is already in the state the delete asks for.
pub(super) async fn delete_gamelift_build(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_by_id(
        ctx,
        region,
        DELETE_BUILD,
        BUILD_ID_MEMBER,
        physical_resource_id,
    )
    .await
}

/// Delete the Realtime script a stack created, the same way: `DeleteScript` "deletes a
/// Realtime script", by the `ScriptId` that is this resource's physical id.
pub(super) async fn delete_gamelift_script(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_by_id(
        ctx,
        region,
        DELETE_SCRIPT,
        SCRIPT_ID_MEMBER,
        physical_resource_id,
    )
    .await
}

/// The build's configuration as Amazon GameLift Servers reports it, for drift detection: that
/// service's own `DescribeBuild` of the id that is this resource's physical id, under the
/// properties the type declares those members as ([`REPORTED_BUILD_PROPERTIES`]). A build it
/// no longer holds was deleted, which is what `None` reports.
pub(super) async fn describe_gamelift_build(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(reported_record(
        ctx,
        region,
        DESCRIBE_BUILD,
        BUILD_ID_MEMBER,
        BUILD_MEMBER,
        physical_resource_id,
    )
    .await?
    .map(|build| reported_as_properties(&build, REPORTED_BUILD_PROPERTIES)))
}

/// The same read for the Realtime script, through `DescribeScript`.
pub(super) async fn describe_gamelift_script(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(reported_record(
        ctx,
        region,
        DESCRIBE_SCRIPT,
        SCRIPT_ID_MEMBER,
        SCRIPT_MEMBER,
        physical_resource_id,
    )
    .await?
    .map(|script| reported_as_properties(&script, REPORTED_SCRIPT_PROPERTIES)))
}

/// The create request one declaration builds: the properties `members` names, carried through
/// under the member each pair gives, with the tag set CloudFormation gives the resource beside
/// the declaration's own as the list each create takes.
fn create_request(
    resource: &StoredResource,
    members: &[(&str, &str)],
    propagated: &[(String, String)],
) -> Value {
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, members, &mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        // `TagList` is modelled `len 0..200`, so an untagged resource carries no list at all
        // rather than an empty one.
        request.insert(TAGS_MEMBER.to_string(), tags_as_list(&tags, UPPER_CASED_TAG));
    }
    Value::Object(request)
}

/// What a create answered, as the resource's identity: the id that service minted is the
/// physical id — the value the type's `Ref` returns and the handle every later call takes —
/// and it is reported back beside the ARN that service built around it.
///
/// A record carrying no id at all is a FAILED create rather than a created resource: there
/// would be no handle a client could call that service with, and nothing for the stack to
/// publish. `kind` names which resource that was, so the reason says what failed.
fn created_resource(
    created: &Value,
    id_member: &str,
    arn_member: &str,
    id_attribute: &str,
    kind: &str,
) -> Result<Created, Refusal> {
    let id = reported_text(created, id_member).unwrap_or_default();
    if id.is_empty() {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the Amazon GameLift Servers control plane answered a created {kind} without \
                 a {id_member}"
            ),
        });
    }
    Ok(Created {
        physical_resource_id: Some(id),
        attributes: reported_attributes(created, id_member, arn_member, id_attribute),
    })
}

/// The attributes ONE reported record gives the resource: the id that service minted and the
/// ARN it built around that id, read off its own report rather than recomputed. A value the
/// report does not carry contributes no attribute at all, so the resource keeps what the
/// naming rules computed rather than taking an empty one.
fn reported_attributes(
    reported: &Value,
    id_member: &str,
    arn_member: &str,
    id_attribute: &str,
) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    if let Some(id) = reported_text(reported, id_member) {
        attributes.insert(id_attribute.to_string(), id);
    }
    if let Some(arn) = reported_text(reported, arn_member) {
        attributes.insert(template::GAMELIFT_ARN_ATTRIBUTE.to_string(), arn);
    }
    attributes
}

/// One `Update…` of a resource this stack created: the members `members` names, as the
/// declaration now states them, beside the identifier the call addresses the resource by.
#[allow(clippy::too_many_arguments)]
async fn call_update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    id_member: &str,
    physical_resource_id: &str,
    members: &[(&str, &str)],
    declared: &Value,
) -> Result<Value, Refusal> {
    let mut request = Map::new();
    request.insert(
        id_member.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    if let Some(properties) = declared.as_object() {
        copy_members(properties, members, &mut request);
    }
    call_gamelift(ctx, region, operation, &Value::Object(request)).await
}

/// One delete of a resource this stack created, by the id it was created under. One that
/// service no longer holds is already in the state the delete asks for.
async fn delete_by_id(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    id_member: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        id_member.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    match call_gamelift(ctx, region, operation, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == GAMELIFT_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The record Amazon GameLift Servers reports for ONE resource, by the id it minted, or
/// `None` where it holds none — its `NotFoundException`, which is what a finished teardown
/// looks like.
async fn reported_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    id_member: &str,
    record_member: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        id_member.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    match call_gamelift(ctx, region, operation, &Value::Object(request)).await {
        Ok(answered) => Ok(answered.get(record_member).cloned()),
        Err(refusal) if refusal.code == GAMELIFT_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Call one Amazon GameLift Servers operation in process, on the awsJson wire that service
/// publishes.
async fn call_gamelift(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        GAMELIFT_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{GAMELIFT_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}
