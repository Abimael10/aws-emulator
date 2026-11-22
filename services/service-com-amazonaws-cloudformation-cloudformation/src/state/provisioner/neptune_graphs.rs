// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Neptune Analytics' own control plane: the graph an
//! `AWS::NeptuneGraph::Graph` declares and the snapshot of one an
//! `AWS::NeptuneGraph::GraphSnapshot` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! Both identities are that service's to mint. `CreateGraph` takes the graph's NAME and
//! answers the `id` it assigned (`^g-[a-z0-9]{10}$`), which is the identity a `Ref` resolves
//! to and the `graphIdentifier` every call on a graph takes; `CreateGraphSnapshot` takes the
//! graph and the snapshot's NAME and answers the `id` it assigned (`^gs-[a-z0-9]{10}$`),
//! which every call on a snapshot takes. So each create REPLACES the stand-in the naming
//! rules computed with the identifier that service answered, and the graph's further
//! published values — the ARN it built and the endpoint it serves the graph on — are read off
//! that same answer rather than recomputed here.
//!
//! Neither lifecycle change is finished when its call answers: a created graph is `CREATING`
//! and a created snapshot `CREATING`, in neither of which is the resource usable, so each
//! resource's creation is held open until that service reports it `AVAILABLE`
//! ([`neptune_graph_readiness`], [`graph_snapshot_readiness`]); and both deletes leave the
//! resource `DELETING`, so each deletion is finished once that service no longer holds it.

use super::*;

/// Neptune Analytics' signing name, from its model (`sigv4Name: neptune-graph`), which speaks
/// `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares, naming the operation so a refusal is reported the
/// way that service's own SDK raises it.
pub(super) const NEPTUNE_GRAPH_SIGNING_NAME: &str = "neptune-graph";

/// The collections that service binds its two resources to, as its own `@http` traits
/// declare: `POST /graphs` creates a graph and `GET`/`PATCH`/`DELETE /graphs/{graphIdentifier}`
/// read, change and remove one; `POST /snapshots` creates a snapshot and
/// `GET`/`DELETE /snapshots/{snapshotIdentifier}` read and remove one.
const GRAPHS_URI: &str = "/graphs";
const SNAPSHOTS_URI: &str = "/snapshots";

/// The collection its tagging surface is bound to: `GET /tags/{resourceArn}` reads a
/// resource's tags, `POST` adds them and `DELETE … ?tagKeys=` drops them, each addressing the
/// resource by the ARN that service minted.
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as Neptune Analytics' own model names them.
const CREATE_GRAPH: &str = "CreateGraph";
const GET_GRAPH: &str = "GetGraph";
const UPDATE_GRAPH: &str = "UpdateGraph";
const DELETE_GRAPH: &str = "DeleteGraph";
const CREATE_GRAPH_SNAPSHOT: &str = "CreateGraphSnapshot";
const GET_GRAPH_SNAPSHOT: &str = "GetGraphSnapshot";
const DELETE_GRAPH_SNAPSHOT: &str = "DeleteGraphSnapshot";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code Neptune Analytics answers for a graph or a snapshot it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, its updates and its deletes).
const NEPTUNE_GRAPH_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member both creates answer the identifier they minted in, and the ones a graph's
/// further published values come back in (`smithy_query(query="operation",
/// operation="CreateGraph")`: `id`, `arn`, `endpoint`).
const ID_MEMBER: &str = "id";
const ARN_MEMBER: &str = "arn";
const ENDPOINT_MEMBER: &str = "endpoint";

/// The member either resource's lifecycle status is reported in.
const STATUS_MEMBER: &str = "status";

/// The status both resources are usable in, from `GraphStatus` and `SnapshotStatus`
/// (`smithy_query(query="shape",shape="GraphStatus,SnapshotStatus")`).
const NEPTUNE_GRAPH_AVAILABLE: &str = "AVAILABLE";

/// The `GraphStatus` values a graph is still SETTLING in: a created graph is `CREATING`, a
/// changed one `UPDATING`, one being loaded `IMPORTING`, one being captured `SNAPSHOTTING`,
/// one being emptied `RESETTING` and one being brought back up `STARTING`. In none of them
/// has the change the resource waits on landed, so each is a state to wait out rather than
/// one to report the resource ready — or failed — in. `DELETING` is not one: a teardown is
/// what [`neptune_graph_removal`] waits on.
const GRAPH_SETTLING: &[&str] = &[
    "CREATING",
    "IMPORTING",
    "RESETTING",
    "SNAPSHOTTING",
    "STARTING",
    "UPDATING",
];

/// The `SnapshotStatus` a created snapshot is still being taken in, the one state it settles
/// out of into [`NEPTUNE_GRAPH_AVAILABLE`].
const SNAPSHOT_SETTLING: &[&str] = &["CREATING"];

/// The status either resource is reported in while that service is still removing it.
const NEPTUNE_GRAPH_DELETING: &str = "DELETING";

/// The properties of `AWS::NeptuneGraph::Graph` whose values are NUMBERS on the wire, with
/// the member of `CreateGraph` each is carried as: the graph's provisioned memory-optimized
/// Neptune Capacity Units (`provisionedMemory`, `@range 16..24576`) and how many replicas it
/// keeps (`replicaCount`, `@range 0..2`).
///
/// A template states a number either in JSON's own spelling or as the TEXT a parameter
/// reference resolves to, so each is read in the one spelling the member takes
/// ([`declared_number`]) before the request is sent.
const GRAPH_NUMBER_MEMBERS: &[(&str, &str)] = &[
    ("ProvisionedMemory", "provisionedMemory"),
    ("ReplicaCount", "replicaCount"),
];

/// The properties of the type whose values are BOOLEANS on the wire, with the member each is
/// carried as: whether the graph may be deleted at all and whether it is reachable over the
/// internet.
const GRAPH_FLAG_MEMBERS: &[(&str, &str)] = &[
    ("DeletionProtection", "deletionProtection"),
    ("PublicConnectivity", "publicConnectivity"),
];

/// The property naming the key the graph's data is encrypted with, and the member it travels
/// as.
const KMS_KEY_PROPERTY: &str = "KmsKeyIdentifier";
const KMS_KEY_MEMBER: &str = "kmsKeyIdentifier";

/// The property the vector index's width is declared in and the member it travels as: the
/// type publishes `VectorSearchConfiguration.VectorSearchDimension`, where that service's own
/// `VectorSearchConfiguration` carries the width as `dimension`
/// (`smithy_query(query="shape",shape="VectorSearchConfiguration")`).
const VECTOR_SEARCH_PROPERTY: &str = "VectorSearchConfiguration";
const VECTOR_SEARCH_DIMENSION_PROPERTY: &str = "VectorSearchDimension";
const VECTOR_SEARCH_MEMBER: &str = "vectorSearchConfiguration";
const VECTOR_SEARCH_DIMENSION_MEMBER: &str = "dimension";

/// The property the graph's NAME is declared in, and the member `CreateGraph` takes it as.
const GRAPH_NAME_PROPERTY: &str = "GraphName";
const GRAPH_NAME_MEMBER: &str = "graphName";

/// The member the tag set travels as on either create, and on `TagResource`: a `TagMap`, so
/// the set is sent as the MAP of key to value that member takes ([`tags_as_map`]) rather than
/// as a list of pairs.
const TAGS_MEMBER: &str = "tags";

/// Every property of `AWS::NeptuneGraph::Graph` this arm carries onto the owning API, so a
/// declaration stating one outside the set is REFUSED rather than reported as created without
/// it ([`refuse_unmapped_properties`]).
const GRAPH_MAPPED_PROPERTIES: &[&str] = &[
    "DeletionProtection",
    "GraphName",
    "KmsKeyIdentifier",
    "ProvisionedMemory",
    "PublicConnectivity",
    "ReplicaCount",
    "Tags",
    "VectorSearchConfiguration",
];

/// The members `UpdateGraph` takes of those: that call carries the graph's provisioned
/// memory, whether it is publicly reachable and whether it is protected from deletion
/// (`smithy_query(query="operation",operation="UpdateGraph")`). Every other property is fixed
/// when the graph is made, which is why a declaration restating one replaces the resource
/// ([`super::template::replacement_properties`]) instead of reaching here.
const GRAPH_UPDATABLE_NUMBER_MEMBERS: &[(&str, &str)] =
    &[("ProvisionedMemory", "provisionedMemory")];

/// The properties of `AWS::NeptuneGraph::GraphSnapshot` this arm carries, with the member of
/// `CreateGraphSnapshot` each is carried as: the graph the snapshot is taken of and the
/// snapshot's own name. The tag set travels beside them ([`tags_of`]).
const SNAPSHOT_MEMBERS: &[(&str, &str)] = &[
    ("GraphIdentifier", "graphIdentifier"),
    ("SnapshotName", "snapshotName"),
];

/// Every property of the snapshot type this arm carries onto the owning API.
const SNAPSHOT_MAPPED_PROPERTIES: &[&str] = &["GraphIdentifier", "SnapshotName", "Tags"];

/// Whether the final snapshot `DeleteGraph` can take on its way out is skipped.
///
/// The type declares no property for it, and that member is REQUIRED
/// (`smithy_query(query="operation",operation="DeleteGraph")`: `skipSnapshot` is a required
/// `@httpQuery`), so the stack's delete has to state one: it skips the capture, because a
/// snapshot taken here would outlive the stack as a resource no stack manages and nothing
/// would ever delete it.
const SKIP_FINAL_SNAPSHOT: &str = "skipSnapshot=true";

/// The prefix of the name a declaration that states none is created under, which the
/// generated name below opens with so it satisfies the modelled `GraphName`
/// (`^(?!g-)[a-z][a-z0-9]*(-[a-z0-9]+)*$`: lower case, a leading letter, and never the `g-`
/// an id carries).
const GENERATED_GRAPH_NAME_PREFIX: &str = "graph-";

/// Create the graph an `AWS::NeptuneGraph::Graph` declares, in Neptune Analytics — so a client
/// that deployed the stack reads the graph back through that service's own `GetGraph` and
/// `ListGraphs` for the identity the stack published, and the tags the deployment gives it are
/// what its `ListTagsForResource` answers.
///
/// The declared properties ARE the create's members, each carried in the spelling its own
/// member takes ([`GRAPH_NUMBER_MEMBERS`], [`GRAPH_FLAG_MEMBERS`]), with the declared tags
/// beside the ones CloudFormation gives the resource ([`tags_of`]) as the `TagMap` that call's
/// own `tags` member takes.
///
/// The identity is that service's: the `id` it answered becomes this resource's physical id —
/// the type's primary identifier, which its `Ref` returns — and the ARN and endpoint it
/// reported are the attributes the type publishes beside it.
pub(super) async fn create_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, GRAPH_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    request.insert(
        GRAPH_NAME_MEMBER.to_string(),
        Value::String(graph_name(&properties, resource)),
    );
    copy_numeric_members(&properties, GRAPH_NUMBER_MEMBERS, &mut request);
    copy_flag_members(&properties, GRAPH_FLAG_MEMBERS, &mut request);
    if let Some(key) = properties.get(KMS_KEY_PROPERTY) {
        request.insert(KMS_KEY_MEMBER.to_string(), key.clone());
    }
    if let Some(configuration) = vector_search_member(&properties) {
        request.insert(VECTOR_SEARCH_MEMBER.to_string(), configuration);
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        CREATE_GRAPH,
        "POST",
        GRAPHS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let id = answered_identifier(&answered, "graph")?;
    Ok(Created {
        physical_resource_id: Some(id),
        attributes: reported_graph_attributes(&answered),
    })
}

/// Apply a changed declaration to the graph the stack created, through `UpdateGraph` — the one
/// call that reconfigures a graph that exists.
///
/// It takes the graph's provisioned memory and its two flags and nothing else, addressed by
/// the id that service minted; every other property is fixed when the graph is made, so a
/// declaration restating one replaces the resource rather than reaching here. The graph keeps
/// the identity the stack publishes.
pub(super) async fn update_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_numeric_members(&properties, GRAPH_UPDATABLE_NUMBER_MEMBERS, &mut request);
    copy_flag_members(&properties, GRAPH_FLAG_MEMBERS, &mut request);
    let answered = call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        UPDATE_GRAPH,
        "PATCH",
        &graph_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(reported_graph_attributes(&answered)))
}

/// Bring the tags of the graph a resource record names to the ones this deployment gives it,
/// through Neptune Analytics' own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this
/// deployment no longer gives the resource and is removed, and one in it with another value is
/// re-valued. Where the two already agree nothing is called at all, which is what makes this
/// safe to ask on every deployment.
///
/// Those calls address a resource by its ARN, which is the handle that service minted rather
/// than one rebuilt here, so it is read back off the graph itself. A graph that service no
/// longer holds has no tags to reconcile.
pub(super) async fn retag_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let read = read_neptune_graph(ctx, region, &resource.physical_resource_id).await?;
    retag_by_arn(ctx, region, read.as_ref(), declared, propagated).await
}

/// The same reconciliation for the SNAPSHOT a stack created: its `Tags` are the one property
/// of that type a deployment can change without replacing the resource — `CreateGraphSnapshot`'s
/// own `tags` map applies to the snapshot that call creates, and that service publishes no call
/// that changes a snapshot otherwise — so this is where the set a snapshot is to end up carrying
/// is reconciled, by the ARN that service built for it.
pub(super) async fn retag_graph_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let read = read_graph_snapshot(ctx, region, &resource.physical_resource_id).await?;
    retag_by_arn(ctx, region, read.as_ref(), declared, propagated).await
}

/// The ONE reconciliation both resources' re-tagging is: the tagging surface of that service
/// addresses either resource by its ARN, so the only thing each arm contributes is the read
/// the ARN comes off.
///
/// A record that service no longer holds, or one carrying no ARN, has no tags to reconcile.
async fn retag_by_arn(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    record: Option<&Value>,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(arn) = record.and_then(|record| reported_text(record, ARN_MEMBER)) else {
        return Ok(());
    };
    let uri = format!("{TAGS_URI}/{}", percent_encoded(&arn));
    let answered = call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await?;
    let held = reported_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            NEPTUNE_GRAPH_SIGNING_NAME,
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
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per
        // key rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            NEPTUNE_GRAPH_SIGNING_NAME,
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

/// Whether Neptune Analytics has finished bringing up — or reconfiguring — the graph a stack
/// created.
///
/// A created graph is `CREATING`, in which it serves no query at all, and the changes a
/// deployment makes to one walk [`GRAPH_SETTLING`] before they land; so
/// [`NEPTUNE_GRAPH_AVAILABLE`] is the state the change the resource waits on is finished in,
/// and every other status is one no wait will bring a graph out of: one whose create failed,
/// one stopped out of this stack's reach, one being torn down. A graph that service no longer
/// holds is in the same position, nothing being left to create it.
pub(super) async fn neptune_graph_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(graph) = read_neptune_graph(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Neptune Analytics holds no graph {physical_resource_id}"
        )));
    };
    Ok(settled(
        &graph,
        GRAPH_SETTLING,
        &format!("graph {physical_resource_id}"),
    ))
}

/// The attributes Neptune Analytics publishes for a graph it now reports up: its own read of
/// the graph, read the one way [`reported_graph_attributes`] reads one.
///
/// A graph still `CREATING` has no endpoint a client can reach — that address is what the
/// service publishes once the graph is serving — so the type's attributes are read off the
/// graph again once it reports it `AVAILABLE`.
pub(super) async fn neptune_graph_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(read_neptune_graph(ctx, region, physical_resource_id)
        .await?
        .as_ref()
        .map(reported_graph_attributes)
        .unwrap_or_default())
}

/// Whether Neptune Analytics has finished removing the graph a stack deleted.
///
/// `DeleteGraph` answers the graph `DELETING`, so it is still there when the delete answers
/// and the deletion is finished once that service no longer holds it.
pub(super) async fn neptune_graph_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    Ok(removal_of(
        read_neptune_graph(ctx, region, physical_resource_id).await?,
    ))
}

/// Delete the graph a stack created, by the id it was created under — this resource's physical
/// id, which is what `Ref` returns for the type. One Neptune Analytics no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message that carries the
/// request's own values) is already in the state the delete asks for.
pub(super) async fn delete_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        DELETE_GRAPH,
        "DELETE",
        &format!(
            "{}?{SKIP_FINAL_SNAPSHOT}",
            graph_uri(physical_resource_id)
        ),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NEPTUNE_GRAPH_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The graph's configuration as Neptune Analytics reports it, for drift detection: `GetGraph`'s
/// own record, read by the id the stack published. One that service no longer holds was
/// deleted.
///
/// The members it reports under the names this type declares are the graph's name, the memory
/// it is provisioned with, how many replicas it keeps and its two flags; a property the read
/// does not report is not evidence of anything ([`super::drift`]).
pub(super) async fn describe_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_neptune_graph(ctx, region, physical_resource_id)
        .await?
        .map(|graph| {
            reported_as_properties(
                &graph,
                &[
                    (GRAPH_NAME_PROPERTY, "name"),
                    ("ProvisionedMemory", "provisionedMemory"),
                    ("ReplicaCount", "replicaCount"),
                    ("DeletionProtection", "deletionProtection"),
                    ("PublicConnectivity", "publicConnectivity"),
                ],
            )
        }))
}

/// Create the snapshot an `AWS::NeptuneGraph::GraphSnapshot` declares, in Neptune Analytics —
/// so a client that deployed the stack reads it back through that service's own
/// `GetGraphSnapshot` and `ListGraphSnapshots` for the identity the stack published, and can
/// restore a graph from it.
///
/// The declaration IS the request ([`SNAPSHOT_MEMBERS`]): the graph the snapshot is taken of
/// — which a template names with a `Ref` on the graph of the same stack, so it is the id that
/// service minted — and the snapshot's own name, with the declared tags beside the ones
/// CloudFormation gives the resource.
///
/// The identity is that service's: the `id` it answered becomes this resource's physical id,
/// and the ARN it built around that id is the attribute the type publishes beside it.
pub(super) async fn create_graph_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SNAPSHOT_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, SNAPSHOT_MEMBERS, &mut request);
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        CREATE_GRAPH_SNAPSHOT,
        "POST",
        SNAPSHOTS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let id = answered_identifier(&answered, "graph snapshot")?;
    Ok(Created {
        physical_resource_id: Some(id),
        attributes: reported_snapshot_attributes(&answered),
    })
}

/// Whether Neptune Analytics has finished taking the snapshot a stack created.
///
/// A created snapshot is `CREATING`, in which there is nothing to restore from, so the
/// resource's creation is finished once that service reports it `AVAILABLE`; `FAILED` is a
/// state no wait will bring it out of, and so is a snapshot that service no longer holds.
pub(super) async fn graph_snapshot_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(snapshot) = read_graph_snapshot(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Neptune Analytics holds no graph snapshot {physical_resource_id}"
        )));
    };
    Ok(settled(
        &snapshot,
        SNAPSHOT_SETTLING,
        &format!("graph snapshot {physical_resource_id}"),
    ))
}

/// Whether Neptune Analytics has finished removing the snapshot a stack deleted.
///
/// `DeleteGraphSnapshot` answers the snapshot `DELETING`, so it is still there when the delete
/// answers and the deletion is finished once that service no longer holds it.
pub(super) async fn graph_snapshot_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    Ok(removal_of(
        read_graph_snapshot(ctx, region, physical_resource_id).await?,
    ))
}

/// Delete the snapshot a stack created, by the id it was created under. One Neptune Analytics
/// no longer holds is already in the state the delete asks for.
pub(super) async fn delete_graph_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        DELETE_GRAPH_SNAPSHOT,
        "DELETE",
        &snapshot_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NEPTUNE_GRAPH_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The snapshot's configuration as Neptune Analytics reports it, for drift detection: the
/// graph it was taken of and the name it was taken under, which are the two properties a
/// declaration of the type states beside its tags.
pub(super) async fn describe_graph_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_graph_snapshot(ctx, region, physical_resource_id)
        .await?
        .map(|snapshot| {
            reported_as_properties(
                &snapshot,
                &[
                    ("GraphIdentifier", "sourceGraphId"),
                    ("SnapshotName", "name"),
                ],
            )
        }))
}

/// The graph record that service holds under `physical_resource_id`, or `None` when it holds
/// none (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_neptune_graph(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_GRAPH,
        &graph_uri(physical_resource_id),
    )
    .await
}

/// The snapshot record that service holds under `physical_resource_id`, or `None` when it
/// holds none.
async fn read_graph_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_GRAPH_SNAPSHOT,
        &snapshot_uri(physical_resource_id),
    )
    .await
}

/// The ONE read both resources' reads are: that service's own `GET` of the record, with the
/// not-found it models read as the record being gone rather than as a refused read.
async fn read_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    uri: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        NEPTUNE_GRAPH_SIGNING_NAME,
        region,
        operation,
        "GET",
        uri,
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == NEPTUNE_GRAPH_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// What one reported record says about the lifecycle change its resource is waiting on:
/// `AVAILABLE` is finished, a status in `settling` is still running, and anything else is a
/// state no wait will bring the resource out of. `subject` names the resource in the reason a
/// failed wait reports.
fn settled(record: &Value, settling: &[&str], subject: &str) -> Readiness {
    let status = reported_text(record, STATUS_MEMBER).unwrap_or_default();
    if status == NEPTUNE_GRAPH_AVAILABLE {
        return Readiness::Ready;
    }
    if settling.contains(&status.as_str()) {
        return Readiness::Pending(status);
    }
    Readiness::Failed(format!(
        "Neptune Analytics reports {subject} as {status}"
    ))
}

/// What one read of a resource a stack DELETED says about the teardown: a record that service
/// no longer answers is gone, and one it still answers is still being removed, in the status
/// it reports.
fn removal_of(record: Option<Value>) -> Removal {
    match record {
        None => Removal::Gone,
        Some(record) => Removal::Pending(
            reported_text(&record, STATUS_MEMBER)
                .unwrap_or_else(|| NEPTUNE_GRAPH_DELETING.to_string()),
        ),
    }
}

/// The identifier a create answered, or the refusal for an answer that carried none: there
/// would be no handle a client could call Neptune Analytics with, and nothing for the stack to
/// publish, so that is a failed create rather than a created resource. `subject` names what was
/// being created.
fn answered_identifier(answered: &Value, subject: &str) -> Result<String, Refusal> {
    match reported_text(answered, ID_MEMBER) {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the Neptune Analytics control plane answered a created {subject} without an id"
            ),
        }),
    }
}

/// The attributes one reported snapshot gives the resource: the ARN that service built for it
/// and the id it minted, read the one way every arm reads a reported attribute
/// ([`reported_attributes`]).
fn reported_snapshot_attributes(snapshot: &Value) -> BTreeMap<String, String> {
    reported_attributes(snapshot, &[("Arn", ARN_MEMBER), ("Id", ID_MEMBER)])
}

/// The attributes one reported graph gives the resource: the id that service minted, the ARN
/// it built around it and the endpoint it serves the graph on, read the same way.
fn reported_graph_attributes(graph: &Value) -> BTreeMap<String, String> {
    reported_attributes(
        graph,
        &[
            ("GraphId", ID_MEMBER),
            ("GraphArn", ARN_MEMBER),
            ("Endpoint", ENDPOINT_MEMBER),
        ],
    )
}

/// The tags one `ListTagsForResource` answer reports, as the ordered pairs the reconciliation
/// compares: that service answers them as a `TagMap`, under the lower-cased member its own
/// model spells ([`TAGS_MEMBER`]), which is the shared reading of that shape.
fn reported_tags(answered: &Value) -> Vec<(String, String)> {
    reported_tag_map(answered, TAGS_MEMBER)
}

/// The NAME the graph is created under: the `GraphName` the declaration states, or — for a
/// declaration that states none, which the type's own documentation allows ("if you don't
/// specify a graph name, a unique graph name is generated for you") — a name derived from the
/// identity CloudFormation computed for this resource, which is a function of the stack and
/// the resource's logical id and therefore unique to it.
///
/// Derived rather than taken as the generated `<stack>-<logical id>-<unique id>` name: that
/// name carries the stack's own characters, which the modelled `GraphName` pattern
/// (`^(?!g-)[a-z][a-z0-9]*(-[a-z0-9]+)*$`) does not admit in general.
fn graph_name(properties: &Map<String, Value>, resource: &StoredResource) -> String {
    if let Some(name) = properties
        .get(GRAPH_NAME_PROPERTY)
        .and_then(scalar_text)
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    let computed = resource
        .physical_resource_id
        .rsplit('-')
        .next()
        .unwrap_or(&resource.physical_resource_id);
    format!("{GENERATED_GRAPH_NAME_PREFIX}{computed}")
}

/// The `vectorSearchConfiguration` the declaration asks for, or `None` for a declaration that
/// states none: the width is read in the one spelling that member takes
/// ([`declared_number`]), and a declaration whose width is no number at all is sent as it came
/// so that operation's own validation is what refuses it.
fn vector_search_member(properties: &Map<String, Value>) -> Option<Value> {
    let declared = properties.get(VECTOR_SEARCH_PROPERTY)?;
    let dimension = declared.get(VECTOR_SEARCH_DIMENSION_PROPERTY)?;
    let carried = declared_number(dimension).unwrap_or_else(|| dimension.clone());
    Some(Value::Object(
        [(VECTOR_SEARCH_DIMENSION_MEMBER.to_string(), carried)]
            .into_iter()
            .collect(),
    ))
}

/// The URI one graph is addressed at: the collection and the `graphIdentifier` `@httpLabel`
/// escaped as a single path segment ([`percent_encoded`]), so a value outside that member's
/// own pattern is refused by that service's validation rather than addressing another path.
fn graph_uri(physical_resource_id: &str) -> String {
    format!("{GRAPHS_URI}/{}", percent_encoded(physical_resource_id))
}

/// The URI one snapshot is addressed at, built the same way from the `snapshotIdentifier`
/// label.
fn snapshot_uri(physical_resource_id: &str) -> String {
    format!("{SNAPSHOTS_URI}/{}", percent_encoded(physical_resource_id))
}
