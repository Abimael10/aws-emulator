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
//! Only PRESENCE is judged by the first of the two catalogues here, because presence is what
//! a schema's `required` states. A property the document carries whose VALUE the type does not
//! admit is a different failure of the same validation, and the one form of it this stage
//! judges is a property whose value the schema constrains to a published SET
//! ([`ENUMERATED_PROPERTIES`]): a declared value outside that set — membership being the rows'
//! own [`crate::state::property_validation::ConstrainedProperty::admits`] — fails the resource
//! at the LOCATION of that property, with the validator's own sentence for the constraint that
//! failed ([`enum_reason`]) — so a value no resource of the type could ever have is refused by
//! the provider rather than sent to the owning service or admitted as the template's business.
//! A value outside a length or a pattern the schema states is the same class of failure and is
//! left where that value is read, as the rest of this stage's partiality is.
//!
//! Both of those questions are asked a second time, of other types, by the PRE-DEPLOYMENT
//! stage ([`crate::state::property_validation`]), which refuses the OPERATION rather than
//! failing the resource. The two stages carry their own disjoint catalogues for that reason:
//! which stage judges a given type's property decides whether a client's `CreateStack` is
//! refused outright or its stack rolls back carrying the resource's failure.

use ::serde_json::Value;

use crate::state::property_validation::ConstrainedProperty;

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
    // `AWS::AppConfig::HostedConfigurationVersion` IS the configuration content hosted for one
    // profile of one application, so every member `CreateHostedConfigurationVersion` declares
    // required — the two ids that address the call, the content it carries and the media type it
    // is carried under — is published "Required: Yes", and the type declares no name
    // CloudFormation could generate.
    RequiredProperties {
        resource_type: crate::state::template::APPCONFIG_HOSTED_CONFIGURATION_VERSION,
        required: &[
            "ApplicationId",
            "ConfigurationProfileId",
            "Content",
            "ContentType",
        ],
    },
    // `AWS::AppConfig::Deployment` IS one rollout of one configuration version into one
    // environment, so every member `StartDeployment` declares required is published
    // "Required: Yes" for it.
    RequiredProperties {
        resource_type: crate::state::template::APPCONFIG_DEPLOYMENT,
        required: &[
            "ApplicationId",
            "ConfigurationProfileId",
            "ConfigurationVersion",
            "DeploymentStrategyId",
            "EnvironmentId",
        ],
    },
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
    // `AWS::Glue::TableOptimizer`: an optimizer is a resource OF a catalog table, so every
    // member `CreateTableOptimizer` declares required — the catalog, the database and the
    // table that identify it, the type of optimizer it is, and the configuration it runs
    // under — is published "Required: Yes" and the type declares no name CloudFormation could
    // generate.
    RequiredProperties {
        resource_type: crate::state::template::GLUE_TABLE_OPTIMIZER,
        required: &[
            "CatalogId",
            "DatabaseName",
            "TableName",
            "TableOptimizerConfiguration",
            "Type",
        ],
    },
    // `AWS::IoT::Authorizer`: the authorizer IS the function that authorizes, so
    // `AuthorizerFunctionArn` is published "Required: Yes" and is the member
    // `CreateAuthorizer` declares required beside the name.
    RequiredProperties {
        resource_type: "AWS::IoT::Authorizer",
        required: &["AuthorizerFunctionArn"],
    },
    // `AWS::IoT::ProvisioningTemplate`: the template IS the body that describes what is
    // provisioned, provisioned AS a role, so both are published "Required: Yes" and both are
    // members `CreateProvisioningTemplate` declares required beside the name.
    RequiredProperties {
        resource_type: super::iot_provisioning_resources::IOT_PROVISIONING_TEMPLATE,
        required: &["ProvisioningRoleArn", "TemplateBody"],
    },
    // `AWS::IoT::RoleAlias`: an alias IS a name for a role, so `RoleArn` is published
    // "Required: Yes" and is the member `CreateRoleAlias` declares required beside the alias.
    RequiredProperties {
        resource_type: super::iot_provisioning_resources::IOT_ROLE_ALIAS,
        required: &["RoleArn"],
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
    // The two AWS Elemental MediaConvert types whose SETTINGS are the resource:
    // `CreateJobTemplate` and `CreatePreset` each declare `Settings` required, which both types
    // publish as `SettingsJson` with "Required: Yes" — a template or a preset describes no
    // transcode at all without it.
    RequiredProperties {
        resource_type: crate::state::template::MEDIACONVERT_JOB_TEMPLATE,
        required: &["SettingsJson"],
    },
    RequiredProperties {
        resource_type: crate::state::template::MEDIACONVERT_PRESET,
        required: &["SettingsJson"],
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

/// The properties whose value this stage judges against a published set.
///
/// A row is the same shape the PRE-DEPLOYMENT stage's own value catalogue carries
/// ([`crate::state::property_validation::ConstrainedProperty`], whose rows are read here
/// rather than redeclared), and the two lists are deliberately DISJOINT, because a row's
/// stage decides what a client sees: a row THERE fails the stack OPERATION before anything is
/// created (and is turned off by `DisableValidation`), while a row HERE fails the RESOURCE,
/// with the provider's own sentence on its failed event and the stack rolled back. Which stage
/// judges a given property is a fact about the service, so it is read off the recorded
/// behaviour of that type rather than chosen.
///
/// Like [`REQUIRED_PROPERTIES`] above, this is a catalogue grown one row at a time and
/// deliberately PARTIAL: a property no row names is left to the owning service, which is the
/// authority on every value this stage does not judge. A row belongs here where the SET is a
/// fact of the type rather than of one request — the owning API's own enumeration, which the
/// type's schema publishes as that property's `enum` — so that a value outside it can never
/// name a resource the service would make.
pub(super) const ENUMERATED_PROPERTIES: &[ConstrainedProperty] = &[
    // `AWS::Glue::TableOptimizer$Type` is which optimizer of the table the resource is, and
    // AWS Glue enumerates them: `smithy_query(query="shape", shape="TableOptimizerType")` is
    // `compaction | retention | orphan_file_deletion`, which is the set that type's schema
    // publishes for the property. Any other value names no optimizer that service could
    // register, and it is this stage that judges it: a stack declaring one is ADMITTED and the
    // optimizer's own resource is what fails.
    ConstrainedProperty {
        resource_type: crate::state::template::GLUE_TABLE_OPTIMIZER,
        property: "Type",
        admitted: &["compaction", "retention", "orphan_file_deletion"],
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
/// such property in the order the type's reference publishes them — or carries one whose value
/// is outside the set its schema publishes ([`ENUMERATED_PROPERTIES`]); `None` for a type with
/// no row and for a document this stage finds nothing wrong with.
///
/// PRESENCE is judged before VALUE, because a property that is not there has no value to
/// judge.
///
/// A document that is not an object at all (a declaration with no `Properties` member) carries
/// no property, so it is reported for the first one its row names.
pub(super) fn refusal(resource_type: &str, properties: &Value) -> Option<Refusal> {
    if let Some(missing) = missing_required_property(resource_type, properties) {
        return Some(Refusal {
            code: MODEL_VALIDATION_CODE.to_string(),
            message: reason(missing),
        });
    }
    let outside = property_outside_published_set(resource_type, properties)?;
    Some(Refusal {
        code: MODEL_VALIDATION_CODE.to_string(),
        message: enum_reason(outside),
    })
}

/// The first property of `resource_type` whose declared value is outside the set its schema
/// publishes for it ([`ENUMERATED_PROPERTIES`]); `None` where every such property the document
/// carries states one of those values, and for a property the document does not carry at all —
/// a value that is not there is the required-property question above, not this one.
///
/// Membership is the rows' own [`ConstrainedProperty::admits`], the one reading of a published
/// value set in this crate, so this stage and the pre-deployment one cannot drift apart about
/// which values a property has.
fn property_outside_published_set(
    resource_type: &str,
    properties: &Value,
) -> Option<&'static str> {
    ENUMERATED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource_type)
        .find(|row| {
            properties
                .get(row.property)
                .is_some_and(|declared| !row.admits(declared))
        })
        .map(|row| row.property)
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

/// How a resource provider's model validation reports a property whose declared value is
/// outside the set its type's schema publishes: the schema validator's own sentence, around
/// the LOCATION of that property in the validated document and the name of the constraint that
/// failed — the same sentence shape as [`reason`], whose fixed text is the validator's.
pub(super) fn enum_reason(property: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{property}: failed validation constraint for keyword \
         [enum])"
    )
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
        assert_eq!(
            enum_reason("Type"),
            "Model validation failed (#/Type: failed validation constraint for keyword [enum])",
            "expected the schema validator's own wording for the failed constraint"
        );
    }

    /// RULE: a property whose value its type's schema constrains to a published set is refused
    /// for any value outside that set, at the location of THAT property — while every value the
    /// set carries passes this stage.
    ///
    /// The rows span the set itself (each published value), values outside it (a near miss, a
    /// value of a neighbouring enum, the wrong case, the empty string), a value that is not a
    /// string at all, a property the declaration leaves out (which is the required-property
    /// question, not this one) and a type this stage holds no enumerated row for.
    #[test]
    fn a_value_outside_the_published_set_is_refused_naming_that_property() {
        let optimizer = crate::state::template::GLUE_TABLE_OPTIMIZER;
        /// The members the Glue optimizer rows carry beside the one under test, so the
        /// required-property stage passes and the VALUE is what each row judges.
        fn declared(optimizer_type: Value) -> Value {
            ::serde_json::json!({
                "CatalogId": "000000000000",
                "DatabaseName": "lake",
                "TableName": "orders",
                "TableOptimizerConfiguration": {"RoleArn": "arn", "Enabled": true},
                "Type": optimizer_type,
            })
        }
        let mut cases: Vec<(String, &str, Value, Option<&str>)> = Vec::new();
        // Every value the published set carries passes, including the two the issue did not
        // cite.
        for published in ["compaction", "retention", "orphan_file_deletion"] {
            cases.push((
                format!("the published value {published:?}"),
                optimizer,
                declared(Value::String(published.to_string())),
                None,
            ));
        }
        for outside in [
            "compact",
            "COMPACTION",
            "Compaction",
            "snapshot_retention",
            "",
        ] {
            cases.push((
                format!("the value {outside:?}, which the set does not carry"),
                optimizer,
                declared(Value::String(outside.to_string())),
                Some("Type"),
            ));
        }
        cases.push((
            "a value that is not a string at all".to_string(),
            optimizer,
            declared(::serde_json::json!({"Fn::Join": []})),
            // No value of a published set either way, and what it fails is a constraint
            // neither validation stage judges, so it is left to the owning service
            // ([`ConstrainedProperty::admits`]).
            None,
        ));
        cases.push((
            "a type this stage holds no enumerated row for".to_string(),
            "AWS::SNS::Topic",
            ::serde_json::json!({"Type": "not-an-optimizer"}),
            None,
        ));
        for (case, resource_type, properties, outside) in cases {
            let refused = refusal(resource_type, &properties);
            assert_eq!(
                refused.as_ref().map(|refusal| refusal.message.clone()),
                outside.map(enum_reason),
                "case {case:?}: expected the failure for {outside:?}, got {refused:?}"
            );
        }
    }

    /// A declaration that leaves an enumerated property OUT is reported for the missing
    /// property rather than for the set: presence is judged before value.
    #[test]
    fn an_absent_enumerated_property_is_reported_as_missing() {
        let refused = refusal(
            crate::state::template::GLUE_TABLE_OPTIMIZER,
            &::serde_json::json!({
                "CatalogId": "000000000000",
                "DatabaseName": "lake",
                "TableName": "orders",
                "TableOptimizerConfiguration": {"RoleArn": "arn"},
            }),
        );
        assert_eq!(
            refused.as_ref().map(|refusal| refusal.message.clone()),
            Some(reason("Type")),
            "case \"an optimizer declaring no type\": expected the missing-property sentence, \
             got {refused:?}"
        );
    }

    /// Every enumerated row names a type once per property and publishes a non-empty set of
    /// values: a row with no values could never admit anything. A row this stage carries must
    /// not be one the PRE-DEPLOYMENT stage carries too — the two judge different stages, so a
    /// property in both would be refused there and never reach here.
    #[test]
    fn every_enumerated_row_publishes_a_set_this_stage_alone_judges() {
        for row in ENUMERATED_PROPERTIES {
            assert!(
                !row.admitted.is_empty(),
                "case {:?}: expected the row to publish values for {:?}",
                row.resource_type,
                row.property
            );
            let rows = ENUMERATED_PROPERTIES
                .iter()
                .filter(|other| {
                    other.resource_type == row.resource_type && other.property == row.property
                })
                .count();
            assert_eq!(
                rows, 1,
                "case {:?}: expected one row per type and property, got {rows}",
                row.resource_type
            );
            assert!(
                !crate::state::property_validation::CONSTRAINED_PROPERTIES
                    .iter()
                    .any(|other| {
                        other.resource_type == row.resource_type
                            && other.property == row.property
                    }),
                "case {:?}: expected {:?} to be judged by one stage only, the \
                 pre-deployment catalogue carries it too",
                row.resource_type,
                row.property
            );
        }
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
