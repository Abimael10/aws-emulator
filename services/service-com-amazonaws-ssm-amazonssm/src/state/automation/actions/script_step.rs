// SPDX-License-Identifier: CC0-1.0

//! The script an `aws:executeScript` step runs.
//!
//! "aws:executeScript … runs the Python or PowerShell script provided using the specified
//! runtime and handler": the step carries the `Script` itself, the `Runtime` it is to run in
//! and the `Handler` to call in it, and `InputPayload` is "a JSON or YAML object that will be
//! passed to the Handler" — so the step's answer is whatever that handler RETURNED, which is
//! why the selectors of such a step read `$.Payload…`.
//!
//! The script is run by the ONE contained Python interpreter this workspace embeds (the
//! Lambda service crate's `state::execution::python`), on a thread of its own with no store
//! guard held, with the AWS SDK modules that runtime supplies on the import path — so a
//! script's own `boto3` call reaches this same emulator, signed as the role the automation
//! runs as, and its effects are real effects. The step's declared `Runtime` is handed to that
//! run, which is how the interpreter the script executes in reports the RELEASE the step asked
//! for rather than the build this process happens to embed. Nothing of the host is reachable
//! from it: the engine registers no socket, subprocess, memory-map or host-library module,
//! supplies its own in-memory operating-system modules, and holds every bound in host code the
//! script cannot reach.
//!
//! What the step reports is what that run came to: the declared outputs selected out of the
//! handler's own return value ([`super::selectors`]), the exception it raised as the step's
//! failure, or the bound it ran past. A runtime this emulator embeds no engine for is a
//! script that did not run, and the step says exactly that rather than reporting an outcome
//! nothing produced.

use ::serde_json::{Map, Value};

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    environment_naming_runtime, AwsAccess,
};

use super::{
    script_budget, selectors, text_input, ActionOutcome, ClaimedAction, SCRIPT_CLOCK_TICK,
    SCRIPT_HOST_CEILING, SCRIPT_HOST_WAIT, SCRIPT_PERMITS, SCRIPT_PERMIT_WAIT,
    SCRIPT_STACK_BYTES,
};

/// The step input carrying the script itself: "The embedded script you want to run during the
/// automation" (`aws:executeScript` inputs).
pub const SCRIPT_INPUT: &str = "Script";

/// The step input naming the runtime: "The language runtime environment in which to run the
/// provided script" (same inputs).
pub const RUNTIME_INPUT: &str = "Runtime";

/// The step input naming the handler: "The entry for script execution, usually a function
/// name" (same inputs).
pub const HANDLER_INPUT: &str = "Handler";

/// The step input carrying what the handler is called with: "A JSON or YAML object that will
/// be passed to the Handler" (same inputs).
pub const INPUT_PAYLOAD_INPUT: &str = "InputPayload";

/// The `Runtime` values this emulator runs, by prefix.
///
/// The published values are versioned (`python3.11`, `python3.10`, …) and ONE interpreter runs
/// every one of them, so every `python…` runtime resolves to it — with the release the step
/// declared installed on the run, so what the script reads of the version it is running on is
/// the one the step named. A runtime outside this set — the PowerShell Core ones — has no
/// engine here, and such a step reports a script that did not run.
pub const PYTHON_RUNTIME_PREFIX: &str = "python";

/// The member of the step's answer the handler's own return value is reported under, which is
/// what every `$.Payload…` selector of such a step reads.
pub const PAYLOAD_MEMBER: &str = "Payload";

/// The module this emulator runs the step's own script FROM, which its traceback frames name.
const SCRIPT_MODULE: &str = "ssm_automation_script";

/// How large a script one step may carry.
///
/// The script arrives inside the runbook's own content, so the content limit that admitted
/// the document is what bounds it — this is that same bound, stated where the run TAKES the
/// script, because resolving the step's references can expand what the content declared
/// (`super::super::runbook::MAX_RESOLVED_INPUT_BYTES` admits twice the content limit across
/// an execution's steps). A script longer than this is not run at all.
pub const MAX_SCRIPT_BYTES: usize = crate::state::MAX_DOCUMENT_CONTENT;

/// Run the step's script and answer what the step came to.
pub async fn run(claimed: &ClaimedAction, access: Option<AwsAccess>) -> ActionOutcome {
    let Some(runtime) = text_input(&claimed.inputs, RUNTIME_INPUT) else {
        return missing(RUNTIME_INPUT, claimed);
    };
    let Some(handler) = text_input(&claimed.inputs, HANDLER_INPUT) else {
        return missing(HANDLER_INPUT, claimed);
    };
    let Some(script) = text_input(&claimed.inputs, SCRIPT_INPUT) else {
        return missing(SCRIPT_INPUT, claimed);
    };
    if !runtime.starts_with(PYTHON_RUNTIME_PREFIX) {
        return ActionOutcome::Failed(format!(
            "The {} action of step {} declares the {runtime} runtime, which this emulator embeds \
             no engine for, so nothing ran for this step",
            claimed.action, claimed.step_name
        ));
    }
    if script.len() > MAX_SCRIPT_BYTES {
        return ActionOutcome::Failed(format!(
            "The script of step {} is larger than the {MAX_SCRIPT_BYTES} bytes one step may carry",
            claimed.step_name
        ));
    }
    // "A JSON or YAML object that will be passed to the Handler": the handler is called with
    // it, and a step that passes none calls its handler with an empty object — which is what
    // a handler reading `events.get(…)` of a step with no payload sees.
    let payload = claimed
        .inputs
        .get(INPUT_PAYLOAD_INPUT)
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let permit = match ::tokio::time::timeout(
        SCRIPT_PERMIT_WAIT,
        ::std::sync::Arc::clone(&SCRIPT_PERMITS).acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        // No interpreter of this emulator is free, so NOTHING ran: the step stays parked
        // and a later call runs it rather than this one recording an outcome nothing
        // produced.
        _ => return ActionOutcome::NotRun(not_run(claimed)),
    };
    let finished = {
        let modules = vec![PackageModule {
            path: format!("{SCRIPT_MODULE}.py"),
            source: script,
        }];
        let entry = format!("{SCRIPT_MODULE}.{handler}");
        let event = payload.to_string();
        let request_id = claimed.execution_id.clone();
        // "The language runtime environment in which to run the provided script": the step
        // declares WHICH release the script runs in, so the run is handed that identifier and
        // the interpreter it runs in reports that release — a script pinned to one release to
        // get its behaviour is answered by it and not by whatever this process was built from.
        let environment = environment_naming_runtime(&runtime);
        let (sender, receiver) = ::tokio::sync::oneshot::channel();
        let spawned = ::std::thread::Builder::new()
            .name("ssm-automation-script".to_string())
            .stack_size(SCRIPT_STACK_BYTES)
            .spawn(move || {
                // Held for as long as the script runs, whoever is still waiting for it.
                let _permit = permit;
                let finished = python::run_against(
                    &python::Run {
                        modules: &modules,
                        handler: &entry,
                        // An `aws:executeScript` step's script is given its values through
                        // its `InputPayload`, so the only thing the run's environment carries
                        // is which runtime the step declared.
                        environment: &environment,
                        // The instruction set the environment the script runs in has, which
                        // the run reports as its own machine. A step names none, so it is
                        // the one the engine's own runtime reports when none is named.
                        architecture:
                            ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                        event: Some(&event),
                        request_id: &request_id,
                        invoked_function_arn: "",
                        client_context: None,
                        // The tenant an AWS Lambda INVOCATION carries, which that runtime
                        // reports to its handler as `context.tenant_id`. The engine uses it
                        // for exactly that and for nothing else — it is one member of the
                        // document the runtime is handed (`python::runtime_input` /
                        // `python::invocation_input`), and NO state of the engine is keyed
                        // by it: the modules, the in-memory operating-system surface and
                        // the bounds a run gets are built per run from this `Run` alone, so
                        // two runs share nothing through it. An Automation execution has no
                        // tenant to report either: neither `StartAutomationExecution` nor
                        // the modelled `AutomationExecution` carries one. What scopes THIS
                        // run is passed beside it: the role its own AWS calls are signed as
                        // (the execution's `AutomationAssumeRole`) and the Region of the
                        // endpoint the start arrived at, both of which reach the guest
                        // through `aws`.
                        tenant_id: None,
                        budget: script_budget(),
                        aws: access.as_ref(),
                        // A script step is no Lambda durable execution's run, so it takes no
                        // step of one and checkpoints nothing onto one.
                        durable: None,
                    },
                    // The clock the run's own span is read against: one the RUN drives, a
                    // tick per traced event it delivers and the waits it asks for, so what
                    // the step comes to rest in is what the script came to rather than what
                    // this host's load made of it — stating what the run may take of THIS
                    // host besides, which is the bound the engine stops it on whatever its
                    // own clock reads.
                    &python::Clock::driven(SCRIPT_CLOCK_TICK).within(SCRIPT_HOST_CEILING),
                );
                let _ = sender.send(finished);
            });
        if spawned.is_err() {
            return ActionOutcome::NotRun(not_run(claimed));
        }
        match ::tokio::time::timeout(SCRIPT_HOST_WAIT, receiver).await {
            Ok(Ok(finished)) => finished,
            // The thread has still not answered a grace period past the host time one run
            // may take — which the engine has by then stopped the run on — so this emulator
            // gave up on it, and the step reports exactly that.
            _ => return ActionOutcome::Failed(gave_up(claimed)),
        }
    };
    read_outcome(claimed, &finished.completion)
}

/// What the step reports for a run the interpreter answered for.
fn read_outcome(claimed: &ClaimedAction, completion: &python::Completion) -> ActionOutcome {
    match completion {
        python::Completion::Returned(returned) => answered(claimed, returned),
        // A run bounded by this emulator answers ONE value, so nothing of such a step
        // streams; a run that answered in writes answered nothing this step can select.
        python::Completion::Streamed { .. } => ActionOutcome::Failed(format!(
            "The script of step {} answered no single value",
            claimed.step_name
        )),
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => ActionOutcome::Failed(format!(
            "The script of step {} raised {error_type}: {error_message}",
            claimed.step_name
        )),
        python::Completion::Stopped(stop) => ActionOutcome::Failed(format!(
            "The script of step {} was stopped: {}",
            claimed.step_name,
            stop.reason()
        )),
        python::Completion::Internal(detail) => ActionOutcome::Failed(format!(
            "The script of step {} could not be run: {detail}",
            claimed.step_name
        )),
    }
}

/// The step's outputs, selected out of the value the handler RETURNED.
///
/// The return value is reported under [`PAYLOAD_MEMBER`], which is the document such a step's
/// selectors read: `$.Payload.value` is the `value` member of what the handler answered.
fn answered(claimed: &ClaimedAction, returned: &str) -> ActionOutcome {
    let payload: Value = ::serde_json::from_str(returned).unwrap_or(Value::Null);
    let mut answer = Map::new();
    answer.insert(PAYLOAD_MEMBER.to_string(), payload);
    let answer = Value::Object(answer);
    if claimed.outputs.is_empty() {
        return ActionOutcome::Succeeded(::std::collections::BTreeMap::new());
    }
    match selectors::selected(&answer, &claimed.outputs) {
        Ok(selected) => ActionOutcome::Succeeded(selected),
        Err(why) => ActionOutcome::Failed(why),
    }
}

/// The step's failure for a script this emulator did not run at all.
fn not_run(claimed: &ClaimedAction) -> String {
    format!(
        "no interpreter of this emulator was free for the script of step {}",
        claimed.step_name
    )
}

/// The step's failure for a run this emulator GAVE UP on: nothing of the script's own span
/// ran out, so it is not reported as a script that was stopped or that failed.
fn gave_up(claimed: &ClaimedAction) -> String {
    format!(
        "The script of step {} was given up on: it had not answered within the {} seconds this \
         emulator waits for one run",
        claimed.step_name,
        SCRIPT_HOST_WAIT.as_secs()
    )
}

/// The step's failure for a step that declared none of the three inputs a script run takes.
fn missing(member: &str, claimed: &ClaimedAction) -> ActionOutcome {
    ActionOutcome::Failed(format!(
        "The {} action of step {} declares no {member} to run",
        claimed.action, claimed.step_name
    ))
}
