// SPDX-License-Identifier: CC0-1.0

//! The `CustomActionLambda` execution block of a workflow step, PERFORMED.
//!
//! The block names AWS Lambda functions ("An Amazon Web Services Lambda execution block",
//! `ExecutionBlockConfiguration$customActionLambdaConfig`), so performing the step is
//! invoking them: the functions are reached through the in-process callback into this same
//! emulator, synchronously, and what they answer is what the step comes to — a function
//! that returns completes it, and a function that cannot be invoked or that fails the
//! invocation fails it. Nothing is claimed for a function that did not run.
//!
//! Three things the model documents decide the invocation:
//!
//! * WHICH function runs. `CustomActionLambdaConfiguration$lambdas` is a list of one or
//!   two functions (`@length(1, 2)`) and `regionToRun` is "The Amazon Web Services Region
//!   for the function to run in", so a block that names one function per Region runs the
//!   one that lives in the Region [`ExecutionRegions::resolve`] resolves. A block whose
//!   functions name no ARN in that Region names only functions to run as they stand, so
//!   each of them is invoked.
//! * WHERE it is addressed: the Region of the function's own ARN, falling back to the
//!   resolved Region for an ARN that names none. A Region ends up in the AUTHORITY of the
//!   endpoint this hop is made on, so it is read under the Region grammar every model
//!   states for the field ([`::service_common::arn::region_of`]) rather than as arbitrary
//!   text: no part of a caller's request is a destination.
//! * WHAT it is handed. `Lambdas$payload` states the rule: "If you don't specify a
//!   payload, Region switch invokes the function with only the execution metadata. If you
//!   provide a JSON object, Region switch merges execution metadata keys (`InvokedBy`,
//!   `PlanArn`, `ExecutionId`, `StepName`) into the top level. If one of your keys has the
//!   same name as a metadata key, your key takes precedence. If you provide any other JSON
//!   type (string, number, boolean, or array), Region switch forwards it to the Lambda
//!   function unchanged, without adding metadata."
//!
//! Every value in the metadata is read off the execution record, so what a function is
//! handed is what `GetPlanExecution` reports about the run that invoked it.

use ::aws_smithy_types::error::display::DisplayErrorContext;
use ::aws_smithy_types::Blob;
use ::serde_json::{Map, Value};

use crate::ops::types::{CustomActionLambdaConfiguration, Lambdas};

/// The ARN vendor of an AWS Lambda function, which `Lambdas$arn` names one of.
const LAMBDA_ARN_VENDOR: &str = "lambda";

/// What the execution metadata names as the invoker of a custom action: this service,
/// under the service principal its namespace gives it
/// (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`:
/// `arnNamespace` `arc-region-switch`), which is the identity the invocation is made as.
pub const INVOKED_BY: &str = "arc-region-switch.amazonaws.com";

/// The metadata member naming the invoker, from `Lambdas$payload`'s documented key set.
const INVOKED_BY_KEY: &str = "InvokedBy";

/// The metadata member naming the plan.
const PLAN_ARN_KEY: &str = "PlanArn";

/// The metadata member naming the execution.
const EXECUTION_ID_KEY: &str = "ExecutionId";

/// The metadata member naming the step whose block this is.
const STEP_NAME_KEY: &str = "StepName";

/// The modelled `RegionToRunIn` values
/// (`smithy_query(query="shape", shape="RegionToRunIn", model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`),
/// documented "For recovery workflows use `activatingRegion` or `deactivatingRegion`. For
/// post-recovery workflows, use `activeRegion` (the Region with customer traffic) or
/// `inactiveRegion` (the Region with no customer traffic)".
const ACTIVATING_REGION: &str = "activatingRegion";
const DEACTIVATING_REGION: &str = "deactivatingRegion";
const ACTIVE_REGION: &str = "activeRegion";
const INACTIVE_REGION: &str = "inactiveRegion";

/// Which concrete Region each modelled `RegionToRunIn` value names for ONE execution.
///
/// Captured when the execution is created, from the Regions that execution shifts traffic
/// between, so a later edit of the plan cannot move where a running execution's functions
/// are invoked.
#[derive(Debug, Clone)]
pub struct ExecutionRegions {
    /// The Region traffic is being shifted TO — which is also the Region that carries
    /// customer traffic once the switch is made. `None` for a `postRecovery` execution,
    /// which shifts no traffic at all.
    pub activating: Option<String>,
    /// The Region traffic is being shifted AWAY from; `None` for the same reason.
    pub deactivating: Option<String>,
    /// The Region the execution itself targets: one of the plan's two, and what every
    /// value falls back to.
    pub target: String,
    /// The plan's OTHER Region.
    pub counterpart: Option<String>,
}

impl ExecutionRegions {
    /// The Region `region_to_run` names for this execution.
    ///
    /// `activatingRegion` and `activeRegion` both name the Region carrying customer
    /// traffic after the switch, and `deactivatingRegion` and `inactiveRegion` both name
    /// the one it is taken from. A `postRecovery` execution shifts no traffic, so it
    /// activates and deactivates nothing: there the Region it targets is the active one
    /// and the plan's other Region is the inactive one.
    #[must_use]
    pub fn resolve(&self, region_to_run: &str) -> String {
        let resolved = match region_to_run {
            ACTIVATING_REGION | ACTIVE_REGION => self.activating.clone(),
            DEACTIVATING_REGION | INACTIVE_REGION => {
                self.deactivating.clone().or_else(|| self.counterpart.clone())
            }
            // A value the enum does not declare names no Region of this execution; the
            // boundary validator has already refused one.
            _ => None,
        };
        resolved.unwrap_or_else(|| self.target.clone())
    }
}

/// The execution metadata every invocation of one step's block carries.
#[derive(Debug, Clone)]
pub struct InvocationMetadata {
    pub plan_arn: String,
    pub execution_id: String,
    pub step_name: String,
}

impl InvocationMetadata {
    /// The metadata as the members `Lambdas$payload` names them.
    fn members(&self) -> Map<String, Value> {
        Map::from_iter([
            (
                INVOKED_BY_KEY.to_string(),
                Value::String(INVOKED_BY.to_string()),
            ),
            (
                PLAN_ARN_KEY.to_string(),
                Value::String(self.plan_arn.clone()),
            ),
            (
                EXECUTION_ID_KEY.to_string(),
                Value::String(self.execution_id.clone()),
            ),
            (
                STEP_NAME_KEY.to_string(),
                Value::String(self.step_name.clone()),
            ),
        ])
    }
}

/// What one step's `CustomActionLambda` block invokes: the functions it names, resolved to
/// the Region each is addressed in, in the order the block lists them.
#[derive(Debug, Clone)]
pub struct LambdaCustomAction {
    pub invocations: Vec<LambdaInvocation>,
}

/// One function invocation of such a block.
#[derive(Debug, Clone)]
pub struct LambdaInvocation {
    /// The function's ARN, as `Lambdas$arn` names it.
    pub function: String,
    /// The Region the invocation is addressed in: the Region of the function's own ARN,
    /// falling back to the Region the block runs in. `None` when neither is a Region LABEL,
    /// which is no Region a Lambda endpoint is reached in — so the invocation is not made
    /// at all rather than addressed at a host read off caller text.
    pub region: Option<String>,
    /// The `payload` the block configures for this function, verbatim.
    pub payload: Option<String>,
}

impl LambdaCustomAction {
    /// What performing `config` invokes, for an execution shifting traffic between
    /// `regions`.
    #[must_use]
    pub fn of(config: &CustomActionLambdaConfiguration, regions: &ExecutionRegions) -> Self {
        let region_to_run = regions.resolve(&config.region_to_run);
        let named: Vec<&Lambdas> = config
            .lambdas
            .iter()
            .filter(|lambda| lambda.arn.as_deref().is_some_and(|arn| !arn.is_empty()))
            .collect();
        let in_region: Vec<&Lambdas> = named
            .iter()
            .copied()
            .filter(|lambda| {
                lambda
                    .arn
                    .as_deref()
                    .and_then(function_region)
                    .is_some_and(|region| region == region_to_run)
            })
            .collect();
        let chosen = if in_region.is_empty() { named } else { in_region };
        Self {
            invocations: chosen
                .into_iter()
                .map(|lambda| {
                    let function = lambda.arn.clone().unwrap_or_default();
                    let region = function_region(&function)
                        .or_else(|| ::service_common::arn::region_label(&region_to_run))
                        .map(ToString::to_string);
                    LambdaInvocation {
                        function,
                        region,
                        payload: lambda.payload.clone(),
                    }
                })
                .collect(),
        }
    }

    /// Invoke every function of the block, in order, and report what the step comes to:
    /// `Ok` once each of them returned, and `Err(why)` for the first one that did not.
    ///
    /// Call it with NO store lock held: each invocation is an in-process call this emulator
    /// serves.
    pub async fn perform(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
        metadata: &InvocationMetadata,
    ) -> Result<(), String> {
        if self.invocations.is_empty() {
            // Nothing names a function to invoke, so there is no custom action to perform
            // and no outcome to report for one.
            return Err(format!(
                "The custom action of step {} names no Lambda function to invoke",
                metadata.step_name
            ));
        }
        for invocation in &self.invocations {
            invocation.perform(callback, metadata).await?;
        }
        Ok(())
    }
}

impl LambdaInvocation {
    /// Invoke this one function and report whether it performed the step.
    async fn perform(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
        metadata: &InvocationMetadata,
    ) -> Result<(), String> {
        let payload = invocation_payload(self.payload.as_deref(), metadata);
        let Some(region) = self.region.as_deref() else {
            return Err(format!(
                "The Lambda function {} names no Region in which a function can be invoked",
                self.function
            ));
        };
        let answered = lambda_client(callback, region)
            .invoke()
            .function_name(&self.function)
            // The outcome of the step is what the function answered, so the invocation is
            // SYNCHRONOUS: `RequestResponse` "keep[s] the connection open until the
            // function returns a response or times out", which is Lambda's own default.
            .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
            .payload(Blob::new(payload))
            .send()
            .await;
        let answer = match answered {
            Ok(answer) => answer,
            Err(error) => {
                return Err(format!(
                    "The Lambda function {} could not be invoked: {}",
                    self.function,
                    DisplayErrorContext(&error)
                ))
            }
        };
        // "An unhandled error is still HTTP 200": a function that failed says so through
        // `FunctionError`, with the error document as the payload, so the invocation is
        // read off that signal rather than off the status.
        if let Some(function_error) = answer.function_error() {
            let detail = answer
                .payload()
                .map(|payload| String::from_utf8_lossy(payload.as_ref()).to_string())
                .unwrap_or_default();
            return Err(format!(
                "The Lambda function {} failed the invocation ({function_error}): {detail}",
                self.function
            ));
        }
        Ok(())
    }
}

/// The Region an AWS Lambda function's ARN names, under the grammar every model's ARN
/// pattern states for the field — so a value that would re-point or truncate the authority
/// of the endpoint this hop is addressed on names no Region at all.
fn function_region(arn: &str) -> Option<&str> {
    ::service_common::arn::region_of(arn, LAMBDA_ARN_VENDOR)
}

/// The bytes one invocation is made with, under `Lambdas$payload`'s documented rule (see
/// the module docs).
fn invocation_payload(configured: Option<&str>, metadata: &InvocationMetadata) -> Vec<u8> {
    let Some(configured) = configured else {
        return Value::Object(metadata.members()).to_string().into_bytes();
    };
    match ::serde_json::from_str::<Value>(configured) {
        Ok(Value::Object(supplied)) => {
            let mut merged = metadata.members();
            // The caller's keys are applied over the metadata, so one that has the same
            // name as a metadata key takes precedence.
            merged.extend(supplied);
            Value::Object(merged).to_string().into_bytes()
        }
        // Any other JSON type is forwarded unchanged — and so is a payload that is not
        // JSON at all, which is no more this service's to rewrite.
        _ => configured.as_bytes().to_vec(),
    }
}

/// An AWS Lambda client over the in-process callback into this same emulator, addressed in
/// `region`.
fn lambda_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://lambda.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials are handed
        // over directly.
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Regions of an `activate` execution targeting the second Region of a plan
    /// covering both.
    fn activating_regions() -> ExecutionRegions {
        ExecutionRegions {
            activating: Some("us-west-2".to_string()),
            deactivating: Some("us-east-1".to_string()),
            target: "us-west-2".to_string(),
            counterpart: Some("us-east-1".to_string()),
        }
    }

    /// Every modelled `RegionToRunIn` value names a Region of the execution, and a
    /// `postRecovery` execution — which activates and deactivates nothing — still resolves
    /// each of them.
    #[test]
    fn every_region_to_run_in_value_resolves_to_a_region_of_the_execution() {
        let post_recovery = ExecutionRegions {
            activating: None,
            deactivating: None,
            target: "eu-west-1".to_string(),
            counterpart: Some("eu-central-1".to_string()),
        };
        for (case, regions, value, expected) in [
            (
                "an activate execution runs in the Region it activates",
                activating_regions(),
                ACTIVATING_REGION,
                "us-west-2",
            ),
            (
                "an activate execution deactivates the plan's other Region",
                activating_regions(),
                DEACTIVATING_REGION,
                "us-east-1",
            ),
            (
                "the Region carrying traffic after the switch is the activated one",
                activating_regions(),
                ACTIVE_REGION,
                "us-west-2",
            ),
            (
                "the Region carrying no traffic after the switch is the deactivated one",
                activating_regions(),
                INACTIVE_REGION,
                "us-east-1",
            ),
            (
                "a post-recovery execution's active Region is the one it targets",
                post_recovery.clone(),
                ACTIVE_REGION,
                "eu-west-1",
            ),
            (
                "a post-recovery execution's inactive Region is the plan's other one",
                post_recovery.clone(),
                INACTIVE_REGION,
                "eu-central-1",
            ),
            (
                "a post-recovery execution activates nothing, so it runs where it targets",
                post_recovery,
                ACTIVATING_REGION,
                "eu-west-1",
            ),
        ] {
            let resolved = regions.resolve(value);
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected {value} to resolve to {expected}, got {resolved:?}"
            );
        }
    }

    /// A block naming one function per Region runs the one in the Region it says to run
    /// in; one whose functions name no ARN in that Region runs the functions it names.
    #[test]
    fn the_functions_invoked_are_the_ones_the_block_runs_in_that_region() {
        let function = |region: &str, name: &str| Lambdas {
            arn: Some(format!("arn:aws:lambda:{region}:000000000000:function:{name}")),
            ..Lambdas::default()
        };
        let config = |lambdas: Vec<Lambdas>, region_to_run: &str| CustomActionLambdaConfiguration {
            timeout_minutes: 60,
            lambdas,
            retry_interval_minutes: 1.0,
            region_to_run: region_to_run.to_string(),
            ungraceful: None,
        };
        for (case, config, expected) in [
            (
                "one function per Region, running in the activating one",
                config(
                    vec![
                        function("us-east-1", "east"),
                        function("us-west-2", "west"),
                    ],
                    ACTIVATING_REGION,
                ),
                vec!["arn:aws:lambda:us-west-2:000000000000:function:west"],
            ),
            (
                "one function per Region, running in the deactivating one",
                config(
                    vec![
                        function("us-east-1", "east"),
                        function("us-west-2", "west"),
                    ],
                    DEACTIVATING_REGION,
                ),
                vec!["arn:aws:lambda:us-east-1:000000000000:function:east"],
            ),
            (
                "one function, in another Region than the block runs in",
                config(vec![function("eu-west-1", "elsewhere")], ACTIVATING_REGION),
                vec!["arn:aws:lambda:eu-west-1:000000000000:function:elsewhere"],
            ),
            (
                "two functions in the same Region as the block runs in",
                config(
                    vec![function("us-west-2", "first"), function("us-west-2", "second")],
                    ACTIVATING_REGION,
                ),
                vec![
                    "arn:aws:lambda:us-west-2:000000000000:function:first",
                    "arn:aws:lambda:us-west-2:000000000000:function:second",
                ],
            ),
            (
                "an entry naming no function at all",
                config(
                    vec![Lambdas::default(), function("us-west-2", "west")],
                    ACTIVATING_REGION,
                ),
                vec!["arn:aws:lambda:us-west-2:000000000000:function:west"],
            ),
        ] {
            let action = LambdaCustomAction::of(&config, &activating_regions());
            let invoked: Vec<&str> = action
                .invocations
                .iter()
                .map(|invocation| invocation.function.as_str())
                .collect();
            assert_eq!(
                invoked, expected,
                "case {case:?}: expected {expected:?} invoked, got {invoked:?}"
            );
        }
    }

    /// The Region an invocation is addressed in only ever comes from the Region GRAMMAR: an
    /// ARN whose Region field is not a label names no Region of its own, so the invocation
    /// is addressed in the Region the block runs in rather than at a host read off caller
    /// text — and a block with no Region label at all is not invoked anywhere.
    #[test]
    fn the_region_an_invocation_is_addressed_in_is_always_a_region_label() {
        let function_of = |arn: &str| CustomActionLambdaConfiguration {
            timeout_minutes: 60,
            lambdas: vec![Lambdas {
                arn: Some(arn.to_string()),
                ..Lambdas::default()
            }],
            retry_interval_minutes: 1.0,
            region_to_run: ACTIVATING_REGION.to_string(),
            ungraceful: None,
        };
        let over_long = "a".repeat(::service_common::arn::MAX_REGION_LENGTH + 1);
        for (case, arn, expected) in [
            (
                "a function ARN naming a Region of its own",
                "arn:aws:lambda:eu-west-1:000000000000:function:switch".to_string(),
                Some("eu-west-1"),
            ),
            (
                "a Region field carrying an authority separator",
                "arn:aws:lambda:example.com/other:000000000000:function:switch".to_string(),
                Some("us-west-2"),
            ),
            (
                "a Region field longer than any Region label",
                format!("arn:aws:lambda:{over_long}:000000000000:function:switch"),
                Some("us-west-2"),
            ),
            (
                "an ARN of another vendor",
                "arn:aws:states:eu-west-1:000000000000:stateMachine:switch".to_string(),
                Some("us-west-2"),
            ),
            (
                "a value that is not an ARN at all",
                "switch".to_string(),
                Some("us-west-2"),
            ),
        ] {
            let action = LambdaCustomAction::of(&function_of(&arn), &activating_regions());
            let addressed: Vec<Option<&str>> = action
                .invocations
                .iter()
                .map(|invocation| invocation.region.as_deref())
                .collect();
            assert_eq!(
                addressed,
                vec![expected],
                "case {case:?}: expected the invocation addressed in {expected:?}, got \
                 {addressed:?}"
            );
        }

        // Neither the ARN nor the Region the block runs in is a Region label: there is no
        // Lambda endpoint to address, so nothing is invoked.
        let nowhere = ExecutionRegions {
            activating: Some("example.com/other".to_string()),
            deactivating: None,
            target: "example.com/other".to_string(),
            counterpart: None,
        };
        let action = LambdaCustomAction::of(&function_of("switch"), &nowhere);
        let addressed: Vec<Option<&str>> = action
            .invocations
            .iter()
            .map(|invocation| invocation.region.as_deref())
            .collect();
        assert_eq!(
            addressed,
            vec![None],
            "case \"no Region label anywhere\": expected no Region to address, got {addressed:?}"
        );
    }

    /// `Lambdas$payload`'s rule: no payload is the metadata alone, a JSON object is the
    /// object with the metadata merged in and the caller's keys winning, and any other JSON
    /// type is forwarded unchanged.
    #[test]
    fn the_payload_follows_the_documented_merge_rule() {
        let metadata = InvocationMetadata {
            plan_arn: "arn:aws:arc-region-switch::000000000000:plan/switch:abc123".to_string(),
            execution_id: "us-west-2/0123456789abcdef".to_string(),
            step_name: "Invoke".to_string(),
        };
        for (case, configured, expected) in [
            (
                "no payload: the metadata alone",
                None,
                Value::Object(metadata.members()),
            ),
            (
                "a JSON object: the metadata merged into it",
                Some(r#"{"mine": 1}"#),
                ::serde_json::json!({
                    "InvokedBy": INVOKED_BY,
                    "PlanArn": metadata.plan_arn,
                    "ExecutionId": metadata.execution_id,
                    "StepName": metadata.step_name,
                    "mine": 1,
                }),
            ),
            (
                "a JSON object colliding with a metadata key: the caller's key wins",
                Some(r#"{"PlanArn": "mine"}"#),
                ::serde_json::json!({
                    "InvokedBy": INVOKED_BY,
                    "PlanArn": "mine",
                    "ExecutionId": metadata.execution_id,
                    "StepName": metadata.step_name,
                }),
            ),
            (
                "a JSON array: forwarded unchanged",
                Some("[1,2]"),
                ::serde_json::json!([1, 2]),
            ),
            (
                "a JSON string: forwarded unchanged",
                Some(r#""plain""#),
                Value::String("plain".to_string()),
            ),
            (
                "a JSON number: forwarded unchanged",
                Some("7"),
                ::serde_json::json!(7),
            ),
        ] {
            let bytes = invocation_payload(configured, &metadata);
            let parsed: Value = ::serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("case {case:?}: the payload is JSON"));
            assert_eq!(
                parsed, expected,
                "case {case:?}: expected the payload {expected}, got {parsed}"
            );
        }
    }

    /// A payload that is not JSON at all is handed over as the bytes it is.
    #[test]
    fn a_payload_that_is_not_json_is_forwarded_as_it_stands() {
        let metadata = InvocationMetadata {
            plan_arn: "arn:aws:arc-region-switch::000000000000:plan/switch:abc123".to_string(),
            execution_id: "us-west-2/0123456789abcdef".to_string(),
            step_name: "Invoke".to_string(),
        };
        let bytes = invocation_payload(Some("not json"), &metadata);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "not json",
            "case \"a payload that is not JSON\": expected it forwarded as it stands, got \
             {bytes:?}"
        );
    }
}
