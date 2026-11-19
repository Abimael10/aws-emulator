// SPDX-License-Identifier: CC0-1.0

//! `AWS::DynamoDB::GlobalTable`: the global table a stack declares, created in Amazon
//! DynamoDB itself — the table in the stack's own Region, plus a replica in each other
//! Region the declaration lists.
//!
//! ## Why the tables are really created
//!
//! The type's whole point is a table a client then reads and writes: "Ref returns the table
//! name", `Fn::GetAtt` reads its `Arn`, `StreamArn` and `TableId`, and a stack that reports
//! this resource created has published a table name that `PutItem` in the stack's Region
//! must resolve — and whose items must appear in every replica Region, because that is what
//! a global table IS. So this arm makes the owning service's own calls and reads the
//! answers back: `CreateTable` in the stack's Region for the table itself, then ONE
//! `UpdateTable` carrying a `ReplicaUpdates` `Create` per other Region — "to create a
//! global table by specifying one or more Create actions in the ReplicaUpdates" — which is
//! what establishes the replication group of the CURRENT global tables version. Nothing
//! here reports a resource created that DynamoDB does not hold.
//!
//! ## The properties
//!
//! The type states for a GROUP of tables what `AWS::DynamoDB::Table`
//! ([`super`]) states for one, so its properties are the same `CreateTable` members under
//! two rearrangements, both of which are TRANSLATED into the calls rather than passed
//! through:
//!
//! * the capacity is split by DIMENSION and by REGION — the group's WRITE capacity is
//!   `WriteProvisionedThroughputSettings`/`WriteOnDemandThroughputSettings` on the type, and
//!   each replica's READ capacity is its own
//!   `ReadProvisionedThroughputSettings`/`ReadOnDemandThroughputSettings` — so the local
//!   replica's read half and the group's write half are what the create's
//!   `ProvisionedThroughput`/`OnDemandThroughput` are built from
//!   ([`provisioned_throughput`], [`on_demand_throughput`]), and every other replica's read
//!   half travels as the `ProvisionedThroughputOverride`/`OnDemandThroughputOverride` of its
//!   own `Create` action ([`replica_create_action`]);
//! * a provisioned capacity on this type is stated as the auto scaling RANGE that manages it
//!   (`CapacityAutoScalingSettings`), which has no plain-units member at all, so the range
//!   both seeds the capacity the table is created with — its `SeedCapacity`, or the minimum
//!   of the range where it states none — and is applied as auto scaling in its own right,
//!   through DynamoDB's `UpdateTableReplicaAutoScaling`
//!   ([`auto_scaling_request`]): a range nothing registered would be a declared setting the
//!   stack reported as applied and no read of that service answers for.
//!
//! `TableName` ADDRESSES the calls rather than travelling beside the other members, exactly
//! as it does for one table, and `StreamSpecification`/`SSESpecification` are spelled as
//! that arm spells them ([`super::stream_specification`], [`super::sse_specification`]).
//!
//! The per-replica settings that are DynamoDB calls of their own (`TimeToLiveSpecification`
//! for the group, and each replica's `PointInTimeRecoverySpecification`,
//! `ContributorInsightsSpecification`, `KinesisStreamSpecification` and `ResourcePolicy`)
//! are made on the regional table they belong to, by the same half that makes them for one
//! table ([`super::settings`]) — in that replica's own Region, which is where its table
//! lives. A declared property this arm does not apply fails the resource rather than being
//! dropped from a success ([`refuse_unapplied_properties`]).
//!
//! ## When the resource is ready
//!
//! A global table is a COMPOSITE resource: the table itself is `CREATING` for its window,
//! and each replica is `CREATING` for its own after that, so the resource's creation is held
//! open until DynamoDB reports the table out of its window AND every declared replica
//! `ACTIVE` ([`readiness`]) — a client that reads the stack as complete finds a table it can
//! write and a replica it can read the write back from.

use super::*;

/// The resource type this module provisions.
pub(crate) const DYNAMODB_GLOBAL_TABLE: &str = "AWS::DynamoDB::GlobalTable";

/// The property listing the Regions the global table spans, and the member each entry names
/// its Region in.
const REPLICAS_PROPERTY: &str = "Replicas";
const REGION_MEMBER: &str = "Region";

/// The properties carrying the group's WRITE capacity, and each replica's READ capacity: the
/// split by dimension and Region this type states a capacity in.
const WRITE_PROVISIONED_SETTINGS: &str = "WriteProvisionedThroughputSettings";
const WRITE_ON_DEMAND_SETTINGS: &str = "WriteOnDemandThroughputSettings";
const READ_PROVISIONED_SETTINGS: &str = "ReadProvisionedThroughputSettings";
const READ_ON_DEMAND_SETTINGS: &str = "ReadOnDemandThroughputSettings";

/// The members a provisioned capacity is stated in: the plain read figure a replica may
/// state, and the auto scaling range that manages either dimension.
const READ_CAPACITY_UNITS_MEMBER: &str = "ReadCapacityUnits";
const WRITE_CAPACITY_UNITS_MEMBER: &str = "WriteCapacityUnits";
const READ_CAPACITY_AUTO_SCALING: &str = "ReadCapacityAutoScalingSettings";
const WRITE_CAPACITY_AUTO_SCALING: &str = "WriteCapacityAutoScalingSettings";

/// The members of an auto scaling range: the capacity it manages between, the capacity the
/// resource is provisioned with while it does, and the policy that steers it.
const MIN_CAPACITY_MEMBER: &str = "MinCapacity";
const MAX_CAPACITY_MEMBER: &str = "MaxCapacity";
const SEED_CAPACITY_MEMBER: &str = "SeedCapacity";
const TARGET_TRACKING_MEMBER: &str = "TargetTrackingScalingPolicyConfiguration";

/// The members an on-demand ceiling is stated in, on either side of the split above.
const MAX_READ_REQUEST_UNITS: &str = "MaxReadRequestUnits";
const MAX_WRITE_REQUEST_UNITS: &str = "MaxWriteRequestUnits";

/// The members `UpdateTableReplicaAutoScaling` takes the ranges above as
/// (`smithy_query(query="operation",operation="UpdateTableReplicaAutoScaling")` and
/// `smithy_query(query="shape",shape="AutoScalingSettingsUpdate")`).
const AUTO_SCALING_WRITE_UPDATE: &str = "ProvisionedWriteCapacityAutoScalingSettingsUpdate";
const AUTO_SCALING_REPLICA_READ_UPDATE: &str =
    "ReplicaProvisionedReadCapacityAutoScalingSettingsUpdate";
const AUTO_SCALING_MINIMUM_UNITS: &str = "MinimumUnits";
const AUTO_SCALING_MAXIMUM_UNITS: &str = "MaximumUnits";
const AUTO_SCALING_POLICY_UPDATE: &str = "ScalingPolicyUpdate";
const AUTO_SCALING_INDEX_UPDATES: &str = "GlobalSecondaryIndexUpdates";
const AUTO_SCALING_REPLICA_INDEX_UPDATES: &str = "ReplicaGlobalSecondaryIndexUpdates";

/// The members of one `ReplicaUpdates` `Create` action
/// (`smithy_query(query="shape",shape="CreateReplicationGroupMemberAction")`).
const REPLICA_UPDATES_MEMBER: &str = "ReplicaUpdates";
const CREATE_ACTION: &str = "Create";
const DELETE_ACTION: &str = "Delete";
const REGION_NAME_MEMBER: &str = "RegionName";
const PROVISIONED_THROUGHPUT_OVERRIDE: &str = "ProvisionedThroughputOverride";
const ON_DEMAND_THROUGHPUT_OVERRIDE: &str = "OnDemandThroughputOverride";
const TABLE_CLASS_OVERRIDE: &str = "TableClassOverride";

/// The per-replica members that are the same setting under another name: its table class,
/// and the KMS key its own `SSESpecification` names.
const TABLE_CLASS_PROPERTY: &str = "TableClass";
const KMS_MASTER_KEY_MEMBER: &str = "KMSMasterKeyId";

/// The members a description reports the group's replicas in
/// (`smithy_query(query="shape",shape="ReplicaDescription")`), and the status a settled
/// replica reports.
const REPLICAS_MEMBER: &str = "Replicas";
const REPLICA_STATUS_MEMBER: &str = "ReplicaStatus";
const REPLICA_ACTIVE: &str = "ACTIVE";

/// The index list, and the member each entry names its index in — the same spelling on the
/// type, on each replica and in every call.
const GLOBAL_SECONDARY_INDEXES: &str = "GlobalSecondaryIndexes";
const INDEX_NAME_MEMBER: &str = "IndexName";

/// What the stream of a global table captures: the recorded deployment of this type reports
/// a stream of both images for every regional table of the group, which is also the stream a
/// group's replication is carried over ("the stream containing both the new and the old
/// images of the item") — `StreamViewType`'s `NEW_AND_OLD_IMAGES`.
const REPLICATED_STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// The members of one index of the type that `CreateTable`'s own `GlobalSecondaryIndex`
/// takes under the same name; its capacity is the split above, translated
/// ([`created_index`]).
const INDEX_MEMBERS: &[&str] = &["IndexName", "KeySchema", "Projection", "WarmThroughput"];

/// The properties of the type that ARE `CreateTable` members under the same name, so the
/// declared value travels into the request as written.
///
/// Source: `CreateTable`'s member list
/// (`smithy_query(query="operation",operation="CreateTable")`), intersected with the
/// properties `AWS::DynamoDB::GlobalTable` publishes.
const CREATE_MEMBERS: &[&str] = &[
    "AttributeDefinitions",
    BILLING_MODE_PROPERTY,
    "KeySchema",
    "LocalSecondaryIndexes",
    "WarmThroughput",
];

/// The per-replica properties that are `CreateTable` members of the LOCAL table under the
/// same name: the stack's own Region holds the table the create makes, so a member that
/// call takes is applied by it rather than by a replica action.
const LOCAL_REPLICA_MEMBERS: &[&str] = &["DeletionProtectionEnabled"];

/// The `Fn::GetAtt` attributes the type publishes, each paired with the `TableDescription`
/// member it is read off.
///
/// Source: the type's own "Return values" section — `Fn::GetAtt` reads `Arn`, `StreamArn`
/// and `TableId` off an `AWS::DynamoDB::GlobalTable` — and `TableDescription`'s own members
/// (`smithy_query(query="shape",shape="TableDescription")`), which is where each value comes
/// from. `StreamArn` is reported only for a table that HAS a stream, for the reason the
/// single-table arm's list records.
const GLOBAL_TABLE_ATTRIBUTES: &[(&str, &str)] = &[
    ("Arn", TABLE_ARN_MEMBER),
    ("StreamArn", "LatestStreamArn"),
    ("TableId", "TableId"),
];

/// The sentence a declaration that lists no replica in the stack's own Region is refused
/// with: a global table's replicas are its regional tables, and the one this stack creates
/// is the one in the Region it is deployed in, so a declaration that does not list that
/// Region asks for a resource this stack cannot create. Fixed text stating the rule, with
/// the Region of the deployment as its only variable.
fn local_region_not_declared(region: &str) -> Refusal {
    Refusal {
        code: DECLARATION_REFUSED.to_string(),
        message: format!(
            "Replicas must include a replica in the Region the stack is deployed in ({region})"
        ),
    }
}

/// The sentence a declaration listing one Region twice is refused with: the entries would
/// contradict each other whatever they state, and the second would silently win.
fn region_declared_twice(region: &str) -> Refusal {
    Refusal {
        code: DECLARATION_REFUSED.to_string(),
        message: format!("Replicas names the Region {region} more than once"),
    }
}

/// Create the global table an `AWS::DynamoDB::GlobalTable` declares: the table in the
/// stack's Region, the replica in each other Region the declaration lists, the settings that
/// are calls of their own, and the auto scaling the declared capacity ranges ask for.
///
/// `Err` is DynamoDB's own refusal, or this handler's own refusal of a declaration it cannot
/// apply — and a create whose replicas or settings that service refused takes the table it
/// had already made away again, so a failed resource leaves no half-made global table
/// behind.
pub(crate) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type)?;
    // ...and the two rules the handler holds the declaration to before the owning service is
    // called at all: the one relating its key properties (the single-table arm's, which is
    // about the declaration as a whole) and the one relating its replica list to the Region
    // the stack is deployed in.
    refuse_undefined_key_attributes(&resource.properties)?;
    let local = checked_replicas(&resource.properties, region)?;
    let name = resource.physical_resource_id.clone();
    let answered = call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.CreateTable"),
        &Value::Object(create_request(&resource.properties, &local, &name, propagated)),
    )
    .await?;
    // Everything below changes the table that now exists, so a failure takes it away again
    // rather than leaving the stack with a table short of its declaration.
    if let Err(refusal) = complete(ctx, region, &resource.properties, &local, &name).await {
        let _ = delete(ctx, region, resource).await;
        return Err(refusal);
    }
    Ok(Created::attributes(attributes_from(
        answered.get(TABLE_DESCRIPTION_MEMBER),
        GLOBAL_TABLE_ATTRIBUTES,
    )))
}

/// Everything the declaration asks for BEYOND the table the create made: the settings of
/// each regional table, the replicas of the group, and the auto scaling of the capacities.
///
/// The replicas are created in ONE `UpdateTable`: the call takes a list of actions ("one or
/// more Create actions"), so the group is established in one change rather than one per
/// Region — which is also what makes the replicas start their own windows together.
async fn complete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    local: &Value,
    name: &str,
) -> Result<(), Refusal> {
    // Nothing is in force on a table that has just been created, so every declared setting
    // is applied from off — the state `CreateTable` leaves each of them in, since that call
    // carries none of them. A declaration the settings half cannot express as calls at all
    // FAILS the resource, exactly as a property this arm does not apply does: dropping it
    // would report a table configured with less than its declaration asked for.
    let local_settings = setting_calls(&settings_properties(properties, local))?;
    if !local_settings.is_empty() {
        await_settled(ctx, region, name).await;
        settings::apply(ctx, region, name, &local_settings).await?;
    }
    let mut creates: Vec<Value> = Vec::new();
    for replica in peer_replicas(properties, region) {
        let replica_region = declared_region(replica)?;
        creates.push(serde_json::json!({
            CREATE_ACTION: replica_create_action(replica, &replica_region),
        }));
    }
    if !creates.is_empty() {
        let mut request = Map::new();
        request.insert(REPLICA_UPDATES_MEMBER.to_string(), Value::Array(creates));
        update_table(ctx, region, name, request).await?;
    }
    // Each replica's settings are calls on ITS regional table, made in its own Region —
    // where that table lives — once the replica's create has settled there. The GROUP's own
    // settings (a `TimeToLiveSpecification` is stated once for the whole global table) are
    // part of each of them, so a group-level setting is in force in every Region of the
    // group rather than in the stack's alone.
    for replica in peer_replicas(properties, region) {
        let replica_region = declared_region(replica)?;
        let calls = setting_calls(&settings_properties(properties, replica))?;
        if calls.is_empty() {
            continue;
        }
        await_settled(ctx, &replica_region, name).await;
        settings::apply(ctx, &replica_region, name, &calls).await?;
    }
    // The auto scaling the declared capacity ranges ask for, over the group and each of its
    // replicas: one call, made on the settled group.
    if let Some(request) = auto_scaling_request(properties) {
        await_settled(ctx, region, name).await;
        let mut request = request;
        request.insert(
            TABLE_NAME_PROPERTY.to_string(),
            Value::String(name.to_string()),
        );
        call_json_rpc(
            ctx,
            DYNAMODB_SIGNING_NAME,
            region,
            AWS_JSON_1_0,
            &format!("{DYNAMODB_TARGET_PREFIX}.UpdateTableReplicaAutoScaling"),
            &Value::Object(request),
        )
        .await?;
    }
    Ok(())
}

/// Whether DynamoDB reports the whole global table this create or change made ready.
///
/// A COMPOSITE resource, so there are three questions, each answered from that service's own
/// read: the table itself is out of the window it serves no item call in (the single-table
/// arm's [`super::readiness`] question), every Region the declaration lists is reported as a
/// replica of it in `ACTIVE` — a replica still `CREATING` serves no read in its Region, so a
/// client that reads the stack as complete and then reads the replica would be answered as
/// if the table were not there — and every declared setting of the local table is in force.
///
/// `declared` is `None` where the records do not say what the resource was declared with;
/// the replicas the table reports are then the ones asked about, since nothing says which
/// Regions this resource's creation owes.
pub(crate) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Readiness {
    let described = match describe(ctx, region, physical_resource_id).await {
        // A read that service refuses, and a table it reports none of, say nothing about the
        // resource — one unanswered read is no evidence — so neither holds it open, exactly
        // as the single-table arm treats a refused read.
        Ok(Some(description)) => description,
        _ => return Readiness::Ready,
    };
    if let Some(status) = description_status(&described) {
        return Readiness::Pending(status);
    }
    if let Some(status) = replicas_pending(&described, declared, region) {
        return Readiness::Pending(status);
    }
    let settings_declared = declared
        .map(|properties| settings_properties(properties, &local_replica(properties, region)));
    match settings::settling(ctx, region, physical_resource_id, settings_declared.as_ref()).await {
        Some(status) => Readiness::Pending(status),
        None => Readiness::Ready,
    }
}

/// The `TableStatus` a description reports while that service is still creating or changing
/// the table, or `None` where it reports the table out of both windows.
fn description_status(description: &Value) -> Option<String> {
    let status = description.get(TABLE_STATUS_MEMBER)?.as_str()?;
    in_change_window(status).then(|| status.to_string())
}

/// The status of the first replica the resource is still waiting on: a Region the
/// declaration lists that the description reports no replica in at all, or one whose
/// `ReplicaStatus` is not yet `ACTIVE`.
fn replicas_pending(description: &Value, declared: Option<&Value>, region: &str) -> Option<String> {
    let reported = description
        .get(REPLICAS_MEMBER)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let awaited: Vec<String> = match declared {
        Some(properties) => peer_replicas(properties, region)
            .into_iter()
            .filter_map(|replica| region_of(replica).map(ToString::to_string))
            .collect(),
        None => reported
            .iter()
            .filter_map(|replica| {
                replica
                    .get(REGION_NAME_MEMBER)
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect(),
    };
    for awaited_region in awaited {
        let status = reported
            .iter()
            .find(|replica| replica.get(REGION_NAME_MEMBER) == Some(&Value::String(awaited_region.clone())))
            .and_then(|replica| replica.get(REPLICA_STATUS_MEMBER))
            .and_then(Value::as_str);
        match status {
            Some(REPLICA_ACTIVE) => {}
            Some(status) => return Some(status.to_string()),
            // The replica the change asked for is not reported at all yet, which is the
            // same wait: the group does not span that Region.
            None => return Some(format!("CREATING ({awaited_region})")),
        }
    }
    None
}

/// Apply a CHANGED declaration to the global table the stack already has: the Regions it
/// now lists, added and removed through `UpdateTable`'s `ReplicaUpdates`.
///
/// `Ok(None)` for a change to anything else: the properties of this type restate the group's
/// schema and capacity, whose in-place paths are the single-table arm's, and a change this
/// arm cannot make is reported as not applied rather than as a success that dropped the
/// caller's change.
pub(crate) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type)?;
    // The declaration is held to the same three rules about its replica list the create
    // holds it to: every entry names its Region, the group includes the Region the stack is
    // deployed in, and no Region is named twice.
    checked_replicas(declared, region)?;
    if changed_outside_replicas(&resource.properties, declared) {
        return Ok(None);
    }
    let held = declared_regions(&resource.properties, region);
    let wanted = declared_regions(declared, region);
    let mut actions: Vec<Value> = Vec::new();
    for replica in peer_replicas(declared, region) {
        let replica_region = declared_region(replica)?;
        if !held.iter().any(|region| region == &replica_region) {
            actions.push(serde_json::json!({
                CREATE_ACTION: replica_create_action(replica, &replica_region),
            }));
        }
    }
    for region_held in &held {
        if !wanted.iter().any(|region| region == region_held) {
            actions.push(serde_json::json!({
                DELETE_ACTION: { REGION_NAME_MEMBER: region_held },
            }));
        }
    }
    if actions.is_empty() {
        // The group already spans the Regions the declaration lists, and nothing outside
        // that list changed.
        return Ok(Some(Created::default()));
    }
    let mut request = Map::new();
    request.insert(REPLICA_UPDATES_MEMBER.to_string(), Value::Array(actions));
    let answered = update_table(ctx, region, &resource.physical_resource_id, request).await?;
    Ok(Some(Created::attributes(attributes_from(
        answered.get(TABLE_DESCRIPTION_MEMBER),
        GLOBAL_TABLE_ATTRIBUTES,
    ))))
}

/// Whether a changed declaration moves anything other than the Regions its `Replicas` list
/// names — including a property of a replica that STAYS, which is a change to that regional
/// table rather than to the group's shape.
fn changed_outside_replicas(in_force: &Value, declared: &Value) -> bool {
    let changed_property = in_force
        .as_object()
        .into_iter()
        .flat_map(|members| members.keys())
        .chain(
            declared
                .as_object()
                .into_iter()
                .flat_map(|members| members.keys()),
        )
        .filter(|property| *property != REPLICAS_PROPERTY)
        .any(|property| declared_in(declared, property) != declared_in(in_force, property));
    if changed_property {
        return true;
    }
    // A replica the two declarations share has to be declared the same way in both: its
    // capacity, class and settings are the configuration of a table that already exists,
    // which this arm changes through no call.
    replicas(declared).into_iter().any(|replica| {
        let Some(region) = region_of(replica) else {
            return false;
        };
        replicas(in_force)
            .into_iter()
            .find(|held| region_of(held) == Some(region))
            .is_some_and(|held| held != replica)
    })
}

/// Delete the global table a stack created: its replicas first, then the table in the
/// stack's own Region.
///
/// In that order because a Region that has SOURCED a replica add cannot be taken out of the
/// group while it is still the copy the new replica is catching up with — that is DynamoDB's
/// own rule about the source Region, and the stack's Region is the source of every replica
/// this arm created. Taking the replicas off first leaves a standalone regional table, which
/// is what `DeleteTable` then removes.
pub(crate) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let name = &resource.physical_resource_id;
    let regions = reported_replica_regions(ctx, region, name).await?;
    if !regions.is_empty() {
        let actions: Vec<Value> = regions
            .into_iter()
            .map(|region| serde_json::json!({ DELETE_ACTION: { REGION_NAME_MEMBER: region } }))
            .collect();
        let mut request = Map::new();
        request.insert(REPLICA_UPDATES_MEMBER.to_string(), Value::Array(actions));
        update_table(ctx, region, name, request).await?;
        await_no_replicas(ctx, region, name).await;
    }
    delete_table(ctx, region, name).await
}

/// The Regions the group spans right now, as DynamoDB reports them — the Regions whose
/// replicas a delete has to take off first.
async fn reported_replica_regions(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Vec<String>, Refusal> {
    let Some(description) = describe(ctx, region, name).await? else {
        return Ok(Vec::new());
    };
    Ok(description
        .get(REPLICAS_MEMBER)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|replica| {
            replica
                .get(REGION_NAME_MEMBER)
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

/// Wait until DynamoDB reports the table with no replicas left, which is what the delete of
/// the table itself needs: a Region that sourced the adds cannot be removed while it still
/// holds one.
///
/// Bounded exactly as [`super::await_settled`] is, and a wait that runs out is not a failure
/// of its own: what fails then is the delete, reported as that service's own refusal of it.
async fn await_no_replicas(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) {
    for _ in 0..SETTLED_CHECKS {
        let Ok(Some(description)) = describe(ctx, region, name).await else {
            return;
        };
        if description
            .get(REPLICAS_MEMBER)
            .and_then(Value::as_array)
            .is_none_or(|replicas| replicas.is_empty())
        {
            return;
        }
        ::tokio::time::sleep(SETTLED_CHECK_INTERVAL).await;
    }
}

/// The global table's configuration as Amazon DynamoDB reports it, for drift detection: the
/// properties of the type that service answers for, each under the name the TYPE declares it
/// as. A table that service no longer holds was deleted (`None`).
///
/// The one property of this type whose value a `DescribeTable` answer carries directly is
/// the group's SHAPE — the Regions it spans, which is what the description's `Replicas`
/// reports — so that is what is compared, beside the members the single-table arm reports
/// under the same spelling on both sides. A property the type states in a shape of its own
/// (the capacity split by dimension and Region, the settings of each replica) is left out
/// rather than guessed at: the comparison takes an absent property as no evidence, which is
/// what a service reporting the configuration it models rather than the template's shape of
/// it means.
pub(crate) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(description) = describe(ctx, region, &resource.physical_resource_id).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    for property in ["KeySchema", "LocalSecondaryIndexes"] {
        if let Some(value) = declared_in(&description, property) {
            reported.insert(property.to_string(), value.clone());
        }
    }
    // The Regions the group spans, in the shape the TYPE states them in: one entry per
    // Region, which is the member of a `ReplicaSpecification` every declaration carries.
    let mut regions: Vec<String> = description
        .get(REPLICAS_MEMBER)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|replica| {
            replica
                .get(REGION_NAME_MEMBER)
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    // The local table is a replica of the group as well — the description reports the
    // group's OTHER Regions — and the declaration lists it among the others.
    if !regions.iter().any(|held| held == region) {
        regions.push(region.to_string());
    }
    regions.sort();
    reported.insert(
        REPLICAS_PROPERTY.to_string(),
        Value::Array(
            regions
                .into_iter()
                .map(|region| serde_json::json!({ REGION_MEMBER: region }))
                .collect(),
        ),
    );
    Ok(Some(Value::Object(reported)))
}

/// The `CreateTable` body for the table in the stack's own Region: the members the type
/// states under `CreateTable`'s own names, the two whose shape differs, and the capacity the
/// group's write half and the local replica's read half add up to.
fn create_request(
    properties: &Value,
    local: &Value,
    name: &str,
    propagated: &[(String, String)],
) -> Map<String, Value> {
    let mut request = Map::new();
    // The table is created under the identifier CloudFormation decided for the resource,
    // which is what `Ref` resolves to and what a client calls DynamoDB with.
    request.insert(
        TABLE_NAME_PROPERTY.to_string(),
        Value::String(name.to_string()),
    );
    for member in CREATE_MEMBERS {
        if let Some(value) = declared_in(properties, member) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    for member in LOCAL_REPLICA_MEMBERS {
        if let Some(value) = declared_in(local, member) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    if let Some(indexes) = created_indexes(properties, local) {
        request.insert(GLOBAL_SECONDARY_INDEXES.to_string(), indexes);
    }
    if let Some(throughput) = provisioned_throughput(properties, local) {
        request.insert(PROVISIONED_THROUGHPUT_PROPERTY.to_string(), throughput);
    }
    if let Some(throughput) = on_demand_throughput(properties, local) {
        request.insert(ON_DEMAND_THROUGHPUT_PROPERTY.to_string(), throughput);
    }
    // A global table DEPLOYED this way reports a stream of both images in every Region of
    // its group, whatever its declaration states: that is what the recorded deployment of an
    // `AWS::DynamoDB::GlobalTable` answers for each regional table, and it is the stream the
    // group's replication is carried over ("the stream containing both the new and the old
    // images of the item"). So a declaration that states no `StreamSpecification` is still
    // created with that one. A declaration that states one travels as it stands, under the
    // API's own flag ([`super::stream_specification`]), for DynamoDB to judge.
    request.insert(
        STREAM_SPECIFICATION_PROPERTY.to_string(),
        stream_specification(properties).unwrap_or_else(|| {
            serde_json::json!({
                STREAM_ENABLED_MEMBER: true,
                STREAM_VIEW_TYPE_MEMBER: REPLICATED_STREAM_VIEW_TYPE,
            })
        }),
    );
    // The group's own encryption settings, and the key the LOCAL replica names for its own
    // table: a replica's `SSESpecification` carries the key alone, which is the member the
    // API's specification spells the same way.
    if let Some(specification) = sse_specification(properties) {
        let mut specification = specification;
        if let (Value::Object(members), Some(key)) = (
            &mut specification,
            declared_in(local, SSE_SPECIFICATION_PROPERTY)
                .and_then(|replica| declared_in(replica, KMS_MASTER_KEY_MEMBER)),
        ) {
            members.insert(KMS_MASTER_KEY_MEMBER.to_string(), key.clone());
        }
        request.insert(SSE_SPECIFICATION_PROPERTY.to_string(), specification);
    }
    if let Some(document) = resource_policy(local) {
        request.insert(RESOURCE_POLICY_PROPERTY.to_string(), Value::String(document));
    }
    if let Some(class) = declared_in(local, TABLE_CLASS_PROPERTY) {
        request.insert(TABLE_CLASS_PROPERTY.to_string(), class.clone());
    }
    // The tags of the LOCAL replica beside the ones CloudFormation propagates: a tag set on
    // this type is stated per replica, and the table this call creates is the local one.
    let tags = storable_tags(local, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_PROPERTY.to_string(), tag_list(&tags));
    }
    request
}

/// The `ProvisionedThroughput` of the local table: the group's write capacity and the local
/// replica's read capacity, each read from the settings that state it.
///
/// `None` where the declaration states neither, which is what a declaration in the on-demand
/// capacity mode states. A declaration that states only ONE of the two is handed over as it
/// stands, for DynamoDB to judge against its own required members, exactly as any other
/// half-stated member of the create is.
fn provisioned_throughput(properties: &Value, local: &Value) -> Option<Value> {
    let read = capacity_in(
        declared_in(local, READ_PROVISIONED_SETTINGS),
        READ_CAPACITY_UNITS_MEMBER,
        READ_CAPACITY_AUTO_SCALING,
    );
    let write = capacity_in(
        declared_in(properties, WRITE_PROVISIONED_SETTINGS),
        WRITE_CAPACITY_UNITS_MEMBER,
        WRITE_CAPACITY_AUTO_SCALING,
    );
    if read.is_none() && write.is_none() {
        return None;
    }
    let mut throughput = Map::new();
    if let Some(read) = read {
        throughput.insert(READ_CAPACITY_UNITS_MEMBER.to_string(), read);
    }
    if let Some(write) = write {
        throughput.insert(WRITE_CAPACITY_UNITS_MEMBER.to_string(), write);
    }
    Some(Value::Object(throughput))
}

/// The capacity one of those settings states: the plain figure where it states one, and
/// otherwise the capacity the auto scaling range it states provisions the resource with —
/// its `SeedCapacity` ("the amount of capacity to provision"), or the minimum of the range
/// where it states none.
fn capacity_in(settings: Option<&Value>, units: &str, auto_scaling: &str) -> Option<Value> {
    let settings = settings?;
    if let Some(stated) = declared_in(settings, units) {
        return Some(stated.clone());
    }
    let range = declared_in(settings, auto_scaling)?;
    declared_in(range, SEED_CAPACITY_MEMBER)
        .or_else(|| declared_in(range, MIN_CAPACITY_MEMBER))
        .cloned()
}

/// The `OnDemandThroughput` of the local table: the group's write ceiling and the local
/// replica's read ceiling, each under the member `CreateTable` takes it as.
fn on_demand_throughput(properties: &Value, local: &Value) -> Option<Value> {
    let read = declared_in(local, READ_ON_DEMAND_SETTINGS)
        .and_then(|settings| declared_in(settings, MAX_READ_REQUEST_UNITS));
    let write = declared_in(properties, WRITE_ON_DEMAND_SETTINGS)
        .and_then(|settings| declared_in(settings, MAX_WRITE_REQUEST_UNITS));
    if read.is_none() && write.is_none() {
        return None;
    }
    let mut throughput = Map::new();
    if let Some(read) = read {
        throughput.insert(MAX_READ_REQUEST_UNITS.to_string(), read.clone());
    }
    if let Some(write) = write {
        throughput.insert(MAX_WRITE_REQUEST_UNITS.to_string(), write.clone());
    }
    Some(Value::Object(throughput))
}

/// The create's `GlobalSecondaryIndexes`: each declared index under `CreateTable`'s own
/// members, with the capacity the index's write settings and the LOCAL replica's read
/// settings for that index add up to.
fn created_indexes(properties: &Value, local: &Value) -> Option<Value> {
    let declared = declared_in(properties, GLOBAL_SECONDARY_INDEXES)?.as_array()?;
    Some(Value::Array(
        declared
            .iter()
            .map(|index| created_index(index, local))
            .collect(),
    ))
}

/// One such index.
fn created_index(index: &Value, local: &Value) -> Value {
    let mut created = Map::new();
    for member in INDEX_MEMBERS {
        if let Some(value) = declared_in(index, member) {
            created.insert((*member).to_string(), value.clone());
        }
    }
    // What the LOCAL replica overrides for this index, which is the read half of its
    // capacity; a replica that states nothing for it states no read capacity at all.
    let replica_index = replica_index_of(local, index_name_of(index))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(throughput) = provisioned_throughput(index, &replica_index) {
        created.insert(PROVISIONED_THROUGHPUT_PROPERTY.to_string(), throughput);
    }
    if let Some(throughput) = on_demand_throughput(index, &replica_index) {
        created.insert(ON_DEMAND_THROUGHPUT_PROPERTY.to_string(), throughput);
    }
    Value::Object(created)
}

/// The name one declared index states, or the empty string for one that states none — which
/// is a declaration DynamoDB refuses for the member it requires.
fn index_name_of(index: &Value) -> &str {
    declared_in(index, INDEX_NAME_MEMBER)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// What one replica overrides for the index `index_name`, if its declaration states that
/// index at all.
fn replica_index_of<'p>(replica: &'p Value, index_name: &str) -> Option<&'p Value> {
    declared_in(replica, GLOBAL_SECONDARY_INDEXES)?
        .as_array()?
        .iter()
        .find(|index| index_name_of(index) == index_name)
}

/// One `ReplicaUpdates` `Create` action for the replica `replica` declares: its Region, the
/// read capacity it overrides of the group's, its table class, its own encryption key, and
/// the per-index read overrides it states.
///
/// The Region is the one the entry states, which every entry does by the time an action is
/// built ([`declared_region`] refuses one that does not), so no action is ever addressed to
/// a Region nothing declared.
fn replica_create_action(replica: &Value, replica_region: &str) -> Value {
    let mut action = Map::new();
    action.insert(
        REGION_NAME_MEMBER.to_string(),
        Value::String(replica_region.to_string()),
    );
    if let Some(units) = capacity_in(
        declared_in(replica, READ_PROVISIONED_SETTINGS),
        READ_CAPACITY_UNITS_MEMBER,
        READ_CAPACITY_AUTO_SCALING,
    ) {
        action.insert(
            PROVISIONED_THROUGHPUT_OVERRIDE.to_string(),
            serde_json::json!({ READ_CAPACITY_UNITS_MEMBER: units }),
        );
    }
    if let Some(units) = declared_in(replica, READ_ON_DEMAND_SETTINGS)
        .and_then(|settings| declared_in(settings, MAX_READ_REQUEST_UNITS))
    {
        action.insert(
            ON_DEMAND_THROUGHPUT_OVERRIDE.to_string(),
            serde_json::json!({ MAX_READ_REQUEST_UNITS: units.clone() }),
        );
    }
    if let Some(class) = declared_in(replica, TABLE_CLASS_PROPERTY) {
        action.insert(TABLE_CLASS_OVERRIDE.to_string(), class.clone());
    }
    if let Some(key) = declared_in(replica, SSE_SPECIFICATION_PROPERTY)
        .and_then(|specification| declared_in(specification, KMS_MASTER_KEY_MEMBER))
    {
        action.insert(KMS_MASTER_KEY_MEMBER.to_string(), key.clone());
    }
    let indexes: Vec<Value> = declared_in(replica, GLOBAL_SECONDARY_INDEXES)
        .and_then(Value::as_array)
        .map(|indexes| indexes.iter().map(replica_index_override).collect())
        .unwrap_or_default();
    if !indexes.is_empty() {
        action.insert(GLOBAL_SECONDARY_INDEXES.to_string(), Value::Array(indexes));
    }
    Value::Object(action)
}

/// One per-index read override of a replica action.
fn replica_index_override(index: &Value) -> Value {
    let mut override_ = Map::new();
    override_.insert(
        INDEX_NAME_MEMBER.to_string(),
        Value::String(index_name_of(index).to_string()),
    );
    if let Some(units) = capacity_in(
        declared_in(index, READ_PROVISIONED_SETTINGS),
        READ_CAPACITY_UNITS_MEMBER,
        READ_CAPACITY_AUTO_SCALING,
    ) {
        override_.insert(
            PROVISIONED_THROUGHPUT_OVERRIDE.to_string(),
            serde_json::json!({ READ_CAPACITY_UNITS_MEMBER: units }),
        );
    }
    if let Some(units) = declared_in(index, READ_ON_DEMAND_SETTINGS)
        .and_then(|settings| declared_in(settings, MAX_READ_REQUEST_UNITS))
    {
        override_.insert(
            ON_DEMAND_THROUGHPUT_OVERRIDE.to_string(),
            serde_json::json!({ MAX_READ_REQUEST_UNITS: units.clone() }),
        );
    }
    Value::Object(override_)
}

/// The `UpdateTableReplicaAutoScaling` body the declared capacity RANGES ask for, or `None`
/// where the declaration states none.
///
/// The group's write range is stated once for the whole global table, which is how that call
/// takes it ("updates auto scaling settings on your global tables"), and each replica's read
/// range travels under the entry for its own Region; the ranges of each index are the same
/// split again.
fn auto_scaling_request(properties: &Value) -> Option<Map<String, Value>> {
    let mut request = Map::new();
    if let Some(range) = auto_scaling_range(
        declared_in(properties, WRITE_PROVISIONED_SETTINGS),
        WRITE_CAPACITY_AUTO_SCALING,
    ) {
        request.insert(AUTO_SCALING_WRITE_UPDATE.to_string(), range);
    }
    let index_updates: Vec<Value> = declared_in(properties, GLOBAL_SECONDARY_INDEXES)
        .and_then(Value::as_array)
        .map(|indexes| {
            indexes
                .iter()
                .filter_map(|index| {
                    let range = auto_scaling_range(
                        declared_in(index, WRITE_PROVISIONED_SETTINGS),
                        WRITE_CAPACITY_AUTO_SCALING,
                    )?;
                    Some(serde_json::json!({
                        INDEX_NAME_MEMBER: index_name_of(index),
                        AUTO_SCALING_WRITE_UPDATE: range,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    if !index_updates.is_empty() {
        request.insert(
            AUTO_SCALING_INDEX_UPDATES.to_string(),
            Value::Array(index_updates),
        );
    }
    let replica_updates: Vec<Value> = replicas(properties)
        .into_iter()
        .filter_map(replica_auto_scaling)
        .collect();
    if !replica_updates.is_empty() {
        request.insert(
            REPLICA_UPDATES_MEMBER.to_string(),
            Value::Array(replica_updates),
        );
    }
    (!request.is_empty()).then_some(request)
}

/// One replica's entry of that call: the read range of its own table and of each index it
/// states one for, or `None` where it states none at all — or where the entry names no
/// Region, which is a declaration the create and the update both refuse
/// ([`declared_region`]), so no range is ever attributed to a Region nothing declared.
fn replica_auto_scaling(replica: &Value) -> Option<Value> {
    let replica_region = region_of(replica)?;
    let mut update = Map::new();
    if let Some(range) = auto_scaling_range(
        declared_in(replica, READ_PROVISIONED_SETTINGS),
        READ_CAPACITY_AUTO_SCALING,
    ) {
        update.insert(AUTO_SCALING_REPLICA_READ_UPDATE.to_string(), range);
    }
    let index_updates: Vec<Value> = declared_in(replica, GLOBAL_SECONDARY_INDEXES)
        .and_then(Value::as_array)
        .map(|indexes| {
            indexes
                .iter()
                .filter_map(|index| {
                    let range = auto_scaling_range(
                        declared_in(index, READ_PROVISIONED_SETTINGS),
                        READ_CAPACITY_AUTO_SCALING,
                    )?;
                    Some(serde_json::json!({
                        INDEX_NAME_MEMBER: index_name_of(index),
                        AUTO_SCALING_REPLICA_READ_UPDATE: range,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    if !index_updates.is_empty() {
        update.insert(
            AUTO_SCALING_REPLICA_INDEX_UPDATES.to_string(),
            Value::Array(index_updates),
        );
    }
    if update.is_empty() {
        return None;
    }
    update.insert(
        REGION_NAME_MEMBER.to_string(),
        Value::String(replica_region.to_string()),
    );
    Some(Value::Object(update))
}

/// One declared capacity range as `AutoScalingSettingsUpdate` states it: the minimum and
/// maximum it manages the capacity between, and the policy that steers it — whose
/// target-tracking configuration both sides spell identically.
fn auto_scaling_range(settings: Option<&Value>, member: &str) -> Option<Value> {
    let range = declared_in(settings?, member)?;
    let mut update = Map::new();
    if let Some(minimum) = declared_in(range, MIN_CAPACITY_MEMBER) {
        update.insert(AUTO_SCALING_MINIMUM_UNITS.to_string(), minimum.clone());
    }
    if let Some(maximum) = declared_in(range, MAX_CAPACITY_MEMBER) {
        update.insert(AUTO_SCALING_MAXIMUM_UNITS.to_string(), maximum.clone());
    }
    if let Some(policy) = declared_in(range, TARGET_TRACKING_MEMBER) {
        update.insert(
            AUTO_SCALING_POLICY_UPDATE.to_string(),
            serde_json::json!({ TARGET_TRACKING_MEMBER: policy.clone() }),
        );
    }
    (!update.is_empty()).then(|| Value::Object(update))
}

/// The properties the settings half reads, for ONE regional table: the group's Time to Live
/// setting — which is stated once for the whole global table — beside the settings the
/// replica states for its own table.
fn settings_properties(properties: &Value, replica: &Value) -> Value {
    let mut carried = Map::new();
    for setting in settings::SETTINGS {
        let declared = declared_in(replica, setting.property)
            .or_else(|| declared_in(properties, setting.property));
        if let Some(value) = declared {
            carried.insert(setting.property.to_string(), value.clone());
        }
    }
    Value::Object(carried)
}

/// The replicas the declaration lists.
fn replicas(properties: &Value) -> Vec<&Value> {
    declared_in(properties, REPLICAS_PROPERTY)
        .and_then(Value::as_array)
        .map(|replicas| replicas.iter().collect())
        .unwrap_or_default()
}

/// The Region one entry names.
fn region_of(replica: &Value) -> Option<&str> {
    declared_in(replica, REGION_MEMBER).and_then(Value::as_str)
}

/// The replicas OTHER than the one in the stack's own Region: the ones a `ReplicaUpdates`
/// `Create` adds, since the local one is the table the create itself makes.
fn peer_replicas<'p>(properties: &'p Value, region: &str) -> Vec<&'p Value> {
    replicas(properties)
        .into_iter()
        .filter(|replica| region_of(replica) != Some(region))
        .collect()
}

/// The Regions the declaration lists beside the stack's own, in declaration order.
fn declared_regions(properties: &Value, region: &str) -> Vec<String> {
    peer_replicas(properties, region)
        .into_iter()
        .filter_map(|replica| region_of(replica).map(ToString::to_string))
        .collect()
}

/// The declaration's entry for the stack's own Region, having held the list to the three
/// rules about it: every entry names the Region it is for, no Region is named twice, and one
/// of them is the Region the stack is deployed in.
fn checked_replicas(properties: &Value, region: &str) -> Result<Value, Refusal> {
    let declared = replicas(properties);
    for (position, replica) in declared.iter().enumerate() {
        let named = declared_region(replica)?;
        if declared[..position]
            .iter()
            .any(|earlier| region_of(earlier) == Some(named.as_str()))
        {
            return Err(region_declared_twice(&named));
        }
    }
    declared
        .into_iter()
        .find(|replica| region_of(replica) == Some(region))
        .cloned()
        .ok_or_else(|| local_region_not_declared(region))
}

/// The Region one `Replicas` entry is for, which every entry has to name: `Region` is the
/// one REQUIRED member of a `ReplicaSpecification`, and an entry that names none addresses
/// no regional table — so it is refused rather than skipped, which would let a later reader
/// invent a Region for it.
fn declared_region(replica: &Value) -> Result<String, Refusal> {
    region_of(replica)
        .map(ToString::to_string)
        .ok_or_else(|| Refusal {
            code: DECLARATION_REFUSED.to_string(),
            message: format!(
                "every entry of {REPLICAS_PROPERTY} must state the {REGION_MEMBER} it is for"
            ),
        })
}

/// The calls that put the settings `declared` asks for into force on a table being created,
/// or the refusal for a declaration the settings half can express no call for.
///
/// Nothing is in force on a table that has just been created, so `None` from that half means
/// a declared setting with no path at all rather than a change that cannot be made in place
/// — which fails the resource, exactly as a property this arm does not apply does.
fn setting_calls(declared: &Value) -> Result<Vec<settings::SettingCall>, Refusal> {
    settings::calls(None, declared).ok_or_else(|| Refusal {
        code: DECLARATION_REFUSED.to_string(),
        message: "this emulator cannot put one of the declared settings of \
                  AWS::DynamoDB::GlobalTable into force on a table it creates"
            .to_string(),
    })
}

/// The local replica's declaration, or a null value where the records carry none — for the
/// readers that only ask what it STATES, which a resource whose declaration is not known
/// here states nothing of.
pub(crate) fn local_replica(properties: &Value, region: &str) -> Value {
    replicas(properties)
        .into_iter()
        .find(|replica| region_of(replica) == Some(region))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a global table from being reported created with less than its
/// declaration asked for.
///
/// What is left of the type is `MultiRegionConsistency`: the mode it states is a
/// configuration of the replication group this arm does not establish (a `STRONG` group
/// spans three Regions, or two and a witness), so a declaration carrying it asks for a group
/// nothing here makes, filed as its own gap rather than dropped from a create that answered
/// success.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    for property in members.keys() {
        if applies(property) {
            continue;
        }
        return Err(unapplied(property, resource_type));
    }
    for replica in replicas(properties) {
        let Value::Object(members) = replica else {
            continue;
        };
        for member in members.keys() {
            if applies_to_a_replica(member) {
                continue;
            }
            return Err(unapplied(
                &format!("{REPLICAS_PROPERTY}.{member}"),
                resource_type,
            ));
        }
        refuse_unapplied_index_members(replica, resource_type)?;
    }
    refuse_unapplied_index_members(properties, resource_type)?;
    // The stream's own resource-based policy is a sub-resource of the STREAM rather than a
    // member of any call this arm makes, for the reason the single-table arm records.
    if let Some(Value::Object(specification)) =
        declared_in(properties, STREAM_SPECIFICATION_PROPERTY)
    {
        for member in specification.keys() {
            if !STREAM_SPECIFICATION_MEMBERS.contains(&member.as_str()) {
                return Err(unapplied(
                    &format!("{STREAM_SPECIFICATION_PROPERTY}.{member}"),
                    resource_type,
                ));
            }
        }
    }
    Ok(())
}

/// The same question for the members of each `GlobalSecondaryIndexes` entry `declared`
/// carries — the type states an index twice, once for the group and once per replica, and
/// an index member this arm carries into no call would be dropped from a create that
/// answered success.
fn refuse_unapplied_index_members(declared: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Some(Value::Array(indexes)) = declared_in(declared, GLOBAL_SECONDARY_INDEXES) else {
        return Ok(());
    };
    for index in indexes {
        let Value::Object(members) = index else {
            continue;
        };
        for member in members.keys() {
            if applies_to_an_index(member) {
                continue;
            }
            return Err(unapplied(
                &format!("{GLOBAL_SECONDARY_INDEXES}.{member}"),
                resource_type,
            ));
        }
    }
    Ok(())
}

/// Whether this arm applies the member `name` of one declared index: a member
/// `CreateTable`'s own `GlobalSecondaryIndex` takes under the same name
/// ([`INDEX_MEMBERS`]), or one of the capacity settings it is translated from
/// ([`created_index`], [`replica_index_override`]).
fn applies_to_an_index(name: &str) -> bool {
    INDEX_MEMBERS.contains(&name)
        || [
            WRITE_PROVISIONED_SETTINGS,
            WRITE_ON_DEMAND_SETTINGS,
            READ_PROVISIONED_SETTINGS,
            READ_ON_DEMAND_SETTINGS,
        ]
        .contains(&name)
}

/// Whether this arm applies the top-level property `name`.
fn applies(name: &str) -> bool {
    CREATE_MEMBERS.contains(&name)
        || settings::is_a_setting(name)
        || [
            TABLE_NAME_PROPERTY,
            REPLICAS_PROPERTY,
            GLOBAL_SECONDARY_INDEXES,
            SSE_SPECIFICATION_PROPERTY,
            STREAM_SPECIFICATION_PROPERTY,
            WRITE_PROVISIONED_SETTINGS,
            WRITE_ON_DEMAND_SETTINGS,
        ]
        .contains(&name)
}

/// Whether this arm applies the member `name` of one replica.
fn applies_to_a_replica(name: &str) -> bool {
    LOCAL_REPLICA_MEMBERS.contains(&name)
        || settings::is_a_setting(name)
        || [
            REGION_MEMBER,
            GLOBAL_SECONDARY_INDEXES,
            READ_PROVISIONED_SETTINGS,
            READ_ON_DEMAND_SETTINGS,
            SSE_SPECIFICATION_PROPERTY,
            TABLE_CLASS_PROPERTY,
            TAGS_PROPERTY,
            RESOURCE_POLICY_PROPERTY,
        ]
        .contains(&name)
}
