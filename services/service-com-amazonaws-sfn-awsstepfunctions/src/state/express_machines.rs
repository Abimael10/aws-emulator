// SPDX-License-Identifier: CC0-1.0

//! Which state machines are of the EXPRESS workflow type, as a walk asks it.
//!
//! A `Task` state's nested-execution integration has to know how to RUN the child it
//! names, because the two workflow types are reported differently: a STANDARD child's
//! run is reported by `DescribeExecution`, which is what the run-a-job (`.sync`)
//! pattern polls, while an EXPRESS child's execution ARN carries the resource type
//! that read refuses ([`super::EXPRESS_EXECUTION_RESOURCE`]) — "This API action is not
//! supported by EXPRESS state machines" — so there is no job of it to poll at all.
//! What the pattern promises either way is that the task ends when the CHILD does, so
//! an Express child is run to completion by the call that starts it
//! ([`super::integration::run_child_synchronously`]).
//!
//! The state machines live in this service's own store, which the walk cannot read —
//! it runs on a blocking thread with no store locked — so the question is handed to it
//! as a closure over the engine the operation holds
//! ([`super::integration::ExpressMachines`]), exactly as the activities a `Resource`
//! may name are ([`super::activity_resources`]).

use ::std::sync::Arc;

use super::integration::ExpressMachines;

/// What answers whether the state machine an ARN names is of the EXPRESS workflow
/// type, over this service's own store; `None` for an engine carrying no store of this
/// service, where there is nothing to ask.
pub fn express_machines<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> Option<ExpressMachines> {
    super::service_state(&**engine)?;
    let engine = Arc::clone(engine);
    Some(Arc::new(move |arn: &str| {
        super::service_state(&*engine).is_some_and(|state| {
            let store = state.lock();
            // A qualified ARN — a version's or an alias's — names the state machine it
            // belongs to, which is the one that would run, so the type is read the way
            // this service's own front door reads such an ARN.
            let unqualified = crate::service::request_conditions::unqualified_arn(arn).to_string();
            store
                .state_machines
                .get(&unqualified)
                .is_some_and(|machine| machine.sm_type == super::STATE_MACHINE_TYPE_EXPRESS)
        })
    }))
}
