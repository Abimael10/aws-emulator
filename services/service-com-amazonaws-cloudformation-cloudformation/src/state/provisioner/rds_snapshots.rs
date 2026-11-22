// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon RDS SNAPSHOT types: the snapshot an
//! `AWS::RDS::DBSnapshot` declares of a DB instance, and the one an `AWS::RDS::ClusterSnapshot`
//! declares of a DB cluster. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner`.
//!
//! The two types declare the same document — the snapshot's own identifier, the resource it is
//! taken OF, and the tag list — and their owning calls differ only in the member each names the
//! source by and in the action names, so ONE arm serves both and the difference between them is
//! a record ([`SnapshotCalls`]), exactly as the two parameter-group types share theirs
//! ([`super::ParameterGroupCalls`]).
//!
//! The snapshot is taken in the service that OWNS it, which is the whole point of provisioning
//! it: a client's own `DescribeDBSnapshots` or `DescribeDBClusterSnapshots` after the stack
//! reports `CREATE_COMPLETE` has to report the snapshot the template declared, and a restore
//! from it is a call on the snapshot that service really holds.
//!
//! Both creates answer the snapshot `creating` — "percent progress" is 0 and nothing can be
//! restored from it yet — so the resource's creation is held open until Amazon RDS reports it
//! `available` ([`snapshot_readiness`]), and the DELETE waits for that same status, since "the
//! DB snapshot must be in the available state to be deleted".
//!
//! Either type is identified by the ARN Amazon RDS minted for the snapshot rather than by the
//! identifier the declaration asked for: that ARN is what each type's `Ref` returns, so it is
//! this resource's physical id, and the calls that address the snapshot read the identifier
//! back out of it ([`addressed_identifier`]).

use super::*;

/// `AWS::RDS::DBSnapshot`: a snapshot of a DB INSTANCE, taken under the
/// `DBSnapshotIdentifier` the declaration states and identified by the ARN Amazon RDS mints
/// for it, which is what the type's `Ref` returns.
pub(crate) const RDS_DB_SNAPSHOT: &str = "AWS::RDS::DBSnapshot";

/// `AWS::RDS::ClusterSnapshot`: the cluster-scoped sibling, a snapshot of a DB CLUSTER taken
/// under the `DBClusterSnapshotIdentifier` the declaration states.
pub(crate) const RDS_CLUSTER_SNAPSHOT: &str = "AWS::RDS::ClusterSnapshot";

/// The identifier the Amazon RDS calls on a snapshot address it by, read off this resource's
/// physical id.
///
/// The physical id is the snapshot's ARN ([`create_snapshot`]), and an RDS snapshot ARN is
/// `arn:aws:rds:<region>:<account>:snapshot:<identifier>` (`cluster-snapshot:` for the
/// cluster-scoped one) — a snapshot identifier carries no colon of its own, so the identifier
/// is the text past the last one. A physical id that is NOT an ARN is the identifier itself,
/// which is what a resource whose create never reported an ARN carries.
fn addressed_identifier(physical_resource_id: &str) -> &str {
    match physical_resource_id.starts_with("arn:") {
        true => physical_resource_id
            .rsplit(':')
            .next()
            .unwrap_or(physical_resource_id),
        false => physical_resource_id,
    }
}

/// The status a snapshot Amazon RDS has finished taking is reported in: "available" is the
/// one a delete and a restore are accepted in (`DBSnapshot$Status`,
/// `DBClusterSnapshot$Status`).
const SNAPSHOT_AVAILABLE: &str = "available";

/// The statuses a snapshot is still being TAKEN in, in which it holds no restorable copy yet:
/// `creating` is what either create answers, and `copying` is what a snapshot copied from
/// another reports while the copy runs.
const SNAPSHOT_STATUSES_BEING_TAKEN: &[&str] = &["creating", "copying"];

/// One Amazon RDS snapshot TYPE, as the calls that take a snapshot of its source, read it and
/// delete it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotCalls {
    /// The resource type, which a property outside the create's catalogue is refused by name
    /// ([`super::query_members`]).
    pub(crate) resource_type: &'static str,
    /// The member every one of these calls addresses the snapshot by, which is also the
    /// identifier the declaration asks for.
    pub(crate) name_member: &'static str,
    /// The member the create names the resource the snapshot is taken OF in.
    pub(crate) source_member: &'static str,
    pub(crate) create: &'static str,
    pub(crate) delete: &'static str,
    pub(crate) describe: &'static str,
    /// The element the create and the describe report one snapshot as.
    pub(crate) record: &'static str,
    /// The `awsQueryError` code that service answers for a snapshot it does not hold, which
    /// is what an awsQuery client branches on.
    pub(crate) not_found: &'static str,
}

impl SnapshotCalls {
    /// The members the create takes, from Amazon RDS's own model
    /// (`smithy_query(query="operation", operation="CreateDBSnapshot,CreateDBClusterSnapshot")`):
    /// the snapshot's identifier, the source's identifier and the tag list, with the list's
    /// element name from its list shape's member `@xmlName`
    /// (`smithy_query(query="shape", shape="TagList")`).
    ///
    /// This is the catalogue the declaration of either type is walked against: the type's
    /// properties ARE these members, so a property whose name is one of them is passed through
    /// under it and one the catalogue does not carry is refused rather than dropped.
    fn create_members(&self) -> [(&'static str, QueryMember); 3] {
        [
            (self.name_member, QueryMember::Scalar),
            (self.source_member, QueryMember::Scalar),
            ("Tags", QueryMember::List("Tag")),
        ]
    }

    /// The declared properties the walk does NOT turn into a create member: the identifier
    /// CloudFormation decides, which is this resource's physical id, and the tag list it
    /// merges — the two every arm here pushes itself.
    fn properties_not_sent(&self) -> [&'static str; 2] {
        [self.name_member, "Tags"]
    }
}

/// `AWS::RDS::DBSnapshot`, whose snapshot is the one `DescribeDBSnapshots` reports and
/// `RestoreDBInstanceFromDBSnapshot` restores an instance from. A snapshot's identifier "is
/// stored as the caller spelled it", so none of these calls folds it.
pub(crate) const DB_SNAPSHOT_CALLS: SnapshotCalls = SnapshotCalls {
    resource_type: RDS_DB_SNAPSHOT,
    name_member: "DBSnapshotIdentifier",
    source_member: "DBInstanceIdentifier",
    create: "CreateDBSnapshot",
    delete: "DeleteDBSnapshot",
    describe: "DescribeDBSnapshots",
    record: "DBSnapshot",
    // `DBSnapshotNotFoundFault`'s own `awsQueryError` code
    // (`smithy_query(query="operation", operation="DeleteDBSnapshot")`).
    not_found: "DBSnapshotNotFound",
};

/// `AWS::RDS::ClusterSnapshot`, whose snapshot is the one `DescribeDBClusterSnapshots` reports
/// and `RestoreDBClusterFromSnapshot` restores a cluster from.
pub(crate) const DB_CLUSTER_SNAPSHOT_CALLS: SnapshotCalls = SnapshotCalls {
    resource_type: RDS_CLUSTER_SNAPSHOT,
    name_member: "DBClusterSnapshotIdentifier",
    source_member: "DBClusterIdentifier",
    create: "CreateDBClusterSnapshot",
    delete: "DeleteDBClusterSnapshot",
    describe: "DescribeDBClusterSnapshots",
    record: "DBClusterSnapshot",
    // `DBClusterSnapshotNotFoundFault`'s own `awsQueryError` code
    // (`smithy_query(query="operation", operation="DeleteDBClusterSnapshot")`).
    not_found: "DBClusterSnapshotNotFoundFault",
};

/// Take the snapshot a declaration of either type asks for.
///
/// The type's properties are its create's own members ([`SnapshotCalls::create_members`]),
/// with the identifier CloudFormation decides — the name the snapshot is taken under — and the
/// merged tag list pushed by this arm, the way every other Amazon RDS arm here pushes theirs.
///
/// What the create ANSWERS replaces the identity CloudFormation had computed: the snapshot's
/// ARN is Amazon RDS's to mint and it is what either type's `Ref` returns, so it becomes this
/// resource's physical id — and the calls that address the snapshot afterwards read the
/// identifier back out of it ([`addressed_identifier`]).
pub(crate) async fn create_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
    calls: SnapshotCalls,
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        &calls.create_members(),
        &[],
        &calls.properties_not_sent(),
        calls.resource_type,
    )?;
    parameters.push((
        calls.name_member.to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        "Tags",
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    let answered = call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        calls.create,
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    let arn_member = format!("{}Arn", calls.record);
    let Some(arn) = xml_element(&answered, &arn_member).filter(|arn| !arn.is_empty()) else {
        // The create answered 2xx without the ARN its output models. That ARN is this
        // resource's identity, so this is reported as a failure rather than recorded as a
        // resource identified by nothing.
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("Amazon RDS answered no {arn_member} for the snapshot it took"),
        });
    };
    // Every name the type publishes its ARN under carries the one Amazon RDS reports
    // ([`super::template::arn_attributes`]), under the conventional spellings and under the
    // member the owning API reports it as — the type's own last segment is not that member's
    // name for the cluster-scoped type, whose `DBClusterSnapshotArn` is published as the API
    // spells it.
    attributes.extend(super::template::arn_attributes(calls.resource_type, &arn));
    attributes.insert(arn_member, arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Whether Amazon RDS has finished taking the snapshot this provisioner asked for.
///
/// Either create answers the snapshot `creating`, in which it holds no restorable copy and no
/// restore of it is accepted, so the resource's creation is finished once that service reports
/// it `available`. A snapshot that is neither — one that service no longer holds, or one in a
/// status it will not come out of — is the resource's own failure.
pub(crate) async fn snapshot_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    calls: SnapshotCalls,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_snapshot(ctx, region, physical_resource_id, calls).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon RDS holds no snapshot named {physical_resource_id}"
        )));
    };
    let status = reported
        .get("Status")
        .and_then(scalar_text)
        .unwrap_or_default();
    if status == SNAPSHOT_AVAILABLE {
        return Ok(Readiness::Ready);
    }
    if SNAPSHOT_STATUSES_BEING_TAKEN.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon RDS reports the snapshot {physical_resource_id} in status {status}"
    )))
}

/// The snapshot's configuration as Amazon RDS reports it, for drift detection and for the
/// readiness arm above. A snapshot it no longer holds is `None`.
///
/// A snapshot is reported as a STRUCTURE whose scalar members carry the source it was taken
/// of, the engine it holds and its status, so those members are the actual configuration the
/// declared one is compared against ([`super::drift`]).
pub(crate) async fn read_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    calls: SnapshotCalls,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        calls.name_member.to_string(),
        addressed_identifier(physical_resource_id).to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        calls.describe,
        &parameters,
    )
    .await
    {
        Ok(answered) => Ok(Some(Value::Object(xml_scalar_children(
            &answered,
            calls.record,
        )))),
        Err(refusal) if refusal.code == calls.not_found => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// How often this module checks whether a snapshot it is waiting on has finished being taken,
/// and how many times.
///
/// The product is the longest a stack's deletion waits for ONE snapshot: an upper bound on the
/// wait, not a duration anything is owed. Amazon RDS reports a snapshot `creating` and makes
/// it available a moment later (its own transition span), so the check is far shorter than
/// that moment and the whole wait is a small multiple of it — enough that a loaded machine
/// still gets there, and bounded so a `DeleteStack` of several snapshots cannot hold its
/// request open.
const SNAPSHOT_CHECK_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(25);
/// See [`SNAPSHOT_CHECK_INTERVAL`].
const SNAPSHOT_CHECKS: usize = 24;

/// Delete the snapshot a stack took, addressed by its identifier — this resource's physical
/// id. A snapshot Amazon RDS no longer holds is already in the state the delete asks for.
///
/// "The DB snapshot must be in the available state to be deleted", so a snapshot still being
/// taken is one that service refuses the delete of: the delete waits for it to be available
/// first — the position a rollback of a stack whose snapshot was still `creating` is in — and
/// gives up waiting after a bounded number of checks, which leaves the refusal that service
/// answers as this resource's reported failure rather than inventing one here.
pub(crate) async fn delete_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    calls: SnapshotCalls,
) -> Result<(), Refusal> {
    for _ in 0..SNAPSHOT_CHECKS {
        match snapshot_readiness(ctx, region, physical_resource_id, calls).await {
            Ok(Readiness::Pending(_)) => {}
            Ok(_) | Err(_) => break,
        }
        ::tokio::time::sleep(SNAPSHOT_CHECK_INTERVAL).await;
    }
    let parameters = vec![(
        calls.name_member.to_string(),
        addressed_identifier(physical_resource_id).to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        calls.delete,
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == calls.not_found => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
