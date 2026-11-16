// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the AWS Batch JOB DEFINITION: the revision an
//! `AWS::Batch::JobDefinition` declares, REGISTERED in AWS Batch itself. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`, beside the compute
//! environment, job queue and service environment arms the same owning service has
//! ([`super::network_and_delivery`]).
//!
//! ## Why the revision is really registered, and why its identity is the ARN
//!
//! "Ref returns the job definition ARN", and that ARN — `jobDefinitionArn`, a `@required`
//! member of `RegisterJobDefinition`'s own output beside the `jobDefinitionName` — is the
//! handle every AWS Batch call that runs or reads a definition takes: `SubmitJob`'s
//! `jobDefinition`, `DescribeJobDefinitions`' `jobDefinitions`, and the `Resource` element
//! of an IAM policy that scopes `batch:SubmitJob` to one definition. A stack that published
//! the definition's bare NAME would hand a client a value that is no ARN at all, and one
//! that reported the resource created while nothing registered it would publish an ARN that
//! resolves to nothing.
//!
//! The ARN also carries the REVISION, and the revision is that service's to allocate — "the
//! revision of the job definition", one past the highest revision the account already holds
//! under the name — so the identity this arm reports is the one AWS Batch answered
//! ([`super::super::template::identity_is_service_assigned`] names the type) and the ARN
//! CloudFormation's naming rules computed is only the stand-in a declaration is resolved
//! against until then.
//!
//! ## The properties
//!
//! The type's properties are `RegisterJobDefinition`'s own members spelled with an
//! upper-case initial, all the way down ([`register_members`]), with three exceptions this
//! module carries:
//!
//! * `JobDefinitionName` ADDRESSES the registration rather than travelling beside the
//!   others: it is the name the revision belongs to, read back off the ARN that is this
//!   resource's physical id ([`declared_job_definition_name`]), so a declaration that names
//!   none is registered under the name CloudFormation generated for the resource;
//! * `Tags` is rebuilt rather than carried, so the revision carries the tags CloudFormation
//!   gives the resource beside the declared ones, as the tag MAP that call takes;
//! * the maps whose keys are a caller's own DATA rather than modelled member names
//!   ([`DATA_MAP_MEMBERS`], and the `Parameters` map itself) are carried through verbatim, so
//!   a parameter placeholder, a log-driver option, a Kubernetes label or annotation and a
//!   container resource name keep the key the template wrote.
//!
//! A declared property this arm does not carry is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a revision registered with less than its declaration
//! asked for is the shallow success this arm exists to avoid.

use super::*;

/// The URIs the three calls this arm makes are bound to, from the AWS Batch model's own
/// `@http` traits (`RegisterJobDefinition`, `DeregisterJobDefinition` and
/// `DescribeJobDefinitions` are each a `POST`).
const BATCH_REGISTER_JOB_DEFINITION_URI: &str = "/v1/registerjobdefinition";
const BATCH_DEREGISTER_JOB_DEFINITION_URI: &str = "/v1/deregisterjobdefinition";
pub(super) const BATCH_DESCRIBE_JOB_DEFINITIONS_URI: &str = "/v1/describejobdefinitions";

/// The member `DescribeJobDefinitions` takes its references in and answers its records
/// under, which is the one name [`batch_described`] needs for both halves of that read.
pub(super) const BATCH_JOB_DEFINITIONS_MEMBER: &str = "jobDefinitions";

/// The member `RegisterJobDefinition` answers the registered revision's ARN under
/// (`jobDefinitionArn`, `@required`), and the attribute `AWS::Batch::JobDefinition`
/// publishes it through `Fn::GetAtt`.
const JOB_DEFINITION_ARN_MEMBER: &str = "jobDefinitionArn";
const JOB_DEFINITION_ARN_ATTRIBUTE: &str = "JobDefinitionArn";

/// The property naming the revision's own job definition, which addresses the registration
/// rather than travelling beside the other members.
const JOB_DEFINITION_NAME_PROPERTY: &str = "JobDefinitionName";

/// The property carrying the resource's own tags, which is rebuilt rather than carried.
const TAGS_PROPERTY: &str = "Tags";

/// The property whose value is a MAP of the template's own keys — "parameters are specified
/// as a key-value pair mapping" (`RegisterJobDefinition$parameters`) — so its entries are
/// carried through exactly as the template wrote them.
const PARAMETERS_PROPERTY: &str = "Parameters";

/// Every property `AWS::Batch::JobDefinition` publishes that travels to
/// `RegisterJobDefinition` as a member of its own, in the order that call's members are
/// modelled.
///
/// Source: `smithy_query(query="operation", operation="RegisterJobDefinition")` against the
/// type's property names, which are those members with an upper-case initial.
/// [`JOB_DEFINITION_NAME_PROPERTY`] and [`TAGS_PROPERTY`] are the two properties NOT here,
/// each for the reason this module's own docs record.
const CARRIED_PROPERTIES: &[&str] = &[
    "Type",
    PARAMETERS_PROPERTY,
    "SchedulingPriority",
    "ContainerProperties",
    "NodeProperties",
    "RetryStrategy",
    "PropagateTags",
    "Timeout",
    "PlatformCapabilities",
    "EksProperties",
    "EcsProperties",
    "ConsumableResourceProperties",
];

/// Every property of the type, which is [`CARRIED_PROPERTIES`] plus the two this module
/// applies its own way. A declared property this list does not carry is one no call here
/// applies, and is refused rather than dropped.
const JOB_DEFINITION_PROPERTIES: &[&str] = &[
    "ConsumableResourceProperties",
    "ContainerProperties",
    "EcsProperties",
    "EksProperties",
    JOB_DEFINITION_NAME_PROPERTY,
    "NodeProperties",
    PARAMETERS_PROPERTY,
    "PlatformCapabilities",
    "PropagateTags",
    "RetryStrategy",
    "SchedulingPriority",
    TAGS_PROPERTY,
    "Timeout",
    "Type",
];

/// The API members that are MAPS whose keys are a caller's own data rather than modelled
/// member names, so the entries below one are carried through exactly as the template wrote
/// them.
///
/// Source: the map-typed members of the shapes a job definition reaches
/// (`smithy_query(query="shape", shape="LogConfiguration,EksMetadata,\
/// EksContainerResourceRequirements")`): a log configuration's `options` ("the configuration
/// options to send to the log driver"), the `labels` and `annotations` of a pod's
/// `EksMetadata`, and the `limits` and `requests` of a Kubernetes container's resource
/// requirements, whose keys are resource names such as `nvidia.com/gpu`. Each is named here
/// by the MEMBER, so the test applies after the key has been re-cased.
const DATA_MAP_MEMBERS: &[&str] = &["annotations", "labels", "limits", "options", "requests"];

/// The segment a job-definition ARN carries its name and revision after
/// (`arn:<partition>:batch:<region>:<account>:job-definition/<name>:<revision>`).
const JOB_DEFINITION_ARN_SEGMENT: &str = "job-definition/";

/// Register the revision an `AWS::Batch::JobDefinition` declares, in AWS Batch itself — so a
/// client that deployed the stack can pass `!Ref <job definition>` straight to `SubmitJob`,
/// to `DescribeJobDefinitions`, or to the `Resource` of a policy that scopes
/// `batch:SubmitJob` to it.
///
/// What this answers as the resource's identity is the ARN AWS Batch reported, revision and
/// all: that service allocates the revision, so the stand-in the naming rules computed is
/// replaced by the identifier really in force.
///
/// `Err` is that service's own refusal of the registration — a Fargate definition with no
/// execution role, a container properties object naming an EC2-only member, a retry strategy
/// outside its documented range — which is what the resource fails with, because a
/// registration that did not answer 2xx registered nothing.
pub(super) async fn create_job_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, JOB_DEFINITION_PROPERTIES)?;
    let mut request = register_members(&resource.properties)?;
    request.insert(
        lower_initial(JOB_DEFINITION_NAME_PROPERTY),
        Value::String(declared_job_definition_name(resource)),
    );
    // The revision carries the tags its declaration asks for plus the ones CloudFormation
    // gives the resource, as `RegisterJobDefinition`'s own `tags` MAP — so a later
    // `ListTagsForResource` on the ARN the stack publishes answers with the stack's own tags.
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_rest_json(
        ctx,
        BATCH_SIGNING_NAME,
        region,
        "POST",
        BATCH_REGISTER_JOB_DEFINITION_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let Some(arn) = answered
        .get(JOB_DEFINITION_ARN_MEMBER)
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
    else {
        // A registration that identified nothing is no created resource: the resource fails
        // rather than the stack publishing an identifier no call of that service takes.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "AWS Batch reported no {JOB_DEFINITION_ARN_MEMBER} for the {} it registered",
                resource.resource_type
            ),
        });
    };
    let mut attributes = BTreeMap::new();
    attributes.insert(JOB_DEFINITION_ARN_ATTRIBUTE.to_string(), arn.to_string());
    Ok(Created {
        physical_resource_id: Some(arn.to_string()),
        attributes,
    })
}

/// Deregister the revision a stack registered, addressed by the ARN AWS Batch knows it by —
/// this resource's physical id, which is what `Ref` returns for the type.
///
/// The revision itself is kept: deregistering SETS its status ("job definitions are
/// permanently deleted after 180 days"), which is what that service does with one, so a
/// revision already out of `ACTIVE` is reported as it stands and a second teardown is the
/// success a repeated delete needs.
pub(super) async fn delete_batch_job_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    call_rest_json(
        ctx,
        BATCH_SIGNING_NAME,
        region,
        "POST",
        BATCH_DEREGISTER_JOB_DEFINITION_URI,
        Some(&serde_json::json!({ "jobDefinition": physical_resource_id })),
    )
    .await
    .map(|_answered| ())
}

/// The revision as AWS Batch reports it, read by the ARN that is this resource's physical
/// id: the record `DescribeJobDefinitions` answers, or `None` where that service holds none.
///
/// Its members are that service's own, in the lower camel case its model spells them with,
/// so none of them is read as this type's property — a property a read does not report is
/// not evidence of anything ([`super::super::drift`]), and the same holds for the other AWS
/// Batch types' reads ([`batch_described`]). What this record IS evidence of is that the
/// revision is still there.
pub(super) async fn describe_batch_job_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    batch_described(
        ctx,
        region,
        BATCH_DESCRIBE_JOB_DEFINITIONS_URI,
        BATCH_JOB_DEFINITIONS_MEMBER,
        physical_resource_id,
    )
    .await
}

/// The members `RegisterJobDefinition` takes from the declared properties, each under that
/// call's own member name and with every member name INSIDE it re-cased the same way
/// ([`api_members`]).
fn register_members(properties: &Value) -> Result<Map<String, Value>, Refusal> {
    let mut request = Map::new();
    for property in CARRIED_PROPERTIES {
        let Some(value) = properties.get(*property).filter(|value| !value.is_null()) else {
            continue;
        };
        // The PARAMETERS map's keys are the template's own placeholders, so its entries are
        // carried exactly as written rather than re-cased.
        let carried = if *property == PARAMETERS_PROPERTY {
            value.clone()
        } else {
            api_members(value, 0)?
        };
        request.insert(lower_initial(property), carried);
    }
    Ok(request)
}

/// The job definition the revision belongs to: the declared `JobDefinitionName`, and
/// otherwise the name the identifier CloudFormation computed for the resource carries —
/// which is the generated name a declaration naming none is registered under.
fn declared_job_definition_name(resource: &StoredResource) -> String {
    if let Some(name) = resource
        .properties
        .get(JOB_DEFINITION_NAME_PROPERTY)
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return name.to_string();
    }
    job_definition_name_of(&resource.physical_resource_id)
}

/// The NAME segment of a computed job-definition ARN
/// (`…:job-definition/<name>:<revision>`), or the whole value where it is no such ARN —
/// which is then what the registration asks for, and AWS Batch judges whether it names a
/// job definition.
fn job_definition_name_of(identifier: &str) -> String {
    let Some((_, path)) = identifier.split_once(JOB_DEFINITION_ARN_SEGMENT) else {
        return identifier.to_string();
    };
    match path.rsplit_once(':') {
        Some((name, _revision)) => name.to_string(),
        None => path.to_string(),
    }
}

/// One property value with every member name inside it spelled as the API member it is
/// ([`lower_initial`]), and the entries of a map whose keys are a caller's own DATA carried
/// through verbatim ([`DATA_MAP_MEMBERS`]).
///
/// `depth` is how many levels are already above this one, so the outermost call passes `0`;
/// the document is caller-supplied, so one nested past [`MAX_PROPERTY_NESTING`] is REFUSED
/// rather than walked or quietly truncated.
fn api_members(value: &Value, depth: usize) -> Result<Value, Refusal> {
    if depth >= MAX_PROPERTY_NESTING {
        return Err(too_deeply_nested());
    }
    match value {
        Value::Object(members) => {
            let mut converted = Map::new();
            for (name, member) in members {
                let member_name = lower_initial(name);
                // The VALUE of a data map is carried as written: re-casing it would rename a
                // log-driver option, a Kubernetes label or a resource name the template wrote.
                let carried = if DATA_MAP_MEMBERS.contains(&member_name.as_str()) {
                    member.clone()
                } else {
                    api_members(member, depth + 1)?
                };
                converted.insert(member_name, carried);
            }
            Ok(Value::Object(converted))
        }
        Value::Array(elements) => {
            let mut converted = Vec::with_capacity(elements.len());
            for element in elements {
                converted.push(api_members(element, depth + 1)?);
            }
            Ok(Value::Array(converted))
        }
        scalar => Ok(scalar.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared properties reach `RegisterJobDefinition` under that call's own member
    /// names, all the way down — with the keys of a data map left exactly as the template
    /// wrote them.
    #[test]
    fn the_declared_properties_are_carried_under_the_api_member_names() {
        let declared = serde_json::json!({
            "Type": "container",
            "SchedulingPriority": 2,
            "PlatformCapabilities": ["FARGATE"],
            "PropagateTags": true,
            "Parameters": {"Codec": "mp4", "bitRate": "128"},
            "Timeout": {"AttemptDurationSeconds": 120},
            "RetryStrategy": {
                "Attempts": 3,
                "EvaluateOnExit": [{"Action": "RETRY", "OnStatusReason": "Host EC2*"}]
            },
            "ContainerProperties": {
                "Image": "public.ecr.aws/example/worker:1",
                "ExecutionRoleArn": "arn:aws:iam::000000000000:role/execution",
                "ResourceRequirements": [{"Type": "VCPU", "Value": "0.25"}],
                "NetworkConfiguration": {"AssignPublicIp": "ENABLED"},
                "LogConfiguration": {
                    "LogDriver": "awslogs",
                    "Options": {"awslogs-group": "/aws/batch/job", "awslogs-region": "us-east-1"}
                }
            },
            "EksProperties": {
                "PodProperties": {
                    "Metadata": {"Labels": {"Team": "reports"}},
                    "Containers": [{
                        "Image": "public.ecr.aws/example/worker:1",
                        "Resources": {"Limits": {"cpu": "1", "nvidia.com/gpu": "1"}}
                    }]
                }
            }
        });
        let carried = Value::Object(
            register_members(&declared).expect("case \"a full declaration\": the walk converts it"),
        );
        let expected = serde_json::json!({
            "type": "container",
            "schedulingPriority": 2,
            "platformCapabilities": ["FARGATE"],
            "propagateTags": true,
            "parameters": {"Codec": "mp4", "bitRate": "128"},
            "timeout": {"attemptDurationSeconds": 120},
            "retryStrategy": {
                "attempts": 3,
                "evaluateOnExit": [{"action": "RETRY", "onStatusReason": "Host EC2*"}]
            },
            "containerProperties": {
                "image": "public.ecr.aws/example/worker:1",
                "executionRoleArn": "arn:aws:iam::000000000000:role/execution",
                "resourceRequirements": [{"type": "VCPU", "value": "0.25"}],
                "networkConfiguration": {"assignPublicIp": "ENABLED"},
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {"awslogs-group": "/aws/batch/job", "awslogs-region": "us-east-1"}
                }
            },
            "eksProperties": {
                "podProperties": {
                    "metadata": {"labels": {"Team": "reports"}},
                    "containers": [{
                        "image": "public.ecr.aws/example/worker:1",
                        "resources": {"limits": {"cpu": "1", "nvidia.com/gpu": "1"}}
                    }]
                }
            }
        });
        assert_eq!(
            carried, expected,
            "case \"a full declaration\": expected {expected:?}, got {carried:?}"
        );
    }

    /// The name the registration is addressed by: the declared one where there is one, and
    /// otherwise the name the ARN CloudFormation computed for the resource carries — whose
    /// revision suffix is not part of it.
    #[test]
    fn the_registration_is_addressed_by_the_declared_or_computed_name() {
        for (case, properties, physical, expected) in [
            (
                "a declared name",
                serde_json::json!({"JobDefinitionName": "declared"}),
                "arn:aws:batch:us-east-1:000000000000:job-definition/computed:1",
                "declared",
            ),
            (
                "no declared name",
                serde_json::json!({}),
                "arn:aws:batch:us-east-1:000000000000:job-definition/computed:1",
                "computed",
            ),
            (
                "an identifier that is no job-definition ARN",
                serde_json::json!({}),
                "plain-name",
                "plain-name",
            ),
        ] {
            let resource = StoredResource {
                logical_id: "Subject".to_string(),
                resource_type: BATCH_JOB_DEFINITION.to_string(),
                physical_resource_id: physical.to_string(),
                attributes: BTreeMap::new(),
                metadata: None,
                properties,
                materialised: false,
                deletion_policy: None,
                update_replace_policy: None,
            };
            let name = declared_job_definition_name(&resource);
            assert_eq!(
                name, expected,
                "case {case:?}: expected {expected:?}, got {name:?}"
            );
        }
    }
}
