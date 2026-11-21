// SPDX-License-Identifier: CC0-1.0

//! The other question a resource type's schema makes decidable about a property's VALUE: the
//! spelling it admits, stated as a pattern.
//!
//! A resource type's schema states, beside the set of values a property publishes, the
//! `pattern` a value of that property has to match — the form a value has to be IN for the
//! owning service to be able to carry the declaration out at all (an ARN of a particular
//! kind, a name over a particular alphabet). So a property document carrying a value outside
//! its property's pattern is one no handler of that type could ever be given: the stage that
//! runs before provisioning reports it, quoting the value and the pattern it does not match,
//! and the operation deploys nothing. That is the question this module answers, beside the
//! four [`super`] already answers (a required property the declaration omits, a member the
//! type does not have, a group it may carry at most one of, and a value outside the set its
//! property publishes).
//!
//! The judgement is made of the DEPLOYED property document, as every judgement of that stage
//! is, and it is the same kind of catalogue: one row per (resource type, property) whose
//! pattern the type's schema states, so a property no row covers is left to the service that
//! owns the resource rather than guessed at. A value that is not TEXT is admitted — it is no
//! spelling either way (an expression this service left unevaluated, a structure the author
//! wrote where the schema states a string), and what it fails is a constraint this stage does
//! not judge.
//!
//! A pattern is applied the way the schema language applies it: as a search over the value, so
//! an expression the schema anchors itself constrains the whole value and one it does not
//! constrains the part of the value it matches. The expressions are this catalogue's own text
//! rather than a caller's, so the one that fails to compile constrains nothing instead of
//! failing a declaration the real service accepts.

use ::serde_json::Value;

use super::{declared_properties::property_path, PropertyFailure, StoredResource};

/// One property whose admitted spelling its type's schema states as a pattern.
#[derive(Debug, Clone, Copy)]
pub struct PatternedProperty {
    /// The resource type the property belongs to, as a template declares it.
    pub resource_type: &'static str,
    /// The property's name, as a template spells it.
    pub property: &'static str,
    /// The regular expression its type's schema states for the property, in the schema's own
    /// spelling — which is also the text a refusal quotes, since what the author has to act on
    /// is the pattern the property is held to.
    pub pattern: &'static str,
}

/// The properties this stage knows a published pattern of.
///
/// Source for each row: the `Pattern` the resource type's entry in the "AWS resource and
/// property types reference" of the CloudFormation User Guide publishes for that property,
/// which is the `pattern` the type's registry schema states — the form the OWNING API
/// publishes for the member the property is passed to, so a value outside it is one that
/// service would refuse and the validation refuses it first.
///
/// Each row carries the SCHEMA's own spelling, which is the text a refusal quotes, and a row's
/// comment names the owning API member's `@pattern` beside it: the two state the same form but
/// not always with the same anchoring (a schema states the CodePipeline role-ARN form
/// unanchored and trailing `.*` where the API anchors it at the start), and what a declaration
/// is held to — and told about — is the schema's.
pub const PATTERNED_PROPERTIES: &[PatternedProperty] = &[
    // `AWS::CodePipeline::Pipeline`'s `RoleArn` is `PipelineDeclaration$roleArn`, "the Amazon
    // Resource Name (ARN) for CodePipeline to use to either perform actions with no
    // actionRoleArn, or to use to assume roles for actions with an actionRoleArn" — an IAM
    // role ARN of this partition and account, the form CodePipeline publishes for that member
    // as `^arn:aws(-[\w]+)*:iam::[0-9]{12}:role/` and the schema states as the row below.
    PatternedProperty {
        resource_type: "AWS::CodePipeline::Pipeline",
        property: "RoleArn",
        pattern: r"arn:aws(-[\w]+)*:iam::[0-9]{12}:role/.*",
    },
    // ...and its `Name` is `PipelineDeclaration$name`, "the name of the pipeline", over the
    // alphabet CodePipeline publishes for that member as `^[A-Za-z0-9.@\-_]+$` and the schema
    // states as the row below.
    PatternedProperty {
        resource_type: "AWS::CodePipeline::Pipeline",
        property: "Name",
        pattern: r"[A-Za-z0-9.@\-_]+",
    },
    // `AWS::Lambda::Function`'s `Role` is `CreateFunction$Role`, "the Amazon Resource Name
    // (ARN) of the function's execution role" — the role ARN form AWS Lambda publishes for
    // that member, which this type's schema states verbatim, anchors and all.
    PatternedProperty {
        resource_type: "AWS::Lambda::Function",
        property: "Role",
        pattern: r"^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$",
    },
    // `AWS::Pipes::Pipe`'s `RoleArn` is `CreatePipe$RoleArn`, "the ARN of the role that allows
    // the pipe to send data to the target" — the role ARN form Amazon EventBridge Pipes
    // publishes for that member, which this type's schema states verbatim, anchors and all.
    PatternedProperty {
        resource_type: "AWS::Pipes::Pipe",
        property: "RoleArn",
        pattern: r"^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z0-9+=,.@\-_/]+$",
    },
    // `AWS::CloudHSM::Cluster`'s `HsmType` is `CreateCluster$HsmType`, "the type of HSM to use
    // in the cluster" — the form CloudHSM publishes for that member, which this type's schema
    // states verbatim, anchors and all.
    PatternedProperty {
        resource_type: crate::state::template::CLOUDHSM_CLUSTER,
        property: "HsmType",
        pattern: r"^((p|)hsm[0-9][a-z.]*\.[a-zA-Z]+)$",
    },
];

/// `Some(failure)` for a resource carrying a property value outside the pattern its type's
/// schema states for that property ([`PATTERNED_PROPERTIES`]) — the first such property in the
/// order the catalogue states them; `None` for a type with no row, for a property the
/// declaration does not carry, for a value that is not text, and for every value its pattern
/// matches.
pub(super) fn value_outside_its_pattern(resource: &StoredResource) -> Option<PropertyFailure> {
    PATTERNED_PROPERTIES
        .iter()
        .enumerate()
        .filter(|(_, row)| row.resource_type == resource.resource_type)
        .find_map(|(index, row)| {
            let declared = declared_text(resource, row.property)?;
            // An expression this catalogue states that this engine will not compile
            // constrains nothing: a declaration is never refused because the stage's own
            // pattern could not be read.
            if compiled(index)?.is_match(declared) {
                return None;
            }
            Some(PropertyFailure {
                logical_id: resource.logical_id.clone(),
                resource_type: resource.resource_type.clone(),
                reason: pattern_failure_reason(declared, row.pattern),
                path: property_path(&resource.logical_id, row.property),
            })
        })
}

/// The TEXT a declaration carries for `property`, or `None` for a property it does not carry
/// and for a value that is not text — neither of which is a spelling this stage judges.
fn declared_text<'a>(resource: &'a StoredResource, property: &str) -> Option<&'a str> {
    match resource.properties.get(property) {
        Some(Value::String(declared)) => Some(declared),
        _ => None,
    }
}

/// How CloudFormation reports a property value outside the pattern its type's schema states:
/// fixed text around the VALUE the declaration carried and the pattern it does not match.
///
/// Both variable parts come from what was judged — the value is the declaration's own, the
/// pattern the one its property is held to ([`PATTERNED_PROPERTIES`]) — and the value is
/// quoted back here, unlike the set-membership failure this stage also reports, because a
/// spelling is only actionable beside the form it had to be in.
pub fn pattern_failure_reason(value: &str, pattern: &str) -> String {
    format!("Property value [{value}] does not match pattern: {pattern}")
}

/// The expression row `row` states, compiled once for the process, or `None` for one this
/// engine will not compile.
///
/// The rows are this catalogue's own static text, so they are compiled ONCE on the first
/// declaration this stage judges rather than per operation, and every match then costs time
/// linear in the value (the engine is the linear-time one).
fn compiled(row: usize) -> Option<&'static ::regex::Regex> {
    static COMPILED: ::std::sync::OnceLock<Vec<Option<::regex::Regex>>> =
        ::std::sync::OnceLock::new();
    COMPILED
        .get_or_init(|| {
            PATTERNED_PROPERTIES
                .iter()
                .map(|row| ::regex::Regex::new(row.pattern).ok())
                .collect()
        })
        .get(row)?
        .as_ref()
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

    /// Every row of the catalogue compiles: a pattern this engine will not read would
    /// constrain nothing at all.
    #[test]
    fn every_stated_pattern_compiles() {
        for (index, row) in PATTERNED_PROPERTIES.iter().enumerate() {
            assert!(
                compiled(index).is_some(),
                "case {:?}: expected the pattern {:?} of {} to compile",
                row.property,
                row.pattern,
                row.resource_type
            );
        }
    }

    /// One row of the table below: the case, the resource type declared, the property
    /// document it carries, and the `(property, value)` the stage must report — `None` where
    /// every judged value is one its pattern admits.
    type PatternRow<'a> = (&'a str, &'a str, Value, Option<(&'a str, &'a str)>);

    /// RULE: a value outside the pattern its type's schema states for the property fails the
    /// validation, quoting that value and that pattern at the property's own path; a value the
    /// pattern matches does not. The rows span the catalogue — types and properties the report
    /// that led here did not cite among them — and the forms a value can take.
    #[test]
    fn a_value_outside_its_pattern_fails_the_validation() {
        // (case, type, properties, the (property, value) the stage must report — `None` for a
        // declaration every judged value of which its pattern matches).
        let rows: &[PatternRow] = &[
            (
                "a pipeline whose role is no ARN at all",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": "not-an-arn"}),
                Some(("RoleArn", "not-an-arn")),
            ),
            (
                "a pipeline whose role ARN names another service",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": "arn:aws:codepipeline:us-east-1:000000000000:role/p"}),
                Some((
                    "RoleArn",
                    "arn:aws:codepipeline:us-east-1:000000000000:role/p",
                )),
            ),
            (
                "a pipeline whose role ARN carries no account",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": "arn:aws:iam:::role/p"}),
                Some(("RoleArn", "arn:aws:iam:::role/p")),
            ),
            (
                "a pipeline whose role ARN is of a partition the form admits",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": "arn:aws-us-gov:iam::000000000000:role/p"}),
                None,
            ),
            (
                "a pipeline whose name is over the alphabet its schema states",
                "AWS::CodePipeline::Pipeline",
                json!({"Name": "release.pipeline-1_2@ci",
                       "RoleArn": "arn:aws:iam::000000000000:role/p"}),
                None,
            ),
            (
                "a pipeline whose name is over no character that alphabet admits",
                "AWS::CodePipeline::Pipeline",
                json!({"Name": "!!!", "RoleArn": "arn:aws:iam::000000000000:role/p"}),
                Some(("Name", "!!!")),
            ),
            (
                "a function whose execution role is a role NAME rather than an ARN",
                "AWS::Lambda::Function",
                json!({"Role": "my-exec-role"}),
                Some(("Role", "my-exec-role")),
            ),
            (
                "a function whose execution role ARN is of this account",
                "AWS::Lambda::Function",
                json!({"Role": "arn:aws:iam::000000000000:role/service-role/exec"}),
                None,
            ),
            (
                "a pipe whose role ARN carries an account of the wrong width",
                "AWS::Pipes::Pipe",
                json!({"RoleArn": "arn:aws:iam::123:role/pipe",
                       "Source": "arn:aws:sqs:us-east-1:000000000000:q",
                       "Target": "arn:aws:sqs:us-east-1:000000000000:t"}),
                Some(("RoleArn", "arn:aws:iam::123:role/pipe")),
            ),
            (
                "a pipe whose role ARN is well formed",
                "AWS::Pipes::Pipe",
                json!({"RoleArn": "arn:aws:iam::000000000000:role/pipe",
                       "Source": "arn:aws:sqs:us-east-1:000000000000:q",
                       "Target": "arn:aws:sqs:us-east-1:000000000000:t"}),
                None,
            ),
            (
                "a pipeline whose role reads an attribute that has not landed yet",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": Value::Null}),
                None,
            ),
            (
                "a pipeline carrying a structure where the schema states a string",
                "AWS::CodePipeline::Pipeline",
                json!({"RoleArn": {"Fn::GetAtt": ["Role", "Arn"]}}),
                None,
            ),
            (
                "a type this stage knows no pattern of",
                "AWS::SNS::Topic",
                json!({"TopicName": "not a topic name"}),
                None,
            ),
            (
                "a cluster whose HSM type is no HSM type at all",
                crate::state::template::CLOUDHSM_CLUSTER,
                json!({"HsmType": "not-a-valid-hsm-type", "SubnetIds": ["subnet-0a1b2c3d"]}),
                Some(("HsmType", "not-a-valid-hsm-type")),
            ),
            (
                "a cluster whose HSM type names the family without a size",
                crate::state::template::CLOUDHSM_CLUSTER,
                json!({"HsmType": "hsm2m", "SubnetIds": ["subnet-0a1b2c3d"]}),
                Some(("HsmType", "hsm2m")),
            ),
            (
                "a cluster whose HSM type is one the form admits",
                crate::state::template::CLOUDHSM_CLUSTER,
                json!({"HsmType": "hsm2m.medium", "SubnetIds": ["subnet-0a1b2c3d"]}),
                None,
            ),
            (
                "a cluster whose HSM type is the earlier generation, which the form admits too",
                crate::state::template::CLOUDHSM_CLUSTER,
                json!({"HsmType": "hsm1.medium", "SubnetIds": ["subnet-0a1b2c3d"]}),
                None,
            ),
        ];
        for (case, resource_type, properties, reported) in rows {
            let resources = vec![declared("R", resource_type, properties.clone())];
            let failed = super::super::failure(&resources);
            let expected = reported.map(|(property, value)| {
                let pattern = PATTERNED_PROPERTIES
                    .iter()
                    .find(|row| row.resource_type == *resource_type && row.property == property)
                    .map(|row| row.pattern)
                    .unwrap_or_default();
                (
                    pattern_failure_reason(value, pattern),
                    format!("/Resources/R/Properties/{property}"),
                )
            });
            assert_eq!(
                failed
                    .as_ref()
                    .map(|failure| (failure.reason.clone(), failure.path.clone())),
                expected,
                "case {case:?}: expected the failure {expected:?}, got {failed:?}"
            );
        }
    }
}
