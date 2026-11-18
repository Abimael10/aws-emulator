// SPDX-License-Identifier: CC0-1.0

//! The address a client opens a notebook at, and the token that authenticates it.
//!
//! `CreatePresignedNotebookUrl` "Gets an authentication token and the URL at which the
//! notebook can be accessed", and `CreatePresignedNotebookUrlResponse$NotebookUrl` states
//! what that URL is made of: "The URL includes the authentication token and notebook file
//! name and points directly to the opened notebook." So the address is built HERE, from the
//! session the request named and the notebook that session was started in, rather than from
//! anything the reading request carried beyond that session id.
//!
//! The URL leads back to THIS emulator: it is query-signed by the emulator's own signer
//! ([`notebook_url`]), the one place a URL a client will open later is minted, so nothing
//! this service hands out addresses a real service endpoint.
//!
//! The TOKEN is the authenticator this store minted for that session's address
//! ([`ComAmazonawsAthenaAmazonathenaState::notebook_auth_token`]), computed under a secret the
//! store drew from the system's randomness and never reveals, over the session the address
//! names and a nonce of its own. So a token is a value only this store could have produced
//! and names which session it was issued for
//! ([`ComAmazonawsAthenaAmazonathenaState::notebook_auth_token_session`]), and each issue is
//! a NEW token — which is what "must be called every 10 minutes to refresh the
//! authentication token" asks of a re-issue.

use std::time::{Duration, SystemTime};

use ::smithy_server_runtime::{SignedUrlRequest, UrlSigner};

use super::*;

/// How long the authentication token a notebook address carries is good for.
///
/// PUBLISHED by the operation that mints it: "During programmatic access,
/// `CreatePresignedNotebookUrl` must be called every 10 minutes to refresh the
/// authentication token" — so ten minutes is the token's lifetime rather than a span this
/// emulator chose, and it is also how long the signed address stays valid, the two being one
/// grant.
pub const NOTEBOOK_AUTH_TOKEN_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// What `CreatePresignedNotebookUrlResponse$AuthTokenExpirationTime` reports: how many
/// seconds the token just minted is good for.
///
/// A LIFETIME, not an instant, and that is what the member carries on the wire. The member's
/// own documentation calls it "The UTC epoch time when the authentication token expires", but
/// the RECORDED response of this operation carries `600` — the ten minutes the operation's
/// prose publishes for refreshing the token, in seconds — for every issue, early or late, and
/// a re-issue reports that same value beside its new token. The recorded wire form governs
/// where it and a member's prose disagree, so this is derived from
/// [`NOTEBOOK_AUTH_TOKEN_LIFETIME`] rather than from the clock: reporting
/// `now + lifetime` here would answer a number no recorded response of this operation ever
/// carried, and would make two calls a minute apart report different values where AWS reports
/// one.
///
/// The instant the grant actually stops working is carried by the address itself — the
/// `X-Amz-Expires` window the signer wrote over that same lifetime — so nothing about the
/// grant's own expiry is lost by reporting the span the member reports.
pub fn auth_token_expiration_time() -> i64 {
    i64::try_from(NOTEBOOK_AUTH_TOKEN_LIFETIME.as_secs()).unwrap_or(i64::MAX)
}

/// The query field the notebook's address carries its authentication token in, which is what
/// makes the token part of the URL ("The URL includes the authentication token").
const AUTH_TOKEN_FIELD: &str = "authToken";

/// The first segment of a notebook address, which is the notebook editor the URL "points
/// directly to".
const NOTEBOOK_PATH_PREFIX: &str = "notebook-editor";

/// The query field the notebook's address carries its authentication token in, read back by
/// the route that serves the address (`crate::service::notebook_editor`).
pub const AUTH_TOKEN_QUERY_FIELD: &str = AUTH_TOKEN_FIELD;

/// The media type the notebook file an address leads to is served as: an ipynb file is a
/// JSON document of the nbformat schema, and this is the media type Jupyter registers for
/// one.
pub const NOTEBOOK_MEDIA_TYPE: &str = "application/x-ipynb+json";

/// The session a notebook address names and the notebook file name it carries, or `None`
/// when `segments` are not the path of one.
///
/// `segments` are the path's segments percent-DECODED, which is the view the signer built
/// the address from, so a notebook name carrying a space resolves to that same name here.
pub fn session_in_notebook_path(segments: &[String]) -> Option<(String, Option<String>)> {
    let (prefix, rest) = segments.split_first()?;
    if prefix != NOTEBOOK_PATH_PREFIX {
        return None;
    }
    let (session_id, name) = match rest {
        [session_id] if !session_id.is_empty() => (session_id.clone(), None),
        [session_id, name] if !session_id.is_empty() && !name.is_empty() => {
            (session_id.clone(), Some(name.clone()))
        }
        _ => return None,
    };
    Some((session_id, name))
}

/// The ipynb content the notebook address of session `session_id` leads to, or `None` when
/// this emulator serves no notebook there.
///
/// Every one of these is answered the same way, so a refusal tells a caller nothing about
/// which it was: an address carrying no authentication token this store minted, one minted
/// for ANOTHER session, a session this account no longer holds, a session whose resources are
/// no longer running (which serves no notebook), a session bound to no notebook, and an
/// address whose file name is not the one that session's notebook carries.
///
/// Read through the same registries the reporting operations read, so an address serves a
/// notebook exactly while `GetNotebookMetadata` reports it and the session that opened it is
/// running, and the content is the STORED notebook's rather than anything rebuilt from the
/// fetch.
pub fn notebook_at_address<Eng: Send + Sync + 'static>(
    engine: &Eng,
    session_id: &str,
    notebook_name: Option<&str>,
    token: &str,
    now: SystemTime,
) -> Option<String> {
    let state = super::service_state(engine)?;
    let store = state.lock();
    if store.notebook_auth_token_session(token)? != session_id {
        return None;
    }
    let session = store.sessions.session(session_id)?;
    if !session.is_active_at(now) {
        return None;
    }
    let notebook = store
        .notebooks
        .notebook(session.notebook_id.as_deref()?)?;
    if notebook_name.is_some_and(|named| named != notebook.name) {
        return None;
    }
    Some(notebook.payload.clone())
}

/// The message a request for the address of a session that is no longer running is refused
/// with.
///
/// RECORDED wording for that class: a session whose resources are gone serves no notebook,
/// so there is no address to hand out, and AWS states it as fixed text carrying nothing of
/// the request — not the session id, not the state it is in.
pub const NOTEBOOK_SESSION_NOT_OPEN: &str = "Invalid Request";

/// The address of the notebook the session `session_id` serves, signed for
/// [`NOTEBOOK_AUTH_TOKEN_LIFETIME`] and carrying `auth_token`.
///
/// `notebook_name` is the file name of the notebook the session was started in, which the
/// member documents the URL as carrying; a session bound to no notebook addresses the
/// session's editor without one.
///
/// `None` when the address cannot be signed, which is the one case this operation has no URL
/// to report rather than reporting one that leads nowhere.
pub fn notebook_url(
    signer: &UrlSigner,
    session_id: &str,
    notebook_name: Option<&str>,
    auth_token: &str,
) -> Option<String> {
    let path = notebook_path(session_id, notebook_name);
    let request = SignedUrlRequest::new(
        ::http::Method::GET,
        crate::state::authorization::ACTION_PREFIX,
        &path,
    )
    .query(AUTH_TOKEN_FIELD, auth_token);
    signer
        .sign(request, NOTEBOOK_AUTH_TOKEN_LIFETIME)
        .ok()
        .map(|url| url.as_str().to_string())
}

/// The path, on this emulator, the address of one session's notebook names: the notebook
/// editor, the session serving it and the notebook's own file name where it has one. Decoded,
/// as the signer takes it, so a notebook name carrying a space survives the signing.
fn notebook_path(session_id: &str, notebook_name: Option<&str>) -> String {
    match notebook_name {
        Some(name) => format!("/{NOTEBOOK_PATH_PREFIX}/{session_id}/{name}"),
        None => format!("/{NOTEBOOK_PATH_PREFIX}/{session_id}"),
    }
}

/// The cursor a notebook address's authenticator is computed over: the session the address
/// names and the nonce of this one issue.
fn notebook_auth_cursor(session_id: &str, nonce: &str) -> String {
    format!("notebookauth{CURSOR_SEPARATOR}{session_id}{CURSOR_SEPARATOR}{nonce}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// A FRESH authentication token for the address of session `session_id`: this store's own
    /// tag over which session the address names, rendered as the hexadecimal the address
    /// spells it in.
    ///
    /// Each call mints another one — the nonce it carries is drawn per issue — so a re-issue
    /// hands the caller a different token for the same session, which is what refreshing the
    /// token means. Nothing is written: the token carries everything needed to resolve it
    /// ([`Self::notebook_auth_token_session`]), so handing one out leaves the store exactly as
    /// the previous caller saw it.
    pub fn notebook_auth_token(&self, session_id: &str) -> String {
        let nonce = ::uuid::Uuid::new_v4().as_hyphenated().to_string();
        ::hex::encode(
            self.notebook_address_key
                .mint(&notebook_auth_cursor(session_id, &nonce)),
        )
    }

    /// The `SessionId` the authentication token `token` was issued for, or `None` for any
    /// value this store did not mint as one: a token of another kind, one tampered with, and
    /// one another store minted all name no session.
    pub fn notebook_auth_token_session(&self, token: &str) -> Option<String> {
        let cursor = self
            .notebook_address_key
            .cursor_of(&::hex::decode(token).ok()?)?;
        let mut parts = cursor.split(CURSOR_SEPARATOR);
        let kind = parts.next()?;
        let session_id = parts.next()?;
        let nonce = parts.next()?;
        if parts.next().is_some() || kind != "notebookauth" || nonce.is_empty() {
            return None;
        }
        Some(session_id.to_string())
    }
}
