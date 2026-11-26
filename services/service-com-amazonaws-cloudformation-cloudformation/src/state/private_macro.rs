// SPDX-License-Identifier: CC0-1.0

//! The PRIVATE MACRO a template invokes: the Lambda function CloudFormation hands the
//! template fragment to, and the change set whose creation is held open while that function
//! runs.
//!
//! ## The rule this module implements
//!
//! "The optional `Transform` section … specifies one or more macros that CloudFormation uses
//! to process your template", and CloudFormation processes them BEFORE it plans anything from
//! the template. AWS's own transforms are applied by this emulator itself ([`super::sam`],
//! [`super::include`], the language extensions in [`super::template`]); any OTHER name is a
//! macro a client REGISTERED — an `AWS::CloudFormation::Macro` resource, which names the
//! function that performs it — and performing it means INVOKING that function with the
//! fragment to process and reading the fragment it answers.
//!
//! That invocation is the work creating a change set does, so it decides the statuses a client
//! polls: `CreateChangeSet` answers while CloudFormation "starts creating the change set", the
//! change set reports `CREATE_IN_PROGRESS` for as long as the macro is running, and it reaches
//! a terminal status only once the macro has answered — `CREATE_COMPLETE` over the document
//! the macro produced, or `FAILED` carrying what the macro failed with. While it is in
//! progress there is no plan to execute, which is exactly the window
//! `InvalidChangeSetStatusException` describes ("the change set status might be
//! CREATE_IN_PROGRESS").
//!
//! ## What is handed over, and what comes back
//!
//! The function is invoked with the macro request AWS documents: the Region and account it
//! runs in, the `transformId` (`<account>::<name>`), a `requestId` of this invocation's own,
//! the `params` the `Transform` declaration carried, the parameter values in force
//! (`templateParameterValues`) and the `fragment` to process — for a template-level macro
//! "the entire template, except for the `Transform` section". The answer carries that same
//! `requestId`, a `status` of `success` and the processed `fragment`; anything else is the
//! macro failing, and the change set reports that failure rather than a plan nobody computed.
//!
//! Each macro the `Transform` section names is invoked IN TURN, the fragment one answers being
//! the fragment the next is handed, which is the order a template declares them in.
//!
//! The invocation runs in a task of its own ([`start`]), for the reason
//! [`super::provider_driver`] records about the work a landed provider answer owes: a macro
//! takes as long as its function takes, and no client call may be held open for it. Nothing
//! that task writes is invented — it is the macro's own answer, or the macro's own failure.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use super::provider_driver::ProvisioningReach;
use super::{
    change_set_status, execution_status, template, ComAmazonawsCloudformationCloudformationState,
};

/// How many macros ONE creation may perform.
///
/// Chosen, as every other bound this store holds is: a `Transform` section names one or two
/// macros in every documented use of it, and each one this emulator performs is a whole
/// function invocation — so the count a caller's template can set off is capped here rather
/// than bounded only by how many entries a template body has room for. A template naming more
/// than this is answered with a failed change set rather than invoking any of them
/// ([`PendingMacros::over_the_bound`]).
pub const MAX_MACROS_PER_TEMPLATE: usize = 8;

/// How long the WHOLE chain of a creation's macros may take, however many it names — and the
/// instant the creation it is computing is settled at when they have not answered.
///
/// Chosen, as every other span this store holds is, and chosen to CONVERGE: a client polls a
/// change set's `Status` for a terminal value, so the creation must reach one inside the
/// window a poll loop gives it. It is comfortably longer than one invocation of a function can
/// be waited for (the Lambda service answers a caller inside its own host ceiling) and
/// comfortably shorter than the span a client's poll loop allows, so a chain of macros that
/// answer is carried through and one that does not still settles the change set promptly.
///
/// It bounds BOTH halves — the work ([`start`] cuts the chain off here) and the status
/// ([`super::StoredChangeSet::settle`] fails the creation here) — so the record a client reads
/// and the work behind it are settled at the same instant.
pub const MACRO_ANSWER_DEADLINE: Duration = Duration::from_secs(45);

/// How many macro expansions this service performs AT ONCE.
///
/// One `CreateChangeSet` whose template invokes a macro starts one expansion, so without a
/// permit the number of invocations in flight would be the number of such calls a client
/// makes. Each expansion waits here for its turn, and the creation it is computing is settled
/// by its own deadline whether or not its turn ever comes ([`PendingMacros::unanswered_reason`]),
/// so a client cannot queue work past this many concurrent chains.
static EXPANSIONS_IN_FLIGHT: Semaphore = Semaphore::const_new(4);

/// The resource type a client REGISTERS a macro with: an `AWS::CloudFormation::Macro` resource
/// is "a CloudFormation macro definition that you can use in subsequent templates".
pub const MACRO_TYPE: &str = "AWS::CloudFormation::Macro";

/// The property that names the macro a template's `Transform` section invokes it by.
const MACRO_NAME: &str = "Name";

/// The property that names the function performing the macro: "the Amazon Resource Name (ARN)
/// of the underlying Lambda function that you want CloudFormation to invoke".
const MACRO_FUNCTION: &str = "FunctionName";

/// The `status` a macro's answer carries when it processed the fragment.
const MACRO_SUCCEEDED: &str = "success";

/// The members of a macro's answer: the processed fragment, the status and why it failed.
const ANSWER_FRAGMENT: &str = "fragment";
const ANSWER_STATUS: &str = "status";
const ANSWER_ERROR: &str = "errorMessage";

/// The members of the macro REQUEST, as AWS documents them.
const REQUEST_REGION: &str = "region";
const REQUEST_ACCOUNT_ID: &str = "accountId";
const REQUEST_FRAGMENT: &str = "fragment";
const REQUEST_TRANSFORM_ID: &str = "transformId";
const REQUEST_PARAMS: &str = "params";
const REQUEST_ID: &str = "requestId";
const REQUEST_TEMPLATE_PARAMETERS: &str = "templateParameterValues";

/// The signing name of the service whose functions perform a macro.
const LAMBDA_SIGNING_NAME: &str = "lambda";

/// The `Transform` section's own members for a macro invoked WITH parameters:
/// `{"Name": "MyMacro", "Parameters": {...}}`.
const TRANSFORM_NAME: &str = "Name";
const TRANSFORM_PARAMETERS: &str = "Parameters";

/// The section of a template that NAMES the macros, which is not part of the fragment a
/// template-level macro is handed.
const TRANSFORM_SECTION: &str = "Transform";

/// One macro a template invokes: the name its `Transform` section states, the function that
/// performs it as the registration names it, and the parameters that declaration carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredMacro {
    pub name: String,
    pub function_name: String,
    pub parameters: Value,
}

/// What a change set's creation still owes: the macros to invoke, in the order the template
/// declares them, and the parameter values in force, which every invocation carries.
///
/// `started` is set when the invocation has been handed to its own task, so a second pass over
/// the store cannot invoke a macro twice.
#[derive(Debug, Clone)]
pub struct PendingMacros {
    pub macros: Vec<DeclaredMacro>,
    pub parameters: BTreeMap<String, String>,
    pub started: bool,
}

/// Why the processed stage of a template whose macros have not answered yet is unavailable:
/// "After CloudFormation finishes creating the change set, the Processed template becomes
/// available", so until then there is no processed document to serve.
pub const NOT_PROCESSED_YET: &str =
    "the template's macros have not been applied yet, so the processed template is not \
     available";

impl PendingMacros {
    /// The macros a template BODY invokes that a client REGISTERED, with the parameter values
    /// in force — or `None` where the template invokes none, or invokes one this store holds
    /// no registration for (which is a template this service cannot process, answered as it
    /// already is).
    ///
    /// The transforms AWS publishes itself are not here: this emulator performs those
    /// ([`template::process`]), so a template declaring only those has nothing invoked for it.
    pub fn declared(
        store: &ComAmazonawsCloudformationCloudformationState,
        region: &str,
        body: &str,
        parameters: BTreeMap<String, String>,
    ) -> Option<Self> {
        let registered = registered_macros(store, region);
        let document = template::document(body);
        let mut macros = Vec::new();
        for name in template::body_declared_transforms(body) {
            if template::applied_by_this_service(&name) {
                continue;
            }
            let function_name = registered.get(&name)?.clone();
            macros.push(DeclaredMacro {
                parameters: declared_parameters(document.as_ref(), &name),
                name,
                function_name,
            });
        }
        (!macros.is_empty()).then_some(Self {
            macros,
            parameters,
            started: false,
        })
    }

    /// The `StatusReason` a change set whose macro never answered settles with: the transform
    /// failure CloudFormation words, carrying what this emulator can say about it — that the
    /// function the macro names answered nothing inside the window one creation's macros are
    /// given ([`MACRO_ANSWER_DEADLINE`]).
    pub fn unanswered_reason(&self) -> String {
        let held = MACRO_ANSWER_DEADLINE.as_secs();
        transform_failure(
            self.macros
                .first()
                .map(|invoked| invoked.name.as_str())
                .unwrap_or_default(),
            &format!("the macro's function did not answer within {held} seconds"),
        )
    }

    /// The `StatusReason` a change set whose template names more macros than one creation may
    /// perform settles with, or `None` for one inside that bound
    /// ([`MAX_MACROS_PER_TEMPLATE`]).
    ///
    /// Such a creation performs NONE of them: the count is checked before anything is invoked,
    /// so a template cannot set off more invocations than the bound by being refused halfway
    /// through.
    pub fn over_the_bound(&self) -> Option<String> {
        (self.macros.len() > MAX_MACROS_PER_TEMPLATE).then(|| {
            transform_failure(
                self.macros
                    .first()
                    .map(|invoked| invoked.name.as_str())
                    .unwrap_or_default(),
                &format!(
                    "the template invokes {} macros, and no more than {MAX_MACROS_PER_TEMPLATE} \
                     are performed for one change set",
                    self.macros.len()
                ),
            )
        })
    }
}

/// The macros the stacks of `region` have REGISTERED, as name → function: every
/// `AWS::CloudFormation::Macro` resource those stacks hold, keyed by the name a `Transform`
/// section invokes it by.
///
/// A macro IS a resource of a stack, so what is registered is read off the stacks rather than
/// kept in a second place: `CreateChangeSet` and the invocation cannot then disagree about
/// which function performs a macro.
fn registered_macros(
    store: &ComAmazonawsCloudformationCloudformationState,
    region: &str,
) -> BTreeMap<String, String> {
    let mut registered = BTreeMap::new();
    for stack in store.stacks.iter().filter(|stack| stack.region == region) {
        for deployed in &stack.resources {
            if deployed.resource.resource_type != MACRO_TYPE {
                continue;
            }
            let properties = &deployed.resource.properties;
            let name = text_of(properties.get(MACRO_NAME));
            let function_name = text_of(properties.get(MACRO_FUNCTION));
            if name.is_empty() || function_name.is_empty() {
                continue;
            }
            registered.insert(name, function_name);
        }
    }
    registered
}

/// The `Parameters` the `Transform` declaration of `name` carried, which travel to the macro
/// as its `params`. An empty object where the declaration carries none: `params` is a member
/// of every request.
fn declared_parameters(document: Option<&Value>, name: &str) -> Value {
    let section = document.and_then(|document| document.get(TRANSFORM_SECTION));
    let stated = |node: &Value| -> Option<Value> {
        if node.get(TRANSFORM_NAME).and_then(Value::as_str) != Some(name) {
            return None;
        }
        node.get(TRANSFORM_PARAMETERS).cloned()
    };
    let parameters = match section {
        Some(Value::Object(_)) => section.and_then(stated),
        Some(Value::Array(items)) => items.iter().find_map(stated),
        _ => None,
    };
    parameters.unwrap_or_else(|| Value::Object(Map::new()))
}

/// One declared value as the text it carries, which is what a property naming a macro or a
/// function states.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// How CloudFormation words a transform that failed: the transform's own id and what it failed
/// with — the one wording this service states such a failure in ([`super::include`] words the
/// same sentence for the transform it performs itself).
fn transform_failure(name: &str, detail: &str) -> String {
    format!("Transform {} failed with: {detail}", transform_id(name))
}

/// The `transformId` a private macro is invoked under: "the id of the macro, in the format
/// `<account-id>::<macro-name>`".
fn transform_id(name: &str) -> String {
    format!("{}::{name}", ::emulator_server::emulator_account_id())
}

/// Perform the macros a change set's creation owes, in a task of its own, and settle the
/// change set on what they answer.
///
/// Started by `CreateChangeSet` for a change set whose template invokes a registered macro.
/// The task holds only cloneable handles to this emulator — the callback every provisioning
/// call rides and this service's identity — so it borrows nothing of the request that started
/// it, exactly as [`super::provider_driver::drive_landed_answer`]'s does, and the Region it
/// runs for is passed to it rather than read off a request.
///
/// What one such task may consume is bounded on every side: it waits its turn among
/// [`EXPANSIONS_IN_FLIGHT`], the macros it performs are at most
/// [`MAX_MACROS_PER_TEMPLATE`], the whole chain is cut off at [`MACRO_ANSWER_DEADLINE`], and it
/// stops the moment the change set it is computing no longer owes the expansion — which the
/// creation's own deadline settles whether this task answers or not.
pub fn start<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Arc<Eng>,
    region: &str,
    change_set_id: &str,
) {
    let reach = ProvisioningReach::of(ctx);
    let engine = Arc::clone(engine);
    let region = region.to_string();
    let change_set_id = change_set_id.to_string();
    // DETACHED on purpose: the macro runs for as long as its function runs, which outlasts the
    // call that submitted the template — exactly as the deployment work a provider's answer
    // owes outlasts the answer. Nothing of it is unbounded, for the reasons above.
    ::tokio::spawn(async move {
        // The change set reports `CREATE_PENDING` until the clock makes the next status due,
        // so the macros are handed over once the creation is reported IN PROGRESS: the status
        // a client polls is the one the work is really in.
        ::tokio::time::sleep(super::STATUS_TRANSITION_DELAY).await;
        // ...and then its turn among the expansions this service performs at once. A task
        // whose turn comes after the creation's own deadline has settled it finds the change
        // set owing nothing and invokes no macro at all ([`still_owed`]).
        let Ok(_in_flight) = EXPANSIONS_IN_FLIGHT.acquire().await else {
            return;
        };
        let Some(state) = super::service_state(&*engine) else {
            return;
        };
        let taken = {
            let mut held = state.lock();
            held.settle(SystemTime::now());
            let Some(change_set) = held
                .change_sets
                .iter_mut()
                .find(|change_set| change_set.change_set_id == change_set_id)
            else {
                return;
            };
            let body = change_set.template_body.clone();
            match change_set.pending_macros.as_mut() {
                Some(pending) if !pending.started => {
                    pending.started = true;
                    (pending.clone(), body)
                }
                // Something has already taken this expansion on, or the change set no longer
                // owes one.
                _ => return,
            }
        };
        let (pending, body) = taken;
        let parts = ::http::Request::new(()).into_parts().0;
        let ctx = ::smithy_server_runtime::OperationContext::new(
            &parts,
            reach.identity(),
            reach.callback().clone(),
        );
        // The CHAIN, however many macros it is, is cut off at the span one creation's macros
        // are given: a function that answers nothing cannot hold the chain past the instant
        // the creation it is computing is settled at.
        let chain = processed_fragment(&ctx, &region, &pending, &body, &state, &change_set_id);
        let answered = match ::tokio::time::timeout(MACRO_ANSWER_DEADLINE, chain).await {
            Ok(answered) => answered,
            Err(_elapsed) => Err(pending.unanswered_reason()),
        };
        let mut held = state.lock();
        // What the CLOCK had already made due is materialised first, so an answer that arrives
        // after the creation's own deadline finds the terminal record the deadline wrote and
        // writes nothing over it ([`conclude`]).
        held.settle(SystemTime::now());
        conclude(&mut held, &change_set_id, answered, SystemTime::now());
    });
}

/// Whether the change set still OWES this expansion: a creation its own deadline has already
/// settled owes nothing, and a chain that kept running would be invoking macros for a record
/// nothing will read.
///
/// What the clock had already made due is materialised first, exactly as every reporting
/// operation materialises it, so the answer is the one a client reading the change set now
/// would get.
fn still_owed(
    state: &::engine::ServiceState<ComAmazonawsCloudformationCloudformationState>,
    change_set_id: &str,
) -> bool {
    let mut held = state.lock();
    held.settle(SystemTime::now());
    held.change_sets.iter().any(|change_set| {
        change_set.change_set_id == change_set_id && change_set.pending_macros.is_some()
    })
}

/// Invoke each macro in turn and answer the document the last of them produced, or `Err` with
/// the transform failure the first one to fail answers.
///
/// The fragment the first macro is handed is the submitted template WITHOUT its `Transform`
/// section, which is what a template-level macro processes; the fragment each macro answers is
/// what the next one is handed.
///
/// `still_owed` is asked before each invocation: a chain whose answer nothing will read any
/// more stops rather than invoking further macros for it.
async fn invoked_chain<Owed: Fn() -> bool>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    pending: &PendingMacros,
    body: &str,
    still_owed: Owed,
) -> Result<String, String> {
    let Some(Value::Object(mut document)) = template::document(body) else {
        return Err(transform_failure(
            pending
                .macros
                .first()
                .map(|invoked| invoked.name.as_str())
                .unwrap_or_default(),
            "the template is not a document this service can read",
        ));
    };
    // "For a template-level macro, CloudFormation passes the entire template, except for the
    // Transform section": the section names the macros, and is not part of what they process.
    document.remove(TRANSFORM_SECTION);
    let mut fragment = Value::Object(document);
    for invoked in &pending.macros {
        if !still_owed() {
            return Err(pending.unanswered_reason());
        }
        fragment = invoke(ctx, region, invoked, &fragment, &pending.parameters).await?;
    }
    serde_json::to_string(&fragment).map_err(|error| {
        transform_failure(
            pending
                .macros
                .last()
                .map(|invoked| invoked.name.as_str())
                .unwrap_or_default(),
            &error.to_string(),
        )
    })
}

/// Perform the REGISTERED macros a template a stack OPERATION submitted invokes, over the
/// document that operation would otherwise deploy, and answer the stage that leaves.
///
/// This is the other half of the rule this module implements: a macro is performed before the
/// template is PLANNED from (which is [`start`], for a change set) and equally before it is
/// DEPLOYED from — "CloudFormation processes the template fragments" a transform names before
/// it does anything with the template's resources. So an operation that deploys a template
/// directly performs the macros here, in the stage that processes the template, and what
/// deploys below is the document the macros produced: the resource a macro INJECTED is as much
/// the stack's as one the body declares itself, and a macro that fails is the stack's own
/// template-processing failure rather than a resource's.
///
/// `included` is what the stage has reached so far — the `AWS::Include` snippets the template
/// names, spliced in ([`super::include::process`]) — and is what the macros are handed: the
/// stage is one pipeline, so a macro processes the document the splices made. A body whose
/// includes could not be read is answered as it stands: that failure is the stage's, and
/// nothing is invoked for a template nothing will deploy.
///
/// The invocation runs INLINE, on the operation's own call, because the operation's deployment
/// does too: the document the resources are created from has to exist before any owning
/// service is asked for one. It is bounded exactly as the change-set expansion is — at most
/// [`MAX_MACROS_PER_TEMPLATE`] macros, the whole chain cut off at [`MACRO_ANSWER_DEADLINE`] —
/// and a template that names no registered macro invokes nothing at all.
pub async fn expand_declared<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    region: &str,
    submitted: &str,
    parameters: &BTreeMap<String, String>,
    included: super::include::Processed,
) -> super::include::Processed {
    use super::include::Processed;

    let body = match &included {
        Processed::Failed(_) => return included,
        Processed::Expanded(spliced) => spliced.clone(),
        Processed::Unchanged => submitted.to_string(),
    };
    let Some(state) = super::service_state(engine) else {
        return included;
    };
    let pending = {
        let mut held = state.lock();
        held.settle(SystemTime::now());
        PendingMacros::declared(&held, region, &body, parameters.clone())
    };
    // A template invoking no macro a client registered is left exactly as the stage above
    // produced it: nothing here guesses at a transform this emulator does not perform.
    let Some(pending) = pending else {
        return included;
    };
    if let Some(over_the_bound) = pending.over_the_bound() {
        return Processed::Failed(over_the_bound);
    }
    // Every macro of the chain is asked for, in turn, and the WHOLE chain — the turn it waits
    // for among the expansions this service performs at once included — is cut off at the span
    // one operation's macros are given, so the call that submitted the template cannot be held
    // past it however many other templates are being expanded.
    let chain = async {
        let Ok(_in_flight) = EXPANSIONS_IN_FLIGHT.acquire().await else {
            return Err(pending.unanswered_reason());
        };
        invoked_chain(ctx, region, &pending, &body, || true).await
    };
    match ::tokio::time::timeout(MACRO_ANSWER_DEADLINE, chain).await {
        Ok(Ok(produced)) => Processed::Expanded(produced),
        Ok(Err(reason)) => Processed::Failed(reason),
        Err(_elapsed) => Processed::Failed(pending.unanswered_reason()),
    }
}

/// Invoke each macro a CHANGE SET's creation owes in turn and answer the document the last of
/// them produced ([`invoked_chain`]).
///
/// The chain STOPS as soon as the change set no longer owes the expansion ([`still_owed`]) — so
/// a creation its own deadline has settled is not followed by invocations nothing will read,
/// and no further macro runs for it.
async fn processed_fragment(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    pending: &PendingMacros,
    body: &str,
    state: &::engine::ServiceState<ComAmazonawsCloudformationCloudformationState>,
    change_set_id: &str,
) -> Result<String, String> {
    invoked_chain(ctx, region, pending, body, || {
        still_owed(state, change_set_id)
    })
    .await
}

/// Invoke ONE macro's function with the request AWS documents, and answer the fragment it
/// processed.
///
/// `Err` is the transform failure: the function refusing the invocation, a handler that raised
/// and a macro answering a status other than `success` are each the macro failing, and each is
/// reported in CloudFormation's own wording for a failed transform.
async fn invoke(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    invoked: &DeclaredMacro,
    fragment: &Value,
    parameters: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let request_id = super::identifiers::new_identifier(&invoked.name);
    let mut request = Map::new();
    request.insert(REQUEST_REGION.to_string(), Value::String(region.to_string()));
    request.insert(
        REQUEST_ACCOUNT_ID.to_string(),
        Value::String(::emulator_server::emulator_account_id().to_string()),
    );
    request.insert(REQUEST_FRAGMENT.to_string(), fragment.clone());
    request.insert(
        REQUEST_TRANSFORM_ID.to_string(),
        Value::String(transform_id(&invoked.name)),
    );
    request.insert(REQUEST_PARAMS.to_string(), invoked.parameters.clone());
    request.insert(REQUEST_ID.to_string(), Value::String(request_id));
    request.insert(
        REQUEST_TEMPLATE_PARAMETERS.to_string(),
        Value::Object(
            parameters
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        ),
    );
    let uri = format!(
        "/2015-03-31/functions/{}/invocations",
        super::provisioning::percent_encoded(&invoked.function_name)
    );
    let answered = super::provisioning::call_rest_json(
        ctx,
        LAMBDA_SIGNING_NAME,
        region,
        "POST",
        &uri,
        Some(&Value::Object(request)),
    )
    .await
    .map_err(|refusal| transform_failure(&invoked.name, &refusal.message))?;
    // A handler that RAISED answers the invocation with its own error document rather than a
    // macro answer, which is the macro failing with what the function reported.
    if answered.get(ANSWER_FRAGMENT).is_none() {
        let detail = answered
            .get(ANSWER_ERROR)
            .and_then(Value::as_str)
            .unwrap_or("the macro's function answered no fragment");
        return Err(transform_failure(&invoked.name, detail));
    }
    let status = answered
        .get(ANSWER_STATUS)
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !status.eq_ignore_ascii_case(MACRO_SUCCEEDED) {
        let detail = answered
            .get(ANSWER_ERROR)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("the macro answered the status {status}"));
        return Err(transform_failure(&invoked.name, &detail));
    }
    Ok(answered
        .get(ANSWER_FRAGMENT)
        .cloned()
        .unwrap_or(Value::Null))
}

/// Settle the change set on what its macros answered: the document they produced becomes the
/// PROCESSED stage the change set would deploy and the plan is computed from it, or the change
/// set FAILS carrying the transform's own failure.
///
/// The plan is the same comparison `CreateChangeSet` makes — the submitted template against
/// the stack as it stands ([`super::changes_against`]) — made here because this is when the
/// document to plan from first exists. A plan that would change nothing fails the change set
/// with the reason CloudFormation gives one, exactly as a change set created without a macro
/// does.
///
/// A change set that no longer owes the expansion is left exactly as it is: its creation has
/// already been settled — by its own deadline, or by a delete — so an answer arriving after
/// that writes nothing over the terminal record a client has already read.
pub fn conclude(
    store: &mut ComAmazonawsCloudformationCloudformationState,
    change_set_id: &str,
    answered: Result<String, String>,
    now: SystemTime,
) {
    let Some(index) = store.change_sets.iter().position(|change_set| {
        change_set.change_set_id == change_set_id && change_set.pending_macros.is_some()
    }) else {
        return;
    };
    let Some((parameters, stack_id, macro_parameters)) =
        store.change_sets.get(index).map(|change_set| {
            (
                change_set.parameters.clone(),
                change_set.stack_id.clone(),
                change_set
                    .pending_macros
                    .as_ref()
                    .map(|pending| pending.parameters.clone())
                    .unwrap_or_default(),
            )
        })
    else {
        return;
    };
    // The document the macros produced, held to every rule a submitted template is held to:
    // the transforms the RESULT declares are applied in turn ([`template::process`]), so a
    // macro that emits AWS's own transform has that one applied too, and one that emits a
    // document this service cannot process fails the change set rather than being planned
    // from.
    let processed = match &answered {
        Ok(produced) => template::process_with_parameters(produced, &macro_parameters),
        Err(_) => template::Processed::Unavailable(String::new()),
    };
    let failure = match (&answered, &processed) {
        (Err(reason), _) => Some(reason.clone()),
        (Ok(_), template::Processed::Unavailable(reason)) => Some(format!(
            "The template's transforms could not be applied: {reason}"
        )),
        _ => None,
    };
    let produced = answered.unwrap_or_default();
    let changes = match failure
        .is_none()
        .then(|| processed.body(&produced).and_then(super::declared_template))
        .flatten()
    {
        Some(submitted) => {
            let current = store
                .stacks
                .iter()
                .find(|stack| stack.stack_id == stack_id)
                .map(|stack| super::DeployedStack {
                    region: &stack.region,
                    stack_name: &stack.stack_name,
                    stack_id: &stack.stack_id,
                    body: stack.template_body.as_deref(),
                    processed: &stack.processed_template,
                    parameters: &stack.parameters,
                    deployed: &stack.resources,
                    status: &stack.status,
                    imports: &stack.imports,
                    notification_arns: &stack.notification_arns,
                });
            super::changes_against(current, &submitted, &parameters)
        }
        None => Vec::new(),
    };
    let settled = match &failure {
        Some(reason) => (
            change_set_status::FAILED,
            execution_status::UNAVAILABLE,
            Some(reason.clone()),
        ),
        None if changes.is_empty() => (
            change_set_status::FAILED,
            execution_status::UNAVAILABLE,
            Some(super::NO_CHANGES_REASON.to_string()),
        ),
        None => (
            change_set_status::CREATE_COMPLETE,
            execution_status::AVAILABLE,
            None,
        ),
    };
    let Some(change_set) = store.change_sets.get_mut(index) else {
        return;
    };
    // A failed expansion leaves no processed stage to serve: the body the macro produced is
    // not a document this service would deploy, and the submitted body is what `GetTemplate`
    // answers for the `Original` stage either way.
    change_set.processed_template = match &failure {
        Some(reason) => template::Processed::Unavailable(reason.clone()),
        None => match processed {
            template::Processed::Same => template::Processed::Applied(produced),
            applied => applied,
        },
    };
    change_set.changes = changes;
    change_set.status = settled.0.to_string();
    change_set.execution_status = settled.1.to_string();
    change_set.status_reason = settled.2;
    // The macros have answered, so nothing of this expansion is owed any more — including the
    // deadline the creation was held to.
    change_set.pending_macros = None;
    change_set.pending = None;
    let status = change_set.status.clone();
    // A change set's creation is an operation of its stack, which concludes in the state the
    // creation reached ([`super::StoredStack::conclude_change_set_operation`]) — the same
    // conclusion the clock-driven settlement performs for a change set it settles itself.
    if let Some(stack) = store
        .stacks
        .iter_mut()
        .find(|stack| stack.stack_id == stack_id)
    {
        stack.conclude_change_set_operation(change_set_id, &status, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// The `params` a `Transform` declaration carries, over the three forms the section takes:
    /// one macro named as a string (no parameters), one named as an object with parameters, and
    /// a list of macros where only one of them carries any.
    #[test]
    fn the_declared_parameters_are_the_ones_the_transform_section_states() {
        let rows: &[(&str, Value, &str, Value)] = &[
            (
                "a macro named as a string",
                json!({ "Transform": "MyMacro" }),
                "MyMacro",
                json!({}),
            ),
            (
                "a macro named with parameters",
                json!({ "Transform": { "Name": "MyMacro", "Parameters": { "Scope": "all" } } }),
                "MyMacro",
                json!({ "Scope": "all" }),
            ),
            (
                "one of a list of macros",
                json!({ "Transform": [
                    "AWS::LanguageExtensions",
                    { "Name": "Second", "Parameters": { "Depth": "2" } }
                ] }),
                "Second",
                json!({ "Depth": "2" }),
            ),
            (
                "a macro the section does not name",
                json!({ "Transform": { "Name": "Other", "Parameters": { "Scope": "all" } } }),
                "MyMacro",
                json!({}),
            ),
        ];
        for (case, document, name, expected) in rows {
            let stated = declared_parameters(Some(document), name);
            assert_eq!(
                stated, *expected,
                "case {case:?}: expected the declared params {expected:?}, got {stated:?}"
            );
        }
    }

    /// A transform failure is worded the way CloudFormation words one: the macro's own
    /// `transformId` — the account and the macro's name — and then what it failed with.
    #[test]
    fn a_transform_failure_names_the_transform_and_what_it_failed_with() {
        let account_id = ::emulator_server::emulator_account_id();
        let rows: &[(&str, &str, &str)] = &[
            ("a macro that raised", "MyMacro", "KeyError: 'fragment'"),
            ("a macro that answered a status", "Other", "the macro answered the status failure"),
        ];
        for (case, name, detail) in rows {
            let worded = transform_failure(name, detail);
            assert_eq!(
                worded,
                format!("Transform {account_id}::{name} failed with: {detail}"),
                "case {case:?}: expected CloudFormation's wording for a failed transform, got \
                 {worded:?}"
            );
        }
    }

    /// How many macros one creation performs is BOUNDED: a template inside the bound owes its
    /// expansion, and one past it is refused on the count — at the bound and one past it, so
    /// the edge the bound names is the one checked.
    #[test]
    fn a_creation_performs_no_more_macros_than_the_bound() {
        let invoked = |count: usize| PendingMacros {
            macros: (0..count)
                .map(|index| DeclaredMacro {
                    name: format!("Macro{index}"),
                    function_name: format!("macro-{index}"),
                    parameters: json!({}),
                })
                .collect(),
            parameters: BTreeMap::new(),
            started: false,
        };
        let rows: &[(&str, usize, bool)] = &[
            ("one macro", 1, false),
            ("as many as the bound allows", MAX_MACROS_PER_TEMPLATE, false),
            ("one past the bound", MAX_MACROS_PER_TEMPLATE + 1, true),
            ("far past the bound", MAX_MACROS_PER_TEMPLATE * 4, true),
        ];
        for (case, count, refused) in rows {
            let over = invoked(*count).over_the_bound();
            assert_eq!(
                over.is_some(),
                *refused,
                "case {case:?}: expected over_the_bound to be {refused}, got {over:?}"
            );
            if let Some(reason) = over {
                assert!(
                    reason.contains("failed with:")
                        && reason.contains(&MAX_MACROS_PER_TEMPLATE.to_string())
                        && reason.contains(&count.to_string()),
                    "case {case:?}: expected the transform failure to state the count and the \
                     bound, got {reason:?}"
                );
            }
        }
    }
}
