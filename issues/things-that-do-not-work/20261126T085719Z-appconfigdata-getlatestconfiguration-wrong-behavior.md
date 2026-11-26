Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 954f40bdc8c8f59a0f2a74042f78050b7862fc3f

# The session's RequiredMinimumPollIntervalInSeconds floor is measured from the presented token ENTRY's last_polled_at, which is never written to the entry that was polled — so re-presenting a token polls as fast as the client likes

_Filed: 2026-11-26T08:57:19Z_
_Operation: GetLatestConfiguration | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `954f40bdc8c8f59a0f2a74042f78050b7862fc3f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartConfigurationSession`'s `RequiredMinimumPollIntervalInSeconds` is documented as a constraint on the SESSION: with 60 seconds, "the client that established the session can't call GetLatestConfiguration more frequently than every 60 seconds". The handler does implement a floor, but it measures it from `session.last_polled_at` of the map entry keyed by the token that was PRESENTED, and nothing ever writes a poll instant onto that entry: the served poll writes `last_polled_at = Some(now)` only onto the freshly minted SUCCESSOR entry it stores under a new key, and the only thing it writes back onto the presented entry is `record_token_use`, which sets `last_used_at` (used solely for eviction ordering) and leaves `last_polled_at` untouched.

Consequence: an entry whose token has already served any number of polls still reports `last_polled_at == None` (for a session's initial token) or the stale instant of the poll that minted it, so the floor branch is skipped or satisfied and the poll is served. A client that keeps presenting the one token it holds — which this crate deliberately keeps usable for the token's whole 24-hour lifetime — is served content and a fresh successor on every call, at any rate, for a session that negotiated a 15s, 60s or 86400s floor. The crate's own suite pins exactly that: three back-to-back presentations of the initial token of a session with `RequiredMinimumPollIntervalInSeconds` of 15 / 60 / 86400 are each asserted to answer 200 with the deployed bytes and a rotated token.

The floor therefore binds only a client that follows the rotation; the model's constraint is on the session, not on one of its tokens. The crate already owns the shape of the correct fix for a session-wide (chain-wide) property: `withdraw_chain`/`write_chain_withdrawal` writes the withdrawal deadline onto EVERY entry of the poll chain precisely because "the client may re-present one it still holds, and that poll has to converge on the refusal too". The poll instant — the other thing a poll of the chain changes — gets no such treatment.

## Evidence

SMITHY (kind A), the member whose constraint is not enforced — smithy_query(query="doc", shape="StartConfigurationSessionRequest", member="RequiredMinimumPollIntervalInSeconds"):
```
Sets a constraint on a session. If you specify a value of, for example, 60 seconds, then
the client that established the session can't call GetLatestConfiguration
more frequently than every 60 seconds.
```

WHAT THE CODE DOES — services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/get_latest_configuration.rs:372-391 (STEP 1):
```
// Honour the floor the session asked for: a session that set
// `RequiredMinimumPollIntervalInSeconds` may not poll faster than that, and
// the refusal serves nothing and rotates nothing, so the same token answers
// once the interval has passed.
...
if let (Some(minimum), Some(last_polled_at)) =
    (required_minimum(&session), session.last_polled_at)
{
    ...
    if since_last_poll < minimum {
        return Err(bad_configuration_token("PollIntervalNotSatisfied"));
    }
}
```
and the only write the served poll makes to the entry it polled — src/ops/get_latest_configuration.rs:508-537:
```
state.record_token_use(&token, now);
let mut next_session = session.clone();
...
next_session.last_polled_at = Some(now);
state.store_session(next_session, now)
```
with src/state.rs:527-536, which writes `last_used_at` and nothing else:
```
/// Record that `token` served a poll at `now`.
///
/// It stays usable until its own expiry — the model bounds a token by time, not by
/// use — so this changes no verdict; it is what tells the store which entries to
/// give up first at its cap ([`Session::has_served_a_poll`]).
pub fn record_token_use(&mut self, token: &str, now: SystemTime) {
    if let Some(session) = self.sessions.get_mut(token) {
        session.last_used_at = Some(now);
    }
}
```
and src/state.rs:513-519, which stores the updated session under a NEW key, leaving the polled entry as it was:
```
pub fn store_session(&mut self, mut session: Session, now: SystemTime) -> String {
    self.reclaim_sessions(now);
    let token = self.mint_token(now);
    ...
    self.sessions.insert(token.clone(), session);
```

REPO SIDE THAT STATES THE RULE THE CODE BREAKS (kind B) — src/state.rs:207-209, the field the floor is measured from:
```
/// When this session was last polled, which is what the session's own minimum
/// poll interval is measured from. `None` until the first poll.
pub last_polled_at: Option<SystemTime>,
```
(the SESSION was polled; the entry that was polled still says `None`), and the sibling suite's rule — tests/all/configuration_session_poll.rs:1466-1468 and :1508:
```
/// The reported poll interval is the session's own minimum when it set one, and a
/// session that set one may not poll faster than it: the early poll is refused with
/// `PollIntervalNotSatisfied` and leaves its token usable.
...
// The first poll is always allowed: the session has not polled yet.
```

THE CRATE ALREADY HANDLES A CHAIN-WIDE PROPERTY THE OTHER WAY — src/ops/get_latest_configuration.rs:521-525:
```
// What was withdrawn is the TRIPLE, which every token of this poll chain
// resolves to, so the deadline is recorded on — and cleared from — every entry
// of the chain and not only the successor this poll mints: a token is bounded by
// time and by nothing else, so the client may re-present one it still holds, and
// that poll has to converge on the refusal too
```
implemented by src/state.rs:457-464:
```
fn write_chain_withdrawal(&mut self, chain: &str, due: Option<SystemTime>) {
    for session in self.sessions.values_mut() {
        if session.chain == chain {
            session.withdrawal_due_at = due;
```

TEST COVERAGE: the divergence is currently asserted as expected by tests/all/token_validation_and_lifetime.rs:296-348 (`a_token_inside_its_lifetime_is_served_however_often_it_has_been_presented`), whose rows include `requested: Some(15)`, `Some(60)` and `Some(86_400)` (:248-271) and which polls the SAME initial token three times with no wait:
```
// Three presentations of the SAME token: the first poll, then two
// re-presentations of a token the client still holds ...
// Each is served, each rotates a fresh successor, and each reports the obligation
// the session negotiated.
for presentation in ["the first poll", "the first re-presentation", "the second re-presentation"] {
    let reply = emulator.get_latest_configuration(&initial).await;
    assert_eq!(reply.status, 200, ...);
```
The reason that suite states for generalising over the floor is about LIFETIME only — "The rows differ along the poll floor the session negotiated ... because none of that changes how long a token lasts" (:223-226) — so it states no reason for waiving the floor, and the Smithy sentence above contradicts the 200s it asserts. tests/all/next_poll_token_poll_rate.rs and configuration_session_poll.rs:1470 only ever present the ROTATED successor, so no test exercises the floor against a re-presented token.

## Steps to Reproduce

1. Boot the emulator with AppConfigData and the AppConfig control plane on the shared engine (as tests/all/configuration_session_poll.rs::Emulator does).
2. Through AppConfig: create an application, environment and hosted configuration profile, create a hosted configuration version and deploy it with an instant strategy until GetDeployment reports COMPLETE.
3. Call StartConfigurationSession with that triple and RequiredMinimumPollIntervalInSeconds=60; keep InitialConfigurationToken as T1.
4. Call GetLatestConfiguration(configuration_token=T1): served 200 with the deployed bytes, Next-Poll-Interval-In-Seconds: 60, and a successor T2.
5. Call GetLatestConfiguration(configuration_token=T1) again IMMEDIATELY (and again, and again, in a tight loop).
6. Observed: every call answers 200 with the configuration and a fresh successor token — the 60-second floor the session negotiated is never applied, because T1's stored entry still has last_polled_at == None. Expected per the model sentence for RequiredMinimumPollIntervalInSeconds (and per the rule tests/all/configuration_session_poll.rs:1466-1468 states): the second and later calls inside the 60 seconds are refused BadRequestException 400, Message "Request too early", Reason InvalidParameters, Details.InvalidParameters.ConfigurationToken.Problem == "PollIntervalNotSatisfied", exactly as presenting T2 immediately is refused today.
7. Contrast control: call GetLatestConfiguration(configuration_token=T2) immediately — refused 400 PollIntervalNotSatisfied — so whether the session's own floor applies depends only on which token of the same chain the client presents.
