// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINTS an `AWS::RDS::DBCluster` and an `AWS::RDS::DBInstance` publish, read off
//! Amazon RDS once it reports the resource serving. One file of [`super`]'s per-type arms,
//! reached by each type's `ready_attributes`.
//!
//! Both types publish their connection endpoint as a return value — `Endpoint.Address` and
//! `Endpoint.Port` on either, plus `ReadEndpoint.Address` on the cluster — and a client that
//! reads one out of a stack output CONNECTS to it. So the value a stack publishes has to be
//! the address the service that owns the resource published for it, which is the address its
//! listener is really bound to: a handle computed here would be a name nothing resolves and
//! no database answers on.
//!
//! Neither create can answer them: `CreateDBCluster` and `CreateDBInstance` leave the
//! resource `creating`, and a resource that is still coming up has no endpoint yet. They are
//! therefore read when that service reports the resource up, exactly as Amazon Redshift's
//! cluster endpoint is ([`super::redshift_cluster_ready_attributes`]).
//!
//! What a declared `Port` means here is the owning service's answer and not this module's: the
//! core owns the port range a DB resource's engine listener is bound in, so Amazon RDS RECORDS
//! the configured port and PUBLISHES the port its listener landed on, and the two differ for a
//! resource whose engine this emulator serves a wire for. The stack publishes the published
//! one — the port a client dials beside the host above it — so a template that declares a port
//! reads its own value back off Amazon RDS rather than out of a stack output. Where the real
//! service is asked for a resource it serves on the declared port, it answers that same value
//! for both, and so does this: the rule is "what the owning service published", not "the
//! listener's".

use super::*;

/// The endpoint attributes `AWS::RDS::DBCluster` publishes, read off the cluster once Amazon
/// RDS reports it `available`.
///
/// A DB cluster reports its two endpoints as SCALAR members of its own structure — `Endpoint`
/// ("the connection endpoint for the primary instance of the DB cluster"), `ReaderEndpoint`
/// ("the reader endpoint for the DB cluster") and `Port` ("the port that the database engine
/// is listening on") (`smithy_query(query="shape", shape="DBCluster")`) — and the type
/// publishes them under the three names its own return values carry.
pub(crate) async fn db_cluster_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut attributes = BTreeMap::new();
    let Some(reported) =
        super::rds_db_clusters::read_db_cluster(ctx, region, physical_resource_id).await?
    else {
        return Ok(attributes);
    };
    for (attribute, member) in [
        ("Endpoint.Address", "Endpoint"),
        ("Endpoint.Port", "Port"),
        ("ReadEndpoint.Address", "ReaderEndpoint"),
    ] {
        let Some(value) = reported
            .get(member)
            .and_then(scalar_text)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        attributes.insert(attribute.to_string(), value);
    }
    Ok(attributes)
}

/// The endpoint attributes `AWS::RDS::DBInstance` publishes, read off the instance once
/// Amazon RDS reports it `available`.
///
/// A DB instance reports its endpoint as the STRUCTURE `Endpoint`, whose `Address` is "the
/// DNS address of the DB instance" and whose `Port` is "the port that the database engine is
/// listening on" (`smithy_query(query="shape", shape="Endpoint")`), so the two are read out
/// of that member by name — the same reading the create takes of its own answer
/// ([`super::create_db_instance`]), which an
/// instance that was still `creating` carried neither of.
pub(crate) async fn db_instance_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut attributes = BTreeMap::new();
    let parameters = vec![(
        "DBInstanceIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeDBInstances",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        // An instance that service no longer holds publishes no endpoint: the readiness arm
        // beside this one is what reports the resource's own failure for it.
        Err(refusal) if refusal.code == super::DB_INSTANCE_NOT_FOUND => return Ok(attributes),
        Err(refusal) => return Err(refusal),
    };
    let endpoint = xml_scalar_children(&answered, "Endpoint");
    for member in ["Address", "Port"] {
        let Some(value) = endpoint
            .get(member)
            .and_then(scalar_text)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        attributes.insert(format!("Endpoint.{member}"), value);
    }
    Ok(attributes)
}
