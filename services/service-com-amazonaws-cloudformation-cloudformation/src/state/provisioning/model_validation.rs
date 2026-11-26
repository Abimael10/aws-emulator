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
//! left where that value is read, as the rest of this stage's partiality is. A value outside a
//! numeric RANGE the schema states is judged here, by the third catalogue
//! ([`BOUNDED_PROPERTIES`]): the sentence is the same one, naming the location of that property
//! and the bound keyword that failed ([`range_reason`]).
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
    // `AWS::EC2::SpotFleet` IS one Spot Fleet request, so the configuration that request is
    // made from is published "Required: Yes" and is the member `RequestSpotFleet` declares
    // required; the type declares no name CloudFormation could generate.
    RequiredProperties {
        resource_type: crate::state::template::EC2_SPOT_FLEET,
        required: &[super::ec2_spot_fleet::SPOT_FLEET_REQUEST_CONFIG_DATA],
    },
    // `AWS::EFS::AccessPoint` is a view INTO one file system, which `CreateAccessPoint`
    // declares required and the type publishes "Required: Yes"; a MOUNT TARGET is the file
    // system reached from one subnet, and `CreateMountTarget` declares both members required.
    // Neither type declares a name CloudFormation could generate.
    RequiredProperties {
        resource_type: super::efs_resources::EFS_ACCESS_POINT,
        required: &["FileSystemId"],
    },
    RequiredProperties {
        resource_type: super::efs_resources::EFS_MOUNT_TARGET,
        required: &["FileSystemId", "SubnetId"],
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
    // `AWS::LakeFormation::DataCellsFilter`: a filter is a resource OF a catalog table, so
    // the four values that address one — the catalog, the database, the table and the
    // filter's own name — are each published "Required: Yes" and are the members
    // `GetDataCellsFilter` and `DeleteDataCellsFilter` declare required, and the type
    // declares no name CloudFormation could generate. Its `RowFilter` is NOT one of them:
    // the type publishes it "Required: No", so a declaration that omits it reaches Lake
    // Formation and is refused by that service's own validation of the filter.
    RequiredProperties {
        resource_type: crate::state::provisioner::lake_formation_data_cells_filters::LAKEFORMATION_DATA_CELLS_FILTER,
        required: crate::state::provisioner::lake_formation_data_cells_filters::DATA_CELLS_FILTER_ADDRESS,
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
    // The two MEMBERS of a SageMaker AI Studio domain are each created IN one, under a name of
    // their own: `CreateUserProfile` and `CreateSpace` each declare the `DomainId` and the
    // member's own name required, both types publish them "Required: Yes", and neither name is
    // one CloudFormation generates — the pair IS the resource's identity
    // ([`crate::state::template::composite_identity_properties`]), so a declaration missing
    // either half names no resource at all. The DOMAIN's own are the four its type publishes
    // "Required: Yes" beside the name CloudFormation generates for a declaration that states
    // none: the mode its members authenticate with, the settings a user profile of it is
    // created with, and the VPC and the subnets its traffic goes through — the last two being
    // required of the TYPE even though that service's own model leaves them optional for a
    // domain created from Amazon SageMaker Unified Studio.
    RequiredProperties {
        resource_type: super::sagemaker_studio::SAGEMAKER_DOMAIN,
        required: &["AuthMode", "DefaultUserSettings", "SubnetIds", "VpcId"],
    },
    RequiredProperties {
        resource_type: super::sagemaker_studio::SAGEMAKER_USER_PROFILE,
        required: &["DomainId", "UserProfileName"],
    },
    RequiredProperties {
        resource_type: super::sagemaker_studio::SAGEMAKER_SPACE,
        required: &["DomainId", "SpaceName"],
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

/// One property whose value its type's schema holds between published BOUNDS.
///
/// A row carries the two keywords a JSON schema states a numeric range with, each `None` where
/// the schema states none: the location and the KEYWORD are what the validator's sentence
/// names ([`range_reason`]), so which of the two a value broke is what a client is told.
#[derive(Debug, Clone, Copy)]
pub(super) struct BoundedProperty {
    /// The resource type the property belongs to, as a template declares it.
    pub(super) resource_type: &'static str,
    /// The property's name, as a template spells it.
    pub(super) property: &'static str,
    /// The schema's `minimum`, which is the smallest value the property admits.
    pub(super) minimum: Option<f64>,
    /// The schema's `maximum`, which is the largest.
    pub(super) maximum: Option<f64>,
}

/// The two JSON-schema keywords a numeric range is stated with, which are the names the
/// validator's sentence carries for a value that broke one of them.
const MINIMUM_KEYWORD: &str = "minimum";
const MAXIMUM_KEYWORD: &str = "maximum";

impl BoundedProperty {
    /// The keyword this row's value broke, or `None` for a value inside the bounds.
    ///
    /// The value is read as the NUMBER the schema says the property is, in either spelling a
    /// template gives one — a JSON number, or the quoted scalar a YAML template writes — so a
    /// declaration that states the bound as text is judged by the same rule. A value that is
    /// no number at all breaks a constraint this stage does not judge (the schema's `type`),
    /// so it is left to the owning service, which is the authority on everything here is not.
    fn broken_keyword(&self, declared: &Value) -> Option<&'static str> {
        let value = declared
            .as_f64()
            .or_else(|| declared.as_str().and_then(|text| text.trim().parse().ok()))?;
        if self.minimum.is_some_and(|minimum| value < minimum) {
            return Some(MINIMUM_KEYWORD);
        }
        if self.maximum.is_some_and(|maximum| value > maximum) {
            return Some(MAXIMUM_KEYWORD);
        }
        None
    }
}

/// The properties whose value this stage holds to the bounds its type's schema publishes.
///
/// A catalogue grown one row at a time and deliberately PARTIAL, for the reason the two above
/// are: a property no row names is left to the service that owns the resource. A row belongs
/// here where the RANGE is a fact of the type rather than of one request — the bounds the
/// type's schema publishes for the property, which the owning API documents as that
/// parameter's own range — so a value outside it can never name a resource that service would
/// make.
pub(super) const BOUNDED_PROPERTIES: &[BoundedProperty] = &[
    // `AWS::CloudFront::RealtimeLogConfig$SamplingRate` is the percentage of viewer requests a
    // configuration's log data represents, and CloudFront publishes the range: "You can specify
    // a whole number between 1 and 100 (inclusive)"
    // (`smithy_query(query="doc",shape="CreateRealtimeLogConfigRequest",member="SamplingRate")`),
    // which is the `minimum`/`maximum` pair that type's schema states for the property. A value
    // outside it names no configuration that service could hold, and it is THIS stage that
    // judges it: a stack declaring one is ADMITTED and the configuration's own resource is what
    // fails.
    //
    // The OWNING service reads the same published range for the rate a configuration may carry
    // at all (its own `MIN_SAMPLING_RATE`/`MAX_SAMPLING_RATE` and `realtime_log_config_refusal`,
    // in
    // `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/state/realtime_log_configs.rs`);
    // the bounds are restated here because nothing of that crate's state is reachable from this
    // one and the two stages word the refusal differently — the schema validator's sentence
    // here, that service's own there — and each doc names the other so they cannot drift
    // silently.
    BoundedProperty {
        resource_type: crate::state::template::CLOUDFRONT_REALTIME_LOG_CONFIG,
        property: "SamplingRate",
        minimum: Some(1.0),
        maximum: Some(100.0),
    },
];

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
    if let Some((property, keyword)) = property_outside_published_bounds(resource_type, properties)
    {
        return Some(Refusal {
            code: MODEL_VALIDATION_CODE.to_string(),
            message: range_reason(property, keyword),
        });
    }
    let outside = property_outside_published_set(resource_type, properties)?;
    Some(Refusal {
        code: MODEL_VALIDATION_CODE.to_string(),
        message: enum_reason(outside),
    })
}

/// The first property of `resource_type` whose declared value is outside the bounds its schema
/// publishes for it ([`BOUNDED_PROPERTIES`]), with the KEYWORD that value broke; `None` where
/// every such property the document carries states a value inside its bounds, and for a
/// property the document does not carry at all.
fn property_outside_published_bounds(
    resource_type: &str,
    properties: &Value,
) -> Option<(&'static str, &'static str)> {
    BOUNDED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource_type)
        .find_map(|row| {
            properties
                .get(row.property)
                .and_then(|declared| row.broken_keyword(declared))
                .map(|keyword| (row.property, keyword))
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

/// How a resource provider's model validation reports a property whose declared value is
/// outside the bounds its type's schema publishes: the same sentence [`enum_reason`] carries,
/// around the LOCATION of that property and the name of the bound keyword that failed
/// (`minimum` or `maximum`) — the validator names the constraint it checked rather than the
/// value it was given.
pub(super) fn range_reason(property: &str, keyword: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{property}: failed validation constraint for keyword \
         [{keyword}])"
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

    /// RULE: a property whose value its type's schema holds between published bounds is refused
    /// for any value outside them, at the location of THAT property and naming the bound
    /// keyword that failed — while every value inside them passes this stage.
    ///
    /// The rows span each published bound, one step past each of them, a value between them,
    /// each spelling a template gives a number (a JSON number and a quoted scalar), a value
    /// that is no number at all and a type this stage holds no bounded row for.
    #[test]
    fn a_value_outside_the_published_bounds_is_refused_naming_the_broken_keyword() {
        let configuration = crate::state::template::CLOUDFRONT_REALTIME_LOG_CONFIG;
        /// The members the configuration carries beside the one under test, so the VALUE is
        /// what each row judges.
        fn declared(sampling_rate: Value) -> Value {
            ::serde_json::json!({
                "Name": "edge-logs",
                "SamplingRate": sampling_rate,
                "Fields": ["timestamp"],
                "EndPoints": [{"StreamType": "Kinesis"}],
            })
        }
        /// One row: what it is about, the type, the declared document, and the property and
        /// bound keyword the validator must report — `None` for a document it must pass.
        struct Row {
            case: &'static str,
            resource_type: &'static str,
            properties: Value,
            broken: Option<(&'static str, &'static str)>,
        }
        let row = |case: &'static str,
                   resource_type: &'static str,
                   properties: Value,
                   broken: Option<(&'static str, &'static str)>| Row {
            case,
            resource_type,
            properties,
            broken,
        };
        let cases = [
            row(
                "the published minimum",
                configuration,
                declared(::serde_json::json!(1)),
                None,
            ),
            row(
                "the published maximum",
                configuration,
                declared(::serde_json::json!(100)),
                None,
            ),
            row(
                "a value between the bounds",
                configuration,
                declared(::serde_json::json!(37)),
                None,
            ),
            row(
                "one past the maximum",
                configuration,
                declared(::serde_json::json!(101)),
                Some(("SamplingRate", MAXIMUM_KEYWORD)),
            ),
            row(
                "one below the minimum",
                configuration,
                declared(::serde_json::json!(0)),
                Some(("SamplingRate", MINIMUM_KEYWORD)),
            ),
            row(
                "far below the minimum",
                configuration,
                declared(::serde_json::json!(-5)),
                Some(("SamplingRate", MINIMUM_KEYWORD)),
            ),
            row(
                "a bound written as the quoted scalar a YAML template gives one",
                configuration,
                declared(::serde_json::json!("100")),
                None,
            ),
            row(
                "a value past the maximum written as a quoted scalar",
                configuration,
                declared(::serde_json::json!("101")),
                Some(("SamplingRate", MAXIMUM_KEYWORD)),
            ),
            row(
                // What it breaks is the schema's `type`, a constraint this stage does not
                // judge, so it is left to the service that owns the resource.
                "a value that is no number at all",
                configuration,
                declared(::serde_json::json!({"Fn::Join": []})),
                None,
            ),
            row(
                "a type this stage holds no bounded row for",
                "AWS::SNS::Topic",
                ::serde_json::json!({"SamplingRate": 1000}),
                None,
            ),
        ];
        for Row {
            case,
            resource_type,
            properties,
            broken,
        } in &cases
        {
            let refused = refusal(resource_type, properties);
            assert_eq!(
                refused.as_ref().map(|refusal| refusal.message.clone()),
                broken.map(|(property, keyword)| range_reason(property, keyword)),
                "case {case:?}: expected the failure for {broken:?}, got {refused:?}"
            );
        }
    }

    /// The sentence a broken bound carries, which is the validator's own wording around the
    /// keyword it checked.
    #[test]
    fn the_range_reason_names_the_keyword_the_schema_states() {
        assert_eq!(
            range_reason("SamplingRate", MAXIMUM_KEYWORD),
            "Model validation failed (#/SamplingRate: failed validation constraint for keyword \
             [maximum])",
            "expected the schema validator's own wording for a broken maximum"
        );
        assert_eq!(
            range_reason("SamplingRate", MINIMUM_KEYWORD),
            "Model validation failed (#/SamplingRate: failed validation constraint for keyword \
             [minimum])",
            "expected the schema validator's own wording for a broken minimum"
        );
    }

    /// Every bounded row names a type once per property and states at least one bound, with the
    /// minimum no greater than the maximum: a row with neither bound could never report
    /// anything.
    #[test]
    fn every_bounded_row_states_a_range_it_can_judge() {
        for row in BOUNDED_PROPERTIES {
            assert!(
                row.minimum.is_some() || row.maximum.is_some(),
                "case {:?}: expected the row to state a bound for {:?}",
                row.resource_type,
                row.property
            );
            if let (Some(minimum), Some(maximum)) = (row.minimum, row.maximum) {
                assert!(
                    minimum <= maximum,
                    "case {:?}: expected {minimum} <= {maximum} for {:?}",
                    row.resource_type,
                    row.property
                );
            }
            let rows = BOUNDED_PROPERTIES
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
        }
    }
}
