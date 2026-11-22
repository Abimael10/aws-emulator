// SPDX-License-Identifier: CC0-1.0

//! The properties a resource type's schema marks READ-ONLY: values the owning service
//! COMPUTES for a resource rather than inputs any handler of the type takes.
//!
//! A resource provider schema classifies a type's properties, and the ones it lists under
//! `readOnlyProperties` are the ones a read of the resource ANSWERS with and no create or
//! update accepts — a resource's assigned identifier, the endpoint a service serves it on.
//! They are still properties of the type, so a template may carry one and a schema
//! validation passes it; what the deployment then does with it is NOTHING: there is no
//! member of any call it could travel as, so the document the owning service is asked to
//! create the resource with is the declaration WITHOUT it, and the value the resource
//! really carries stays that service's own.
//!
//! That is what this module holds: one row per resource type whose schema's read-only
//! classification this emulator knows, and the reading of the deployed property document
//! that drops them ([`without_read_only`], which
//! [`super::resolve_properties`] applies as it evaluates each declaration). A type with no
//! row is left exactly as declared, the same way every other catalogue of a type's schema
//! facts here is deliberately partial ([`crate::state::property_validation`] says so in
//! full).

use ::serde_json::Value;

/// One resource type and the properties its schema marks read-only.
#[derive(Debug, Clone, Copy)]
pub struct ReadOnlyProperties {
    /// The resource type the row is about, as a template declares it.
    pub resource_type: &'static str,
    /// Every property its schema lists under `readOnlyProperties`, in the order that list
    /// publishes them.
    pub read_only: &'static [&'static str],
}

/// The types this emulator knows the READ-ONLY properties of.
///
/// Source for each row: the resource type's `readOnlyProperties` pointers, with the
/// `/properties/` prefix of each dropped — the properties the type's "Return values"
/// section publishes as `Fn::GetAtt` attributes and its property reference does not
/// document as settable, which is the same classification Cloud Control API reads a
/// resource model's identity out of.
pub const READ_ONLY_PROPERTIES: &[ReadOnlyProperties] = &[
    // `AWS::ApplicationInsights::Application`: the `ApplicationARN` Application Insights
    // addresses the application by, which that service builds from the resource group the
    // application monitors and no member of `CreateApplication` accepts.
    ReadOnlyProperties {
        resource_type: super::APPLICATION_INSIGHTS_APPLICATION,
        read_only: &[super::APPLICATION_INSIGHTS_APPLICATION_ARN_ATTRIBUTE],
    },
    // `AWS::ApiGatewayV2::Api`: the api's own id, the endpoint API Gateway v2 serves it on —
    // "the default endpoint for an API" — and the ARN of that api's own invoke surface. That
    // service assigns the id when it makes the api and the other two are built from it, and
    // none of the three is a member of `CreateApi`, `ImportApi` or `UpdateApi`.
    ReadOnlyProperties {
        resource_type: super::APIGATEWAYV2_API,
        read_only: &["ApiEndpoint", "ApiId", "ExecuteApiArn"],
    },
    // `AWS::ApiGatewayV2::RoutingRule`: the rule's own id and the ARN API Gateway v2 builds
    // around it. That service assigns the id when it makes the rule and the ARN carries it, and
    // neither is a member of `CreateRoutingRule` or `PutRoutingRule`.
    ReadOnlyProperties {
        resource_type: super::APIGATEWAYV2_ROUTING_RULE,
        read_only: &[
            crate::state::provisioner::ROUTING_RULE_ARN_ATTRIBUTE,
            crate::state::provisioner::ROUTING_RULE_ID_ATTRIBUTE,
        ],
    },
    // `AWS::LakeFormation::PrincipalPermissions`: the two halves of the grant's composite
    // primary identifier, which the type's handler computes from the principal and the
    // resource the declaration names and no create or update accepts.
    ReadOnlyProperties {
        resource_type:
            crate::state::provisioner::lake_formation_permissions::LAKEFORMATION_PRINCIPAL_PERMISSIONS,
        read_only: &[
            crate::state::provisioner::lake_formation_permissions::PRINCIPAL_IDENTIFIER_PROPERTY,
            crate::state::provisioner::lake_formation_permissions::RESOURCE_IDENTIFIER_PROPERTY,
        ],
    },
    // `AWS::SecretsManager::Secret`, `AWS::SecretsManager::ResourcePolicy` and
    // `AWS::SecretsManager::RotationSchedule` each publish ONE read-only property, `Id`: the
    // ARN Secrets Manager assigns the secret, which the type's own "Return values" section
    // publishes as its `Fn::GetAtt` attribute and no member of `CreateSecret`,
    // `PutResourcePolicy` or `RotateSecret` accepts. A secret's ARN ends in six characters
    // that service draws per secret, so nothing but that service could state it.
    ReadOnlyProperties {
        resource_type: super::SECRETSMANAGER_SECRET,
        read_only: &[SECRETSMANAGER_ID],
    },
    ReadOnlyProperties {
        resource_type: "AWS::SecretsManager::ResourcePolicy",
        read_only: &[SECRETSMANAGER_ID],
    },
    ReadOnlyProperties {
        resource_type: "AWS::SecretsManager::RotationSchedule",
        read_only: &[SECRETSMANAGER_ID],
    },
    // `AWS::Redshift::EventSubscription`: the six values Amazon Redshift ANSWERS for a
    // subscription and no member of `CreateEventSubscription` or `ModifyEventSubscription`
    // accepts — the account it belongs to, the name that service holds it under, its status,
    // the instant it was created, and the source and category lists as that service RESOLVED
    // them (the declaration states its own under `SourceIds` and `EventCategories`).
    ReadOnlyProperties {
        resource_type:
            crate::state::provisioner::redshift_event_subscriptions::REDSHIFT_EVENT_SUBSCRIPTION,
        read_only: &[
            "CustSubscriptionId",
            "CustomerAwsId",
            "EventCategoriesList",
            "SourceIdsList",
            "Status",
            "SubscriptionCreationTime",
        ],
    },
];

/// The one read-only property the `AWS::SecretsManager::*` types above publish.
const SECRETSMANAGER_ID: &str = "Id";

/// The properties of `resource_type` its schema marks read-only ([`READ_ONLY_PROPERTIES`]),
/// and the empty set for a type with no row.
pub fn read_only_properties(resource_type: &str) -> &'static [&'static str] {
    READ_ONLY_PROPERTIES
        .iter()
        .find(|row| row.resource_type == resource_type)
        .map_or(&[], |row| row.read_only)
}

/// `properties` as the document the owning service is asked to apply: the declaration
/// without any member its type's schema marks read-only.
///
/// Such a member is no input to any call of the type, so a deployment that carried it would
/// have nowhere to send it — and one that refused the declaration for it would refuse a
/// template the real service deploys. Dropping it is what makes the value the resource
/// carries the OWNING service's own, which is what every reader of the resource then
/// reports.
///
/// A document that is not an object carries no member to drop, and a type with no row keeps
/// every member exactly as declared.
pub fn without_read_only(resource_type: &str, properties: Value) -> Value {
    let read_only = read_only_properties(resource_type);
    if read_only.is_empty() {
        return properties;
    }
    let Value::Object(declared) = properties else {
        return properties;
    };
    Value::Object(
        declared
            .into_iter()
            .filter(|(property, _)| !read_only.contains(&property.as_str()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::json;

    /// RULE: a member a type's schema marks read-only is left out of the document the owning
    /// service is asked to apply, and every other member is carried exactly as declared — for
    /// a type whose read-only classification this emulator knows. A type with no row keeps
    /// every member, whatever it is called.
    ///
    /// The rows span what the rule generalises over: which read-only property is carried, how
    /// many at once, the form of its value, a document carrying none of them, a document that
    /// is not an object at all, and a type the catalogue holds no row for — including one
    /// whose own property set states a member another type marks read-only.
    #[test]
    fn a_read_only_property_is_left_out_of_the_deployed_document() {
        // (case, resource type, the declaration, the document the deployment applies).
        let rows: &[(&str, &str, Value, Value)] = &[
            (
                "an api carrying the read-only endpoint beside its own members",
                super::super::APIGATEWAYV2_API,
                json!({"Name": "an-api", "ProtocolType": "HTTP", "ApiEndpoint": "https://x"}),
                json!({"Name": "an-api", "ProtocolType": "HTTP"}),
            ),
            (
                "an api carrying both read-only properties, one of them not text",
                super::super::APIGATEWAYV2_API,
                json!({"Body": {"openapi": "3.0.1"}, "ApiEndpoint": "https://x", "ApiId": 7}),
                json!({"Body": {"openapi": "3.0.1"}}),
            ),
            (
                "an api carrying the read-only ARN of its own invoke surface",
                super::super::APIGATEWAYV2_API,
                json!({"Name": "an-api", "ExecuteApiArn": "arn:aws:execute-api:us-east-1:0:x"}),
                json!({"Name": "an-api"}),
            ),
            (
                "an api carrying none of them",
                super::super::APIGATEWAYV2_API,
                json!({"Name": "an-api", "ProtocolType": "WEBSOCKET"}),
                json!({"Name": "an-api", "ProtocolType": "WEBSOCKET"}),
            ),
            (
                "an api declaring no property document at all",
                super::super::APIGATEWAYV2_API,
                Value::Null,
                Value::Null,
            ),
            (
                "a type the catalogue holds no row for, whose own properties include a \
                 member another type marks read-only",
                super::super::APIGATEWAYV2_STAGE,
                json!({"ApiId": {"Ref": "Api"}, "StageName": "prod"}),
                json!({"ApiId": {"Ref": "Api"}, "StageName": "prod"}),
            ),
        ];
        for (case, resource_type, declared, applied) in rows {
            let deployed = without_read_only(resource_type, declared.clone());
            assert_eq!(
                &deployed, applied,
                "case {case:?}: expected the document the deployment applies, got {deployed:?}"
            );
        }
    }
}
