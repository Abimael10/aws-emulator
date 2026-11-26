Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: bb631773176252a429a8a518c5447d51faf32ae8

# A poll whose token resolves to no session is decided against the single resource "*", so a least-privilege caller is refused 403 AccessDeniedException instead of the modelled BadRequestException

_Filed: 2026-11-26T05:48:26Z_
_Operation: GetLatestConfiguration | Class: error-response_

> _Note: this issue was identified against the code base at commit `bb631773176252a429a8a518c5447d51faf32ae8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When `GetLatestConfiguration` cannot resolve the presented token to a stored session (a value this store never minted, or an entry already reclaimed), the handler states that the authorization decision is then taken "on the action alone", and the suite that pins that rule spells it "on the action alone, against every resource". The code does not implement that: it passes `ANY_RESOURCE`, which is the single literal `"*"`, as the REQUEST resource of the IAM decision, so the resource still participates in the decision. The crate's own unit test proves what that costs: a document granting the action on one configuration ARN yields `allowed == false` when the request's resource is `ANY_RESOURCE`.

Consequence: a caller whose identity policy grants `appconfig:GetLatestConfiguration` on its own configuration ARN — the least-privilege policy, and one that is accepted by `StartConfigurationSession` and by every poll whose token DOES resolve — is answered HTTP 403 `AccessDeniedException` the moment it presents a token this store does not hold, rather than the modelled `BadRequestException` (400) with `Details.InvalidParameters.ConfigurationToken.Problem = "Corrupted"`. The integration suite asserts exactly that 400 for a caller holding `appconfig:*` on `*`, so whether an equally authorized caller is told "restart your session" or "fix your IAM policy" depends only on how wide its grant is spelled. The decision is taken before any verdict about the token, so the modelled verdict is unreachable for such a caller.

The shared evaluator this gate already depends on publishes the function for exactly this question and documents the trap the handler fell into.

## Evidence

Both sides inside this crate.

The rule the code claims — services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/get_latest_configuration.rs:319-324:
```
// A poll's own input names only the token, so the configuration the action is
// judged against is the one the session that token resolves to is over — the
// identifiers it was started with — and a token resolving to no session at all
// addresses no configuration this service can name, so the decision is taken on
// the action alone.
```
and the suite that pins it — services/service-com-amazonaws-appconfigdata-appconfigdata/tests/all/identity_authorization.rs:726-730:
```
/// RULE, the other half of the poll's resource: a poll's own input names only its token, so a
/// token that resolves to no session addresses no configuration — the decision is taken on the
/// action alone, against every resource.
```

What the code does instead — src/ops/get_latest_configuration.rs:336-343:
```
if let Some(denial) = crate::state::identity_authorization::refusal(
    &*self.engine,
    ctx,
    <Self as smithy_server_runtime::Operation>::NAME,
    addressed
        .as_deref()
        .unwrap_or(crate::state::identity_authorization::ANY_RESOURCE),
) {
```
with src/state/identity_authorization.rs:86 `pub const ANY_RESOURCE: &str = "*";` and src/state/identity_authorization.rs:168 `let request = iam::Request::new(&action, resource, &[]);` feeding the ordinary resource-matching decision at src/state/identity_authorization.rs:178-184:
```
if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
    return Some(iam::not_authorized_by_identity_policy(...));
}
```

The crate's OWN unit test shows that resource is not "every resource" — src/state/identity_authorization.rs:405-418, inside `a_grant_is_read_for_the_configuration_it_names`, whose grant is `"Action":"appconfig:StartConfigurationSession"` on one configuration ARN:
```
(
    "the granted action naming no configuration",
    "StartConfigurationSession",
    ANY_RESOURCE.to_string(),
    false,
),
...
let said = decision(&document, &action_for(operation), resource);
let actual = iam::is_allowed(said, Decision::NotStated);
```
and services/common/src/iam_authorization.rs:601-606 confirms `is_allowed(NotStated, NotStated) == false`:
```
pub fn is_allowed(identity: Decision, resource: Decision) -> bool {
    if identity == Decision::Denied || resource == Decision::Denied { return false; }
    identity == Decision::Allowed || resource == Decision::Allowed
}
```

The shared crate this gate already uses documents the exact trap — services/common/src/iam_authorization.rs:608-625 (`identity_may_allow_action`):
```
/// That is the question a caller asks here when it knows the ACTION but not the resource ARNs
/// the request addresses — a service's request-time gate standing in front of an action that
/// supports resource-level permissions, where withholding a grant on [`identity_decision`]
/// over `*` alone would refuse a request a resource-scoped statement permits.
```

Test coverage: the only case exercised is a caller granted `appconfig:*` on `"*"` — tests/all/identity_authorization.rs:735-740 and the assertion at :786-795 that such a caller presenting `"0".repeat(64)` gets `"BadRequestException", 400`. No case presents an unresolvable token under a resource-scoped grant, so the divergence is untested.

## Steps to Reproduce

1. Boot the emulator with AppConfigData, AppConfig, IAM and STS mounted on the shared engine (as tests/all/identity_authorization.rs does).
2. Through AppConfig, create an application, environment, configuration profile (LocationUri 'hosted'), a hosted configuration version and a deployment that reaches COMPLETE.
3. Create an IAM role whose identity policy is a single Allow of action `appconfig:StartConfigurationSession` and `appconfig:GetLatestConfiguration` on the ONE configuration ARN the data plane spells for that triple (`arn:aws:appconfig:<region>:<account>:application/<app>/environment/<env>/configuration/<profile>`, i.e. `configuration_resource(...)`), and assume it.
4. Call StartConfigurationSession with that credential: it is served 201, so the grant is correct for this configuration.
5. Call GetLatestConfiguration with that same credential and `configuration_token` set to a value this store never minted (e.g. 64 zeroes), simulating a token lost across a restart or reclaimed by the store's cap.
6. Observed: HTTP 403 `AccessDeniedException` with IAM's denial sentence naming the resource `*`. Expected, per the handler comment at src/ops/get_latest_configuration.rs:319-324 and the suite rule at tests/all/identity_authorization.rs:726-730 (and matching what a caller holding `appconfig:*` on `*` gets at tests/all/identity_authorization.rs:786-795): HTTP 400 `BadRequestException` with `Reason: InvalidParameters` and `Details.InvalidParameters.ConfigurationToken.Problem = "Corrupted"`.
