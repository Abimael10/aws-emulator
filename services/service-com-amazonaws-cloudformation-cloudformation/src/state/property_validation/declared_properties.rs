// SPDX-License-Identifier: CC0-1.0

//! The other half of what a resource type's schema makes decidable without asking anybody:
//! WHICH properties that type has at all.
//!
//! A resource type's schema states its `properties` — the complete set of members a
//! declaration of that type may carry — so a property document carrying a member the schema
//! does not state is one no handler of that type could ever be given: the stage that runs
//! before provisioning reports it, naming the member it does not recognise, and the operation
//! deploys nothing. That is the question this module answers, beside the three
//! [`super`] already answers (a required property the declaration omits, a group it may carry
//! at most one of, and a value outside the set a property publishes).
//!
//! The judgement is made of the DEPLOYED property document, as every judgement of that stage
//! is, and it is the same kind of catalogue: one row per resource type whose schema states its
//! properties in full, so a type with no row is left to the service that owns it rather than
//! guessed at. A row has to be COMPLETE to be safe — a property the schema states but the row
//! omits would fail a declaration the real service accepts — which is why the rows here are
//! the types whose schemas state few enough properties to carry in full.

use ::serde_json::Value;

use super::{properties_path, PropertyFailure, StoredResource};

/// One resource type and the properties its schema states.
#[derive(Debug, Clone, Copy)]
pub struct DeclaredProperties {
    /// The resource type the row is about, as a template declares it.
    pub resource_type: &'static str,
    /// Every property its schema states, in the order the reference publishes them
    /// (alphabetically).
    pub declared: &'static [&'static str],
}

/// The types this stage knows the COMPLETE property set of.
///
/// Source for each row: the resource type's entry in the "AWS resource and property types
/// reference" of the CloudFormation User Guide, which lists every property the type's
/// registry schema states under `properties` — the whole list, since a member missing from a
/// row would be reported as one the type does not have.
pub const DECLARED_PROPERTIES: &[DeclaredProperties] = &[
    // `AWS::SSM::Parameter`: the `PutParameter` members the type passes straight through
    // ([`crate::state::provisioner`] maps them), plus the `Name` CloudFormation may generate
    // and the `Tags` the type publishes as a map.
    DeclaredProperties {
        resource_type: "AWS::SSM::Parameter",
        declared: &[
            "AllowedPattern",
            "DataType",
            "Description",
            "Name",
            "Policies",
            "Tags",
            "Tier",
            "Type",
            "Value",
        ],
    },
    // `AWS::SNS::Subscription`: the `Subscribe` request's own members — the topic the
    // subscription is on, the protocol it is delivered over, where it is delivered and the
    // attributes that shape that delivery.
    DeclaredProperties {
        resource_type: "AWS::SNS::Subscription",
        declared: &[
            "DeliveryPolicy",
            "Endpoint",
            "FilterPolicy",
            "FilterPolicyScope",
            "Protocol",
            "RawMessageDelivery",
            "RedrivePolicy",
            "Region",
            "ReplayPolicy",
            "SubscriptionRoleArn",
            "TopicArn",
        ],
    },
    // `AWS::SQS::Queue`: the queue's `QueueName`, every one of its own ATTRIBUTES the owning
    // API publishes as settable — named identically on both sides
    // (`::service_com_amazonaws_sqs_amazonsqs::state::SETTABLE_ATTRIBUTES`) — LESS the
    // queue's `Policy`, for which this type's schema states no property at all (a queue's
    // access policy is carried by resource types of its own, `AWS::SQS::QueuePolicy` and
    // `AWS::SQS::QueueInlinePolicy`), plus the `Tags` the type publishes as a list and the
    // two the schema marks READ-ONLY (`Arn`, `QueueUrl`) — the values Amazon SQS computes for
    // the queue, which a declaration may carry and a deployment ignores.
    DeclaredProperties {
        resource_type: "AWS::SQS::Queue",
        declared: &[
            "Arn",
            "ContentBasedDeduplication",
            "DeduplicationScope",
            "DelaySeconds",
            "FifoQueue",
            "FifoThroughputLimit",
            "KmsDataKeyReusePeriodSeconds",
            "KmsMasterKeyId",
            "MaximumMessageSize",
            "MessageRetentionPeriod",
            "QueueName",
            "QueueUrl",
            "ReceiveMessageWaitTimeSeconds",
            "RedriveAllowPolicy",
            "RedrivePolicy",
            "SqsManagedSseEnabled",
            "Tags",
            "VisibilityTimeout",
        ],
    },
    // The three resources that ARE one resource policy, each stating the document and the
    // resources it is installed on, and nothing else.
    DeclaredProperties {
        resource_type: "AWS::SNS::TopicPolicy",
        declared: &["PolicyDocument", "Topics"],
    },
    DeclaredProperties {
        resource_type: "AWS::SQS::QueuePolicy",
        declared: &["PolicyDocument", "Queues"],
    },
    DeclaredProperties {
        resource_type: "AWS::S3::BucketPolicy",
        declared: &["Bucket", "PolicyDocument"],
    },
    // `AWS::ApiGatewayV2::Api`: the api's own configuration — the `CreateApi` members the
    // type passes straight through, the four that belong to the OpenAPI definition an api is
    // imported from (`Body`, `BodyS3Location`, `BasePath`, `FailOnWarnings`), the `RouteKey`
    // and `Target` of a quick-create api, the `Tags` the type publishes as a map, and the
    // three the schema marks READ-ONLY (`ApiEndpoint`, `ApiId`, `ExecuteApiArn`) — the values
    // API Gateway v2 computes for the api, which a declaration may carry and a deployment
    // ignores ([`crate::state::template::READ_ONLY_PROPERTIES`]).
    DeclaredProperties {
        resource_type: crate::state::template::APIGATEWAYV2_API,
        declared: &[
            "ApiEndpoint",
            "ApiId",
            "ApiKeySelectionExpression",
            "BasePath",
            "Body",
            "BodyS3Location",
            "CorsConfiguration",
            "CredentialsArn",
            "Description",
            "DisableExecuteApiEndpoint",
            "DisableSchemaValidation",
            "ExecuteApiArn",
            "FailOnWarnings",
            "IpAddressType",
            "Name",
            "ProtocolType",
            "RouteKey",
            "RouteSelectionExpression",
            "Tags",
            "Target",
            "Version",
        ],
    },
    // `AWS::Logs::LogStream`: a stream is its name and the log group it lives in.
    DeclaredProperties {
        resource_type: "AWS::Logs::LogStream",
        declared: &["LogGroupName", "LogStreamName"],
    },
    // `AWS::SecretsManager::Secret`: the secret's own configuration — the two alternative
    // forms its value is stated in (`SecretString`, `GenerateSecretString`), the `Name`
    // CloudFormation may generate, the `Description` and `KmsKeyId` `CreateSecret` takes under
    // the same names, the `ReplicaRegions` the secret is replicated to, the `Tags` the type
    // publishes as a list, and the read-only `Id` — the ARN Secrets Manager assigns, which a
    // declaration may carry and a deployment ignores. Every one but `Id` is a property this
    // emulator's own provider applies
    // (`crate::state::provisioning::secretsmanager_secret`).
    DeclaredProperties {
        resource_type: crate::state::template::SECRETSMANAGER_SECRET,
        declared: &[
            "Description",
            "GenerateSecretString",
            "Id",
            "KmsKeyId",
            "Name",
            "ReplicaRegions",
            "SecretString",
            "Tags",
        ],
    },
    // `AWS::SecretsManager::ResourcePolicy`: the resource-based policy document, the secret it
    // is attached to, the switch that refuses a policy granting broad access, and the
    // read-only `Id`.
    DeclaredProperties {
        resource_type: "AWS::SecretsManager::ResourcePolicy",
        declared: &["BlockPublicPolicy", "Id", "ResourcePolicy", "SecretId"],
    },
    // `AWS::SecretsManager::RotationSchedule`: the secret rotation is configured for, the
    // function that rotates it (either named outright or the hosted one this type asks Secrets
    // Manager to make), the schedule, whether the first rotation happens on update, and the
    // read-only `Id`.
    DeclaredProperties {
        resource_type: "AWS::SecretsManager::RotationSchedule",
        declared: &[
            "HostedRotationLambda",
            "Id",
            "RotateImmediatelyOnUpdate",
            "RotationLambdaARN",
            "RotationRules",
            "SecretId",
        ],
    },
    // `AWS::MediaConvert::CertificateAssociation` publishes ONE property: the `Arn` of the ACM
    // certificate the account's MediaConvert service is to trust, which is the whole of the
    // resource.
    DeclaredProperties {
        resource_type: crate::state::template::MEDIACONVERT_CERTIFICATE_ASSOCIATION,
        declared: &["Arn"],
    },
    // `AWS::CloudFormation::WaitConditionHandle` publishes NO property at all ("this type has
    // no properties"), so every member a declaration of it carries is one the type does not
    // have.
    DeclaredProperties {
        resource_type: "AWS::CloudFormation::WaitConditionHandle",
        declared: &[],
    },
];

/// `Some(failure)` for a resource whose property document carries a member its type's schema
/// does not state ([`DECLARED_PROPERTIES`]) — the first such member in the document's own
/// order; `None` for a type with no row, and for one every member of whose document the
/// schema states.
pub(super) fn undeclared_property(resource: &StoredResource) -> Option<PropertyFailure> {
    let row = DECLARED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == resource.resource_type)?;
    let carried = match &resource.properties {
        Value::Object(document) => document,
        // A declaration with no property document at all carries no member to judge.
        _ => return None,
    };
    let unsupported = carried
        .keys()
        .find(|property| !row.declared.contains(&property.as_str()))?;
    Some(PropertyFailure {
        logical_id: resource.logical_id.clone(),
        resource_type: resource.resource_type.clone(),
        reason: unsupported_property_reason(unsupported),
        path: property_path(&resource.logical_id, unsupported),
    })
}

/// How CloudFormation reports a property its type's schema does not state: fixed text around
/// the MEMBER it does not recognise, which is the one variable part and comes from the
/// request's own template.
pub fn unsupported_property_reason(property: &str) -> String {
    format!("Unsupported property [{property}]")
}

/// The path THIS validation is applied at: the property itself, under the `Properties` of the
/// resource that declared it.
///
/// `OperationEvent$ValidationPath` is "the path within the resource where the validation was
/// applied", and what was judged here is one member of the document rather than the document
/// as a whole — the member is what the author has to take out — so the path names it.
pub fn property_path(logical_id: &str, property: &str) -> String {
    format!("{}/{property}", properties_path(logical_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::json;
    use ::std::collections::BTreeMap;

    /// One declared resource of `resource_type` carrying `properties`.
    fn declared(logical_id: &str, resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: logical_id.to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: format!("{logical_id}-physical"),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// RULE: a member a type's schema does not state fails the validation, naming that member
    /// and the path it was carried at; a document made only of members the schema states does
    /// not. The rows span the catalogue — types the report that led here did not cite among
    /// them — and the forms a document can take.
    #[test]
    fn a_member_the_schema_does_not_state_fails_the_validation() {
        // (case, type, properties, the member the stage must name — `None` for a document
        // every member of which the schema states).
        let rows: &[(&str, &str, Value, Option<&str>)] = &[
            (
                "a parameter carrying a member no schema states",
                "AWS::SSM::Parameter",
                json!({"Type": "String", "Value": "v", "ThisPropertyDoesNotExist": "x"}),
                Some("ThisPropertyDoesNotExist"),
            ),
            (
                "a parameter carrying every member its schema states",
                "AWS::SSM::Parameter",
                json!({
                    "Name": "p", "Type": "String", "Value": "v", "Description": "d",
                    "AllowedPattern": "^.*$", "Tier": "Standard", "DataType": "text",
                    "Policies": "[]", "Tags": {"team": "ops"},
                }),
                None,
            ),
            (
                "a parameter whose member is the owning API's spelling rather than the type's",
                "AWS::SSM::Parameter",
                json!({"Type": "String", "Value": "v", "Overwrite": true}),
                Some("Overwrite"),
            ),
            (
                "a subscription carrying a misspelled endpoint",
                "AWS::SNS::Subscription",
                json!({"TopicArn": "arn:aws:sns:us-east-1:000000000000:t", "Protocol": "sqs",
                       "EndPoint": "arn:aws:sqs:us-east-1:000000000000:q"}),
                Some("EndPoint"),
            ),
            (
                "a subscription carrying only members its schema states",
                "AWS::SNS::Subscription",
                json!({"TopicArn": "arn:aws:sns:us-east-1:000000000000:t", "Protocol": "sqs",
                       "Endpoint": "arn:aws:sqs:us-east-1:000000000000:q",
                       "RawMessageDelivery": true}),
                None,
            ),
            (
                "a queue policy carrying the member another policy type states",
                "AWS::SQS::QueuePolicy",
                json!({"PolicyDocument": {"Version": "2012-10-17"}, "Queues": ["q"],
                       "Topics": ["t"]}),
                Some("Topics"),
            ),
            (
                "a log stream carrying a retention it does not have",
                "AWS::Logs::LogStream",
                json!({"LogGroupName": "g", "RetentionInDays": 7}),
                Some("RetentionInDays"),
            ),
            (
                "a queue carrying the access policy its own schema states no property for",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "VisibilityTimeout": 30,
                       "Policy": {"Version": "2012-10-17"}}),
                Some("Policy"),
            ),
            (
                "a queue carrying only members its schema states, the read-only ones among \
                 them",
                "AWS::SQS::Queue",
                json!({"QueueName": "q.fifo", "FifoQueue": true,
                       "ContentBasedDeduplication": true, "DeduplicationScope": "queue",
                       "FifoThroughputLimit": "perQueue", "DelaySeconds": 0,
                       "KmsDataKeyReusePeriodSeconds": 300, "KmsMasterKeyId": "alias/aws/sqs",
                       "MaximumMessageSize": 262144, "MessageRetentionPeriod": 345600,
                       "ReceiveMessageWaitTimeSeconds": 0,
                       "RedriveAllowPolicy": {"redrivePermission": "allowAll"},
                       "RedrivePolicy": {"maxReceiveCount": 3},
                       "SqsManagedSseEnabled": true, "VisibilityTimeout": 30,
                       "Tags": [{"Key": "team", "Value": "ops"}],
                       "Arn": "arn:aws:sqs:us-east-1:000000000000:q.fifo",
                       "QueueUrl": "https://example.invalid/q.fifo"}),
                None,
            ),
            (
                "a wait condition handle, which has no property at all",
                "AWS::CloudFormation::WaitConditionHandle",
                json!({"Anything": "at all"}),
                Some("Anything"),
            ),
            (
                "a wait condition handle declaring nothing",
                "AWS::CloudFormation::WaitConditionHandle",
                Value::Null,
                None,
            ),
            (
                "a type this stage knows no complete property set of",
                "AWS::SNS::Topic",
                json!({"NotATopicProperty": "x"}),
                None,
            ),
        ];
        for (case, resource_type, properties, unsupported) in rows {
            let resources = vec![declared("R", resource_type, properties.clone())];
            let failed = super::super::failure(&resources);
            assert_eq!(
                failed.as_ref().map(|failure| failure.reason.clone()),
                unsupported.map(unsupported_property_reason),
                "case {case:?}: expected the unsupported member {unsupported:?}, got {failed:?}"
            );
            if let (Some(failure), Some(property)) = (&failed, unsupported) {
                assert_eq!(
                    failure.path,
                    format!("/Resources/R/Properties/{property}"),
                    "case {case:?}: expected the path of the member judged, got {failure:?}"
                );
            }
        }
    }
}
