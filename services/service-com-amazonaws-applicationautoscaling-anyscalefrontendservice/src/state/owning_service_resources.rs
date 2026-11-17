// SPDX-License-Identifier: CC0-1.0

//! The resource a scalable target names, resolved by CALLING the service that OWNS it, for
//! the namespaces whose resource this crate names no state type for.
//!
//! A scalable target is a capacity dimension OF A RESOURCE — "the resource that you want to
//! scale" — so registering one has to resolve that resource where it lives: a resource the
//! owning service does not hold has no capacity to scale, so the registration is refused
//! instead of stored. That is the same rule the DynamoDB and ECS resolvers apply through
//! those services' state types ([`super::resolve_dynamodb_capacity`],
//! [`super::resolve_ecs_capacity`]) and the Keyspaces, Lambda and custom-resource resolvers
//! apply over the wire ([`super::keyspaces_tables`], [`super::lambda_functions`],
//! [`super::custom_resource_endpoint`]); this module applies it to the REMAINING namespaces
//! of the `ServiceNamespace` enum, one arm per namespace, so adding the next one is adding an
//! arm here rather than a branch at each call site.
//!
//! ## Why these are asked over the wire
//!
//! Each of them is resolved by CALLING the owning service's own read — the operation that
//! reports the resource the resource id names — in-process over the callback transport,
//! signed the way every other in-process caller signs ([`::service_common::in_process_sigv4`]).
//! The answer is that service's own, so the two can no more disagree than two readers of one
//! store can, and this crate names none of those services' types. One call per registration,
//! made with this service's store NOT held.
//!
//! ## What an answer decides
//!
//! * the owning service REFUSED with its own "no such resource" code — the resource does not
//!   exist, so the registration is refused with the `ValidationException` that names the
//!   resource kind the namespace scales;
//! * it served the read — the resource exists and the registration goes ahead;
//! * anything else, including an engine on which that service is not mounted (the router's
//!   `UnknownService`) and an operation it has not implemented — nothing was resolved, so the
//!   registration is answered on its own terms, exactly as a namespace whose owning store
//!   this service cannot reach is.
//!
//! The capacity these resources hold is not read here: the dimensions this module resolves
//! are not among the ones whose capacity this service tracks and moves
//! ([`super::remote_resources::is_remote_capacity_namespace`]), so what each answer decides is
//! existence alone.

use ::serde_json::Value;

use super::TargetKey;

/// The media type an `awsJson1_1` request and its answer carry.
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// The media type a `restJson1` request and its answer carry.
const APPLICATION_JSON: &str = "application/json";

/// The media type an `awsQuery`/`ec2Query` request carries.
const FORM_URLENCODED: &str = "application/x-www-form-urlencoded";

/// Resolve the resource `target` names in the service that owns it.
///
/// * `Ok(())` — the resource exists, or nothing this emulator could ask answered for it;
/// * `Err(message)` — the owning service holds no such resource, as the
///   `ValidationException` message naming the resource that could not be resolved.
pub async fn resolve_owning_service_resource(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    target: &TargetKey,
) -> Result<(), String> {
    let resource_id = target.resource_id.as_str();
    let resource_type = super::dimension_resource_type(&target.scalable_dimension);
    match target.service_namespace.as_str() {
        "rds" => resolve_rds_cluster(callback, region, resource_id).await,
        "neptune" => resolve_neptune_cluster(callback, region, resource_id).await,
        "elasticache" => {
            resolve_elasticache_resource(callback, region, resource_id, resource_type).await
        }
        "ec2" => resolve_spot_fleet_request(callback, region, resource_id).await,
        "sagemaker" => {
            resolve_sagemaker_resource(
                callback,
                region,
                resource_id,
                &target.scalable_dimension,
                resource_type,
            )
            .await
        }
        "kafka" => resolve_msk_cluster(callback, region, resource_id).await,
        "comprehend" => {
            resolve_comprehend_endpoint(callback, region, resource_id, &target.scalable_dimension)
                .await
        }
        "appstream" => resolve_appstream_fleet(callback, region, resource_id).await,
        "elasticmapreduce" => resolve_emr_instance_group(callback, region, resource_id).await,
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Amazon RDS and Amazon Neptune: the DB cluster a `cluster:<name>` id names
// ---------------------------------------------------------------------------

/// Amazon RDS' signing name, which is what routes the in-process call to its own handlers
/// (`sigv4Name: rds`). It speaks `awsQuery`.
const RDS_ROUTE_KEY: &str = "rds";

/// The routing key Amazon Neptune answers on: the shared `rds` signing name resolves to
/// Amazon RDS, so a request for Neptune's own view of that Query front door names `neptune`
/// (that service's `ROUTE_KEY`, its `sdkId`).
const NEPTUNE_ROUTE_KEY: &str = "neptune";

/// The API version an `awsQuery` call to either view of that front door carries
/// (`AmazonRDSv19`'s own service version).
const RDS_API_VERSION: &str = "2014-10-31";

/// The error code either view answers for a DB cluster it does not hold
/// (`DBClusterNotFoundFault`, the modelled 404 of `DescribeDBClusters`, whose `awsQueryError`
/// code is the shape's own name).
const DB_CLUSTER_NOT_FOUND: &str = "DBClusterNotFoundFault";

/// The DB cluster identifier an `rds` or `neptune` `ResourceId` names: `cluster:<name>`, the
/// form Application Auto Scaling documents for that namespace's cluster resource type.
pub fn split_cluster_resource_id(resource_id: &str) -> Option<&str> {
    let identifier = resource_id.strip_prefix("cluster:")?;
    (!identifier.is_empty() && !identifier.contains([':', '/'])).then_some(identifier)
}

/// Resolve the Aurora DB cluster an `rds` scalable target names, in Amazon RDS itself.
async fn resolve_rds_cluster(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    resolve_db_cluster(
        callback,
        RDS_ROUTE_KEY,
        region,
        resource_id,
        &rds_cluster_absent_message(resource_id),
    )
    .await
}

/// Resolve the Neptune DB cluster a `neptune` scalable target names, in Amazon Neptune
/// itself.
async fn resolve_neptune_cluster(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    resolve_db_cluster(
        callback,
        NEPTUNE_ROUTE_KEY,
        region,
        resource_id,
        &neptune_cluster_absent_message(resource_id),
    )
    .await
}

/// `DescribeDBClusters` for one identifier, against the view of that front door `route_key`
/// names.
async fn resolve_db_cluster(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    route_key: &str,
    region: &str,
    resource_id: &str,
    absent: &str,
) -> Result<(), String> {
    let Some(identifier) = split_cluster_resource_id(resource_id) else {
        return Ok(());
    };
    match query_action(
        callback,
        route_key,
        region,
        RDS_API_VERSION,
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await
    {
        Answered::Refused { code, .. } if code == DB_CLUSTER_NOT_FOUND => Err(absent.to_string()),
        _ => Ok(()),
    }
}

/// How a registration naming an Aurora DB cluster Amazon RDS does not hold is refused: the
/// resource kind the namespace scales, and the resource id the request named.
pub fn rds_cluster_absent_message(resource_id: &str) -> String {
    format!("RDS cluster doesn't exist: {resource_id}")
}

/// The same refusal for Amazon Neptune's own cluster.
pub fn neptune_cluster_absent_message(resource_id: &str) -> String {
    format!("Neptune cluster doesn't exist: {resource_id}")
}

// ---------------------------------------------------------------------------
// Amazon ElastiCache: the replication group or cache cluster an id names
// ---------------------------------------------------------------------------

/// Amazon ElastiCache's signing name (`sigv4Name: elasticache`). It speaks `awsQuery`.
const ELASTICACHE_ROUTE_KEY: &str = "elasticache";

/// The API version an `awsQuery` call to Amazon ElastiCache carries (its service version).
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// The error codes Amazon ElastiCache answers for a resource it does not hold: the modelled
/// 404s of `DescribeReplicationGroups` and `DescribeCacheClusters`, each as the Query wire
/// carries it — which is the `awsQueryError` code the model gives the error shape where it
/// gives one (`CacheClusterNotFoundFault` travels as `CacheClusterNotFound`) and the shape's
/// own name otherwise.
const REPLICATION_GROUP_NOT_FOUND: &str = "ReplicationGroupNotFoundFault";
/// See [`REPLICATION_GROUP_NOT_FOUND`].
const CACHE_CLUSTER_NOT_FOUND: &str = "CacheClusterNotFound";

/// The resource types the `elasticache` namespace scales, as the middle segment of its
/// dimensions (`elasticache:replication-group:*`, `elasticache:cache-cluster:Nodes`).
const REPLICATION_GROUP_TYPE: &str = "replication-group";
/// See [`REPLICATION_GROUP_TYPE`].
const CACHE_CLUSTER_TYPE: &str = "cache-cluster";

/// Resolve the ElastiCache resource an `elasticache` scalable target names, in Amazon
/// ElastiCache itself: which resource that is follows the dimension's own resource type.
async fn resolve_elasticache_resource(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
    resource_type: &str,
) -> Result<(), String> {
    let (operation, member, not_found, absent) = match resource_type {
        REPLICATION_GROUP_TYPE => (
            "DescribeReplicationGroups",
            "ReplicationGroupId",
            REPLICATION_GROUP_NOT_FOUND,
            replication_group_absent_message(resource_id),
        ),
        CACHE_CLUSTER_TYPE => (
            "DescribeCacheClusters",
            "CacheClusterId",
            CACHE_CLUSTER_NOT_FOUND,
            cache_cluster_absent_message(resource_id),
        ),
        _ => return Ok(()),
    };
    let Some(identifier) = split_slash_resource_id(resource_id, resource_type) else {
        return Ok(());
    };
    match query_action(
        callback,
        ELASTICACHE_ROUTE_KEY,
        region,
        ELASTICACHE_API_VERSION,
        operation,
        &[(member, identifier)],
    )
    .await
    {
        Answered::Refused { code, .. } if code == not_found => Err(absent),
        _ => Ok(()),
    }
}

/// How a registration naming a replication group Amazon ElastiCache does not hold is refused.
pub fn replication_group_absent_message(resource_id: &str) -> String {
    format!("ElastiCache replication group doesn't exist: {resource_id}")
}

/// How a registration naming a cache cluster Amazon ElastiCache does not hold is refused.
pub fn cache_cluster_absent_message(resource_id: &str) -> String {
    format!("ElastiCache cache cluster doesn't exist: {resource_id}")
}

// ---------------------------------------------------------------------------
// Amazon EC2: the Spot Fleet request a `spot-fleet-request/<id>` id names
// ---------------------------------------------------------------------------

/// Amazon EC2's signing name (`sigv4Name: ec2`). It speaks `ec2Query`.
const EC2_ROUTE_KEY: &str = "ec2";

/// The API version an `ec2Query` call carries (that service's own version).
const EC2_API_VERSION: &str = "2016-11-15";

/// The error code Amazon EC2 answers for a well-formed Spot Fleet request id this account
/// holds no fleet for.
const SPOT_FLEET_REQUEST_NOT_FOUND: &str = "InvalidSpotFleetRequestId.NotFound";

/// Resolve the Spot Fleet request an `ec2` scalable target names, in Amazon EC2 itself.
async fn resolve_spot_fleet_request(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    let Some(request_id) = split_slash_resource_id(resource_id, "spot-fleet-request") else {
        return Ok(());
    };
    // An `ec2Query` list member is numbered from one (`SpotFleetRequestId.1`), which is how
    // that protocol carries a one-element list.
    match query_action(
        callback,
        EC2_ROUTE_KEY,
        region,
        EC2_API_VERSION,
        "DescribeSpotFleetRequests",
        &[("SpotFleetRequestId.1", request_id)],
    )
    .await
    {
        Answered::Refused { code, .. } if code == SPOT_FLEET_REQUEST_NOT_FOUND => {
            Err(spot_fleet_request_absent_message(resource_id))
        }
        _ => Ok(()),
    }
}

/// How a registration naming a Spot Fleet request Amazon EC2 does not hold is refused.
pub fn spot_fleet_request_absent_message(resource_id: &str) -> String {
    format!("Spot fleet request doesn't exist: {resource_id}")
}

// ---------------------------------------------------------------------------
// Amazon SageMaker: the endpoint variant or inference component an id names
// ---------------------------------------------------------------------------

/// Amazon SageMaker's signing name (`sigv4Name: sagemaker`) and the service shape an
/// `awsJson1_1` `X-Amz-Target` names an operation under.
const SAGEMAKER_ROUTE_KEY: &str = "sagemaker";
/// See [`SAGEMAKER_ROUTE_KEY`].
const SAGEMAKER_TARGET_PREFIX: &str = "SageMaker";

/// The code Amazon SageMaker's `Describe*` answers for a resource it does not hold: those
/// operations model no error of their own, so the refusal is the request-validation envelope.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The resource types the `sagemaker` namespace scales.
const VARIANT_TYPE: &str = "variant";
/// See [`VARIANT_TYPE`].
const INFERENCE_COMPONENT_TYPE: &str = "inference-component";

/// The endpoint and variant a `sagemaker:variant:*` `ResourceId` names:
/// `endpoint/<endpoint name>/variant/<variant name>`.
pub fn split_variant_resource_id(resource_id: &str) -> Option<(&str, &str)> {
    let rest = resource_id.strip_prefix("endpoint/")?;
    let (endpoint, variant) = rest.split_once("/variant/")?;
    (!endpoint.is_empty() && !variant.is_empty() && !variant.contains('/'))
        .then_some((endpoint, variant))
}

/// Resolve the SageMaker resource a `sagemaker` scalable target names, in Amazon SageMaker
/// itself: an endpoint's production variant, or an inference component.
async fn resolve_sagemaker_resource(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
    scalable_dimension: &str,
    resource_type: &str,
) -> Result<(), String> {
    let refused =
        |reason: String| super::resource_validation_failed(resource_id, scalable_dimension, &reason);
    match resource_type {
        VARIANT_TYPE => {
            let Some((endpoint, variant)) = split_variant_resource_id(resource_id) else {
                return Ok(());
            };
            match sagemaker_call(
                callback,
                region,
                "DescribeEndpoint",
                &::serde_json::json!({"EndpointName": endpoint}),
            )
            .await
            {
                Answered::Refused { code, message } if code == VALIDATION_EXCEPTION => {
                    Err(refused(reason_or(
                        &message,
                        could_not_find(ENDPOINT_KIND, &endpoint_arn(region, endpoint)),
                    )))
                }
                // The endpoint resolved, so what is left is whether it carries the variant
                // the dimension scales: a variant no production-variant summary names has no
                // instance count to scale. An answer naming no variant at all is one this
                // reader cannot judge, so it resolves nothing.
                Answered::Served(answered) => match variant_names(&answered) {
                    Some(names) if !names.iter().any(|name| name == variant) => {
                        Err(refused(could_not_find(VARIANT_KIND, variant)))
                    }
                    _ => Ok(()),
                },
                _ => Ok(()),
            }
        }
        INFERENCE_COMPONENT_TYPE => {
            let Some(name) = split_slash_resource_id(resource_id, INFERENCE_COMPONENT_TYPE) else {
                return Ok(());
            };
            match sagemaker_call(
                callback,
                region,
                "DescribeInferenceComponent",
                &::serde_json::json!({"InferenceComponentName": name}),
            )
            .await
            {
                Answered::Refused { code, message } if code == VALIDATION_EXCEPTION => {
                    Err(refused(reason_or(
                        &message,
                        could_not_find(
                            INFERENCE_COMPONENT_KIND,
                            &inference_component_arn(region, name),
                        ),
                    )))
                }
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// The REASON a relaying refusal carries: the sentence the owning service stated, and
/// `stated_here` for an answer that stated none at all.
///
/// A refusal whose reason is the owning service's own account of what it could not resolve is
/// relayed rather than re-worded, so there is ONE statement of that fact — the one the service
/// that owns the resource makes, where a client reading that service directly reads it too.
fn reason_or(relayed: &str, stated_here: String) -> String {
    if relayed.trim().is_empty() {
        return stated_here;
    }
    relayed.trim().to_string()
}

/// The production variants a described endpoint reports, by name.
fn variant_names(answered: &Value) -> Option<Vec<String>> {
    let variants = answered.get("ProductionVariants")?.as_array()?;
    Some(
        variants
            .iter()
            .filter_map(|variant| variant.get("VariantName"))
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

/// One `awsJson1_1` call to Amazon SageMaker.
async fn sagemaker_call(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    operation: &str,
    request: &Value,
) -> Answered {
    json_rpc(
        callback,
        SAGEMAKER_ROUTE_KEY,
        SAGEMAKER_TARGET_PREFIX,
        AWS_JSON_1_1,
        region,
        operation,
        request,
    )
    .await
}

/// How Amazon SageMaker states that a reference resolved to nothing: the resource kind it
/// words that reference by, then the identifier, in double quotes.
///
/// This is the REASON clause of the refusal, which the envelope
/// ([`super::resource_validation_failed`]) carries: the owning service's own account of what
/// could not be resolved, the same shape the `lambda` namespace's reason takes.
pub fn could_not_find(kind: &str, identifier: &str) -> String {
    format!("Could not find {kind} \"{identifier}\".")
}

/// The kind label [`could_not_find`] names an endpoint by.
const ENDPOINT_KIND: &str = "endpoint";

/// The kind label [`could_not_find`] names a production variant by.
const VARIANT_KIND: &str = "variant";

/// The kind label [`could_not_find`] names an inference component by.
const INFERENCE_COMPONENT_KIND: &str = "inference component";

/// The ARN of one SageMaker endpoint: `arn:aws:sagemaker:<region>:<account>:endpoint/<name>`,
/// the form that service's `EndpointArn` declares — which is how the endpoint a scalable
/// target names is referred to across services.
fn endpoint_arn(region: &str, endpoint_name: &str) -> String {
    ::emulator_server::emulator_arn(
        SAGEMAKER_ARN_NAMESPACE,
        region,
        &format!("endpoint/{endpoint_name}"),
    )
}

/// The ARN of one inference component, the sibling form of [`endpoint_arn`].
fn inference_component_arn(region: &str, name: &str) -> String {
    ::emulator_server::emulator_arn(
        SAGEMAKER_ARN_NAMESPACE,
        region,
        &format!("inference-component/{name}"),
    )
}

/// The namespace Amazon SageMaker's ARNs carry (that model's `arnNamespace: sagemaker`),
/// which is its signing name rather than its longer endpoint prefix.
const SAGEMAKER_ARN_NAMESPACE: &str = "sagemaker";

// ---------------------------------------------------------------------------
// Amazon MSK: the cluster a `kafka` resource ARN names
// ---------------------------------------------------------------------------

/// Amazon MSK's signing name (`sigv4Name: kafka`). It speaks `restJson1`.
const KAFKA_ROUTE_KEY: &str = "kafka";

/// The error code Amazon MSK answers for a cluster it does not hold (`NotFoundException`,
/// the modelled 404 of `DescribeCluster`).
const NOT_FOUND_EXCEPTION: &str = "NotFoundException";

/// Resolve the MSK cluster a `kafka` scalable target names, in Amazon MSK itself.
///
/// A `kafka` resource id IS the cluster's ARN, which `DescribeCluster` takes as an
/// `@httpLabel` — so its `:` and `/` are percent-encoded to keep it one path segment.
async fn resolve_msk_cluster(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    if !resource_id.starts_with("arn:") {
        return Ok(());
    }
    match rest_get(
        callback,
        KAFKA_ROUTE_KEY,
        region,
        &format!("/v1/clusters/{}", percent_encoded(resource_id)),
    )
    .await
    {
        Answered::Refused { code, message } if code == NOT_FOUND_EXCEPTION => {
            Err(super::scalable_target_validation_failed(&reason_or(
                &message,
                msk_cluster_absent_reason(),
            )))
        }
        _ => Ok(()),
    }
}

/// The reason a registration naming a cluster Amazon MSK does not hold is refused with, for
/// an answer that stated none of its own: that the resource the request asked to scale is not
/// one the owning service holds.
fn msk_cluster_absent_reason() -> String {
    "The requested resource doesn\u{2019}t exist.".to_string()
}

// ---------------------------------------------------------------------------
// Amazon Comprehend: the endpoint a `comprehend` resource ARN names
// ---------------------------------------------------------------------------

/// Amazon Comprehend's signing name (`sigv4Name: comprehend`) and the service shape its
/// `awsJson1_1` `X-Amz-Target` names an operation under.
const COMPREHEND_ROUTE_KEY: &str = "comprehend";
/// See [`COMPREHEND_ROUTE_KEY`].
const COMPREHEND_TARGET_PREFIX: &str = "Comprehend_20171127";

/// The error code Amazon Comprehend answers for an endpoint it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of `DescribeEndpoint`).
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// Resolve the inference endpoint a `comprehend` scalable target names, in Amazon Comprehend
/// itself. A `comprehend` resource id IS that endpoint's ARN.
async fn resolve_comprehend_endpoint(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
    scalable_dimension: &str,
) -> Result<(), String> {
    if !resource_id.starts_with("arn:") {
        return Ok(());
    }
    match json_rpc(
        callback,
        COMPREHEND_ROUTE_KEY,
        COMPREHEND_TARGET_PREFIX,
        AWS_JSON_1_1,
        region,
        "DescribeEndpoint",
        &::serde_json::json!({"EndpointArn": resource_id}),
    )
    .await
    {
        Answered::Refused { code, message } if code == RESOURCE_NOT_FOUND_EXCEPTION => {
            Err(super::resource_validation_failed(
                resource_id,
                scalable_dimension,
                &reason_or(&message, comprehend_endpoint_absent_reason()),
            ))
        }
        _ => Ok(()),
    }
}

/// The reason a registration naming an endpoint Amazon Comprehend does not hold is refused
/// with, for an answer that stated none of its own.
fn comprehend_endpoint_absent_reason() -> String {
    "RESOURCE_NOT_FOUND: Could not find specified resource.".to_string()
}

// ---------------------------------------------------------------------------
// Amazon AppStream 2.0: the fleet a `fleet/<name>` id names
// ---------------------------------------------------------------------------

/// Amazon AppStream 2.0's signing name (`sigv4Name: appstream`) and the service shape its
/// `awsJson1_1` `X-Amz-Target` names an operation under.
const APPSTREAM_ROUTE_KEY: &str = "appstream";
/// See [`APPSTREAM_ROUTE_KEY`].
const APPSTREAM_TARGET_PREFIX: &str = "PhotonAdminProxyService";

/// Resolve the fleet an `appstream` scalable target names, in Amazon AppStream 2.0 itself.
async fn resolve_appstream_fleet(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    let Some(name) = split_slash_resource_id(resource_id, "fleet") else {
        return Ok(());
    };
    match json_rpc(
        callback,
        APPSTREAM_ROUTE_KEY,
        APPSTREAM_TARGET_PREFIX,
        AWS_JSON_1_1,
        region,
        "DescribeFleets",
        &::serde_json::json!({"Names": [name]}),
    )
    .await
    {
        Answered::Refused { code, .. } if code == RESOURCE_NOT_FOUND_EXCEPTION => {
            Err(appstream_fleet_absent_message(resource_id))
        }
        _ => Ok(()),
    }
}

/// How a registration naming a fleet Amazon AppStream 2.0 does not hold is refused.
pub fn appstream_fleet_absent_message(resource_id: &str) -> String {
    format!("Appstream fleet doesn't exist: {resource_id}")
}

// ---------------------------------------------------------------------------
// Amazon EMR: the instance group an `instancegroup/<cluster>/<group>` id names
// ---------------------------------------------------------------------------

/// Amazon EMR's signing name (`sigv4Name: elasticmapreduce`) and the service shape its
/// `awsJson1_1` `X-Amz-Target` names an operation under.
const EMR_ROUTE_KEY: &str = "elasticmapreduce";
/// See [`EMR_ROUTE_KEY`].
const EMR_TARGET_PREFIX: &str = "ElasticMapReduce";

/// The cluster id and instance group id an `elasticmapreduce` `ResourceId` names:
/// `instancegroup/<cluster id>/<instance group id>`.
pub fn split_instance_group_resource_id(resource_id: &str) -> Option<(&str, &str)> {
    let rest = resource_id.strip_prefix("instancegroup/")?;
    let (cluster, group) = rest.split_once('/')?;
    (!cluster.is_empty() && !group.is_empty() && !group.contains('/')).then_some((cluster, group))
}

/// Resolve the instance group an `elasticmapreduce` scalable target names, in Amazon EMR
/// itself.
///
/// The instance group is resolved through the cluster that runs it, which is how EMR itself
/// addresses one: a cluster that does not exist runs no group, and a cluster that does has to
/// list the group the resource id names. A listing whose page does not name it and that
/// reports more groups to come resolves nothing, since the group may be on a later page.
async fn resolve_emr_instance_group(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    resource_id: &str,
) -> Result<(), String> {
    let Some((cluster_id, group_id)) = split_instance_group_resource_id(resource_id) else {
        return Ok(());
    };
    match json_rpc(
        callback,
        EMR_ROUTE_KEY,
        EMR_TARGET_PREFIX,
        AWS_JSON_1_1,
        region,
        "ListInstanceGroups",
        &::serde_json::json!({"ClusterId": cluster_id}),
    )
    .await
    {
        // The cluster is not one this account holds, so neither is the group.
        Answered::Refused { code, .. } if code == INVALID_REQUEST_EXCEPTION => {
            Err(emr_instance_group_absent_message(resource_id))
        }
        Answered::Served(answered) => {
            let listed = instance_group_ids(&answered);
            if listed.iter().any(|id| id == group_id) {
                return Ok(());
            }
            match answered.get("Marker").and_then(Value::as_str) {
                Some(_) => Ok(()),
                None => Err(emr_instance_group_absent_message(resource_id)),
            }
        }
        _ => Ok(()),
    }
}

/// The error code Amazon EMR answers for a cluster it does not hold
/// (`InvalidRequestException`, the modelled 400 of `ListInstanceGroups`).
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The instance groups a listing reports, by id.
fn instance_group_ids(answered: &Value) -> Vec<String> {
    answered
        .get("InstanceGroups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group.get("Id"))
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// How a registration naming an instance group Amazon EMR does not hold is refused.
pub fn emr_instance_group_absent_message(resource_id: &str) -> String {
    format!("EMR instance group doesn't exist: {resource_id}")
}

// ---------------------------------------------------------------------------
// Reading one resource id, and the three transports these reads are made over
// ---------------------------------------------------------------------------

/// The unique identifier of a `<resource type>/<identifier>` resource id, which is the form
/// most namespaces' single-segment resource types take.
pub fn split_slash_resource_id<'a>(resource_id: &'a str, resource_type: &str) -> Option<&'a str> {
    let identifier = resource_id.strip_prefix(resource_type)?.strip_prefix('/')?;
    (!identifier.is_empty() && !identifier.contains(['/', ':'])).then_some(identifier)
}

/// What one call to an owning service answered.
#[derive(Debug)]
enum Answered {
    /// It served the read, and this is the document it answered (`Null` for an answer that
    /// carries none this reader can parse, which an XML protocol's does).
    Served(Value),
    /// It refused, naming this code, and stating this as the message.
    Refused {
        /// The shape the refusal named itself by, which is what a caller branches on.
        code: String,
        /// The sentence it stated, which is the owning service's own account of why: the
        /// REASON a relaying refusal carries.
        message: String,
    },
    /// Nothing answered: the call could not be made, no mounted service claimed it, or the
    /// answer named no code at all.
    Unreachable,
}

/// One `awsJson1_0`/`awsJson1_1` call: the operation in the `X-Amz-Target` header and the
/// input document as the body.
async fn json_rpc(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    route_key: &str,
    target_prefix: &str,
    media_type: &str,
    region: &str,
    operation: &str,
    request: &Value,
) -> Answered {
    let Ok(body) = ::serde_json::to_vec(request) else {
        return Answered::Unreachable;
    };
    let Ok(call) = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", service_host(route_key, region))
        .header("content-type", media_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(body)
    else {
        return Answered::Unreachable;
    };
    answered(callback, route_key, region, call).await
}

/// One `restJson1` read: the method and path the operation declares, with no body.
async fn rest_get(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    route_key: &str,
    region: &str,
    path: &str,
) -> Answered {
    let Ok(call) = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", service_host(route_key, region))
        .header("accept", APPLICATION_JSON)
        .body(Vec::new())
    else {
        return Answered::Unreachable;
    };
    answered(callback, route_key, region, call).await
}

/// One `awsQuery`/`ec2Query` call: the action, the API version and the operation's parameters
/// as the fields of a form body.
async fn query_action(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    route_key: &str,
    region: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&percent_encoded(value));
    }
    let Ok(call) = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", service_host(route_key, region))
        .header("content-type", FORM_URLENCODED)
        .body(body.into_bytes())
    else {
        return Answered::Unreachable;
    };
    answered(callback, route_key, region, call).await
}

/// Sign and dispatch one call through this service's single in-process caller, and read what
/// the called service answered.
async fn answered(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    route_key: &str,
    region: &str,
    call: ::http::Request<Vec<u8>>,
) -> Answered {
    let Some(answer) = super::in_process_calls::call(callback, route_key, region, call).await else {
        return Answered::Unreachable;
    };
    if answer.is_success() {
        return Answered::Served(answer.document().unwrap_or(Value::Null));
    }
    // The emulator's own front door refusing a call nothing served is not an answer about the
    // resource: an engine that does not hold the owning service resolves nothing.
    if answer.unserved() {
        return Answered::Unreachable;
    }
    // The code and the sentence are read where the called service's protocol carries them,
    // which is `in_process_calls`' own reading ([`super::in_process_calls::Answered`]).
    let code = answer.named_code();
    if code.is_empty() {
        return Answered::Unreachable;
    }
    Answered::Refused {
        code,
        message: answer.stated_message(),
    }
}

/// The host a call to `route_key` is addressed on, which is also what names the service a
/// host-addressed request resolves to.
fn service_host(route_key: &str, region: &str) -> String {
    format!("{route_key}.{region}.amazonaws.com")
}

/// Percent-encode one URI component or form field value, through the maintained coder for
/// that encoding: every byte outside the ASCII alphanumerics is escaped — the conservative
/// set, and over-encoding an unreserved byte is still correct (RFC 3986 section 2.1) — so a
/// value carrying an ARN's `:` and `/` stays one path segment and one form field.
fn percent_encoded(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a `cluster:<name>` resource id names ONE DB cluster; anything else names none.
    #[test]
    fn a_cluster_resource_id_names_one_db_cluster() {
        /// (case, the resource id, the identifier it names)
        type Row<'a> = (&'a str, &'a str, Option<&'a str>);
        let rows: &[Row<'_>] = &[
            ("a cluster", "cluster:orders-db", Some("orders-db")),
            ("a name carrying digits", "cluster:db-2", Some("db-2")),
            ("the slash form", "cluster/orders-db", None),
            ("a further segment", "cluster:orders:db", None),
            ("an empty name", "cluster:", None),
            ("another namespace's id", "table/orders", None),
            ("nothing at all", "", None),
        ];
        for (case, resource_id, expected) in rows {
            let actual = split_cluster_resource_id(resource_id);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a `<resource type>/<identifier>` resource id names ONE resource of that type.
    #[test]
    fn a_slash_resource_id_names_one_resource_of_its_type() {
        /// (case, the resource id, the resource type, the identifier it names)
        type Row<'a> = (&'a str, &'a str, &'a str, Option<&'a str>);
        let rows: &[Row<'_>] = &[
            (
                "a replication group",
                "replication-group/orders",
                "replication-group",
                Some("orders"),
            ),
            (
                "a cache cluster",
                "cache-cluster/sessions",
                "cache-cluster",
                Some("sessions"),
            ),
            (
                "a Spot Fleet request",
                "spot-fleet-request/sfr-1234",
                "spot-fleet-request",
                Some("sfr-1234"),
            ),
            (
                "an AppStream fleet",
                "fleet/desktops",
                "fleet",
                Some("desktops"),
            ),
            (
                "an inference component",
                "inference-component/ic-1",
                "inference-component",
                Some("ic-1"),
            ),
            (
                "another resource type",
                "fleet/desktops",
                "cache-cluster",
                None,
            ),
            ("no identifier", "fleet/", "fleet", None),
            ("a further segment", "fleet/a/b", "fleet", None),
            ("the colon form", "fleet:desktops", "fleet", None),
            ("nothing at all", "", "fleet", None),
        ];
        for (case, resource_id, resource_type, expected) in rows {
            let actual = split_slash_resource_id(resource_id, resource_type);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a `sagemaker:variant:*` resource id names ONE variant of ONE endpoint.
    #[test]
    fn a_variant_resource_id_names_one_variant_of_one_endpoint() {
        /// (case, the resource id, the endpoint and variant it names)
        type Row<'a> = (&'a str, &'a str, Option<(&'a str, &'a str)>);
        let rows: &[Row<'_>] = &[
            (
                "a variant of an endpoint",
                "endpoint/my-endpoint/variant/AllTraffic",
                Some(("my-endpoint", "AllTraffic")),
            ),
            ("no variant segment", "endpoint/my-endpoint", None),
            ("an empty variant", "endpoint/my-endpoint/variant/", None),
            ("an empty endpoint", "endpoint//variant/AllTraffic", None),
            ("a variant without its endpoint", "variant/AllTraffic", None),
            ("nothing at all", "", None),
        ];
        for (case, resource_id, expected) in rows {
            let actual = split_variant_resource_id(resource_id);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: an `elasticmapreduce` resource id names ONE instance group of ONE cluster.
    #[test]
    fn an_instance_group_resource_id_names_one_group_of_one_cluster() {
        /// (case, the resource id, the cluster and group it names)
        type Row<'a> = (&'a str, &'a str, Option<(&'a str, &'a str)>);
        let rows: &[Row<'_>] = &[
            (
                "a group of a cluster",
                "instancegroup/j-1234ABCD/ig-5678EFGH",
                Some(("j-1234ABCD", "ig-5678EFGH")),
            ),
            ("no group segment", "instancegroup/j-1234ABCD", None),
            ("an empty group", "instancegroup/j-1234ABCD/", None),
            ("a further segment", "instancegroup/j-1/ig-1/extra", None),
            ("nothing at all", "", None),
        ];
        for (case, resource_id, expected) in rows {
            let actual = split_instance_group_resource_id(resource_id);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: where the owning service states what could not be resolved, the refusal's REASON
    /// clause is that statement — the resource kind that service words the reference by and
    /// the identifier it looked for, which for a resource referred to across services is its
    /// ARN in the Region the request was addressed to.
    #[test]
    fn a_sagemaker_reason_is_that_services_own_account_of_what_it_could_not_find() {
        /// (case, the kind, the identifier, the reason)
        type Row<'a> = (&'a str, &'a str, String, String);
        let rows: &[Row<'_>] = &[
            (
                "an endpoint, named by its ARN",
                ENDPOINT_KIND,
                endpoint_arn("us-east-1", "my-endpoint"),
                format!(
                    "Could not find endpoint \
                     \"arn:aws:sagemaker:us-east-1:{}:endpoint/my-endpoint\".",
                    ::emulator_server::EMULATOR_ACCOUNT_ID
                ),
            ),
            (
                "an inference component in another Region",
                INFERENCE_COMPONENT_KIND,
                inference_component_arn("eu-west-1", "ic-1"),
                format!(
                    "Could not find inference component \
                     \"arn:aws:sagemaker:eu-west-1:{}:inference-component/ic-1\".",
                    ::emulator_server::EMULATOR_ACCOUNT_ID
                ),
            ),
            (
                "a variant of an endpoint, named as the request named it",
                VARIANT_KIND,
                "AllTraffic".to_string(),
                "Could not find variant \"AllTraffic\".".to_string(),
            ),
        ];
        for (case, kind, identifier, expected) in rows {
            let actual = could_not_find(kind, identifier);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the reason a relaying refusal carries is the sentence the owning service stated,
    /// and the one this service states for itself only where that answer stated none.
    #[test]
    fn a_relayed_reason_is_the_owning_services_own_sentence() {
        /// (case, what the owning service stated, the reason the refusal carries)
        type Row<'a> = (&'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "a service that stated its own sentence",
                "The requested resource doesn\u{2019}t exist.",
                "The requested resource doesn\u{2019}t exist.",
            ),
            (
                "a sentence carrying surrounding whitespace",
                "  Could not find endpoint \"e\".  ",
                "Could not find endpoint \"e\".",
            ),
            ("an answer that stated none", "", "stated here"),
            ("an answer that stated only space", "   ", "stated here"),
        ];
        for (case, relayed, expected) in rows {
            let actual = reason_or(relayed, "stated here".to_string());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a value that travels in a URI path or a form field carries only unreserved
    /// bytes, so every other byte of it is percent-encoded.
    #[test]
    fn a_component_travels_percent_encoded() {
        /// (case, the value, its encoding)
        type Row<'a> = (&'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            ("a plain identifier", "orders2", "orders2"),
            ("a hyphenated identifier", "orders-db", "orders%2Ddb"),
            (
                "an ARN, whose separators are reserved",
                "arn:aws:kafka:us-east-1:000000000000:cluster/demo/1",
                "arn%3Aaws%3Akafka%3Aus%2Deast%2D1%3A000000000000%3Acluster%2Fdemo%2F1",
            ),
            ("nothing at all", "", ""),
        ];
        for (case, value, expected) in rows {
            let actual = percent_encoded(value);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal of a resource the owning service does not hold names the resource
    /// KIND the namespace scales and the resource id the request supplied — no other value.
    #[test]
    fn an_absent_resource_is_refused_by_its_kind_and_the_id_the_request_named() {
        /// (case, the message, the resource id it names)
        type Row<'a> = (&'a str, String, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "an RDS cluster",
                rds_cluster_absent_message("cluster:orders-db"),
                "cluster:orders-db",
            ),
            (
                "a Neptune cluster",
                neptune_cluster_absent_message("cluster:graph"),
                "cluster:graph",
            ),
            (
                "a replication group",
                replication_group_absent_message("replication-group/orders"),
                "replication-group/orders",
            ),
            (
                "a cache cluster",
                cache_cluster_absent_message("cache-cluster/sessions"),
                "cache-cluster/sessions",
            ),
            (
                "a Spot Fleet request",
                spot_fleet_request_absent_message("spot-fleet-request/sfr-1"),
                "spot-fleet-request/sfr-1",
            ),
            (
                "an Appstream fleet",
                appstream_fleet_absent_message("fleet/desktops"),
                "fleet/desktops",
            ),
            (
                "an EMR instance group",
                emr_instance_group_absent_message("instancegroup/j-1/ig-1"),
                "instancegroup/j-1/ig-1",
            ),
        ];
        for (case, message, resource_id) in rows {
            assert!(
                message.ends_with(&format!(": {resource_id}")),
                "case {case:?}: expected the message to end with the resource id \
                 {resource_id:?}, got {message:?}"
            );
            assert!(
                message.contains("doesn't exist"),
                "case {case:?}: expected the message to say the resource does not exist, got \
                 {message:?}"
            );
        }
    }
}
