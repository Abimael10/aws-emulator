Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 6eef05c8c0781f6452b9ce529041b12664bce7b1

# A page cursor is scoped to the presented access TOKEN, so a refresh rotation refuses a nextToken this service issued to the same sign-in session

_Filed: 2026-11-15T01:26:26Z_
_Operation: ListAccounts | Class: pagination_

> _Note: this issue was identified against the code base at commit `6eef05c8c0781f6452b9ce529041b12664bce7b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::cursor_scope` is documented as binding a cursor to "the listing it walks, and the session that asked for it", and `decode_cursor`/`page_of` refuse anything outside that scope. But the value it digests is the presented ACCESS TOKEN, not the sign-in session — and this crate states, in three separate places, that the two are not the same thing: a sign-in session is "what a whole chain of tokens belongs to rather than any one of them", and "a refresh supersedes the access token it rotates" while the sign-in itself continues (the crate even asserts subject continuity across a rotation).

Consequence, on the ordinary federated path this crate's own test suite exercises: a client walks `ListAccounts` (or `ListAccountRoles`) with `max_result`, gets a `nextToken`, then its OIDC refresh grant rotates its token pair — which the mint records as the SAME sign-in session and which drops the old access token, so the client MUST present the new one. The next page request carries the rotated bearer token plus the `nextToken` this service minted for that session's listing, and `cursor_scope` now produces a different digest, `decode_cursor` fails the `strip_prefix`, and the handler answers 400 `InvalidRequestException` "nextToken is not a page token this operation issued for this listing" — for a cursor this service DID issue for this listing and this session. The remaining page is unreachable; the walk has to restart from the beginning.

The sign-in session identifier the scope needs is already in the record this crate reads: `sign_in::minted_sign_in` fetches the mint's `IssuedToken` and reads `issued.client_id` and `issued.expires_at()` off it, while the same struct carries a public `sign_in_session` the portal never reads. So nothing new has to be invented or recorded — the value is in hand and is dropped.

Filed on `ListAccounts` because `list_accounts.rs` is the first handler to call `cursor_scope`/`page_of`; the identical codec serves `ListAccountRoles`. (Corroboration only, not the defect filed here: the same token-vs-session conflation keys `IssuedCredentialKey` by access token, so `GetRoleCredentials` mints a SECOND credential for one (sign-in, account, role) after a rotation although its own header says "the same session asking again sees the same credential until it expires".)

## Evidence

The rule the codec states for itself — services/service-com-amazonaws-sso-swbportalservice/src/state.rs:570-579:
```rust
/// The scope a cursor is bound to: the listing it walks, and the session that asked
/// for it. A cursor presented to another listing or by another session is refused.
///
/// The session is identified by a digest of its access token, never the token itself:
/// the token is `@sensitive` and a cursor travels back out in a response and into a
/// client's next URL, so the token must not be recoverable from it.
pub fn cursor_scope(listing: &str, access_token: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(access_token.as_bytes());
    format!("{listing}:{}", hex::encode(&digest[..16]))
}
```
(the justification given is for the DIGEST, not for choosing the token as the session's identity; no comment or test in the crate states that a rotation invalidates a cursor)

The same crate's rule that a token is NOT the session — src/state/sign_in.rs:35-38:
```rust
//! A sign-in session is not a token, though: it is what a whole chain of tokens belongs to,
//! and that chain is the MINT's record.
```
and src/state.rs:72-76:
```rust
    /// ANSWERS as a sign-in of its own: the mint's own record is what says whether such a
    /// token still names one, so a token the mint no longer holds (a refresh supersedes the
    /// access token it rotates) stops being served at that instant instead of being served
    /// from this copy.
```
and src/state/sign_in.rs:146-148, which asserts the sign-in CONTINUES across a rotation:
```rust
/// is the identity the emulator records for it. It is stable for the life of that
/// registration — rotating a refresh token re-issues against the same `clientId` — so a
/// credential minted before a refresh speaks for the same subject as one minted after.
```

Where the token (and not the session) becomes the cursor scope — src/ops/list_accounts.rs:198-211:
```rust
        let page = crate::state::page_of(
            assigned,
            &crate::state::cursor_scope("accounts", &input.access_token),
            input.next_token.as_deref(),
            input.max_results,
        )
        .map_err(|_| {
            ListAccountsErrorModeled::InvalidRequestException(InvalidRequestException {
                message: Some(
                    "nextToken is not a page token this operation issued for this listing"
                        .to_string(),
                ),
```
and the refusal it produces — src/state.rs:604-613 (`strip_prefix(&format!("{CURSOR_VERSION}:{scope}:")).ok_or(InvalidCursor)?`).

The session identifier is present and unread — src/state/sign_in.rs:153-162:
```rust
fn minted_sign_in<Eng: Send + Sync + 'static>(
    engine: &Eng,
    access_token: &str,
    now: SystemTime,
) -> Option<(String, SystemTime)> {
    let tokens = oidc::state::service_state(engine)?;
    let tokens = tokens.lock();
    let issued = tokens.access_tokens.get(access_token)?;
    (!issued.is_expired(now)).then(|| (issued.client_id.clone(), issued.expires_at()))
}
```
against the record it reads — services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:407-413:
```rust
pub struct IssuedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub client_id: String,
    /// The sign-in session this token belongs to; see [`SignInSession`].
    pub sign_in_session: String,
```
and the rotation that supersedes the access token while keeping that session — same file, :813-818:
```rust
    /// Rotates `refresh_token`: the refresh token is spent, the access token it
    /// was issued with is superseded (and dropped, so a refresh loop leaves
    /// nothing resident), and a new access and refresh token pair carrying the
    /// same scopes takes their place.
```

Test coverage: the suite pins that the rotated token IS served for the same sign-in, but never paginates across the rotation — tests/all/minted_sign_in.rs:1048-1052 and :1076-1084:
```rust
/// RULE: the mint's own record is what says whether a token still names a sign-in, so a
/// token the mint no longer holds — one a refresh superseded — is refused from that instant,
/// ... while the token the
/// rotation issued is served and the credential minted before it goes on working for the
/// rest of its own duration.
```
```rust
    let rotated = emulator.refreshed(&minted).await;
    ...
    let (status, body) = emulator.list_accounts(&rotated).await;
    assert_eq!(
        (status, reported_accounts(&body)),
        (StatusCode::OK, expected),
        "the token the rotation issued must be served, got {status} {body}"
    );
```
No row in that test, in tests/all/page_token_spelling.rs or in tests/all/page_bound_clamp.rs (which computes `state::cursor_scope("accounts", ACCESS_TOKEN)` for one fixed token, :125) presents a cursor minted before a rotation, so the divergence is unpinned.

## Steps to Reproduce

1. Stand the portal up beside the OIDC, IAM Identity Center and Organizations services on one shared engine, as tests/all/minted_sign_in.rs:1053-1058 does (`assigned_emulator` + `minted_sign_in`), with enough assigned accounts that a listing pages (two is enough with `max_result=1`).
2. Walk the first page: `GET /assignment/accounts?max_result=1` with `x-amz-sso_bearer_token: <minted.access_token>`. Keep the returned `nextToken` — it is `hex("v1:accounts:<sha256(minted.access_token)[..16]>:<first account id>")` (src/state.rs:576-579, :590-592).
3. Rotate the pair as a client whose access token is about to run out does: `let rotated = emulator.refreshed(&minted).await;` (tests/all/minted_sign_in.rs:368-372). The mint records the new access token under the SAME `sign_in_session` and drops the old one (services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:813-818), so the old token can no longer be presented at all (tests/all/minted_sign_in.rs:1086-1120 pins it as 401).
4. Ask for the second page with the only token the client now has: `GET /assignment/accounts?max_result=1&next_token=<that nextToken>` with `x-amz-sso_bearer_token: <rotated>`.
5. Observed: HTTP 400 `InvalidRequestException`, message "nextToken is not a page token this operation issued for this listing" (src/ops/list_accounts.rs:204-211) — `cursor_scope` digested a different token, so `decode_cursor`'s `strip_prefix` failed (src/state.rs:607-609). The second page cannot be reached with either token: the old one is 401, the new one is 400.
6. Expected per src/state.rs:570-571 ("the session that asked for it"), src/state/sign_in.rs:35-38 and :146-148, and the rule tests/all/minted_sign_in.rs:1048-1052 states (the rotation's token is served for the same sign-in): the cursor this service issued to this sign-in is honoured and the remaining accounts are reported.
7. Contrast on the same store, with no HTTP: `state::cursor_scope("accounts", &minted.access_token) != state::cursor_scope("accounts", &rotated)`, although `oidc::state::service_state(&engine).lock().access_tokens[&minted.access_token].sign_in_session == ...[&rotated].sign_in_session` — one sign-in session, two cursor scopes, from a field src/state/sign_in.rs:160 already has in hand and does not read.
