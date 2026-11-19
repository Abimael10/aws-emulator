// SPDX-License-Identifier: CC0-1.0

//! Which ACTIVITIES this emulator holds, as a walk asks it.
//!
//! "An activity is a worker — a program — that you host": the task a `Task` state whose
//! `Resource` names an activity schedules is queued under the activity a
//! `CreateActivity` call created, and the next `GetActivityTask` on that ARN hands it
//! out. A `Resource` naming an activity that does not exist therefore names no queue at
//! all: nothing a worker polls can reach such a task, so the state is not run for it.
//!
//! The activities live in this service's own store, which the walk cannot read — it runs
//! on a blocking thread with no store locked — so the question is handed to it as a
//! closure over the engine the operation holds
//! ([`super::integration::Activities`]), exactly as the callback handovers
//! ([`super::callbacks`]) and the execution role's own grants
//! ([`super::task_authorization`]) are.

use ::std::sync::Arc;

use super::integration::Activities;

/// What answers whether this emulator holds the activity an ARN names, over this
/// service's own store; `None` for an engine carrying no store of this service, where
/// there is nothing to ask.
pub fn activities<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> Option<Activities> {
    super::service_state(&**engine)?;
    let engine = Arc::clone(engine);
    Some(Arc::new(move |arn: &str| {
        super::service_state(&*engine)
            // An engine whose store has gone answers that the activity is held: the
            // walk's own question is whether this emulator KNOWS the activity does not
            // exist, and a store it cannot read is evidence of nothing.
            .is_none_or(|state| state.lock().activities.contains_key(arn))
    }))
}
