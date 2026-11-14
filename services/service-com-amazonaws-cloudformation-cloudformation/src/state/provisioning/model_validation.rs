// SPDX-License-Identifier: CC0-1.0

//! The MODEL VALIDATION a resource provider runs on the declaration it is handed, before it
//! calls the service that owns the type.
//!
//! ## The rule this module implements
//!
//! What a resource provider is given is its type's MODEL — the resource's property document —
//! and it validates that document against the type's own schema before it makes a single
//! call. A property the schema marks REQUIRED that the document does not carry fails that
//! validation, so the resource fails with the schema validator's own sentence naming the
//! member that is not there ([`reason`]), and NOTHING is asked of the service that owns the
//! type: no resource is created anywhere, and the deployment is rolled back carrying that
//! sentence on the resource's own failed event — CloudFormation reports what the provider
//! worded, inside the envelope every handler refusal is quoted in
//! ([`super::handler_failure_reason`]).
//!
//! The sentence is the validator's, not this service's: its fixed text is the validator's own
//! wording, its LOCATION is the place in the validated document the failure is about ([`ROOT`]
//! for a property of the type itself, which is the document's root), and its one other
//! variable part is the member the schema names.
//!
//! CloudFormation reports the very same omission in a sentence of ITS own in the stage BEFORE
//! this one ([`crate::state::property_validation`], whose `Required property [<member>] not
//! found` fails the operation before any resource is created, where that stage covers the
//! type and the request did not turn it off). This one is the provider's, so it is reached
//! where the resource would have been created: the resource records its own failure, as a
//! deployment that got that far does.
//!
//! ## What this stage judges, and what it deliberately leaves alone
//!
//! [`REQUIRED_PROPERTIES`] is a catalogue: one row per resource type, carrying the properties
//! that type's entry in the "AWS resource and property types reference" of the CloudFormation
//! User Guide publishes with "Required: Yes" that this stage judges — the ones its provider
//! cannot build the create's request without, which are the members the owning API declares
//! required on that call, less the name CloudFormation generates for a declaration that
//! states none ([`crate::state::template::physical_name`]).
//!
//! It is deliberately PARTIAL and fail-open, as the pre-deployment catalogue beside it is: a
//! property no row names, and a type with no row at all, are left to the type's own arm and to
//! the service that owns it, which is the authority on everything this stage is not. A
//! declaration this stage passes is still provisioned and still judged there.
//!
//! Only PRESENCE is judged, because presence is what a schema's `required` states. A property
//! the document carries whose VALUE the type does not admit — the empty string where the
//! schema states a length, a value outside a published set — is a different failure of the
//! same validation, reported where that value is read.

use ::serde_json::Value;

use super::Refusal;

/// One resource type's required properties, as this stage judges them.
#[derive(Debug, Clone, Copy)]
pub(super) struct RequiredProperties {
    /// The resource type, as a template declares it.
    pub(super) resource_type: &'static str,
    /// The properties its schema marks required, in the order the type's reference publishes
    /// them (alphabetically) — which is observable only in which property a declaration
    /// missing several of them is reported for.
    pub(super) required: &'static [&'static str],
}

/// The types whose provider's model validation this stage runs — see the module docs for what
/// a row carries and where it comes from.
pub(super) const REQUIRED_PROPERTIES: &[RequiredProperties] = &[
    // `AWS::AppSync::DomainName`: a custom domain IS a name served with a certificate, and
    // `CreateDomainName` declares both members required.
    RequiredProperties {
        resource_type: super::appsync_domain_names::APPSYNC_DOMAIN_NAME,
        required: &["CertificateArn", "DomainName"],
    },
    // `AWS::AppSync::DomainNameApiAssociation` IS the mapping of one API onto one domain, and
    // `AssociateApi` declares both members required.
    RequiredProperties {
        resource_type: super::appsync_domain_names::APPSYNC_DOMAIN_NAME_API_ASSOCIATION,
        required: &["ApiId", "DomainName"],
    },
    // `AWS::IoT::Authorizer`: the authorizer IS the function that authorizes, so
    // `AuthorizerFunctionArn` is published "Required: Yes" and is the member
    // `CreateAuthorizer` declares required beside the name.
    RequiredProperties {
        resource_type: "AWS::IoT::Authorizer",
        required: &["AuthorizerFunctionArn"],
    },
    // `AWS::Lambda::Function`: the deployment package is the function, and `CreateFunction`
    // declares `Code` required.
    RequiredProperties {
        resource_type: "AWS::Lambda::Function",
        required: &["Code"],
    },
    // The two Amazon CloudWatch Logs filters a stack declares exist ON a log group, which
    // `PutMetricFilter` and `PutSubscriptionFilter` each declare required and both types
    // publish "Required: Yes".
    RequiredProperties {
        resource_type: "AWS::Logs::MetricFilter",
        required: &["LogGroupName"],
    },
    RequiredProperties {
        resource_type: "AWS::Logs::SubscriptionFilter",
        required: &["LogGroupName"],
    },
    // `AWS::SecretsManager::ResourcePolicy` IS a policy ON a secret: `PutResourcePolicy`
    // declares both the document and the secret required, and the type publishes both
    // "Required: Yes".
    RequiredProperties {
        resource_type: "AWS::SecretsManager::ResourcePolicy",
        required: &["ResourcePolicy", "SecretId"],
    },
    // `AWS::SecretsManager::RotationSchedule` is the rotation OF a secret, which `RotateSecret`
    // addresses by `SecretId`.
    RequiredProperties {
        resource_type: "AWS::SecretsManager::RotationSchedule",
        required: &["SecretId"],
    },
    // `AWS::StepFunctions::StateMachine`: a state machine runs its executions AS a role, so
    // `RoleArn` is published "Required: Yes" and is the member `CreateStateMachine` declares
    // required beside the definition and the name.
    RequiredProperties {
        resource_type: "AWS::StepFunctions::StateMachine",
        required: &["RoleArn"],
    },
];

/// The LOCATION a failure about a property of the type itself is reported at: the root of the
/// document the provider validated, which is the resource's own property document.
const ROOT: &str = "#";

/// The error code a model-validation failure carries: the provider refused the REQUEST it was
/// handed, which is the class CloudFormation reports such a failure under
/// (`HandlerErrorCode: InvalidRequest`, per [`super::handler_error_code`]).
const MODEL_VALIDATION_CODE: &str = "InvalidRequest";

/// `Some(refusal)` where the property document `properties` of a resource of `resource_type`
/// does not carry a property this stage judges required ([`REQUIRED_PROPERTIES`]) — the first
/// such property in the order the type's reference publishes them; `None` for a type with no
/// row and for a document carrying every property its row names.
///
/// A document that is not an object at all (a declaration with no `Properties` member) carries
/// no property, so it is reported for the first one its row names.
pub(super) fn refusal(resource_type: &str, properties: &Value) -> Option<Refusal> {
    let missing = missing_required_property(resource_type, properties)?;
    Some(Refusal {
        code: MODEL_VALIDATION_CODE.to_string(),
        message: reason(missing),
    })
}

/// The first property of `resource_type` this stage judges required that `properties` does not
/// carry.
fn missing_required_property(resource_type: &str, properties: &Value) -> Option<&'static str> {
    REQUIRED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource_type)
        .find_map(|row| {
            row.required
                .iter()
                .find(|property| properties.get(*property).is_none())
                .copied()
        })
}

/// How a resource provider's model validation reports a property its type's schema marks
/// required that the declaration does not carry: the schema validator's own sentence, around
/// the location in the validated document the failure is about and the member that is missing.
pub(super) fn reason(member: &str) -> String {
    format!("Model validation failed ({ROOT}: required key [{member}] not found)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a declaration that does not carry a property this stage judges required is
    /// refused with the validator's sentence naming THAT member, whichever type and property
    /// it is — and one carrying every property its row names is not refused here at all.
    #[test]
    fn a_missing_required_property_is_refused_naming_the_member() {
        let cases: &[(&str, &str, Value, Option<&str>)] = &[
            (
                "a state machine declaring no role",
                "AWS::StepFunctions::StateMachine",
                ::serde_json::json!({"DefinitionString": "{}"}),
                Some("RoleArn"),
            ),
            (
                "a state machine declaring the role it runs as",
                "AWS::StepFunctions::StateMachine",
                ::serde_json::json!({
                    "DefinitionString": "{}",
                    "RoleArn": "arn:aws:iam::000000000000:role/r",
                }),
                None,
            ),
            (
                "a resource policy declaring neither of the two it needs, reported for the \
                 first the reference publishes",
                "AWS::SecretsManager::ResourcePolicy",
                ::serde_json::json!({}),
                Some("ResourcePolicy"),
            ),
            (
                "a resource policy declaring the document but not the secret",
                "AWS::SecretsManager::ResourcePolicy",
                ::serde_json::json!({"ResourcePolicy": {"Version": "2012-10-17"}}),
                Some("SecretId"),
            ),
            (
                "a filter declaring no log group",
                "AWS::Logs::MetricFilter",
                ::serde_json::json!({"FilterPattern": ""}),
                Some("LogGroupName"),
            ),
            (
                "a declaration with no property document at all",
                "AWS::IoT::Authorizer",
                Value::Null,
                Some("AuthorizerFunctionArn"),
            ),
            (
                "a type this stage holds no row for",
                "AWS::SNS::Topic",
                Value::Null,
                None,
            ),
        ];
        for (case, resource_type, properties, missing) in cases {
            let refused = refusal(resource_type, properties);
            assert_eq!(
                refused.as_ref().map(|refusal| refusal.message.clone()),
                missing.map(reason),
                "case {case:?}: expected the failure for {missing:?}, got {refused:?}"
            );
            if let Some(refusal) = refused {
                assert_eq!(
                    refusal.code, MODEL_VALIDATION_CODE,
                    "case {case:?}: expected the request to be the thing refused, got {refusal:?}"
                );
            }
        }
    }

    /// The sentence itself: fixed text around the root of the validated document and the
    /// member, with nothing of the resource or the stack in it.
    #[test]
    fn the_reason_is_the_validators_sentence() {
        assert_eq!(
            reason("RoleArn"),
            "Model validation failed (#: required key [RoleArn] not found)",
            "expected the schema validator's own wording"
        );
    }

    /// Every row names a type once, and no row is empty: a type whose row names no property
    /// would be a row this stage can never report anything for.
    #[test]
    fn every_row_names_one_type_and_at_least_one_property() {
        for row in REQUIRED_PROPERTIES {
            assert!(
                !row.required.is_empty(),
                "case {:?}: expected the row to name a property",
                row.resource_type
            );
            let rows = REQUIRED_PROPERTIES
                .iter()
                .filter(|other| other.resource_type == row.resource_type)
                .count();
            assert_eq!(
                rows, 1,
                "case {:?}: expected one row per type, got {rows}",
                row.resource_type
            );
        }
    }
}
