// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Directory Service's own control plane: the directory an
//! `AWS::DirectoryService::MicrosoftAD` or an `AWS::DirectoryService::SimpleAD` declares. One
//! file of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! Both types declare the SAME resource — a directory of that service, placed in the caller's
//! VPC — and differ only in which create makes it and in the one property that sizes it:
//! `CreateMicrosoftAD` takes an `Edition` and `CreateDirectory` a `Size`
//! ([`DirectoryKind`]). Every other property of either type is a member of that create under
//! the same name (`Name`, `ShortName`, `Password`, `Description`, `VpcSettings` with its own
//! `VpcId` and `SubnetIds`), so the evaluated property document IS the request once the two
//! properties CloudFormation applies itself are lifted out of it — the alias the declaration
//! asks for and the single sign-on flag, each a call of its own below.
//!
//! The identity is that service's: `CreateMicrosoftAD`/`CreateDirectory` take no identifier
//! and answer the `DirectoryId` (`^d-[0-9a-f]{10}$`) they minted, which is the handle every
//! operation on a directory takes and what either type's `Ref` returns — so it replaces the
//! stand-in the naming rules computed
//! ([`super::super::template::identity_is_service_assigned`] names both types).
//!
//! A created directory is `Requested` and then `Creating` — its domain controllers are still
//! being built, and it serves no directory traffic in either stage — so the resource's
//! creation is held open until that service reports it `Active`
//! ([`ds_directory_readiness`]), which is also when the two calls CloudFormation makes for
//! the declaration's own flags are made: an alias can only be assigned to a directory that is
//! serving, and single sign-on needs the access URL an alias gives it. Its teardown is
//! asynchronous too: `DeleteDirectory` leaves the directory `Deleting` and the resource's
//! deletion is finished once that service no longer holds it ([`ds_directory_removal`]).

use super::*;

/// Directory Service's signing name and `X-Amz-Target` prefix, from its model (`sigv4Name:
/// ds`, service shape `DirectoryService_20150416`, protocol `awsJson1_1`) — the wire these
/// calls are written on, as that service's own suites drive it.
const DS_SIGNING_NAME: &str = "ds";
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// The operations this module calls, as Directory Service's own model names them.
const CREATE_MICROSOFT_AD: &str = "CreateMicrosoftAD";
const CREATE_DIRECTORY: &str = "CreateDirectory";
const DESCRIBE_DIRECTORIES: &str = "DescribeDirectories";
const DELETE_DIRECTORY: &str = "DeleteDirectory";
const CREATE_ALIAS: &str = "CreateAlias";
const ENABLE_SSO: &str = "EnableSso";
const DISABLE_SSO: &str = "DisableSso";

/// The error code Directory Service answers for a directory it does not hold
/// (`EntityDoesNotExistException`, the modelled refusal of its delete and of a read narrowed
/// to an id the account never had).
const DS_ENTITY_DOES_NOT_EXIST: &str = "EntityDoesNotExistException";

/// The members a `DescribeDirectories` request narrows to one directory with, and the member
/// its answer carries the descriptions in.
const DIRECTORY_IDS_MEMBER: &str = "DirectoryIds";
const DIRECTORY_DESCRIPTIONS_MEMBER: &str = "DirectoryDescriptions";

/// The members of a `DirectoryDescription` this module reads: the id that service minted, the
/// stage the directory is in, the alias it answers its access URL under, the addresses its
/// DNS servers hold, and whether single sign-on is on.
const DIRECTORY_ID_MEMBER: &str = "DirectoryId";
const STAGE_MEMBER: &str = "Stage";
const ALIAS_MEMBER: &str = "Alias";
const DNS_IP_ADDRS_MEMBER: &str = "DnsIpAddrs";
const SSO_ENABLED_MEMBER: &str = "SsoEnabled";

/// The member `CreateAlias` takes the alias text as.
const ALIAS_REQUEST_MEMBER: &str = "Alias";

/// The `DirectoryStage` a directory SERVES in: "Active" — the stage its domain controllers
/// answer in, and the one every stage-gated operation of that service requires.
const DIRECTORY_STAGE_ACTIVE: &str = "Active";

/// The `DirectoryStage` values a directory being BUILT walks, from the modelled
/// `DirectoryStage` enum (`smithy_query(query="shape",shape="DirectoryStage")`): the stage a
/// create records (`Requested`), the one its controllers come up in (`Creating`) and the one
/// that service reports a built directory in before it serves (`Created`). In none of them
/// does the directory serve the traffic the declaration asked for, so each is a stage to wait
/// out rather than one to report the resource ready — or failed — in. The stages of a
/// TEARDOWN are not here: a directory being removed is what [`ds_directory_removal`] waits
/// on.
const DIRECTORY_STAGES_COMING_UP: &[&str] = &["Requested", "Creating", "Created"];

/// The attributes either type publishes: the directory's alias — "the alias for the
/// directory. If no alias exists, the alias is the directory identifier" — and the addresses
/// of its DNS servers, which is a LIST and so is carried one element per indexed key
/// ([`list_attribute_element`], the one carrier a string-valued attribute map has for one).
const ALIAS_ATTRIBUTE: &str = "Alias";
const DNS_IP_ADDRESSES_ATTRIBUTE: &str = "DnsIpAddresses";

/// The attribute a Simple AD publishes BESIDE those two: the identifier that service minted,
/// which is also what either type's `Ref` returns. A Managed Microsoft AD publishes no such
/// attribute, so one is given to a directory of that type nowhere.
const DIRECTORY_ID_ATTRIBUTE: &str = "DirectoryId";

/// The property that asks CloudFormation to assign the directory an ALIAS of its own, and the
/// one that asks it to turn single sign-on on. Neither is a member of either create: each is a
/// call of that service's own, made once the directory is serving ([`apply_declared_flags`]).
const CREATE_ALIAS_PROPERTY: &str = "CreateAlias";
const ENABLE_SSO_PROPERTY: &str = "EnableSso";

/// What the alias this provisioner asks for is built from: the prefix, and the identifier
/// Directory Service minted for the directory with its own `d-` prefix taken off.
///
/// An alias "cannot be deleted or reused", so it has to be unique in the account, and the one
/// value unique to the directory is the id that service minted for it. The modelled
/// `AliasName` admits a value of this form and no value starting with `d-` or `D-`
/// (`^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*$`, length 1..62), which is why the id is
/// prefixed rather than sent as it stands.
const ALIAS_PREFIX: &str = "alias-";
/// The prefix a `DirectoryId` carries (`^d-[0-9a-f]{10}$`), taken off the id to build the
/// alias above.
const DIRECTORY_ID_PREFIX: &str = "d-";

/// Which of the two directory types an arm is working on, so the one set of create, readiness,
/// delete and update arms serves both rather than two copies of each. Which it is, is decided
/// once by [`super::provisioner_for`] and travels with the `Provisioner` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectoryKind {
    /// `AWS::DirectoryService::MicrosoftAD`: the AWS Managed Microsoft AD directory
    /// `CreateMicrosoftAD` makes, sized by the `Edition` the declaration states.
    MicrosoftAd,
    /// `AWS::DirectoryService::SimpleAD`: the Simple AD directory `CreateDirectory` makes,
    /// sized by the `Size` the declaration states.
    SimpleAd,
}

impl DirectoryKind {
    /// The operation that makes a directory of this kind.
    fn create_operation(self) -> &'static str {
        match self {
            Self::MicrosoftAd => CREATE_MICROSOFT_AD,
            Self::SimpleAd => CREATE_DIRECTORY,
        }
    }

    /// The members that create takes, each the declared property of the same name: the
    /// directory's fully qualified name, its NetBIOS short name, the administrator password
    /// and the placement both kinds share, plus the one property that sizes this kind.
    fn create_members(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::MicrosoftAd => MICROSOFT_AD_MEMBERS,
            Self::SimpleAd => SIMPLE_AD_MEMBERS,
        }
    }

    /// Every property the type publishes, which is what a declaration may state: a property
    /// no arm here maps onto that create's request or onto one of the two calls below is
    /// refused rather than dropped silently.
    fn properties(self) -> &'static [&'static str] {
        match self {
            Self::MicrosoftAd => MICROSOFT_AD_PROPERTIES,
            Self::SimpleAd => SIMPLE_AD_PROPERTIES,
        }
    }
}

/// The members `CreateMicrosoftAD` takes the properties of `AWS::DirectoryService::MicrosoftAD`
/// as: that model spells each of them exactly as the type's property is spelled, `VpcSettings`
/// and its own two members included (`DirectoryVpcSettings$VpcId`,
/// `DirectoryVpcSettings$SubnetIds`), so no member is renamed on the way.
const MICROSOFT_AD_MEMBERS: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("ShortName", "ShortName"),
    ("Password", "Password"),
    ("VpcSettings", "VpcSettings"),
    ("Edition", "Edition"),
];

/// The same for `CreateDirectory` and `AWS::DirectoryService::SimpleAD`, whose size member is
/// `Size` and which publishes a `Description` beside it.
const SIMPLE_AD_MEMBERS: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("ShortName", "ShortName"),
    ("Password", "Password"),
    ("Description", "Description"),
    ("VpcSettings", "VpcSettings"),
    ("Size", "Size"),
];

/// Every property `AWS::DirectoryService::MicrosoftAD` publishes.
const MICROSOFT_AD_PROPERTIES: &[&str] = &[
    CREATE_ALIAS_PROPERTY,
    "Edition",
    ENABLE_SSO_PROPERTY,
    "Name",
    "Password",
    "ShortName",
    "VpcSettings",
];

/// Every property `AWS::DirectoryService::SimpleAD` publishes.
const SIMPLE_AD_PROPERTIES: &[&str] = &[
    CREATE_ALIAS_PROPERTY,
    "Description",
    ENABLE_SSO_PROPERTY,
    "Name",
    "Password",
    "ShortName",
    "Size",
    "VpcSettings",
];

/// Create the directory an `AWS::DirectoryService::MicrosoftAD` or an
/// `AWS::DirectoryService::SimpleAD` declares, in Directory Service — so a client that
/// deployed the stack finds the directory in that service's own `DescribeDirectories` under
/// the id the stack published, reaches it with every operation that takes a directory id, and
/// joins its instances to a domain that really exists rather than to an id nothing created.
///
/// The declaration IS the request ([`DirectoryKind::create_members`]), with the two properties
/// CloudFormation applies through calls of their own left out of it
/// ([`apply_declared_flags`]). Neither type publishes a `Tags` property, so nothing is
/// propagated to the directory: a tag set the declaration cannot state is one the resource
/// does not carry.
///
/// The identity is that service's: the `DirectoryId` it minted becomes this resource's
/// physical id — what either type's `Ref` returns and the handle every call on a directory
/// takes — and the type's published return values are read off that service's own description
/// of the directory ([`reported_directory_attributes`]) rather than recomputed here.
pub(super) async fn create_ds_directory(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: DirectoryKind,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, kind.properties())?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, kind.create_members(), &mut request);
    let answered = call_ds(
        ctx,
        region,
        kind.create_operation(),
        &Value::Object(request),
    )
    .await?;
    let directory_id = reported_text(&answered, DIRECTORY_ID_MEMBER).unwrap_or_default();
    if directory_id.is_empty() {
        // There would be no handle a client could call Directory Service with, and nothing
        // for the stack to publish, so this is a failed create rather than a created
        // resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Directory Service control plane answered a created directory \
                      without a directory id"
                .to_string(),
        });
    }
    // Every published value is read off that service's own description of the directory it
    // has just minted — one reading for the create and for the readiness read below, so what
    // a stack publishes cannot depend on which call last looked at it.
    let described = described_directory(ctx, region, &directory_id).await?;
    let mut attributes = reported_directory_attributes(kind, described.as_ref());
    attributes
        .entry(ALIAS_ATTRIBUTE.to_string())
        .or_insert_with(|| directory_id.clone());
    if kind == DirectoryKind::SimpleAd {
        attributes.insert(DIRECTORY_ID_ATTRIBUTE.to_string(), directory_id.clone());
    }
    Ok(Created {
        physical_resource_id: Some(directory_id),
        attributes,
    })
}

/// Whether Directory Service has finished building the directory a stack created, and the two
/// flags its declaration carries APPLIED.
///
/// A created directory is `Requested` and then `Creating` — its domain controllers are still
/// coming up and it serves no directory traffic — so [`DIRECTORY_STAGE_ACTIVE`] is the stage
/// the build is finished in, [`DIRECTORY_STAGES_COMING_UP`] are the stages it is still running
/// in, and every other stage is one no wait brings a new directory out of: one whose build
/// failed, one that is impaired or inoperable, one being torn down out of this stack's reach.
/// A directory that service no longer holds is in the same position, nothing being left to
/// create it.
///
/// A directory that IS serving then takes the declaration's own alias and single sign-on
/// flags ([`apply_declared_flags`]): neither create carries either, and that service takes
/// both only once the directory is active, so they are applied here rather than dropped — and
/// one it REFUSES is the resource's own failure in that service's words.
pub(super) async fn ds_directory_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some(described) = described_directory(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Directory Service holds no directory {physical_resource_id}"
        )));
    };
    let stage = reported_text(&described, STAGE_MEMBER).unwrap_or_default();
    if stage == DIRECTORY_STAGE_ACTIVE {
        if let Some(refusal) =
            apply_declared_flags(ctx, region, physical_resource_id, &described, declared).await?
        {
            return Ok(Readiness::Refused(refusal));
        }
        return Ok(Readiness::Ready);
    }
    if DIRECTORY_STAGES_COMING_UP.contains(&stage.as_str()) {
        return Ok(Readiness::Pending(stage));
    }
    Ok(Readiness::Failed(format!(
        "Directory Service reports the directory {physical_resource_id} in stage {stage}"
    )))
}

/// The attributes Directory Service publishes for a directory it now reports serving: its own
/// description of the directory, read the one way [`reported_directory_attributes`] reads it.
///
/// A directory's alias is the id itself until an alias is assigned, and its DNS servers hold
/// no address until its controllers are up, so both are read again once that service reports
/// the directory active rather than left as the create's answer carried them.
pub(super) async fn ds_directory_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: DirectoryKind,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(reported_directory_attributes(
        kind,
        described_directory(ctx, region, physical_resource_id)
            .await?
            .as_ref(),
    ))
}

/// Whether Directory Service has finished removing the directory a stack deleted.
///
/// `DeleteDirectory` does not take the directory away while the caller waits: the modelled
/// `DirectoryStage` carries `Deleting` and `Deleted` for that window and
/// `DescribeDirectories` keeps describing the directory in them, so the deletion is finished
/// once that service no longer holds it.
pub(super) async fn ds_directory_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match described_directory(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(described) => Ok(Removal::Pending(
            reported_text(&described, STAGE_MEMBER).unwrap_or_default(),
        )),
    }
}

/// Delete the directory a stack created, by the id it was created under — this resource's
/// physical id. One the service no longer holds (`EntityDoesNotExistException`, judged on the
/// CODE and never on a message that carries the request's own values) is already in the state
/// the delete asks for.
pub(super) async fn delete_ds_directory(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        DIRECTORY_ID_MEMBER.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    match call_ds(ctx, region, DELETE_DIRECTORY, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DS_ENTITY_DOES_NOT_EXIST => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Apply a CHANGED declaration to the directory the stack created: the single sign-on flag,
/// which is the one property of either type a call of that service changes in place
/// (`EnableSso` turns it on, `DisableSso` takes it off). Every other property is fixed when a
/// directory is made — that service publishes no call that renames, re-sizes, re-places or
/// re-aliases one — which is why a declaration restating any of them REPLACES the resource
/// ([`super::super::template::replacement_properties`]) instead of reaching here.
///
/// `Ok(None)` is a change this call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it. There is none for this type: the one property
/// that reaches here is the flag.
pub(super) async fn update_ds_directory(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: DirectoryKind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let wanted = declared_flag(Some(declared), ENABLE_SSO_PROPERTY);
    let operation = if wanted { ENABLE_SSO } else { DISABLE_SSO };
    let mut request = Map::new();
    request.insert(
        DIRECTORY_ID_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    call_ds(ctx, region, operation, &Value::Object(request)).await?;
    // Neither call answers a document, so what the resource now reports is read off that
    // service's own description of the directory.
    Ok(Some(Created::attributes(ds_directory_ready_attributes(
        ctx,
        region,
        kind,
        &resource.physical_resource_id,
    )
    .await?)))
}

/// The two flags the declaration carries, applied to a directory that is now SERVING:
/// `Some(refusal)` is the one Directory Service answered, which is the resource's failure
/// rather than this provisioner's.
///
/// Each is applied only where the directory is not already in the state asked for, so the pass
/// that releases the wait a create opened can ask as often as it likes and neither call is
/// made twice: an alias "cannot be deleted or reused", so a directory that already carries one
/// is left alone (its reported `Alias` is the directory id itself until one is assigned), and a
/// directory whose `SsoEnabled` is already true asks for nothing.
///
/// The ORDER is the one single sign-on needs: it is reached through the directory's access
/// URL, which exists only once an alias has been assigned, so the alias is asked for first.
async fn apply_declared_flags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    directory_id: &str,
    described: &Value,
    declared: Option<&Value>,
) -> Result<Option<Refusal>, Refusal> {
    if declared_flag(declared, CREATE_ALIAS_PROPERTY)
        && reported_text(described, ALIAS_MEMBER).as_deref() == Some(directory_id)
    {
        let mut request = Map::new();
        request.insert(
            DIRECTORY_ID_MEMBER.to_string(),
            Value::String(directory_id.to_string()),
        );
        request.insert(
            ALIAS_REQUEST_MEMBER.to_string(),
            Value::String(alias_for(directory_id)),
        );
        if let Err(refusal) = call_ds(ctx, region, CREATE_ALIAS, &Value::Object(request)).await {
            return Ok(Some(refusal));
        }
    }
    if declared_flag(declared, ENABLE_SSO_PROPERTY)
        && described.get(SSO_ENABLED_MEMBER) != Some(&Value::Bool(true))
    {
        let mut request = Map::new();
        request.insert(
            DIRECTORY_ID_MEMBER.to_string(),
            Value::String(directory_id.to_string()),
        );
        if let Err(refusal) = call_ds(ctx, region, ENABLE_SSO, &Value::Object(request)).await {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// The alias CloudFormation asks Directory Service to assign the directory, where the
/// declaration asks for one: [`ALIAS_PREFIX`] and the identifier that service minted, its own
/// `d-` prefix taken off ([`DIRECTORY_ID_PREFIX`]).
///
/// Derived from that identifier because an alias is unique in the account and can never be
/// reused, and the id is the one value unique to this directory — so the same declaration
/// deployed twice asks for two different aliases rather than colliding on one.
fn alias_for(directory_id: &str) -> String {
    format!(
        "{ALIAS_PREFIX}{}",
        directory_id
            .strip_prefix(DIRECTORY_ID_PREFIX)
            .unwrap_or(directory_id)
    )
}

/// Whether the declaration states `property` as TRUE. Both flags default to off — "if you
/// don't specify a value, CloudFormation" leaves the alias unassigned and single sign-on
/// disabled — so an absent property, and any value that is not the boolean `true`, asks for
/// nothing.
fn declared_flag(declared: Option<&Value>, property: &str) -> bool {
    declared.and_then(|declared| declared.get(property)) == Some(&Value::Bool(true))
}

/// The attributes one described directory gives the resource: its `Alias` and the addresses of
/// its DNS servers, read off that service's own description rather than recomputed.
///
/// The addresses are a LIST, which the string-valued attribute map carries one element per
/// indexed key ([`list_attribute_element`]) — the same carrier a hosted zone's name servers
/// use — so a template reads the whole list through `Fn::GetAtt` and one element through the
/// dotted notation. A directory that service reports nothing about gives no attribute at all,
/// so the resource keeps what it already publishes rather than taking an empty value.
fn reported_directory_attributes(
    kind: DirectoryKind,
    described: Option<&Value>,
) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(described) = described else {
        return attributes;
    };
    if let Some(alias) = reported_text(described, ALIAS_MEMBER) {
        attributes.insert(ALIAS_ATTRIBUTE.to_string(), alias);
    }
    // Only a Simple AD publishes the directory id as an attribute of its own; a Managed
    // Microsoft AD publishes it through `Ref` alone, so it is given no such attribute.
    if kind == DirectoryKind::SimpleAd {
        if let Some(directory_id) = reported_text(described, DIRECTORY_ID_MEMBER) {
            attributes.insert(DIRECTORY_ID_ATTRIBUTE.to_string(), directory_id);
        }
    }
    if let Some(Value::Array(addresses)) = described.get(DNS_IP_ADDRS_MEMBER) {
        for (index, address) in addresses.iter().enumerate() {
            if let Some(address) = address.as_str() {
                attributes.insert(
                    list_attribute_element(DNS_IP_ADDRESSES_ATTRIBUTE, index),
                    address.to_string(),
                );
            }
        }
    }
    attributes
}

/// The description Directory Service reports for ONE directory, by the id it minted, or `None`
/// where it reports none.
///
/// `DescribeDirectories` narrowed to the one id is that service's read of a single directory —
/// it "obtains information about the directories that belong to this account" and takes the
/// ids to narrow to. An id the account HAD but no longer holds is an empty page rather than a
/// refusal, which is what a finished teardown looks like; an id it never had is
/// `EntityDoesNotExistException`, which is the same answer for this reading: that service
/// holds no such directory.
async fn described_directory(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    directory_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        DIRECTORY_IDS_MEMBER.to_string(),
        Value::Array(vec![Value::String(directory_id.to_string())]),
    );
    let answered = match call_ds(
        ctx,
        region,
        DESCRIBE_DIRECTORIES,
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == DS_ENTITY_DOES_NOT_EXIST => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(Value::Array(described)) = answered.get(DIRECTORY_DESCRIPTIONS_MEMBER) else {
        return Ok(None);
    };
    Ok(described
        .iter()
        .find(|description| {
            reported_text(description, DIRECTORY_ID_MEMBER).as_deref() == Some(directory_id)
        })
        .cloned())
}

/// The directory's configuration as Directory Service reports it, for drift detection: its own
/// description of the directory, by the id that is this resource's physical id. A directory it
/// no longer holds was deleted, which is what `None` reports.
///
/// The members it reports under the names either type declares are the directory's fully
/// qualified name, its NetBIOS short name, its description, the edition or size it was made
/// at, and whether single sign-on is on
/// ([`DIRECTORY_REPORTED_PROPERTIES`]). Its PLACEMENT is not among them: `VpcSettings` is a
/// structure rather than a scalar, which is the position every non-scalar property is in
/// ([`super::drift`]), and the administrator `Password` is reported by nothing at all —
/// Directory Service answers no read that carries it, which is what keeps it out of a drift
/// report.
pub(super) async fn describe_ds_directory(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(described_directory(ctx, region, physical_resource_id)
        .await?
        .map(|described| reported_as_properties(&described, DIRECTORY_REPORTED_PROPERTIES)))
}

/// The scalar properties a described directory reports, as `(property, member)` pairs: that
/// model spells each of them exactly as either type's property is spelled, beside the single
/// sign-on flag, whose member is `SsoEnabled` while the property that asks for it is
/// `EnableSso`.
const DIRECTORY_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("Name", "Name"),
    ("ShortName", "ShortName"),
    ("Description", "Description"),
    ("Edition", "Edition"),
    ("Size", "Size"),
    (ENABLE_SSO_PROPERTY, SSO_ENABLED_MEMBER),
];

/// Call one Directory Service operation in process, on the awsJson wire that service
/// publishes.
async fn call_ds(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        DS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{DS_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}
