// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of CloudHSM's own control plane: the cluster an
//! `AWS::CloudHSM::Cluster` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`.
//!
//! A cluster is placed in the caller's network: `CreateCluster` takes the subnets it goes in,
//! the type of HSM it will hold and the mode it runs in, and every property of the type is a
//! member of that call under the SAME name — so the evaluated property document IS the
//! request, with the one property that is spelled differently lifted out of it (`Tags`, which
//! that call takes as its own `TagList` of `{Key, Value}` pairs).
//!
//! The identity is that service's: `CreateCluster` takes no identifier and answers the
//! `ClusterId` it minted (`^cluster-[2-7a-zA-Z]{11,16}$`), which is the handle every operation
//! on a cluster takes, and the type's `Ref` returns the ARN built around that id — so both
//! replace the stand-ins the naming rules computed
//! ([`super::super::template::identity_is_service_assigned`] names the type).
//!
//! A created cluster is `CREATE_IN_PROGRESS` — its hardware is still being placed, and it
//! takes no HSM and no initialization in that state — so the resource's creation is held open
//! until that service reports it `UNINITIALIZED` ([`cloudhsm_cluster_readiness`]), the state a
//! cluster that has been created rests in until `InitializeCluster` is called. Its teardown is
//! asynchronous too: `DeleteCluster` leaves the cluster `DELETE_IN_PROGRESS` and the
//! resource's deletion is finished once that service reports it `DELETED`
//! ([`cloudhsm_cluster_removal`]) — the terminal state that cluster's record is reported in
//! after a delete, rather than its disappearance.
//!
//! There is no UPDATE arm: CloudHSM publishes no call that changes the subnets, the mode or
//! the tags of a cluster that exists through this provider, so every property of the type is
//! create-only ([`super::super::template::replacement_properties`]) and a declaration
//! restating one replaces the resource instead.

use super::*;

/// CloudHSM's signing name and `X-Amz-Target` prefix, from its model (`sigv4Name: cloudhsm`,
/// service shape `BaldrApiService`, protocol `awsJson1_1`) — the wire these calls are written
/// on, as that service's own suites drive it.
const CLOUDHSM_SIGNING_NAME: &str = "cloudhsm";
const CLOUDHSM_TARGET_PREFIX: &str = "BaldrApiService";

/// The operations this module calls, as CloudHSM's own model names them.
const CREATE_CLUSTER: &str = "CreateCluster";
const DESCRIBE_CLUSTERS: &str = "DescribeClusters";
const DELETE_CLUSTER: &str = "DeleteCluster";

/// The error code CloudHSM answers for a cluster it does not hold
/// (`CloudHsmResourceNotFoundException`, the modelled refusal of a call "that refers to a
/// resource that cannot be found").
const CLOUDHSM_RESOURCE_NOT_FOUND: &str = "CloudHsmResourceNotFoundException";

/// The properties `CreateCluster` takes as the member of the SAME name, which is every
/// property of the type but its `Tags` ([`tags_of`] reads those).
const CLUSTER_MEMBERS: &[(&str, &str)] = &[
    ("BackupRetentionPolicy", "BackupRetentionPolicy"),
    ("HsmType", "HsmType"),
    ("Mode", "Mode"),
    ("NetworkType", "NetworkType"),
    ("SourceBackupId", "SourceBackupId"),
    ("SubnetIds", "SubnetIds"),
];

/// Every property the type publishes, which is what a declaration may state: a property no arm
/// here maps onto that create's request is refused rather than dropped silently
/// ([`refuse_unmapped_properties`]).
const CLUSTER_PROPERTIES: &[&str] = &[
    "BackupRetentionPolicy",
    "HsmType",
    "Mode",
    "NetworkType",
    "SourceBackupId",
    "SubnetIds",
    TAGS_PROPERTY,
];

/// The property the type publishes its tag set as, and the member `CreateCluster` takes it as:
/// a list of `Tag` structures whose members are spelled `Key`/`Value` ([`UPPER_CASED_TAG`]).
const TAGS_PROPERTY: &str = "Tags";
const TAG_LIST_MEMBER: &str = "TagList";

/// The member a create answers the cluster it made in, the member a describe answers the
/// clusters in, and the member `DeleteCluster` takes the cluster's id as.
const CLUSTER_MEMBER: &str = "Cluster";
const CLUSTERS_MEMBER: &str = "Clusters";
const CLUSTER_ID_MEMBER: &str = "ClusterId";

/// How a `DescribeClusters` request is narrowed to ONE cluster: the `Filters` map, keyed by the
/// `clusterIds` field its own doc publishes ("`clusterIds`, `vpcIds`, `states`").
const FILTERS_MEMBER: &str = "Filters";
const CLUSTER_IDS_FILTER: &str = "clusterIds";

/// The member a described cluster reports its state in.
const STATE_MEMBER: &str = "State";

/// The `ClusterState` a cluster being CREATED is in while that service places it, from the
/// modelled `ClusterState` enum (`smithy_query(query="shape",shape="ClusterState")`): the
/// cluster holds no HSM and takes no initialization in it, so it is a state to wait out rather
/// than one to report the resource ready in.
const CLUSTER_STATE_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";

/// The `ClusterState` a CREATED cluster rests in: "UNINITIALIZED" — the state CloudHSM reports
/// a cluster in once it has been created and before `InitializeCluster` has been called, which
/// is the furthest a cluster a template declares gets, since initializing one needs a
/// certificate only its owner can sign.
const CLUSTER_STATE_UNINITIALIZED: &str = "UNINITIALIZED";

/// The `ClusterState` a DELETED cluster is reported in, which is where this provider's teardown
/// ends: `DeleteCluster` leaves the cluster `DELETE_IN_PROGRESS` and that service then reports
/// it `DELETED` — the documented terminal state of a deleted cluster, which its own reads keep
/// answering with.
const CLUSTER_STATE_DELETED: &str = "DELETED";

/// The members a described cluster reports the type's read-only attributes under: the id that
/// service minted, the state it holds the cluster in, the network it placed it in, the security
/// group it made for it and the backup policy it keeps its backups under. Each is spelled
/// exactly as the attribute is ([`template::CLOUDHSM_CLUSTER_ATTRIBUTES`] less the `Arn`, which
/// the `Cluster` shape carries no member for and which is built around the id below).
const REPORTED_CLUSTER_ATTRIBUTES: &[&str] = &[
    template::CLOUDHSM_CLUSTER_ID_ATTRIBUTE,
    template::CLOUDHSM_CLUSTER_STATE_ATTRIBUTE,
    template::CLOUDHSM_CLUSTER_VPC_ID_ATTRIBUTE,
    template::CLOUDHSM_CLUSTER_SECURITY_GROUP_ATTRIBUTE,
    template::CLOUDHSM_CLUSTER_BACKUP_POLICY_ATTRIBUTE,
];

/// The scalar properties a described cluster reports, as `(property, member)` pairs: that model
/// spells each of them exactly as the type's property is spelled. Its `SubnetIds` is NOT among
/// them — a cluster reports its placement as the `SubnetMapping` from Availability Zone to
/// subnet rather than as the list a declaration states, which is the position every non-scalar
/// property is in ([`super::drift`]) — and neither is its tag set.
const REPORTED_CLUSTER_PROPERTIES: &[(&str, &str)] = &[
    ("HsmType", "HsmType"),
    ("Mode", "Mode"),
    ("NetworkType", "NetworkType"),
];

/// Create the cluster an `AWS::CloudHSM::Cluster` declares, in CloudHSM — so a client that
/// deployed the stack finds the cluster in that service's own `DescribeClusters` under the id
/// the stack published, adds HSMs to it and initializes it, rather than holding an identifier
/// nothing created.
///
/// The declaration IS the request ([`CLUSTER_MEMBERS`]), with the tag set CloudFormation gives
/// the resource beside the declaration's own sent as the `TagList` that create takes. The
/// cluster is recorded in the service that OWNS it, so everything the declaration states is
/// resolved by CloudHSM exactly as it is for a client's own call: a subnet Amazon EC2 holds no
/// record of, two subnets in one Availability Zone or in two VPCs, and an HSM type that service
/// no longer provisions are its refusals, which fail the resource and roll the stack back
/// rather than publishing an identifier for a cluster nothing holds.
///
/// The identity is CloudHSM's: the `ClusterId` it minted is the handle every cluster API takes,
/// and the ARN built around it is what this type's `Ref` returns and so this resource's
/// physical id — reported back beside the read-only attributes that service holds the cluster
/// with ([`REPORTED_CLUSTER_ATTRIBUTES`]).
pub(super) async fn create_cloudhsm_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, CLUSTER_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, CLUSTER_MEMBERS, &mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        // `TagList` is modelled `len 1..50`, so an untagged cluster carries no list at all
        // rather than an empty one.
        request.insert(
            TAG_LIST_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    let answered = call_cloudhsm(ctx, region, CREATE_CLUSTER, &Value::Object(request)).await?;
    let created = answered.get(CLUSTER_MEMBER).cloned().unwrap_or(Value::Null);
    let cluster_id = reported_text(&created, CLUSTER_ID_MEMBER).unwrap_or_default();
    if cluster_id.is_empty() {
        // There would be no handle a client could call CloudHSM with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the CloudHSM control plane answered a created cluster without a cluster id"
                .to_string(),
        });
    }
    Ok(Created {
        physical_resource_id: Some(cluster_arn(region, &cluster_id)),
        attributes: cluster_attributes(region, &created),
    })
}

/// Whether CloudHSM has finished placing the cluster a stack created.
///
/// A created cluster is [`CLUSTER_STATE_CREATE_IN_PROGRESS`] — that service is still placing
/// its hardware, and the cluster takes no HSM and no initialization while it is — so
/// [`CLUSTER_STATE_UNINITIALIZED`] is the state its creation is finished in, and every other
/// state is one no wait brings a NEW cluster into: one being torn down out of this stack's
/// reach, one that service reports degraded, one somebody else is initializing. A cluster that
/// service no longer holds is in the same position, nothing being left to create it.
pub(super) async fn cloudhsm_cluster_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let cluster_id = cluster_id_of(physical_resource_id);
    let Some(described) = described_cluster(ctx, region, &cluster_id).await? else {
        return Ok(Readiness::Failed(format!(
            "CloudHSM holds no cluster {cluster_id}"
        )));
    };
    let state = reported_text(&described, STATE_MEMBER).unwrap_or_default();
    if state == CLUSTER_STATE_UNINITIALIZED {
        return Ok(Readiness::Ready);
    }
    if state == CLUSTER_STATE_CREATE_IN_PROGRESS {
        return Ok(Readiness::Pending(state));
    }
    Ok(Readiness::Failed(format!(
        "CloudHSM reports the cluster {cluster_id} in state {state}"
    )))
}

/// The attributes CloudHSM publishes for a cluster it now reports created: its own description
/// of the cluster, read the one way [`cluster_attributes`] reads it.
///
/// A cluster's `State` is `CREATE_IN_PROGRESS` when the create answers, so the attributes are
/// read again once that service reports the cluster created rather than left as that answer
/// carried them.
pub(super) async fn cloudhsm_cluster_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let cluster_id = cluster_id_of(physical_resource_id);
    Ok(described_cluster(ctx, region, &cluster_id)
        .await?
        .map(|described| cluster_attributes(region, &described))
        .unwrap_or_default())
}

/// Whether CloudHSM has finished removing the cluster a stack deleted.
///
/// `DeleteCluster` does not take the cluster away while the caller waits: it leaves the cluster
/// in the modelled `DELETE_IN_PROGRESS` state and that service then reports it
/// [`CLUSTER_STATE_DELETED`] — the terminal state of a deleted cluster, which its own reads go
/// on answering with — so the deletion is finished in that state, and in a cluster that service
/// no longer reports at all.
pub(super) async fn cloudhsm_cluster_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let cluster_id = cluster_id_of(physical_resource_id);
    match described_cluster(ctx, region, &cluster_id).await? {
        None => Ok(Removal::Gone),
        Some(described) => {
            let state = reported_text(&described, STATE_MEMBER).unwrap_or_default();
            if state == CLUSTER_STATE_DELETED {
                return Ok(Removal::Gone);
            }
            Ok(Removal::Pending(state))
        }
    }
}

/// Delete the cluster a stack created, by the id CloudHSM minted for it — the id inside the ARN
/// that is this resource's physical id. One that service no longer holds
/// (`CloudHsmResourceNotFoundException`, judged on the CODE and never on a message that carries
/// the request's own values) is already in the state the delete asks for.
pub(super) async fn delete_cloudhsm_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        CLUSTER_ID_MEMBER.to_string(),
        Value::String(cluster_id_of(physical_resource_id)),
    );
    match call_cloudhsm(ctx, region, DELETE_CLUSTER, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == CLOUDHSM_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The cluster's configuration as CloudHSM reports it, for drift detection: its own description
/// of the cluster, by the id inside this resource's physical id, under the properties the type
/// declares those members as ([`REPORTED_CLUSTER_PROPERTIES`]). A cluster that service no
/// longer holds was deleted, which is what `None` reports.
pub(super) async fn describe_cloudhsm_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let cluster_id = cluster_id_of(physical_resource_id);
    Ok(described_cluster(ctx, region, &cluster_id)
        .await?
        .map(|described| reported_as_properties(&described, REPORTED_CLUSTER_PROPERTIES)))
}

/// The attributes ONE described cluster gives the resource: the ARN built around the id that
/// service minted, and the read-only values it reports for the cluster
/// ([`REPORTED_CLUSTER_ATTRIBUTES`]), read off that service's own report rather than recomputed.
///
/// A value the report does not carry contributes no attribute at all, so the resource keeps
/// what it already publishes rather than taking an empty one.
fn cluster_attributes(region: &str, described: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    for attribute in REPORTED_CLUSTER_ATTRIBUTES {
        if let Some(reported) = reported_text(described, attribute) {
            attributes.insert((*attribute).to_string(), reported);
        }
    }
    if let Some(cluster_id) = reported_text(described, CLUSTER_ID_MEMBER) {
        attributes.insert(
            template::CLOUDHSM_CLUSTER_ARN_ATTRIBUTE.to_string(),
            cluster_arn(region, &cluster_id),
        );
    }
    attributes
}

/// The ARN a cluster of `cluster_id` is published under:
/// `arn:<partition>:cloudhsm:<region>:<account>:cluster/<clusterId>`, built in the ONE place
/// both the create's answer and the readiness read build it, so the two cannot differ. CloudHSM
/// reports no ARN member for a cluster at all — its `Cluster` shape carries the id and no ARN —
/// so this is the form the type's own return values publish.
fn cluster_arn(region: &str, cluster_id: &str) -> String {
    ::emulator_server::emulator_arn(
        template::CLOUDHSM_ARN_NAMESPACE,
        region,
        &format!("{}/{cluster_id}", template::CLOUDHSM_CLUSTER_PATH),
    )
}

/// The cluster id inside this resource's physical id, which is the ARN above: the segment after
/// the resource path. A physical id that is not such an ARN is taken as the id itself, which is
/// what a value a stand-in was never re-pointed from would be.
fn cluster_id_of(physical_resource_id: &str) -> String {
    physical_resource_id
        .rsplit_once('/')
        .map_or(physical_resource_id, |(_, id)| id)
        .to_string()
}

/// The description CloudHSM reports for ONE cluster, by the id it minted, or `None` where it
/// reports none.
///
/// `DescribeClusters` narrowed by the `clusterIds` filter is that service's read of a single
/// cluster — it "gets information about CloudHSM clusters" and takes that filter to narrow to
/// the ones named. An id the account HAD but no longer holds is an empty page rather than a
/// refusal, which is what a finished teardown looks like; an id it never had is
/// `CloudHsmResourceNotFoundException`, which is the same answer for this reading: that service
/// holds no such cluster.
async fn described_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut filters = Map::new();
    filters.insert(
        CLUSTER_IDS_FILTER.to_string(),
        Value::Array(vec![Value::String(cluster_id.to_string())]),
    );
    let mut request = Map::new();
    request.insert(FILTERS_MEMBER.to_string(), Value::Object(filters));
    let answered = match call_cloudhsm(
        ctx,
        region,
        DESCRIBE_CLUSTERS,
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == CLOUDHSM_RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(Value::Array(described)) = answered.get(CLUSTERS_MEMBER) else {
        return Ok(None);
    };
    Ok(described
        .iter()
        .find(|cluster| reported_text(cluster, CLUSTER_ID_MEMBER).as_deref() == Some(cluster_id))
        .cloned())
}

/// Call one CloudHSM operation in process, on the awsJson wire that service publishes.
async fn call_cloudhsm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        CLOUDHSM_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{CLOUDHSM_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}
