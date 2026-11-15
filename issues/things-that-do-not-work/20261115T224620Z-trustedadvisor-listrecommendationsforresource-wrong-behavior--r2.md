Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: f53b60d37da65728f91690e0e656985cb7bded33

# A non-account value in the account position of awsResourceArn is treated as "another account", so a well-formed query is refused 403 instead of reporting its (empty) list

_Filed: 2026-11-15T22:46:20Z_
_Operation: ListRecommendationsForResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f53b60d37da65728f91690e0e656985cb7bded33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListRecommendationsForResource` is the ONE operation of this service whose input ARN is not one of the service's own identifiers: `awsResourceArn` targets `AwsResourceArn`, whose pattern leaves the 5th (account) segment as `[\w\d_-]*` — it may hold letters and underscores, and real AWS-managed resource ARNs do (`arn:aws:iam::aws:policy/ReadOnlyAccess`, `arn:aws:iam::aws:role/aws-service-role/...`).

The handler hands that ARN straight to `crate::state::foreign_resource_refusal`, which decides ownership with `is_foreign_resource` → `arn_account_id`: ANY non-empty 5th segment is taken to be an account id and, when it differs from `engine::emulator_account_id()`, the request is refused with HTTP 403 `AccessDeniedException` before the store is even read.

So `arn:aws:iam::aws:policy/ReadOnlyAccess` — a value the model accepts (38 bytes, matches the pattern) — is answered 403 "Access denied to resource: …", although `aws` is not an account at all: the ONE account notion this service models is `AccountId`, `@length(12,12)` + `@pattern("^\\d+$")`, which `aws` cannot satisfy, and which is exactly the member `arn_account_id` is documented to feed. The crate's own ownership rule says an ARN that names no account is NOT another account's resource (state.rs:2262-2263), and the crate's own suite states the answer for an ARN nothing in the store flagged is "an EMPTY list and HTTP 200" (tests/all/recommendations_for_resource.rs:453-455) — both broken for this family of inputs, which the suite exercises only in its empty-segment form (`arn:aws:s3:::bucket`).

Every other caller of `foreign_resource_refusal` passes one of this service's own identifiers (`AccountRecommendationIdentifier`, `RecommendationResourceArn`), whose patterns pin the account segment to `\d{12}`, so this handler is the only place the derivation is fed a segment the model does not constrain to an account id.

## Evidence

The model leaves the account position unconstrained — `smithy_query(query="shape", shape="AwsResourceArn")`:
```
"AwsResourceArn": {... "length": {"min": 20, "max": 2048}, "pattern": "^arn:aws(-\\w+)*:[\\w\\d-]+:([\\w\\d-]*)?:[\\w\\d_-]*([:/].+)*$"}
```
while the only account this service models is 12 digits — `smithy_query(query="shape", shape="AccountId")`:
```
"AccountId": {"type": "string", "length": {"min": 12, "max": 12}, "pattern": "^\\d+$"}
```

The handler, which refuses on that segment before anything else — services/service-com-amazonaws-trustedadvisor-trustedadvisor/src/ops/list_recommendations_for_resource.rs:234-243:
```
        // This operation's input names an ARBITRARY AWS resource, so the resource it
        // targets is authorized FIRST: an ARN in another account is refused for that
        // resource, whatever the caller's support plan, because no plan grants an
        // account access to another account's resources. See
        // [`crate::state::foreign_resource_refusal`].
        if let Some(denied) = crate::state::foreign_resource_refusal(&input.aws_resource_arn) {
            return Err(
                ListRecommendationsForResourceErrorModeled::AccessDeniedException(denied).into(),
            );
        }
```

The derivation it uses, documented as producing the value a modelled `accountId` member carries — src/state.rs:2245-2258:
```
/// The owning account a modelled `accountId` member reports for a resource,
/// derived from that resource's own ARN: its account segment, owned, and `None`
/// when the ARN names no account at all — it carries fewer segments than an ARN,
/// or its account segment is EMPTY, which (see [`arn_account`]) is what the
/// segment is for a resource type whose ARNs carry no account.
...
pub fn arn_account_id(arn: &str) -> Option<String> {
    arn_account(arn)
        .filter(|account| !account.is_empty())
        .map(str::to_string)
}
```
and the test built on it, which accepts ANY non-empty segment as an account — src/state.rs:2260-2276:
```
/// Whether `arn` names a resource owned by an account OTHER than the calling
/// one: the account segment of the resource's OWN ARN ([`arn_account_id`])
/// differs from the account the request is made from. An ARN that names no
/// account at all (see [`arn_account`]) is not another account's resource.
...
pub fn is_foreign_resource(arn: &str) -> bool {
    arn_account_id(arn).is_some_and(|account| account != ::engine::emulator_account_id())
}
```

The crate's own stated answer for an ARN nothing in the store flagged — tests/all/recommendations_for_resource.rs:453-455:
```
/// An ARN no stored recommendation flagged reports an EMPTY list and HTTP 200 —
/// never a 404, which this operation models no error for. The rows differ along
/// WHY nothing matches.
```
and the companion rule for ownership — tests/all/support_level_gate.rs:598-602:
```
/// The other side of that rule: an ARN in the caller's own account — or one whose
/// resource type carries no account segment at all — is NOT refused for the resource. It
/// reaches the support-level stage, and an entitled caller reaches the operation itself,
/// which reports the recommendations that flagged that resource (200, possibly an empty
/// list — see `tests/all/recommendations_for_resource.rs`).
```
Test coverage: the two suites above exercise only the 12-digit-account form (tests/all/support_level_gate.rs:607-611) and the EMPTY-segment form (`"arn:aws:s3:::trusted-advisor-probe-bucket"`, tests/all/support_level_gate.rs:615 and tests/all/recommendations_for_resource.rs:486). No row of either suite uses an ARN whose account position holds a non-account token, so the divergence is untested.

## Steps to Reproduce

1. Start the emulator with EMULATOR_TRUSTEDADVISOR_SUPPORT_PLAN=business so the support-level gate admits the caller (src/state.rs:157-162).
2. Call `GET /v1/recommendations-for-resource/arn%3Aaws%3Aiam%3A%3Aaws%3Apolicy%2FReadOnlyAccess` — the value satisfies `AwsResourceArn` (38 bytes, matches `^arn:aws(-\w+)*:[\w\d-]+:([\w\d-]*)?:[\w\d_-]*([:/].+)*$`), so the framework's constraint walk accepts it and the handler runs.
3. Observe HTTP 403 `AccessDeniedException` with `message: "Access denied to resource: arn:aws:iam::aws:policy/ReadOnlyAccess"` (src/state.rs:2295-2299), not the 200-with-empty-list the suite states at tests/all/recommendations_for_resource.rs:453-455.
4. Trace why: `arn_account(arn)` = `arn.split(':').nth(4)` = `"aws"` (src/state.rs:2241-2243); it is non-empty, so `arn_account_id` returns `Some("aws")` (src/state.rs:2254-2258) and `is_foreign_resource` reports `true` because `"aws" != engine::emulator_account_id()` (src/state.rs:2274-2276).
5. Compare with the model: `AccountId` is `@length(12,12)` + `@pattern("^\\d+$")`, so `"aws"` cannot be an account of this service — the ARN names no account, which the crate's own rule (src/state.rs:2262-2263) says is not another account's resource.
6. Contrast with the one accountless row the suite does cover: `GET /v1/recommendations-for-resource/arn%3Aaws%3As3%3A%3A%3Atrusted-advisor-probe-bucket` answers 200 with an empty list, because there the segment is EMPTY rather than a non-account token.
