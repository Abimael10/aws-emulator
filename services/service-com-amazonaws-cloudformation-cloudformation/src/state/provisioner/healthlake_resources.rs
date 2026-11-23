// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS HealthLake: the FHIR data store an
//! `AWS::HealthLake::FHIRDatastore` declares and the data transformation profile an
//! `AWS::HealthLake::DataTransformationProfile` declares. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! Each type's properties ARE its create's own members, named identically on both sides —
//! that service spells its members in upper camel case, as a CloudFormation property is
//! spelled, and renames none of them ([`DATASTORE_MEMBERS`], [`PROFILE_MEMBERS`]) — so the
//! evaluated property document is the request, with the tag set lifted out because it
//! carries the tags CloudFormation gives the resource beside the ones the declaration asks
//! for. A declared property no arm maps onto the request is REFUSED rather than dropped, so
//! a stack never reports a resource configured with less than its declaration asked for.
//!
//! Neither identity is a value a naming rule could compute: `CreateFHIRDatastore` mints the
//! store's `DatastoreId` and answers the ARN and the endpoint built around it, and
//! `CreateDataTransformationProfile` mints the profile's `ProfileId`. So the id that service
//! answered becomes the data store's physical id ("Ref returns the DatastoreId"), and the
//! profile's is the ARN built around the id it answered — the handle that service's own
//! tagging surface addresses a profile by, which is what this arm READS BACK to establish
//! that the profile the stack publishes an ARN for is one HealthLake resolves
//! ([`resolve_profile_arn`]).
//!
//! A created data store is `CREATING` — in which it serves no FHIR interaction and its
//! endpoint answers nothing — so the resource's creation is held open until HealthLake
//! reports it `ACTIVE` ([`fhir_datastore_readiness`]), and its endpoint and status are read
//! off that service's own report of the store once it is up
//! ([`fhir_datastore_ready_attributes`]). Its teardown is asynchronous the same way:
//! `DeleteFHIRDatastore` leaves the store `DELETING` and the resource's deletion is finished
//! once that service reports it `DELETED` ([`fhir_datastore_removal`]). A PROFILE is created
//! in DRAFT by the call that answers and removed by the call that deletes it, so neither of
//! its lifecycles waits on anything.

use super::*;

/// AWS HealthLake's signing name, from its model (`sigv4Name: healthlake`), and the service
/// shape its awsJson1_0 `X-Amz-Target` names an operation under (`HealthLake.<Operation>`).
const HEALTHLAKE_SIGNING_NAME: &str = "healthlake";
const HEALTHLAKE_TARGET_PREFIX: &str = "HealthLake";

/// The operations this module calls, as HealthLake's own model names them.
const CREATE_FHIR_DATASTORE: &str = "CreateFHIRDatastore";
const DESCRIBE_FHIR_DATASTORE: &str = "DescribeFHIRDatastore";
const DELETE_FHIR_DATASTORE: &str = "DeleteFHIRDatastore";
const CREATE_DATA_TRANSFORMATION_PROFILE: &str = "CreateDataTransformationProfile";
const GET_DATA_TRANSFORMATION_PROFILE: &str = "GetDataTransformationProfile";
const DELETE_DATA_TRANSFORMATION_PROFILE: &str = "DeleteDataTransformationProfile";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";

/// The error code HealthLake answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, its deletes and its tag
/// reads).
const HEALTHLAKE_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The properties `AWS::HealthLake::FHIRDatastore` publishes, and the members
/// `CreateFHIRDatastore` takes each of them as: every one is the member of the SAME name.
/// `Tags` is lifted out of the mapping because the set that travels is not the declaration's
/// alone ([`create_fhir_datastore`]).
const DATASTORE_MEMBERS: &[(&str, &str)] = &[
    ("DatastoreName", "DatastoreName"),
    ("DatastoreTypeVersion", "DatastoreTypeVersion"),
    ("IdentityProviderConfiguration", "IdentityProviderConfiguration"),
    ("PreloadDataConfig", "PreloadDataConfig"),
    ("SseConfiguration", "SseConfiguration"),
];

/// Every property of that type an arm of this module maps onto the create, the tag set
/// included: one the list does not name is refused rather than dropped.
const DATASTORE_PROPERTIES: &[&str] = &[
    "DatastoreName",
    "DatastoreTypeVersion",
    "IdentityProviderConfiguration",
    "PreloadDataConfig",
    "SseConfiguration",
    "Tags",
];

/// The properties `AWS::HealthLake::DataTransformationProfile` publishes, and the members
/// `CreateDataTransformationProfile` takes each of them as — the same identity mapping, with
/// `Source` the union whose one declared variant names the initial content.
const PROFILE_MEMBERS: &[(&str, &str)] = &[
    ("KmsKeyId", "KmsKeyId"),
    ("ProfileDescription", "ProfileDescription"),
    ("ProfileName", "ProfileName"),
    ("Source", "Source"),
    ("SourceFormat", "SourceFormat"),
];

/// Every property of that type an arm of this module maps onto the create, the tag set
/// included.
const PROFILE_PROPERTIES: &[&str] = &[
    "KmsKeyId",
    "ProfileDescription",
    "ProfileName",
    "Source",
    "SourceFormat",
    "Tags",
];

/// The member the data store's create and its delete address a store by, and the member its
/// own read answers the store's record in.
const DATASTORE_ID_MEMBER: &str = "DatastoreId";
const DATASTORE_PROPERTIES_MEMBER: &str = "DatastoreProperties";

/// The members a data store's record carries that this type publishes as return values.
const DATASTORE_ARN_MEMBER: &str = "DatastoreArn";
const DATASTORE_ENDPOINT_MEMBER: &str = "DatastoreEndpoint";
const DATASTORE_STATUS_MEMBER: &str = "DatastoreStatus";
const DATASTORE_NAME_MEMBER: &str = "DatastoreName";
const DATASTORE_TYPE_VERSION_MEMBER: &str = "DatastoreTypeVersion";

/// The `DatastoreStatus` a data store SERVES in: "the data store status" a created store
/// reaches once HealthLake has brought it up, which is when its FHIR endpoint answers.
const DATASTORE_ACTIVE: &str = "ACTIVE";

/// The `DatastoreStatus` a created store is answered in, in which no FHIR interaction is
/// served yet.
const DATASTORE_CREATING: &str = "CREATING";

/// The terminal `DatastoreStatus` of a store HealthLake has removed. A deleted store is
/// reported `DELETING` until it reaches this one, which is what a removal waits out.
const DATASTORE_DELETED: &str = "DELETED";

/// The member the profile's create answers the id it minted in, and the members its own read
/// and that create report under the names the type declares them as.
const PROFILE_ID_MEMBER: &str = "ProfileId";
const PROFILE_NAME_MEMBER: &str = "ProfileName";
const PROFILE_DESCRIPTION_MEMBER: &str = "ProfileDescription";
const SOURCE_FORMAT_MEMBER: &str = "SourceFormat";
const TARGET_FORMAT_MEMBER: &str = "TargetFormat";
const VERSION_MEMBER: &str = "Version";
const LAST_UPDATED_AT_MEMBER: &str = "LastUpdatedAt";

/// The members each create takes its tags as: a data store's create takes the `TagList` of
/// `{Key, Value}` pairs ([`UPPER_CASED_TAG`]), while a profile's takes the `TagMap` of key to
/// value. `ResourceARN` is the member the one tag READ of this service takes, which is how a
/// profile's identity is established ([`resolve_profile_arn`]).
const TAGS_MEMBER: &str = "Tags";
const RESOURCE_ARN_MEMBER: &str = "ResourceARN";

/// The attributes each type publishes, spelled as `Fn::GetAtt` reads them.
const DATASTORE_ARN_ATTRIBUTE: &str = "DatastoreArn";
const DATASTORE_ENDPOINT_ATTRIBUTE: &str = "DatastoreEndpoint";
const DATASTORE_ID_ATTRIBUTE: &str = "DatastoreId";
const DATASTORE_STATUS_ATTRIBUTE: &str = "DatastoreStatus";
const PROFILE_ARN_ATTRIBUTE: &str = "ProfileArn";
const PROFILE_ID_ATTRIBUTE: &str = "ProfileId";
const TARGET_FORMAT_ATTRIBUTE: &str = "TargetFormat";
const VERSION_ATTRIBUTE: &str = "Version";
const LAST_UPDATED_AT_ATTRIBUTE: &str = "LastUpdatedAt";

/// Call one HealthLake operation on the awsJson1_0 wire its model declares.
async fn call_healthlake(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        HEALTHLAKE_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{HEALTHLAKE_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// Create the FHIR data store an `AWS::HealthLake::FHIRDatastore` declares, so a client that
/// read the id out of the stack's outputs finds the store in HealthLake's own
/// `DescribeFHIRDatastore` and its FHIR endpoint is one that service serves.
///
/// The declared properties ARE the create's members ([`DATASTORE_MEMBERS`]), and the declared
/// tags travel beside the ones CloudFormation gives the resource ([`tags_of`]) as the
/// `TagList` of `{Key, Value}` pairs that call's own `Tags` member takes.
///
/// The identity is HealthLake's: the `DatastoreId` it minted becomes this resource's physical
/// id — the value this type's `Ref` returns and the handle every data-store API takes — and
/// the ARN, the endpoint and the status it answered are reported back as the type's own
/// attributes.
pub(super) async fn create_fhir_datastore(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, DATASTORE_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, DATASTORE_MEMBERS, &mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    let answered =
        call_healthlake(ctx, region, CREATE_FHIR_DATASTORE, &Value::Object(request)).await?;
    let datastore_id = reported_text(&answered, DATASTORE_ID_MEMBER).unwrap_or_default();
    if datastore_id.is_empty() {
        // There would be no handle a client could call HealthLake with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the AWS HealthLake control plane answered a created data store without a \
                      data store id"
                .to_string(),
        });
    }
    Ok(Created {
        physical_resource_id: Some(datastore_id),
        attributes: datastore_attributes(&answered),
    })
}

/// Whether HealthLake has finished bringing up the data store a stack created.
///
/// A created store is [`DATASTORE_CREATING`] — in which it serves no FHIR interaction and the
/// endpoint the stack publishes answers nothing — so [`DATASTORE_ACTIVE`] is the status its
/// creation is finished in. Every other status is one no wait brings a NEW store into: a
/// failed create, a store being torn down, a store another caller is changing. A store that
/// service no longer holds is in the same position, nothing being left to create it.
pub(super) async fn fhir_datastore_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = reported_datastore(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AWS HealthLake holds no data store {physical_resource_id}"
        )));
    };
    let status = reported_text(&reported, DATASTORE_STATUS_MEMBER).unwrap_or_default();
    if status == DATASTORE_ACTIVE {
        return Ok(Readiness::Ready);
    }
    if status == DATASTORE_CREATING {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "AWS HealthLake reports the data store {physical_resource_id} in status {status}"
    )))
}

/// The attributes HealthLake publishes for a data store it now reports `ACTIVE`: its own
/// report of the store, read the one way [`datastore_attributes`] reads one.
///
/// A store's `DatastoreStatus` is `CREATING` when the create answers, and its endpoint is
/// that service's to decide, so the attributes are read again once it reports the store up
/// rather than left as that answer carried them.
pub(super) async fn fhir_datastore_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(reported_datastore(ctx, region, physical_resource_id)
        .await?
        .map(|reported| datastore_attributes(&reported))
        .unwrap_or_default())
}

/// Whether HealthLake has finished removing the data store a stack deleted.
///
/// `DeleteFHIRDatastore` does not take the store away while the caller waits: it leaves it
/// `DELETING` and that service then reports it [`DATASTORE_DELETED`] — the
/// terminal status its own reads go on answering with — so the deletion is finished in that
/// status, and in a store that service no longer reports at all.
pub(super) async fn fhir_datastore_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match reported_datastore(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(reported) => {
            let status = reported_text(&reported, DATASTORE_STATUS_MEMBER).unwrap_or_default();
            if status == DATASTORE_DELETED {
                return Ok(Removal::Gone);
            }
            Ok(Removal::Pending(status))
        }
    }
}

/// Delete the data store a stack created, by the `DatastoreId` it was created under — this
/// resource's physical id. One that service no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message) is already in the
/// state the delete asks for.
pub(super) async fn delete_fhir_datastore(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        DATASTORE_ID_MEMBER.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    match call_healthlake(ctx, region, DELETE_FHIR_DATASTORE, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == HEALTHLAKE_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The data store's configuration as HealthLake reports it, for drift detection: its own
/// report of the store, by the id that is this resource's physical id, under the properties
/// the type declares those members as. A store that service no longer holds was deleted,
/// which is what `None` reports.
///
/// The two members it reports under the names this type declares are the store's NAME and the
/// FHIR release version it hosts; a property the report does not carry is not evidence of
/// anything ([`super::drift`]).
pub(super) async fn describe_fhir_datastore(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(reported_datastore(ctx, region, physical_resource_id)
        .await?
        .map(|reported| {
            reported_as_properties(
                &reported,
                &[
                    ("DatastoreName", DATASTORE_NAME_MEMBER),
                    ("DatastoreTypeVersion", DATASTORE_TYPE_VERSION_MEMBER),
                ],
            )
        }))
}

/// HealthLake's own report of ONE data store, by the id it minted for it, or `None` where it
/// no longer holds one under that id — judged on that service's own not-found CODE.
async fn reported_datastore(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    datastore_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        DATASTORE_ID_MEMBER.to_string(),
        Value::String(datastore_id.to_string()),
    );
    match call_healthlake(
        ctx,
        region,
        DESCRIBE_FHIR_DATASTORE,
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => Ok(answered.get(DATASTORE_PROPERTIES_MEMBER).cloned()),
        Err(refusal) if refusal.code == HEALTHLAKE_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes one reported data store gives the resource: the ARN HealthLake minted, the
/// endpoint it serves the store's FHIR interactions at, the id it drew and the status it
/// reports the store in. A member the report does not carry contributes no attribute, so the
/// resource keeps the stand-in the naming rules computed rather than taking an empty value.
///
/// Read off BOTH shapes that report a store, which carry these members under the same names:
/// the create's own answer and the `DatastoreProperties` of that service's read.
fn datastore_attributes(reported: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    for (attribute, member) in [
        (DATASTORE_ARN_ATTRIBUTE, DATASTORE_ARN_MEMBER),
        (DATASTORE_ENDPOINT_ATTRIBUTE, DATASTORE_ENDPOINT_MEMBER),
        (DATASTORE_ID_ATTRIBUTE, DATASTORE_ID_MEMBER),
        (DATASTORE_STATUS_ATTRIBUTE, DATASTORE_STATUS_MEMBER),
    ] {
        if let Some(value) = reported_text(reported, member) {
            attributes.insert(attribute.to_string(), value);
        }
    }
    attributes
}

/// Create the data transformation profile an `AWS::HealthLake::DataTransformationProfile`
/// declares, so a client that read the id out of the stack's outputs finds the profile in
/// HealthLake's own `GetDataTransformationProfile` and `ListDataTransformationProfiles`.
///
/// The declared properties ARE the create's members ([`PROFILE_MEMBERS`]), and the declared
/// tags travel beside the ones CloudFormation gives the resource as the `TagMap` that call's
/// own `Tags` member takes.
///
/// The identity is the ARN built around the `ProfileId` that service minted — the value this
/// type's `Ref` returns, and one of the two handles HealthLake addresses a profile by. The
/// create answers the id alone, so the ARN is composed around it under that service's own
/// template (`dataTransformationProfile/{ProfileId}`) and then READ BACK through the one
/// surface of that service which resolves one ([`resolve_profile_arn`]): what the stack
/// publishes is an ARN HealthLake itself answers for, rather than one composed and never
/// checked. Every other value the type publishes is read off that create's own answer.
pub(super) async fn create_data_transformation_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, PROFILE_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, PROFILE_MEMBERS, &mut request);
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_healthlake(
        ctx,
        region,
        CREATE_DATA_TRANSFORMATION_PROFILE,
        &Value::Object(request),
    )
    .await?;
    let profile_id = reported_text(&answered, PROFILE_ID_MEMBER).unwrap_or_default();
    if profile_id.is_empty() {
        // There would be no handle a client could call HealthLake with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the AWS HealthLake control plane answered a created data transformation \
                      profile without a profile id"
                .to_string(),
        });
    }
    let profile_arn = profile_arn(region, &profile_id);
    // The ARN the stack is about to publish is one HealthLake resolves, which is what this
    // read establishes: a refusal of it is a failed create rather than an identity nothing
    // answers for.
    resolve_profile_arn(ctx, region, &profile_arn).await?;
    let mut attributes = BTreeMap::new();
    attributes.insert(PROFILE_ARN_ATTRIBUTE.to_string(), profile_arn.clone());
    attributes.insert(PROFILE_ID_ATTRIBUTE.to_string(), profile_id);
    // The version the profile was created at and the format it converts to are what that
    // create answered, read off its own answer rather than restated here; the instant it
    // recorded is that service's clock, read the ONE way this provisioner reads a timestamp
    // ([`instant_text`]), since a CloudFormation attribute is text and a timestamp is a value
    // on the wire. A member the answer does not carry contributes no attribute, so the
    // resource keeps the stand-in the naming rules computed.
    for (attribute, member) in [
        (TARGET_FORMAT_ATTRIBUTE, TARGET_FORMAT_MEMBER),
        (VERSION_ATTRIBUTE, VERSION_MEMBER),
    ] {
        if let Some(value) = reported_text(&answered, member) {
            attributes.insert(attribute.to_string(), value);
        }
    }
    if let Some(instant) = answered.get(LAST_UPDATED_AT_MEMBER).and_then(instant_text) {
        attributes.insert(LAST_UPDATED_AT_ATTRIBUTE.to_string(), instant);
    }
    Ok(Created {
        physical_resource_id: Some(profile_arn),
        attributes,
    })
}

/// Delete the data transformation profile a stack created, by the `ProfileId` it was created
/// under — the id inside the ARN that is this resource's physical id. One that service no
/// longer holds (`ResourceNotFoundException`, judged on the CODE) is already in the state the
/// delete asks for.
pub(super) async fn delete_data_transformation_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        PROFILE_ID_MEMBER.to_string(),
        Value::String(profile_id_of(physical_resource_id)),
    );
    match call_healthlake(
        ctx,
        region,
        DELETE_DATA_TRANSFORMATION_PROFILE,
        &Value::Object(request),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == HEALTHLAKE_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The profile's configuration as HealthLake reports it, for drift detection: its own read of
/// the profile, by the id inside this resource's physical id, under the properties the type
/// declares those members as. A profile that service no longer holds was deleted, which is
/// what `None` reports.
pub(super) async fn describe_data_transformation_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        PROFILE_ID_MEMBER.to_string(),
        Value::String(profile_id_of(physical_resource_id)),
    );
    match call_healthlake(
        ctx,
        region,
        GET_DATA_TRANSFORMATION_PROFILE,
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => Ok(Some(reported_as_properties(
            &answered,
            &[
                ("ProfileName", PROFILE_NAME_MEMBER),
                ("ProfileDescription", PROFILE_DESCRIPTION_MEMBER),
                ("SourceFormat", SOURCE_FORMAT_MEMBER),
            ],
        ))),
        Err(refusal) if refusal.code == HEALTHLAKE_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Establish that HealthLake RESOLVES the profile ARN this arm composed: its own tag read of
/// a profile takes exactly that handle, so an answer means the ARN names a profile it holds
/// and `Err` is its own refusal of it — which, for a create, is what says the composed ARN
/// names nothing. The tags it reports are not read: what this call establishes is the
/// identity, and the set a profile carries is what the create's own `Tags` member sent.
async fn resolve_profile_arn(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_arn: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        RESOURCE_ARN_MEMBER.to_string(),
        Value::String(resource_arn.to_string()),
    );
    call_healthlake(ctx, region, LIST_TAGS_FOR_RESOURCE, &Value::Object(request)).await?;
    Ok(())
}

/// The ARN of the data transformation profile `profile_id`, in the form that service's own
/// `DataTransformationProfile` resource states (`aws.api#arn` template
/// `dataTransformationProfile/{ProfileId}`) and the modelled `AmazonResourceName` pattern
/// admits.
fn profile_arn(region: &str, profile_id: &str) -> String {
    ::emulator_server::emulator_arn(
        HEALTHLAKE_ARN_NAMESPACE,
        region,
        &format!("{HEALTHLAKE_PROFILE_ARN_PATH}/{profile_id}"),
    )
}

/// The `ProfileId` inside a profile ARN, which is the resource portion past that ARN's own
/// path. A value that is no such ARN is passed through as it stands, so that service's own
/// validation of the member is what reports it rather than a guess here.
fn profile_id_of(physical_resource_id: &str) -> String {
    physical_resource_id
        .rsplit_once(&format!("{HEALTHLAKE_PROFILE_ARN_PATH}/"))
        .map(|(_, profile_id)| profile_id.to_string())
        .unwrap_or_else(|| physical_resource_id.to_string())
}
