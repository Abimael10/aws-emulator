Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 75aa740b57f2ad0444d0e0fc35d1abe0f9938e12

# The idempotency fingerprint is order-sensitive for Targets only, so an identical replay with the targets listed in another order is refused 400 instead of replaying the rule

_Filed: 2026-11-26T06:49:36Z_
_Operation: CreateNotificationRule | Class: error-response_

> _Note: this issue was identified against the code base at commit `75aa740b57f2ad0444d0e0fc35d1abe0f9938e12`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateNotificationRule` builds the fingerprint a `ClientRequestToken` is allowed to replay from the request's scalar members plus three collections. Two of the three are deliberately order-normalised before they enter the fingerprint — `EventTypeIds` is cloned and `sort()`ed, and the tag map is a `BTreeMap` (key-ordered) — but the target list is folded in exactly as the request happened to order it. A caller that re-issues the SAME create with the SAME token and the SAME two targets, listed in the other order, therefore produces a different fingerprint and is answered HTTP 400 `ValidationException` ("ClientRequestToken ... was already used with different parameters") instead of the original rule's ARN, even though every parameter of the request is identical and the handler has already normalised the list by address (`rule_targets_from` keys targets on `TargetAddress`, dropping duplicates, so order carries no meaning for the stored rule either).

This is one computation applying two different rules to its own sibling inputs: the member's own documentation states that a replay with the same parameters returns the first request's resource, and the handler already implements that order-insensitivity for `EventTypeIds` and `Tags` in the three lines above. This is distinct from the already-filed CreateNotificationRule/wrong-behavior issue (the ledger key not carrying the region), which concerns WHICH record a token resolves to, not whether an identical request matches the record it already has.

## Evidence

The model's own sentence — smithy_query(query="operation",operation="CreateNotificationRule"), member `ClientRequestToken` (also quoted verbatim in the handler's own module doc, src/ops/create_notification_rule.rs:39-45):
> "A unique, client-generated idempotency token that, when provided in a request, ensures the request cannot be repeated with a changed parameter. If a request with the same parameters is received and a token is included, the request returns information about the initial request that used that token."

The fingerprint, src/ops/create_notification_rule.rs:235-252 — note the explicit sort for one list and the raw iteration order for the other:
```
// The parameters this token is allowed to replay: anything else under the same
// token is the "repeated with a changed parameter" the member documents.
let mut fields = vec![
    input.name.clone(),
    input.resource.clone(),
    input.detail_type.clone(),
    status.clone(),
];
let mut sorted_event_type_ids = input.event_type_ids.clone();
sorted_event_type_ids.sort();
fields.extend(sorted_event_type_ids);
fields.extend(
    targets
        .iter()
        .map(|t| format!("{}={}", t.target_type, t.target_address)),
);
fields.extend(tags.iter().map(|(k, v)| format!("{k}={v}")));
let fingerprint = st::fingerprint(&fields);
```
`targets` is the request's order verbatim — src/state.rs:733-762 (`rule_targets_from`) pushes in submitted order and only collapses REPEATED addresses: "an address appears once (a repeated address is one subscription, with the type last given for it)" (src/state.rs:728-730). `tags` is a `BTreeMap` (src/state.rs:800-818, `rule_tags_from` → `BTreeMap`), so the tag component is key-ordered for the same reason the event ids are sorted.

The refusal that is then answered, src/ops/create_notification_rule.rs:264-270:
```
if let Some(token) = input.client_request_token.as_deref() {
    if let Some(previous) = store.idempotent_request(Self::NAME, token) {
        if previous.fingerprint != fingerprint {
            return Err(invalid(format!(
                "ClientRequestToken {token} was already used with different parameters"
            )));
```

Test coverage: `an_idempotency_token_replays_the_first_rule` (tests/all/notification_rule_lifecycle.rs:665-717) replays the BYTE-IDENTICAL body (`let replay = emulator.ok(token, "/createNotificationRule", &body).await;`, line 680) and only varies `Name` for the changed-parameter row (line 705), so a reordered-but-identical target list is unpinned by any test.

## Steps to Reproduce

1. POST /createNotificationRule with {"Name":"order-1","Resource":"arn:aws:codecommit:us-east-1:123456789012:MyRepo","DetailType":"BASIC","EventTypeIds":["codecommit-repository-pull-request-created","codecommit-repository-comments-on-commits"],"Targets":[{"TargetType":"SNS","TargetAddress":"arn:aws:sns:us-east-1:123456789012:a"},{"TargetType":"SNS","TargetAddress":"arn:aws:sns:us-east-1:123456789012:b"}],"ClientRequestToken":"order-token"} -> 200 with Arn R.
2. Re-POST the same request with ONLY the two EventTypeIds swapped, same token -> 200 with Arn R (the sort at src/ops/create_notification_rule.rs:243-245 makes that order irrelevant).
3. Re-POST the original request with ONLY the two Targets entries swapped (same addresses, same types), same token -> 400 ValidationException "ClientRequestToken order-token was already used with different parameters", instead of the 200 with Arn R the ClientRequestToken member documents for a request with the same parameters.
4. POST /listNotificationRules {}: still one rule, confirming nothing about the stored rule depends on the target order the refused request used.
