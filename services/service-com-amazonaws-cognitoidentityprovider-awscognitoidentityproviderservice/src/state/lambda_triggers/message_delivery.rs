// SPDX-License-Identifier: CC0-1.0

//! Which of a pool's Lambda triggers one of its MESSAGES is routed to: the one place every
//! operation that issues a message asks that question, so no two of them can answer it
//! differently.
//!
//! A pool configures two kinds of trigger over the one point it issues a message, and they are
//! alternatives rather than a sequence:
//!
//! * a custom SENDER for the medium the message travels over — `CustomEmailSender` "routes all
//!   email notifications from a user pool to a Lambda function" and `CustomSMSSender` "all SMS
//!   notifications … to a Lambda function that delivers the message using custom logic" (this
//!   service's model) — in which case the pool delivers nothing itself and the function is
//!   handed the code ([`super::custom_sender`]);
//! * a custom MESSAGE trigger, which "returns a runtime-customized message subject and body
//!   FOR YOUR USER POOL TO DELIVER to a user" (`LambdaConfigType$CustomMessage`), in which case
//!   the pool is still the one delivering, so the function is asked for the wording and never
//!   handed the code ([`super::custom_message`]).
//!
//! A pool that routes a medium's messages away has no message of its own left to have worded,
//! so the sender is what that medium's message reaches; the custom message trigger is asked for
//! every other message the pool delivers. A pool that configures neither for the medium
//! delivers its own message and invokes nothing.
//!
//! The work splits the way every trigger of this service does ([`super`]): [`pending`] resolves
//! the trigger and builds its event from the pool and the user read under the state lock, and
//! [`invoke`] performs the invocation once that lock is RELEASED. The [`fire`] family is for an
//! operation holding NO lock, which takes one of its own to read the pool and the user with.

use std::collections::HashMap;

use super::{custom_message, custom_sender, Caller, Failure};
use crate::state::{User, UserPool};

pub use super::custom_sender::{Sender, Stage};

/// One message-trigger invocation an operation owes: the trigger the message is routed to,
/// with the event that trigger is handed.
///
/// No `Debug`: a custom sender's own pending invocation holds the code the message carries.
#[derive(Clone)]
pub enum Pending {
    /// The message is DELIVERED by the custom sender of its medium, which is handed the code.
    Delivered(custom_sender::Pending),
    /// The message is delivered by the pool, with the WORDING the custom message trigger
    /// returns for it.
    Worded(custom_message::Pending),
}

/// The invocation a message issued to `user` of `pool` over `sender`'s medium owes, or `None`
/// when the pool configures no trigger over that message at all and so it is delivered with the
/// pool's own wording, by the pool.
///
/// Built while the caller still holds the state lock — each event reports the pool and the user
/// as the issuing operation left them — and invoked by [`invoke`] once that lock is released.
#[must_use]
pub fn pending(
    pool: &UserPool,
    user: &User,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Option<Pending> {
    if let Some(delivered) =
        custom_sender::pending(pool, user, sender, stage, caller, client_metadata, code)
    {
        return Some(Pending::Delivered(delivered));
    }
    custom_message::pending(pool, user, sender, stage, caller, client_metadata)
        .map(Pending::Worded)
}

/// The invocation a message the pool DELIVERS ITSELF owes, for an operation that hands no code
/// over because it issues none — the invitation an administrative create sends over a medium
/// the pool configures no custom sender for.
///
/// The same decision as [`pending`] for a pool that routes nothing away: only the custom
/// message trigger can be owed, since there is no code for a sender to be handed.
#[must_use]
pub fn pending_wording(
    pool: &UserPool,
    user: &User,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
) -> Option<Pending> {
    custom_message::pending(pool, user, sender, stage, caller, client_metadata)
        .map(Pending::Worded)
}

/// Perform `owed` and report what the trigger leaves the issuing operation to answer.
///
/// Call it with the state lock RELEASED, for the reason each trigger's own invocation states.
pub async fn invoke(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    owed: Pending,
) -> Result<(), Failure> {
    match owed {
        Pending::Delivered(owed) => custom_sender::invoke(ctx, owed).await,
        Pending::Worded(owed) => custom_message::invoke(ctx, owed).await,
    }
}

/// Fire the trigger a message issued for `username` in `user_pool_id` over `sender`'s medium is
/// routed to, and report what it leaves the issuing operation to answer.
///
/// For an operation that holds NO lock: the pool and the user are read under a lock of its own,
/// which is released before any function is invoked, so no other request of this service waits
/// on one. A pool that configures no trigger over that message is answered by nothing being
/// invoked at all.
#[allow(clippy::too_many_arguments)]
pub async fn fire(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let owed = owed_under_the_lock(store, user_pool_id, username, |pool, user| {
        pending(pool, user, sender, stage, caller, client_metadata, code)
    });
    match owed {
        Some(owed) => invoke(ctx, owed).await,
        None => Ok(()),
    }
}

/// The same, for a message the pool delivers itself with no code handed over
/// ([`pending_wording`]).
#[allow(clippy::too_many_arguments)]
pub async fn fire_wording(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
) -> Result<(), Failure> {
    let owed = owed_under_the_lock(store, user_pool_id, username, |pool, user| {
        pending_wording(pool, user, sender, stage, caller, client_metadata)
    });
    match owed {
        Some(owed) => invoke(ctx, owed).await,
        None => Ok(()),
    }
}

/// The same as [`fire`], for a code delivered over `medium`: the trigger over that medium's
/// message is the one fired, and a medium no `DeliveryMediumType` names has no trigger over it.
#[allow(clippy::too_many_arguments)]
pub async fn fire_over_medium(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    medium: &str,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let Some(sender) = Sender::of_medium(medium) else {
        return Ok(());
    };
    fire(
        ctx,
        store,
        user_pool_id,
        username,
        sender,
        stage,
        caller,
        client_metadata,
        code,
    )
    .await
}

/// The same, for a code the message was addressed to `attribute`'s stored value: the medium
/// that attribute is reached over is the one the message travels on, and an attribute no
/// message is addressed over has no trigger over it.
#[allow(clippy::too_many_arguments)]
pub async fn fire_over_attribute(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    attribute: &str,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let Some(sender) = Sender::of_attribute(attribute) else {
        return Ok(());
    };
    fire(
        ctx,
        store,
        user_pool_id,
        username,
        sender,
        stage,
        caller,
        client_metadata,
        code,
    )
    .await
}

/// What `build` decides this message owes, from the pool and the user read under a lock this
/// call owns: it is taken and RELEASED here, so the caller holds none of it while a function is
/// invoked. A pool or a user the state does not hold owes nothing.
fn owed_under_the_lock(
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    build: impl FnOnce(&UserPool, &User) -> Option<Pending>,
) -> Option<Pending> {
    let guard = store.lock();
    let pool = guard.pool(user_pool_id)?;
    let user = pool.user(username)?;
    build(pool, user)
}
