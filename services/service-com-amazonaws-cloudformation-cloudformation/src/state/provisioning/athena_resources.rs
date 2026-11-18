// SPDX-License-Identifier: CC0-1.0

//! `AWS::Athena::WorkGroup`, `AWS::Athena::DataCatalog` and `AWS::Athena::NamedQuery`: the
//! workgroup a stack declares queries run in, the data catalog they are routed through, and the
//! query a stack saves in that workgroup, created in Amazon Athena itself.
//!
//! This is the per-type half of provisioning for two resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::iot_authorizer`] for the reason that module records — the
//! catalogue file is far past the length a change to this repository may touch — and is reached
//! through the same seam ([`super::Owner`]).
//!
//! ## Why they are really created
//!
//! A workgroup and a data catalog are each NAMED in the owning service: `GetWorkGroup`,
//! `UpdateWorkGroup` and `DeleteWorkGroup` address one by its name, `StartQueryExecution` runs a
//! query IN one and honours the result location and the enforcement setting it holds, and
//! `GetDataCatalog` and every metadata read address a catalog by its name. So a stack that
//! reported either created while nothing created it publishes a name that resolves to nothing:
//! the very next read of the declared name is refused, and every query that depends on the
//! declared output location or on the declared catalog fails or writes nowhere. Creating each
//! resource where it belongs is what makes those answers the owning service's own — including
//! the refusal of a second declaration of a name that account already holds, which Athena is
//! the authority on.
//!
//! ## The properties
//!
//! `AWS::Athena::WorkGroup`'s properties are `CreateWorkGroup`'s own members under the same
//! names — `Description`, `Name` (the workgroup is created UNDER it, and it is this resource's
//! physical id and what its `Ref` resolves to) and `Tags` — plus three that are not members of
//! that call:
//!
//! * `WorkGroupConfiguration` IS the call's `Configuration` structure, member for member
//!   ([`CONFIGURATION_PROPERTIES`]), so the declared document travels as the template wrote it;
//! * `State` is applied by `UpdateWorkGroup`, the only call that takes a workgroup's state: a
//!   create leaves the workgroup [`WORK_GROUP_STATE_ENABLED`], so a declaration asking for
//!   another state is carried out by that call once the create has answered;
//! * `RecursiveDeleteOption` governs the DELETE — it is `DeleteWorkGroup`'s own member — so it
//!   is applied there rather than at creation.
//!
//! `AWS::Athena::DataCatalog`'s properties are `CreateDataCatalog`'s own members under the same
//! names: `Description`, `Name`, `Parameters`, `Tags` and the `Type` both the type and the call
//! declare required.
//!
//! `AWS::Athena::NamedQuery`'s properties are `CreateNamedQuery`'s own members under the same
//! names ([`NAMED_QUERY_PROPERTIES`]): `Database`, `Description`, `Name`, `QueryString` and the
//! `WorkGroup` the query is saved in. None of them is this resource's identity: Athena MINTS a
//! `NamedQueryId` for the saved query, which is what the create reports back as the resource's
//! physical id ([`create_named_query`]) and what every call on one takes — the `Name` a
//! declaration states is only what the query is called, and a workgroup may hold two queries
//! under one name. The type publishes no `Tags` property and `CreateNamedQuery` takes no tag
//! list, so CloudFormation propagates no tag to a saved query.
//!
//! A declared property this arm does not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]), at the top level and inside the workgroup configuration
//! alike: a resource reported created with less than its declaration asked for is the shallow
//! success this arm exists to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    applicable_tags, call_json_rpc, key_value_tag_list, Created, Readiness, Refusal, Removal,
    AWS_JSON_1_1,
};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The resource types this module provisions.
pub(super) const ATHENA_WORK_GROUP: &str = "AWS::Athena::WorkGroup";
/// See [`ATHENA_WORK_GROUP`].
pub(super) const ATHENA_DATA_CATALOG: &str = "AWS::Athena::DataCatalog";
/// See [`ATHENA_WORK_GROUP`]. Visible to the rest of this service's state because more than one
/// module answers about this type: the identity in force is the `NamedQueryId` Athena minted
/// ([`crate::state::template::identity_is_service_assigned`]), and the properties a saved query
/// cannot take anew live beside it
/// ([`crate::state::template::replacement_properties`]).
pub(in crate::state) const ATHENA_NAMED_QUERY: &str = "AWS::Athena::NamedQuery";

/// Amazon Athena's signing name, which is what routes the in-process call to its own handlers,
/// and the service shape an awsJson `X-Amz-Target` names an operation under (that model's
/// `sigv4Name: athena` and service shape `AmazonAthena`,
/// `smithy_query(query="service_info")`). It speaks `awsJson1_1`.
const ATHENA_SIGNING_NAME: &str = "athena";
/// See [`ATHENA_SIGNING_NAME`].
const ATHENA_TARGET_PREFIX: &str = "AmazonAthena";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the same
/// one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// The state a created workgroup is in, which is one of the two values Athena's `WorkGroupState`
/// enum admits (`smithy_query(query="shape",shape="WorkGroupState")`): `CreateWorkGroup` takes no
/// state member at all, so this is the state a declaration stating none asks for.
const WORK_GROUP_STATE_ENABLED: &str = "ENABLED";

/// Which of the two types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::Athena::WorkGroup`.
    WorkGroup,
    /// `AWS::Athena::DataCatalog`.
    DataCatalog,
    /// `AWS::Athena::NamedQuery`.
    NamedQuery,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        ATHENA_WORK_GROUP => Some(Kind::WorkGroup),
        ATHENA_DATA_CATALOG => Some(Kind::DataCatalog),
        ATHENA_NAMED_QUERY => Some(Kind::NamedQuery),
        _ => None,
    }
}

/// Every property `AWS::Athena::WorkGroup` publishes. A declared property this list does not
/// carry is one no call here applies, and is refused rather than dropped.
const WORK_GROUP_PROPERTIES: &[&str] = &[
    "Description",
    "Name",
    "RecursiveDeleteOption",
    "State",
    "Tags",
    "WorkGroupConfiguration",
];

/// Every member the type's `WorkGroupConfiguration` property publishes, which are the members of
/// `CreateWorkGroup`'s own `Configuration` structure under the same names.
///
/// Source: `WorkGroupConfiguration`'s member list
/// (`smithy_query(query="shape",shape="WorkGroupConfiguration")`), intersected with the members
/// the type's `WorkGroupConfiguration` property publishes.
const CONFIGURATION_PROPERTIES: &[&str] = &[
    "AdditionalConfiguration",
    "BytesScannedCutoffPerQuery",
    "CustomerContentEncryptionConfiguration",
    "EnforceWorkGroupConfiguration",
    "EngineVersion",
    "ExecutionRole",
    "PublishCloudWatchMetricsEnabled",
    "RequesterPaysEnabled",
    "ResultConfiguration",
];

/// Every member the configuration's `ResultConfiguration` publishes
/// (`smithy_query(query="shape",shape="ResultConfiguration")`).
const RESULT_CONFIGURATION_PROPERTIES: &[&str] = &[
    "AclConfiguration",
    "EncryptionConfiguration",
    "ExpectedBucketOwner",
    "OutputLocation",
];

/// The property carrying the workgroup's configuration, and the member of `CreateWorkGroup` that
/// takes it.
const WORK_GROUP_CONFIGURATION: &str = "WorkGroupConfiguration";
/// See [`WORK_GROUP_CONFIGURATION`].
const CONFIGURATION_MEMBER: &str = "Configuration";

/// The member of `UpdateWorkGroup` that carries a changed configuration, and the member of it
/// that carries a changed result configuration.
///
/// `WorkGroupConfigurationUpdates` is `WorkGroupConfiguration` member for member except for the
/// result configuration, which it takes as `ResultConfigurationUpdates`
/// (`smithy_query(query="shape",shape="WorkGroupConfigurationUpdates")`) — "the result
/// configuration information about the queries in this workgroup that will be updated" — so a
/// declared configuration reaches the update under those two names and travels as written
/// otherwise.
const CONFIGURATION_UPDATES_MEMBER: &str = "ConfigurationUpdates";
/// See [`CONFIGURATION_UPDATES_MEMBER`].
const RESULT_CONFIGURATION_UPDATES_MEMBER: &str = "ResultConfigurationUpdates";

/// The configuration members `WorkGroupConfigurationUpdates` publishes a `Remove…` flag for,
/// paired with that flag: the flag is what CLEARS the stored member, so a deployment that drops
/// one of these really takes it off the workgroup.
const CONFIGURATION_REMOVE_FLAGS: &[(&str, &str)] = &[
    (
        "BytesScannedCutoffPerQuery",
        "RemoveBytesScannedCutoffPerQuery",
    ),
    (
        "CustomerContentEncryptionConfiguration",
        "RemoveCustomerContentEncryptionConfiguration",
    ),
];

/// The same for the members of `ResultConfigurationUpdates`
/// (`smithy_query(query="shape",shape="ResultConfigurationUpdates")`), every one of which has a
/// flag of its own.
const RESULT_REMOVE_FLAGS: &[(&str, &str)] = &[
    ("AclConfiguration", "RemoveAclConfiguration"),
    ("EncryptionConfiguration", "RemoveEncryptionConfiguration"),
    ("ExpectedBucketOwner", "RemoveExpectedBucketOwner"),
    ("OutputLocation", "RemoveOutputLocation"),
];

/// The members of `CreateWorkGroup` a declaration states under the same names, beside the
/// configuration and the name the workgroup is created under.
const WORK_GROUP_CREATE_MEMBERS: &[&str] = &["Description"];

/// Every property `AWS::Athena::DataCatalog` publishes, which are `CreateDataCatalog`'s own
/// members under the same names
/// (`smithy_query(query="operation",operation="CreateDataCatalog")`).
const DATA_CATALOG_PROPERTIES: &[&str] =
    &["Description", "Name", "Parameters", "Tags", "Type"];

/// The members of `CreateDataCatalog` a declaration states beside the name and the required
/// type.
const DATA_CATALOG_CREATE_MEMBERS: &[&str] = &["Description", "Parameters"];

/// Every property `AWS::Athena::NamedQuery` publishes, which are `CreateNamedQuery`'s own
/// members under the same names
/// (`smithy_query(query="operation",operation="CreateNamedQuery")`, less the
/// `ClientRequestToken` that call mints an idempotency token under, which the TYPE does not
/// publish). A declared property this list does not carry is one no call here applies, and is
/// refused rather than dropped.
const NAMED_QUERY_PROPERTIES: &[&str] =
    &["Database", "Description", "Name", "QueryString", "WorkGroup"];

/// The members of `CreateNamedQuery` a declaration states under the same names, which for this
/// type is every property it publishes: the name is a member of the CALL rather than this
/// resource's identity, so none of them is held back for the identity the create answers with.
const NAMED_QUERY_CREATE_MEMBERS: &[&str] = NAMED_QUERY_PROPERTIES;

/// The member `CreateNamedQuery` answers the saved query's minted id under, and the member
/// `GetNamedQuery` and `DeleteNamedQuery` address one by.
const NAMED_QUERY_ID_MEMBER: &str = "NamedQueryId";

/// The property, and member, naming what kind of catalog it is — required by the type and by
/// `CreateDataCatalog` and `UpdateDataCatalog` alike, so a declaration stating none describes no
/// catalog that could be created.
const CATALOG_TYPE: &str = "Type";

/// The member every per-resource Athena call addresses a workgroup by.
const WORK_GROUP_MEMBER: &str = "WorkGroup";
/// The member every per-resource Athena call addresses a data catalog by.
const NAME_MEMBER: &str = "Name";

/// The property asking for a workgroup's queries, named queries and prepared statements to be
/// removed with it, which is `DeleteWorkGroup`'s own `RecursiveDeleteOption` member.
const RECURSIVE_DELETE_OPTION: &str = "RecursiveDeleteOption";

/// The property carrying the workgroup's state, which is `UpdateWorkGroup`'s own `State` member.
const STATE_PROPERTY: &str = "State";

/// The property carrying a resource's own tags, which every create here takes as its `Tags`
/// list.
const TAGS_PROPERTY: &str = "Tags";

/// The member `GetDataCatalog`'s answer carries the catalog under, and the member of that
/// structure reporting how far its creation or deletion has got
/// (`smithy_query(query="shape",shape="DataCatalog")`).
const DATA_CATALOG_MEMBER: &str = "DataCatalog";
/// See [`DATA_CATALOG_MEMBER`].
const STATUS_MEMBER: &str = "Status";

/// The status a created data catalog has reached when its creation is complete, and the marker
/// every status reporting a creation that could not be completed carries
/// (`smithy_query(query="shape",shape="DataCatalogStatus")`: `CREATE_COMPLETE` beside
/// `CREATE_FAILED` and the three `CREATE_FAILED_CLEANUP_…` it can move on to).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const FAILED_MARKER: &str = "FAILED";

/// The members these types declare that Athena models as NUMBERS: the one data-usage limit the
/// configuration carries (`BytesScannedCutoffPerQuery`, a `long`).
const NUMERIC_MEMBERS: &[&str] = &["BytesScannedCutoffPerQuery"];

/// The members they declare that Athena models as BOOLEANS: the three configuration switches and
/// the recursive-delete option, each a `BoxedBoolean`.
const BOOLEAN_MEMBERS: &[&str] = &[
    "EnforceWorkGroupConfiguration",
    "PublishCloudWatchMetricsEnabled",
    "RecursiveDeleteOption",
    "RequesterPaysEnabled",
];

/// Create the resource one of these types declares, in Amazon Athena itself — so a client that
/// deployed the stack can pass `!Ref <workgroup>` straight to `GetWorkGroup` or
/// `StartQueryExecution`, `!Ref <catalog>` to `GetDataCatalog` or a metadata read, and
/// `!GetAtt <query>.NamedQueryId` to `GetNamedQuery`.
///
/// `Err` is the owning service's own refusal of the create, which is what the resource fails
/// with: a name the account already holds, a catalog type it will not take, a parameter set a
/// catalog of that type does not admit, a workgroup no query can be saved in.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::WorkGroup => create_work_group(ctx, region, resource, propagated).await,
        Kind::DataCatalog => create_data_catalog(ctx, region, resource, propagated).await,
        // A saved query is not taggable: the type publishes no `Tags` property and
        // `CreateNamedQuery` takes no tag list, so there is no call that could carry one of
        // the tags CloudFormation propagates to it.
        Kind::NamedQuery => create_named_query(ctx, region, resource).await,
    }
}

/// Create the workgroup an `AWS::Athena::WorkGroup` declares, and put it in the state its
/// declaration asks for.
async fn create_work_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        WORK_GROUP_PROPERTIES,
    )?;
    refuse_unapplied_configuration(&resource.properties, &resource.resource_type)?;
    let name = resource.physical_resource_id.clone();
    let mut request = declared_members(&resource.properties, WORK_GROUP_CREATE_MEMBERS);
    request.insert(NAME_MEMBER.to_string(), Value::String(name.clone()));
    if let Some(configuration) = declared(&resource.properties, WORK_GROUP_CONFIGURATION) {
        request.insert(
            CONFIGURATION_MEMBER.to_string(),
            modelled(CONFIGURATION_MEMBER, configuration),
        );
    }
    insert_tags(&mut request, &resource.properties, propagated);
    call_athena(ctx, region, "CreateWorkGroup", &Value::Object(request)).await?;
    // `CreateWorkGroup` takes no state, so a declaration asking for one other than the state a
    // create leaves is carried out by the one call that takes a workgroup's state.
    if let Some(state) = declared_text(&resource.properties, STATE_PROPERTY)
        .filter(|state| state != WORK_GROUP_STATE_ENABLED)
    {
        let mut update = Map::new();
        update.insert(WORK_GROUP_MEMBER.to_string(), Value::String(name));
        update.insert(STATE_PROPERTY.to_string(), Value::String(state));
        call_athena(ctx, region, "UpdateWorkGroup", &Value::Object(update)).await?;
    }
    // The workgroup was created under the name CloudFormation decided, which is the one it
    // answers to, and the type publishes no `Fn::GetAtt` attribute this arm reports.
    Ok(Created::default())
}

/// Register the data catalog an `AWS::Athena::DataCatalog` declares.
async fn create_data_catalog(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        DATA_CATALOG_PROPERTIES,
    )?;
    let mut request = declared_members(&resource.properties, DATA_CATALOG_CREATE_MEMBERS);
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    request.insert(
        CATALOG_TYPE.to_string(),
        Value::String(required_catalog_type(resource, &resource.properties)?),
    );
    insert_tags(&mut request, &resource.properties, propagated);
    call_athena(ctx, region, "CreateDataCatalog", &Value::Object(request)).await?;
    Ok(Created::default())
}

/// Save the query an `AWS::Athena::NamedQuery` declares, and report the `NamedQueryId` Athena
/// minted for it as this resource's identity.
///
/// That id is what `Ref` resolves to and what the type's `NamedQueryId` attribute carries, and
/// no naming rule could compute it: Athena mints it, a workgroup may hold two queries under one
/// name, and `GetNamedQuery`, `UpdateNamedQuery` and `DeleteNamedQuery` each address a query by
/// that id and by nothing else. A create that answered no id would leave the stack publishing a
/// handle that resolves to nothing, so it is refused rather than reported as a resource created.
async fn create_named_query(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        NAMED_QUERY_PROPERTIES,
    )?;
    let request = declared_members(&resource.properties, NAMED_QUERY_CREATE_MEMBERS);
    let answered = call_athena(ctx, region, "CreateNamedQuery", &Value::Object(request)).await?;
    let named_query_id = answered
        .get(NAMED_QUERY_ID_MEMBER)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "Amazon Athena reported no {NAMED_QUERY_ID_MEMBER} for the named query it saved, \
                 which is what a reference to this resource resolves to"
            ),
        })?
        .to_string();
    Ok(Created {
        physical_resource_id: Some(named_query_id),
        // The `Id`/`NamedQueryId` attributes this type publishes are read off the identity
        // above, which the deployment re-points through the one place that step is written
        // ([`crate::state::template::adopt_identity`]), so none is reported a second time
        // here.
        attributes: BTreeMap::new(),
    })
}

/// Whether the owning service reports the created resource ready.
///
/// A WORKGROUP is nothing is asked about: it exists from the moment `CreateWorkGroup` answers —
/// `GetWorkGroup` resolves it and a query can be submitted to it — and the state it is in is the
/// one its declaration asked for rather than a transient state it settles out of. A NAMED QUERY
/// is in that same position: `CreateNamedQuery` answers with the id `GetNamedQuery` already
/// resolves, and the modelled `NamedQuery` shape publishes no status at all, so there is nothing
/// a read could wait for.
///
/// A DATA CATALOG publishes a `Status` of its own, and Athena creates one of them
/// asynchronously: "the `LAMBDA`, `GLUE`, and `HIVE` data catalog types are created
/// synchronously … the `FEDERATED` data catalog type is created asynchronously", which is why
/// this asks rather than assumes. A status reporting a creation that could not be completed is
/// a failure nothing is going to bring up, so the resource fails with it rather than waiting the
/// window out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    if kind != Kind::DataCatalog {
        return Ok(Readiness::Ready);
    }
    let mut request = Map::new();
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    let answered = call_athena(ctx, region, "GetDataCatalog", &Value::Object(request)).await?;
    let Some(status) = answered
        .get(DATA_CATALOG_MEMBER)
        .and_then(|catalog| catalog.get(STATUS_MEMBER))
        .and_then(Value::as_str)
    else {
        // A catalog type Athena creates synchronously reports no status at all, and the create
        // already answered for it.
        return Ok(Readiness::Ready);
    };
    Ok(if status == CREATE_COMPLETE {
        Readiness::Ready
    } else if status.contains(FAILED_MARKER) {
        Readiness::Failed(status.to_string())
    } else {
        Readiness::Pending(status.to_string())
    })
}

/// Whether the owning service has finished removing a deleted resource.
///
/// Nothing is asked: `DeleteWorkGroup`, `DeleteDataCatalog` and `DeleteNamedQuery` each take the
/// resource away as they answer — the catalog's delete answers with the record the registry no
/// longer holds — so Athena reports no state a removed one is still being deleted in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has, through the owning
/// service's own update — the call that takes the resource's NAME, so it keeps its identity and
/// nothing is replaced.
///
/// `Ok(None)` is a change neither call can make: a declaration that DROPS a value the resource
/// was created with, where the update publishes no member that clears it. Athena's updates take
/// what to SET; the only clearing members either of them has are the `Remove…` flags of a
/// workgroup's configuration ([`CONFIGURATION_REMOVE_FLAGS`], [`RESULT_REMOVE_FLAGS`]), which
/// are sent for exactly the members they govern. Reporting the rest as not applied is what keeps
/// a deployment from being called a success that left the old value in place.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared_properties: &Value,
) -> Result<Option<Created>, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource carries
    // the properties it was CREATED with, so a property the new declaration introduces is only
    // in `declared_properties`.
    match kind {
        Kind::WorkGroup => update_work_group(ctx, region, resource, declared_properties).await,
        Kind::DataCatalog => update_data_catalog(ctx, region, resource, declared_properties).await,
        // A saved query takes no changed declaration at all: every property of the type is
        // fixed when the query is saved, so a declaration stating one of them differently asks
        // for a DIFFERENT query and is carried out by REPLACING the resource
        // ([`crate::state::template::replacement_properties`] carries the four beside its
        // name). Reporting a change that reached here as not applied is what keeps such a
        // deployment from being called a success that left the old query in place.
        Kind::NamedQuery => Ok(None),
    }
}

/// Apply a changed declaration to the workgroup the stack already has.
async fn update_work_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared_properties: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(
        declared_properties,
        &resource.resource_type,
        WORK_GROUP_PROPERTIES,
    )?;
    refuse_unapplied_configuration(declared_properties, &resource.resource_type)?;
    // A description the declaration drops cannot be taken off the workgroup: `UpdateWorkGroup`
    // publishes no member that clears one.
    if dropped(&resource.properties, declared_properties, "Description") {
        return Ok(None);
    }
    let Some(configuration_updates) = configuration_updates(
        declared(&resource.properties, WORK_GROUP_CONFIGURATION),
        declared(declared_properties, WORK_GROUP_CONFIGURATION),
    ) else {
        return Ok(None);
    };
    let mut request = declared_members(declared_properties, WORK_GROUP_CREATE_MEMBERS);
    request.insert(
        WORK_GROUP_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    // A declaration stating no state asks for the state a create leaves, which is the one value
    // `UpdateWorkGroup` has to be told to go back to.
    request.insert(
        STATE_PROPERTY.to_string(),
        Value::String(
            declared_text(declared_properties, STATE_PROPERTY)
                .unwrap_or_else(|| WORK_GROUP_STATE_ENABLED.to_string()),
        ),
    );
    if !configuration_updates.is_empty() {
        request.insert(
            CONFIGURATION_UPDATES_MEMBER.to_string(),
            Value::Object(configuration_updates),
        );
    }
    call_athena(ctx, region, "UpdateWorkGroup", &Value::Object(request)).await?;
    Ok(Some(Created::default()))
}

/// Apply a changed declaration to the data catalog the stack already has.
async fn update_data_catalog(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared_properties: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(
        declared_properties,
        &resource.resource_type,
        DATA_CATALOG_PROPERTIES,
    )?;
    // `UpdateDataCatalog` publishes no member that clears a description or a parameter set, so a
    // declaration that drops either reports the change as not applied.
    for property in DATA_CATALOG_CREATE_MEMBERS {
        if dropped(&resource.properties, declared_properties, property) {
            return Ok(None);
        }
    }
    let mut request = declared_members(declared_properties, DATA_CATALOG_CREATE_MEMBERS);
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    request.insert(
        CATALOG_TYPE.to_string(),
        Value::String(required_catalog_type(resource, declared_properties)?),
    );
    call_athena(ctx, region, "UpdateDataCatalog", &Value::Object(request)).await?;
    Ok(Some(Created::default()))
}

/// Delete the resource a stack created, addressed by the identifier it is known by: the name a
/// workgroup and a data catalog were created under, and the id Athena minted for a saved query.
///
/// A workgroup's delete carries the `RecursiveDeleteOption` its declaration asked for, which is
/// what releases the named queries and prepared statements the workgroup holds with it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    let (operation, member) = match kind {
        Kind::WorkGroup => {
            if let Some(recursive) = declared(&resource.properties, RECURSIVE_DELETE_OPTION) {
                request.insert(
                    RECURSIVE_DELETE_OPTION.to_string(),
                    modelled(RECURSIVE_DELETE_OPTION, recursive),
                );
            }
            ("DeleteWorkGroup", WORK_GROUP_MEMBER)
        }
        Kind::DataCatalog => ("DeleteDataCatalog", NAME_MEMBER),
        // A saved query is addressed by the id Athena minted for it, which is this resource's
        // physical id ([`create_named_query`]).
        Kind::NamedQuery => ("DeleteNamedQuery", NAMED_QUERY_ID_MEMBER),
    };
    request.insert(
        member.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    call_athena(ctx, region, operation, &Value::Object(request))
        .await
        .map(|_| ())
}

/// The `ConfigurationUpdates` a changed declaration reaches `UpdateWorkGroup` with, or `None`
/// where it drops a configuration member the call publishes no way to clear.
///
/// `before` is the configuration the resource was created with and `now` the one the deployment
/// declares. Each member `now` states travels under the update's own name for it; each member
/// `before` stated that `now` does not is cleared through the flag that governs it, and reports
/// the change as not applied where there is no such flag.
fn configuration_updates(
    before: Option<&Value>,
    now: Option<&Value>,
) -> Option<Map<String, Value>> {
    let mut updates = Map::new();
    for property in CONFIGURATION_PROPERTIES {
        let stated = now.and_then(|configuration| declared(configuration, property));
        if *property == "ResultConfiguration" {
            let result = result_configuration_updates(
                before.and_then(|configuration| declared(configuration, property)),
                stated,
            )?;
            if !result.is_empty() {
                updates.insert(
                    RESULT_CONFIGURATION_UPDATES_MEMBER.to_string(),
                    Value::Object(result),
                );
            }
            continue;
        }
        match stated {
            Some(value) => {
                updates.insert((*property).to_string(), modelled(property, value));
            }
            None => {
                if before
                    .and_then(|configuration| declared(configuration, property))
                    .is_some()
                {
                    let flag = remove_flag(CONFIGURATION_REMOVE_FLAGS, property)?;
                    updates.insert(flag.to_string(), Value::Bool(true));
                }
            }
        }
    }
    Some(updates)
}

/// [`configuration_updates`] for the result configuration, whose every member has a `Remove…`
/// flag of its own.
fn result_configuration_updates(
    before: Option<&Value>,
    now: Option<&Value>,
) -> Option<Map<String, Value>> {
    let mut updates = Map::new();
    for property in RESULT_CONFIGURATION_PROPERTIES {
        match now.and_then(|result| declared(result, property)) {
            Some(value) => {
                updates.insert((*property).to_string(), modelled(property, value));
            }
            None => {
                if before
                    .and_then(|result| declared(result, property))
                    .is_some()
                {
                    let flag = remove_flag(RESULT_REMOVE_FLAGS, property)?;
                    updates.insert(flag.to_string(), Value::Bool(true));
                }
            }
        }
    }
    Some(updates)
}

/// The `Remove…` flag that clears `property`, or `None` where the update publishes none.
fn remove_flag(flags: &'static [(&'static str, &'static str)], property: &str) -> Option<&'static str> {
    flags
        .iter()
        .find(|(member, _)| *member == property)
        .map(|(_, flag)| *flag)
}

/// Whether a changed declaration DROPS a property the resource was created with.
fn dropped(before: &Value, now: &Value, property: &str) -> bool {
    declared(before, property).is_some() && declared(now, property).is_none()
}

/// The `Type` a data catalog's declaration states — required by the type and by the calls that
/// create and update one — or the refusal for a declaration stating none, which describes no
/// catalog that could be created.
fn required_catalog_type(
    resource: &StoredResource,
    properties: &Value,
) -> Result<String, Refusal> {
    declared_text(properties, CATALOG_TYPE).ok_or_else(|| Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!("{} declares no {CATALOG_TYPE}", resource.resource_type),
    })
}

/// Put the tags the resource carries on the request, as the `Tags` list Athena's own `Tag`
/// spells them with — less the keys in AWS's reserved namespace, which this service documents as
/// its own ("the `aws:` namespace holds the tags Amazon Web Services generates itself") and
/// refuses on every public call that takes a tag list.
fn insert_tags(
    request: &mut Map<String, Value>,
    properties: &Value,
    propagated: &[(String, String)],
) {
    let tags = applicable_tags(&tags_of(properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_PROPERTY.to_string(), key_value_tag_list(&tags));
    }
}

/// One declared property, or `None` where the declaration states none or states it null.
fn declared<'a>(properties: &'a Value, property: &str) -> Option<&'a Value> {
    properties.get(property).filter(|value| !value.is_null())
}

/// One declared property as the TEXT it carries, or `None` where the declaration states none or
/// states it empty — an empty value names nothing.
fn declared_text(properties: &Value, property: &str) -> Option<String> {
    declared(properties, property)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// The members of `members` a declaration states, as the request carries them: the type and the
/// API spell each one identically, so every value goes through as the template wrote it — read
/// as the TYPE the owning API models it as ([`modelled`]).
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    for member in members {
        if let Some(value) = declared(properties, member) {
            request.insert((*member).to_string(), modelled(member, value));
        }
    }
    request
}

/// One declared value as the TYPE Athena models that member as, through the one walk every
/// provisioning arm in this position shares ([`super::modelled_declaration`]): the members these
/// types declare as NUMBERS are [`NUMERIC_MEMBERS`] and the ones they declare as BOOLEANS are
/// [`BOOLEAN_MEMBERS`], and every other member travels as the template wrote it.
fn modelled(member: &str, value: &Value) -> Value {
    super::modelled_declaration(member, value, &|member| {
        if NUMERIC_MEMBERS.contains(&member) {
            super::ModelledScalar::Number
        } else if BOOLEAN_MEMBERS.contains(&member) {
            super::ModelledScalar::Boolean
        } else {
            super::ModelledScalar::AsWritten
        }
    })
}

/// The refusal for a declared property this module does not apply, naming which property it was:
/// refusing is what keeps a resource from being reported as created — or as updated — with less
/// than its declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

/// [`refuse_unapplied_properties`] one level in, for the members of a workgroup's declared
/// configuration and of the result configuration inside it: a configuration member no call here
/// carries would leave the workgroup configured with less than the declaration asked for, which
/// is the setting a query in it then does not honour.
fn refuse_unapplied_configuration(
    properties: &Value,
    resource_type: &str,
) -> Result<(), Refusal> {
    let Some(configuration) = declared(properties, WORK_GROUP_CONFIGURATION) else {
        return Ok(());
    };
    refuse_unapplied_properties(configuration, resource_type, CONFIGURATION_PROPERTIES)?;
    if let Some(result) = declared(configuration, "ResultConfiguration") {
        refuse_unapplied_properties(result, resource_type, RESULT_CONFIGURATION_PROPERTIES)?;
    }
    Ok(())
}

/// Call one Amazon Athena operation on the awsJson1_1 wire its model declares.
async fn call_athena(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        ATHENA_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ATHENA_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A resource record of one of these types, as the naming rules leave one.
    fn record(resource_type: &str, name: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Declared".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: name.to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// RULE: every member these arms send is a published property of the type they send it for,
    /// so a declaration is carried out in the owning service's own vocabulary and nothing is
    /// invented for it.
    #[test]
    fn every_sent_member_is_a_published_property() {
        let rows: &[(&str, &[&str], &[&str])] = &[
            (
                ATHENA_WORK_GROUP,
                WORK_GROUP_CREATE_MEMBERS,
                WORK_GROUP_PROPERTIES,
            ),
            (
                ATHENA_DATA_CATALOG,
                DATA_CATALOG_CREATE_MEMBERS,
                DATA_CATALOG_PROPERTIES,
            ),
            (
                ATHENA_NAMED_QUERY,
                NAMED_QUERY_CREATE_MEMBERS,
                NAMED_QUERY_PROPERTIES,
            ),
        ];
        for (resource_type, sent, published) in rows {
            for member in *sent {
                assert!(
                    published.contains(member),
                    "case {resource_type:?}: expected the sent member {member:?} to be a \
                     published property, got {published:?}"
                );
            }
        }
        for (member, _) in CONFIGURATION_REMOVE_FLAGS {
            assert!(
                CONFIGURATION_PROPERTIES.contains(member),
                "case \"a configuration remove flag\": expected {member:?} to be a published \
                 configuration member, got {CONFIGURATION_PROPERTIES:?}"
            );
        }
        for (member, _) in RESULT_REMOVE_FLAGS {
            assert!(
                RESULT_CONFIGURATION_PROPERTIES.contains(member),
                "case \"a result-configuration remove flag\": expected {member:?} to be a \
                 published member, got {RESULT_CONFIGURATION_PROPERTIES:?}"
            );
        }
    }

    /// The three types this module owns are the three it answers for, and nothing else is.
    #[test]
    fn the_kind_is_resolved_from_the_type_name() {
        let rows: &[(&str, Option<Kind>)] = &[
            (ATHENA_WORK_GROUP, Some(Kind::WorkGroup)),
            (ATHENA_DATA_CATALOG, Some(Kind::DataCatalog)),
            (ATHENA_NAMED_QUERY, Some(Kind::NamedQuery)),
            ("AWS::Athena::NamedQuerySomethingElse", None),
            ("AWS::Athena::WorkGroupSomethingElse", None),
        ];
        for (resource_type, expected) in rows {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, *expected,
                "case {resource_type:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The declared properties reach `CreateWorkGroup` under that call's own member names, with
    /// a quoted flag carried as the boolean Athena models and a quoted limit as the number — and
    /// the configuration travels as the `Configuration` structure, member for member.
    #[test]
    fn the_workgroup_declaration_is_carried_under_the_api_member_names() {
        let declared = serde_json::json!({
            "Name": "declared-name",
            "Description": "the analytics workgroup",
            "State": "DISABLED",
            "RecursiveDeleteOption": true,
            "WorkGroupConfiguration": {
                "EnforceWorkGroupConfiguration": "true",
                "PublishCloudWatchMetricsEnabled": false,
                "BytesScannedCutoffPerQuery": "10000000",
                "ResultConfiguration": {"OutputLocation": "s3://results/out/"},
            },
        });
        let mut request = declared_members(&declared, WORK_GROUP_CREATE_MEMBERS);
        request.insert(NAME_MEMBER.to_string(), Value::String("wg-1".to_string()));
        request.insert(
            CONFIGURATION_MEMBER.to_string(),
            modelled(
                CONFIGURATION_MEMBER,
                declared.get(WORK_GROUP_CONFIGURATION).expect("declared"),
            ),
        );
        let actual = Value::Object(request);
        let expected = serde_json::json!({
            "Name": "wg-1",
            "Description": "the analytics workgroup",
            "Configuration": {
                "EnforceWorkGroupConfiguration": true,
                "PublishCloudWatchMetricsEnabled": false,
                "BytesScannedCutoffPerQuery": 10_000_000,
                "ResultConfiguration": {"OutputLocation": "s3://results/out/"},
            },
        });
        assert_eq!(
            actual, expected,
            "case \"a full workgroup declaration\": expected {expected}, got {actual}"
        );
    }

    /// A changed configuration reaches `UpdateWorkGroup` as `ConfigurationUpdates`, with the
    /// result configuration under that call's own `ResultConfigurationUpdates` name and a
    /// `Remove…` flag for each member the declaration dropped that publishes one.
    #[test]
    fn a_changed_configuration_is_carried_as_the_updates_the_call_takes() {
        let before = serde_json::json!({
            "EnforceWorkGroupConfiguration": true,
            "BytesScannedCutoffPerQuery": 20_000_000,
            "ResultConfiguration": {
                "OutputLocation": "s3://results/old/",
                "ExpectedBucketOwner": "000000000000",
            },
        });
        let now = serde_json::json!({
            "EnforceWorkGroupConfiguration": false,
            "ResultConfiguration": {"OutputLocation": "s3://results/new/"},
        });
        let actual = Value::Object(
            configuration_updates(Some(&before), Some(&now))
                .expect("every dropped member publishes a remove flag"),
        );
        let expected = serde_json::json!({
            "EnforceWorkGroupConfiguration": false,
            "RemoveBytesScannedCutoffPerQuery": true,
            "ResultConfigurationUpdates": {
                "OutputLocation": "s3://results/new/",
                "RemoveExpectedBucketOwner": true,
            },
        });
        assert_eq!(
            actual, expected,
            "case \"a configuration with a dropped limit and a dropped bucket owner\": expected \
             {expected}, got {actual}"
        );
    }

    /// A dropped configuration member the update publishes no clearing member for is a change
    /// with no in-place path, which is reported rather than applied as a success that left the
    /// old value in force.
    #[test]
    fn a_dropped_member_with_no_remove_flag_has_no_in_place_path() {
        let before = serde_json::json!({"EnforceWorkGroupConfiguration": true});
        let now = serde_json::json!({});
        assert!(
            configuration_updates(Some(&before), Some(&now)).is_none(),
            "case \"a dropped enforcement switch\": expected no in-place update, got one"
        );
    }

    /// A property no call here applies is refused, naming it, at the top level and inside the
    /// declared configuration alike; every property the type publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let full = serde_json::json!({
            "Name": "wg",
            "Description": "d",
            "State": "ENABLED",
            "RecursiveDeleteOption": false,
            "Tags": [],
            "WorkGroupConfiguration": {
                "AdditionalConfiguration": "{}",
                "BytesScannedCutoffPerQuery": 10_000_000,
                "CustomerContentEncryptionConfiguration": {},
                "EnforceWorkGroupConfiguration": true,
                "EngineVersion": {"SelectedEngineVersion": "AUTO"},
                "ExecutionRole": "arn:aws:iam::000000000000:role/spark",
                "PublishCloudWatchMetricsEnabled": true,
                "RequesterPaysEnabled": false,
                "ResultConfiguration": {
                    "AclConfiguration": {},
                    "EncryptionConfiguration": {},
                    "ExpectedBucketOwner": "000000000000",
                    "OutputLocation": "s3://results/out/",
                },
            },
        });
        assert!(
            refuse_unapplied_properties(&full, ATHENA_WORK_GROUP, WORK_GROUP_PROPERTIES).is_ok()
                && refuse_unapplied_configuration(&full, ATHENA_WORK_GROUP).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        let rows: &[(&str, Value, &str)] = &[
            (
                "a property no call carries",
                serde_json::json!({"Name": "wg", "Unheard": 1}),
                "Unheard",
            ),
            (
                "a configuration member no call carries",
                serde_json::json!({"WorkGroupConfiguration": {"Unheard": 1}}),
                "Unheard",
            ),
            (
                "a result-configuration member no call carries",
                serde_json::json!({
                    "WorkGroupConfiguration": {"ResultConfiguration": {"Unheard": 1}}
                }),
                "Unheard",
            ),
        ];
        for (case, properties, named) in rows {
            let refusal = refuse_unapplied_properties(
                properties,
                ATHENA_WORK_GROUP,
                WORK_GROUP_PROPERTIES,
            )
            .and_then(|()| refuse_unapplied_configuration(properties, ATHENA_WORK_GROUP))
            .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {:?}",
                refusal.message
            );
        }
    }

    /// A data catalog declaring no `Type` describes no catalog that could be created, and is
    /// refused here naming that property rather than sent as a request the owning service would
    /// reject.
    #[test]
    fn a_catalog_declaring_no_type_is_refused() {
        let silent = record(ATHENA_DATA_CATALOG, "catalog", serde_json::json!({}));
        let refusal = required_catalog_type(&silent, &silent.properties)
            .expect_err("case \"a catalog declaring no Type\": expected a refusal");
        assert!(
            refusal.message.contains(CATALOG_TYPE),
            "case \"a catalog declaring no Type\": expected the refusal to name it, got {:?}",
            refusal.message
        );
        let stated = record(
            ATHENA_DATA_CATALOG,
            "catalog",
            serde_json::json!({"Type": "GLUE"}),
        );
        assert_eq!(
            required_catalog_type(&stated, &stated.properties).ok(),
            Some("GLUE".to_string()),
            "case \"a catalog declaring GLUE\": expected that type to be carried"
        );
    }

    /// The tag list carries the declaration's own tags and the ones CloudFormation propagates,
    /// and never a key in the reserved namespace, which no public Athena call writes.
    #[test]
    fn the_tag_list_leaves_the_reserved_namespace_alone() {
        let declared = serde_json::json!({"Tags": [{"Key": "Owner", "Value": "analytics"}]});
        let propagated = vec![
            ("Stack".to_string(), "athena-suite".to_string()),
            (
                "aws:cloudformation:logical-id".to_string(),
                "WorkGroup".to_string(),
            ),
        ];
        let mut request = Map::new();
        insert_tags(&mut request, &declared, &propagated);
        let expected = serde_json::json!([
            {"Key": "Owner", "Value": "analytics"},
            {"Key": "Stack", "Value": "athena-suite"},
        ]);
        assert_eq!(
            request.get(TAGS_PROPERTY),
            Some(&expected),
            "case \"a declaration beside the propagated tags\": expected {expected}, got \
             {request:?}"
        );
    }

    /// A named query's declaration reaches `CreateNamedQuery` under that call's own member
    /// names, every published property included and nothing else added: the id the query is
    /// known by is the one Athena answers with, so the request carries no identifier at all.
    /// A property the type does not publish is refused rather than dropped.
    #[test]
    fn the_named_query_declaration_is_carried_under_the_api_member_names() {
        let rows: &[(&str, Value, Value)] = &[
            (
                "every published property",
                serde_json::json!({
                    "Name": "answer",
                    "Database": "default",
                    "Description": "the one row",
                    "QueryString": "SELECT 1 AS answer",
                    "WorkGroup": "analytics",
                }),
                serde_json::json!({
                    "Name": "answer",
                    "Database": "default",
                    "Description": "the one row",
                    "QueryString": "SELECT 1 AS answer",
                    "WorkGroup": "analytics",
                }),
            ),
            (
                "only the properties a minimal declaration states",
                serde_json::json!({"Name": "answer", "Database": "db", "QueryString": "SELECT 2"}),
                serde_json::json!({"Name": "answer", "Database": "db", "QueryString": "SELECT 2"}),
            ),
        ];
        for (case, declared, expected) in rows {
            let actual = Value::Object(declared_members(declared, NAMED_QUERY_CREATE_MEMBERS));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
        let unapplied = serde_json::json!({"Name": "answer", "Retention": "forever"});
        let refusal =
            refuse_unapplied_properties(&unapplied, ATHENA_NAMED_QUERY, NAMED_QUERY_PROPERTIES)
                .expect_err("case \"a property the type does not publish\": expected a refusal");
        assert!(
            refusal.message.contains("Retention"),
            "case \"a property the type does not publish\": expected the refusal to name it, \
             got {:?}",
            refusal.message
        );
    }
}
