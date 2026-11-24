// SPDX-License-Identifier: CC0-1.0

//! The SESSION MANAGER record of an execute-command session: the one Systems Manager holds,
//! beside the one this store holds.
//!
//! # The rule this module implements
//!
//! An execute-command session IS a Session Manager session: "the details for the execute
//! command session" an `ExecuteCommand` answers are a `sessionId`, the `streamUrl` of its data
//! channel and the `tokenValue` that channel is authenticated with — the three members
//! `ssm:StartSession` answers — and the session they name is one Systems Manager is holding,
//! so `ssm:DescribeSessions` reports it among the ACTIVE sessions of the account until it
//! ends. A session a client is handed and that service has no record of would be a session
//! nothing is tracking: an id, a URL and a token with nothing behind them.
//!
//! So the record is written where sessions LIVE — in the Systems Manager service's own store
//! in the shared engine, through that service's own session registry
//! ([`::service_com_amazonaws_ssm_amazonssm::state::sessions`]) — rather than in a copy kept
//! here, and a client reads it back with that service's own `DescribeSessions`. What this
//! store holds is the session's own stream; what that one holds is the session.
//!
//! The members are the ones that service records for a start: the id this service minted, the
//! TARGET the session is to — an execute-command session names its container, which is
//! `ecs:<cluster>_<task id>_<container runtime id>` — the principal that called
//! `ExecuteCommand` as its owner, and the Session Manager document an interactive
//! execute-command session runs ([`EXECUTE_COMMAND_DOCUMENT`]). The token is NOT among them:
//! it is this emulator's secret for the channel and no record of either service carries it.
//!
//! LOCK ORDER: the Systems Manager store is taken with NO guard of this one held. Both calls
//! here are made from a door that has just written a session, or from the settle step that
//! carries out what a clock pass left owed ([`settle_exec_session_records`]), and neither
//! holds this store's lock while that one is taken.

use ::std::time::SystemTime;

use ::service_com_amazonaws_ssm_amazonssm::state as ssm;

use crate::state::*;

/// The Session Manager document an interactive execute-command session runs, which is what
/// its record reports as the document the session was started with.
pub const EXECUTE_COMMAND_DOCUMENT: &str = "AmazonECS-ExecuteInteractiveCommand";

/// The `Target` a Session Manager session of a container is held under: the kind of target,
/// then the cluster, the task and the container's own runtime id — the three that identify one
/// container of one task of one cluster.
#[must_use]
pub fn exec_session_target(cluster_name: &str, task_id: &str, container_runtime_id: &str) -> String {
    format!("ecs:{cluster_name}_{task_id}_{container_runtime_id}")
}

/// Record the session `session_id` names with Systems Manager, as the session it is.
///
/// Call it with NO guard of this store held, from the call that opened the session: it takes
/// the Systems Manager store's lock (see the module's LOCK ORDER note). An engine carrying no
/// Systems Manager, and a Systems Manager store already holding as many sessions as it can,
/// leave no record — the session's own stream is carried either way, and nothing is invented
/// in a store that could not take it.
pub fn register_ssm_session<Eng: Send + Sync + 'static>(
    engine: &Eng,
    session_id: &str,
    region: &str,
    target: &str,
    owner: &str,
    at: SystemTime,
) {
    let Some(state) = ssm::service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    if !store.can_hold_another_session() {
        ::tracing::warn!(
            %session_id,
            "Systems Manager holds as many sessions as it can, so this execute-command \
             session is not recorded with it"
        );
        return;
    }
    store.insert_session(
        region,
        ssm::sessions::SessionRecord::started(
            session_id.to_string(),
            target.to_string(),
            owner.to_string(),
            Some(EXECUTE_COMMAND_DOCUMENT.to_string()),
            // `Session$Reason` is "the reason for connecting to the instance", which a
            // caller supplies; `ExecuteCommand` takes no such member, so the session has
            // none rather than one worded here.
            None,
            at,
        ),
    );
}

/// End the Systems Manager record of every session of this store that has ended, and end
/// every session of this store whose Systems Manager record has ended — so the two services
/// hold ONE account of each session.
///
/// Both directions are the same fact read from either end. A session this store has finished
/// carrying is reported among the account's session HISTORY rather than among its active
/// sessions; and a session Systems Manager has been asked to end (`ssm:TerminateSession`
/// "permanently ends a session and closes the data connection between the Session Manager
/// client and SSM Agent") is one this store carries no further, which is what the connection
/// reads between frames.
///
/// Call it with NO guard of either store held, from a door that has just read or written a
/// task, from the connection that carries a session, or from the clock pump: it takes this
/// store's lock to collect what is owed, the Systems Manager store's to carry it out and to
/// read what that service holds, and this store's again to end what ended there — in that
/// order and never two at once.
pub fn settle_exec_session_records<Eng: Send + Sync + 'static>(engine: &Eng, at: SystemTime) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let (ended, carried) = {
        let mut store = state.lock();
        (store.take_ended_exec_sessions(), store.carried_exec_sessions())
    };
    if ended.is_empty() && carried.is_empty() {
        return;
    }
    let Some(ssm_state) = ssm::service_state(engine) else {
        return;
    };
    let ended_elsewhere: Vec<String> = {
        let mut store = ssm_state.lock();
        for (region, session_id) in &ended {
            if let Some(record) = store.session_mut(region, session_id) {
                record.terminate(at);
            }
        }
        carried
            .iter()
            .filter(|(region, session_id)| {
                // A session Systems Manager holds no record of is left alone: this store's own
                // account of it is then the only one there is. One whose record has ENDED is
                // over, whoever ended it.
                store
                    .session(region, session_id)
                    .is_some_and(|record| record.state() != ssm::sessions::SESSION_STATE_ACTIVE)
            })
            .map(|(_, session_id)| session_id.clone())
            .collect()
    };
    if ended_elsewhere.is_empty() {
        return;
    }
    let mut store = state.lock();
    for session_id in ended_elsewhere {
        store.end_exec_session(&session_id);
    }
}
