// SPDX-License-Identifier: CC0-1.0

//! Reading the declaration of an `AWS::Lambda::*` resource as the TYPES AWS Lambda models its
//! own request members as.
//!
//! A template's scalar is TEXT wherever its author quoted it, wherever the YAML a transform
//! rendered carries it quoted, and wherever it arrived through a `Ref` to a parameter — while
//! the member of the Lambda call it reaches is modelled as an integer or a boolean. So every arm
//! that provisions one of these types reads its declaration through [`lambda_declaration`]
//! before the request is sent, which is the walk the provisioning layer already shares
//! ([`super::modelled_declaration`]) under the member catalogue below.
//!
//! The catalogue is ONE classifier for the whole family rather than one per type, because the
//! member names come from a single model: a name AWS Lambda types as an integer in one of its
//! requests is that integer in every request that carries it, and a resource type's declaration
//! can only hold the members of the calls its own arm makes.

use ::serde_json::Value;

use super::{modelled_declaration, ModelledScalar};

/// `declared`, with every member AWS Lambda's model types as a number or a boolean read as that
/// type from the text a template may carry it as, at every depth the declaration nests it.
///
/// A value already of the modelled type travels as written, and text that names no such value is
/// left exactly as the template wrote it: AWS Lambda's own validation is what judges it, and its
/// refusal is what the resource then fails with.
pub(super) fn lambda_declaration(declared: &Value) -> Value {
    modelled_declaration("", declared, &lambda_scalar)
}

/// What AWS Lambda's model types one member of its own requests as.
///
/// Source: the member lists of the calls this family's arms make, as that service's own model
/// states them (`smithy_query(query="operation",operation="CreateFunction")` and the same query
/// for `CreateEventSourceMapping`, `PutFunctionConcurrency`,
/// `PutFunctionEventInvokeConfig`, `PutProvisionedConcurrencyConfig` and `CreateAlias`, with the
/// nested structures read through `smithy_query(query="shape",…)`).
fn lambda_scalar(member: &str) -> ModelledScalar {
    match member {
        // `CreateFunction`: `Timeout` and `MemorySize` are `@range`-bounded integers, and
        // `EphemeralStorage$Size`, `DurableConfig$RetentionPeriodInDays` and
        // `DurableConfig$ExecutionTimeout` are the integers its nested structures hold.
        "Timeout"
        | "MemorySize"
        | "Size"
        | "RetentionPeriodInDays"
        | "ExecutionTimeout"
        // `PutFunctionConcurrency$ReservedConcurrentExecutions` and
        // `PutProvisionedConcurrencyConfig$ProvisionedConcurrentExecutions`: the integers the
        // two concurrency calls a function, a version and an alias are configured through take.
        | "ReservedConcurrentExecutions"
        | "ProvisionedConcurrentExecutions"
        // `CreateEventSourceMapping`: its own integer members, plus
        // `ScalingConfig$MaximumConcurrency` and `ProvisionedPollerConfig`'s two bounds.
        | "BatchSize"
        | "MaximumBatchingWindowInSeconds"
        | "MaximumRecordAgeInSeconds"
        | "MaximumRetryAttempts"
        | "ParallelizationFactor"
        | "TumblingWindowInSeconds"
        | "MaximumConcurrency"
        | "MinimumPollers"
        | "MaximumPollers"
        // `PutFunctionEventInvokeConfig$MaximumEventAgeInSeconds` (its `MaximumRetryAttempts` is
        // the same member name as the mapping's, above).
        | "MaximumEventAgeInSeconds"
        // `CreateEventSourceMapping$StartingPositionTimestamp`: modelled as a timestamp, which
        // this wire carries as epoch SECONDS, and which the resource type publishes as a number
        // of its own — so a template that quotes it is read the same way. Text that names no
        // number at all travels as written, so a date spelled out is left to AWS Lambda.
        | "StartingPositionTimestamp" => ModelledScalar::Number,
        // `VpcConfig$Ipv6AllowedForDualStack`, and `CreateEventSourceMapping`'s `Enabled` and
        // `BisectBatchOnFunctionError`: the booleans this family's requests carry.
        "Ipv6AllowedForDualStack" | "Enabled" | "BisectBatchOnFunctionError" => {
            ModelledScalar::Boolean
        }
        // `Environment$Variables` is a map of `TagKey`-like names to STRING values, and `Tags` a
        // map of tag keys to tag values: both are keyed by names the requester chose, and a
        // variable or a tag whose value reads as a number is still the text it was declared as.
        "Variables" | "Tags" => ModelledScalar::TextThroughout,
        // `AliasRoutingConfiguration$AdditionalVersionWeights` is a map of VERSION to `Weight`,
        // a double: its keys are version numbers rather than member names, so every value under
        // it is the number the model types it as.
        "AdditionalVersionWeights" => ModelledScalar::NumberThroughout,
        _ => ModelledScalar::AsWritten,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a member AWS Lambda's model types as a number or a boolean is read as that type
    /// from the text a template carries it as, at every depth, while a collection keyed by the
    /// requester's own names is read as the kind of value the model types that collection to
    /// hold.
    ///
    /// The rows differ along what the rule generalises over: the owning call, the modelled type,
    /// the depth the member sits at, whether it stands inside a list, whether the declaration
    /// already states the modelled type, and whether the holder is a collection of text or of
    /// numbers. The catalogue is checked on members the issue did not cite as well as on the two
    /// it did.
    #[test]
    fn a_declared_scalar_is_read_as_the_type_lambda_models_it_as() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "a function's two quoted integers",
                ::serde_json::json!({"MemorySize": "512", "Timeout": "30"}),
                ::serde_json::json!({"MemorySize": 512, "Timeout": 30}),
            ),
            (
                "an integer a nested structure holds",
                ::serde_json::json!({"EphemeralStorage": {"Size": "1024"}}),
                ::serde_json::json!({"EphemeralStorage": {"Size": 1024}}),
            ),
            (
                "a quoted boolean beside lists of text",
                ::serde_json::json!({"VpcConfig": {
                    "SubnetIds": ["subnet-1"],
                    "Ipv6AllowedForDualStack": "true"
                }}),
                ::serde_json::json!({"VpcConfig": {
                    "SubnetIds": ["subnet-1"],
                    "Ipv6AllowedForDualStack": true
                }}),
            ),
            (
                "a mapping's own integers and boolean",
                ::serde_json::json!({
                    "BatchSize": "10",
                    "Enabled": "false",
                    "ScalingConfig": {"MaximumConcurrency": "5"}
                }),
                ::serde_json::json!({
                    "BatchSize": 10,
                    "Enabled": false,
                    "ScalingConfig": {"MaximumConcurrency": 5}
                }),
            ),
            (
                "a starting position stated in the epoch seconds the wire carries",
                ::serde_json::json!({
                    "StartingPosition": "AT_TIMESTAMP",
                    "StartingPositionTimestamp": "1700000000"
                }),
                ::serde_json::json!({
                    "StartingPosition": "AT_TIMESTAMP",
                    "StartingPositionTimestamp": 1_700_000_000
                }),
            ),
            (
                "an event invoke configuration's two integers",
                ::serde_json::json!({
                    "MaximumEventAgeInSeconds": "600",
                    "MaximumRetryAttempts": "1"
                }),
                ::serde_json::json!({"MaximumEventAgeInSeconds": 600, "MaximumRetryAttempts": 1}),
            ),
            (
                "a declaration already of the modelled type",
                ::serde_json::json!({"MemorySize": 256, "Timeout": 3}),
                ::serde_json::json!({"MemorySize": 256, "Timeout": 3}),
            ),
            (
                "environment variables and tags that read as numbers",
                ::serde_json::json!({
                    "Environment": {"Variables": {"MemorySize": "512", "RETRIES": "3"}},
                    "Tags": {"Timeout": "30"}
                }),
                ::serde_json::json!({
                    "Environment": {"Variables": {"MemorySize": "512", "RETRIES": "3"}},
                    "Tags": {"Timeout": "30"}
                }),
            ),
            (
                "the weights an alias routes with",
                ::serde_json::json!({"RoutingConfig": {"AdditionalVersionWeights": {"2": "0.3"}}}),
                ::serde_json::json!({"RoutingConfig": {"AdditionalVersionWeights": {"2": 0.3}}}),
            ),
            (
                "text that names no number",
                ::serde_json::json!({"MemorySize": "lots"}),
                ::serde_json::json!({"MemorySize": "lots"}),
            ),
        ];
        for (case, declared, expected) in cases {
            let actual = lambda_declaration(declared);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }
}
