Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 904a9b0dd76d90cdc6214ea4b7f02cf73fd6ab75

# Minting a session sweeps every EXPIRED token entry in the store, so an expired token of an unrelated chain is then refused the 'Corrupted' verdict instead of the modelled 'Expired' one

_Filed: 2026-11-26T09:24:19Z_
_Operation: StartConfigurationSession | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `904a9b0dd76d90cdc6214ea4b7f02cf73fd6ab75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartConfigurationSession` persists its session through `ComAmazonawsAppconfigdataAppconfigdataState::start_session`, which calls `store_session`, whose first act is `reclaim_sessions(now)` — and `reclaim_sessions` begins by DELETING every session entry in the whole store whose own expiry has passed, not just entries of the chain being minted (`self.sessions.retain(|_, session| now < session.expires_at)`).

`GetLatestConfiguration` decides its token verdict from exactly that map: an entry it still finds past its expiry earns the modelled `InvalidParameterProblem` value `Expired` (Message "Token expired"), while a token that resolves to NO entry earns `Corrupted` (Message "Token not valid"), on the stated ground that "a token this store never minted carries no session and cannot be understood". Once the sweep has run, a token this store DID mint and that has merely run out of lifetime is indistinguishable from one it never minted, so the same expired token is answered with a different modelled problem and a different sentence depending only on whether some unrelated call — another client's `StartConfigurationSession`, or any other session's successful poll, which rotates through the same `store_session` — happened to run after the expiry instant. A refused poll does not sweep (it returns before the rotation), so the divergence is not even self-consistent for one client: present the expired token first and you get `Expired`; mint any other session and present it again and you get `Corrupted`.

The crate assumes the two are interchangeable only in `reclaim_sessions`'s own comment ("its token is refused either way"); everywhere else it treats the discriminator as the thing that matters — it keeps a distinct fixed sentence per problem, states that "WHICH problem applies is what a client branches on", and its suite asserts `Expired` for an expired token and `Corrupted` only for a value "this emulator never minted". The model likewise separates them: `Expired` is "The parameter was expired and can no longer be used" while `Corrupted` is "The parameter was corrupted and could not be understood by the service". A client that backs off on `Expired` (restart the session) versus `Corrupted` (the token text is wrong) takes the wrong branch.

The write is on the StartConfigurationSession path (the only caller of `start_session`) and in the state module it reaches; the wrong answer is emitted by `GetLatestConfiguration`'s verdict step, whose two open issues (`...getlatestconfiguration-error-response` on the ANY_RESOURCE auth resource, `...getlatestconfiguration-wrong-behavior` on the poll floor) cover unrelated causes.

## Evidence

THE DESTRUCTIVE WRITE, reached by this operation — services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/start_configuration_session.rs:407-419:
```
        let token = {
            let mut store = state.lock();
            store.bind_names(binding.learned_names, now);
            store.start_session(
```
services/service-com-amazonaws-appconfigdata-appconfigdata/src/state.rs:406-422 (`start_session` ends in `store_session`):
```
        let chain = self.mint_chain(now);
        let session = Session { ... };
        self.store_session(session, now)
```
src/state.rs:513-518:
```
    pub fn store_session(&mut self, mut session: Session, now: SystemTime) -> String {
        self.reclaim_sessions(now);
        let token = self.mint_token(now);
```
src/state.rs:538-549 — the sweep, and the comment that asserts the equivalence this issue disproves:
```
    /// Drop the session entries that can no longer serve a poll, and hold the store
    /// to [`MAX_SESSIONS`].
    ///
    /// Two classes go, in order: an entry past its own expiry (its token is refused
    /// either way), and — only if the store is still at the cap — ...
    fn reclaim_sessions(&mut self, now: SystemTime) {
        self.sessions.retain(|_, session| now < session.expires_at);
```
(the `retain` is over the WHOLE `sessions` map, so it drops the expired entries of every other poll chain too).

THE VERDICT THAT DEPENDS ON THE DELETED ENTRY — src/ops/get_latest_configuration.rs:360-370:
```
            // The token is opaque, so one this store never minted carries no session
            // and cannot be understood: `Corrupted`.
            let Some(session) = state.session(&token).cloned() else {
                return Err(bad_configuration_token("Corrupted"));
            };

            // A token is usable for at most `TOKEN_LIFETIME` after it was minted; past
            // that it can no longer be used: `Expired`.
            if let Some(expired) = expiry_refusal(&session, now) {
                return Err(expired);
            }
```
and src/ops/get_latest_configuration.rs:624-628:
```
fn expiry_refusal(
    session: &crate::state::Session,
    now: std::time::SystemTime,
) -> Option<GetLatestConfigurationError> {
    (now >= session.expires_at).then(|| bad_configuration_token("Expired"))
```

THE CRATE SAYS THE DISCRIMINATOR IS WHAT A CLIENT BRANCHES ON (so "refused either way" is not equivalent) — src/ops/get_latest_configuration.rs:206-214:
```
/// The sentences are the service's OWN fixed text, not prose of the emulator's, and
/// carry nothing from the request: WHICH problem applies is what a client branches on,
/// and it travels in `Details.InvalidParameters[…].Problem` rather than in the
/// sentence. ...
const TOKEN_PROBLEM_MESSAGES: &[(&str, &str)] = &[
    ("Corrupted", "Token not valid"),
    ("Expired", "Token expired"),
```

SMITHY — the two values are distinct statements (smithy_query query="shape" shape="InvalidParameterProblem"):
```
"CORRUPTED": {"doc": "The parameter was corrupted and could not be understood by the service.", "value": "Corrupted"}
"EXPIRED":   {"doc": "The parameter was expired and can no longer be used.", "value": "Expired"}
```

TEST COVERAGE — the suite pins both verdicts separately, and never mints another session between the expiry and the poll, so the divergence is untested:
tests/all/token_validation_and_lifetime.rs:385-387 (expired ⇒ `Expired`):
```
        set_expiry(&emulator, &initial, SystemTime::now());
        let expired = emulator.get_latest_configuration(&initial).await;
        assert_expired(&expired, name);
```
with tests/all/token_validation_and_lifetime.rs:414-419:
```
        body["Details"]["InvalidParameters"]["ConfigurationToken"]["Problem"].as_str(),
        Some("Expired"),
```
and tests/all/token_validation_and_lifetime.rs:105-117, where `Corrupted` is reserved for "a value this emulator never minted":
```
    TokenParameterCase {
        name: "a value this emulator never minted",
        query: Some("configuration_token=not-a-real-token"),
        expected: Expected::CorruptedVerdict,
    },
```

## Steps to Reproduce

1. Boot the emulator with AppConfigData and the AppConfig control plane on one shared engine (as tests/all/configuration_session_poll.rs::Emulator does).
2. Through AppConfig: create an application, an environment and a hosted configuration profile, write a hosted configuration version and deploy it until GetDeployment reports COMPLETE.
3. Call StartConfigurationSession over that triple and keep InitialConfigurationToken as T.
4. Put T past its lifetime, as tests/all/token_validation_and_lifetime.rs::set_expiry does (or simply let 24 hours pass): state.lock().sessions.get_mut(T).expires_at = SystemTime::now().
5. Call GetLatestConfiguration(configuration_token=T): 400 BadRequestException with Message "Token expired" and Details.InvalidParameters.ConfigurationToken.Problem == "Expired" — the answer the suite pins. The refusal rotates nothing, so T's entry is still in the store.
6. Now make any call that STORES a session — a second StartConfigurationSession over the same (or any) triple, or a successful poll of some other live session: this runs store_session -> reclaim_sessions, which retains only entries with now < expires_at and therefore deletes T's entry.
7. Call GetLatestConfiguration(configuration_token=T) again. Observed: 400 BadRequestException with Message "Token not valid" and Problem == "Corrupted" — the verdict reserved for a token this store never minted. Expected: the same "Expired" verdict as in step 5, because nothing about T changed; only unrelated traffic ran in between.
