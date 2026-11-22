Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: f6f2bb42c89747f6e79918c6c5742560f32c8949

# A request whose NextToken AND PageSize are both unusable is refused with the sentence-less PageSize body, dropping the 'nextToken' sentence the crate has for it

_Filed: 2026-11-20T14:06:58Z_
_Operation: ListConfigurationSets | Class: error-response_

> _Note: this issue was identified against the code base at commit `f6f2bb42c89747f6e79918c6c5742560f32c8949`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListConfigurationSets` is the one operation of this service with two parameters its OWN validation can reject: `PageSize` (a value it cannot read as a page bound — refused with NO detail text at all, by design) and `NextToken` (a token this service did not mint — refused with the sentence `The following parameter is null, empty or invalid: ['nextToken']`).

The handler evaluates `PageSize` FIRST and returns on it, so `NextToken` is never looked at. A request carrying an unusable value for BOTH (e.g. `?NextToken=not-a-token&PageSize=0`) is answered with the sentence-less refusal: `BadRequestException`, HTTP 400, `message: null` — a body that names no parameter at all, even though the crate has a sentence for the unusable `NextToken` and the input binds `NextToken` before `PageSize`.

That contradicts the rule this crate states for exactly this class of refusal in `ops/types/refusal_wording.rs`: one refusal names EVERY parameter the refusing layer cannot use, in the order the input binds them, "not merely the first one looked at" — and the `invalid_parameters` helper is built around a SLICE of member names so several can be named in one sentence. Both parameters here are rejected by the same layer (this handler's own validation), so the rule applies, yet the refusal reports the one class that carries no sentence and silently discards the other. The caller is told only "the input you provided is invalid" with a null detail member, which per the crate's own reading of that null means "this error class carries no sentence" — a claim that is false for the `NextToken` half of the request.

## Evidence

The rule, stated by the wording module — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/refusal_wording.rs:22-25:
```
//! [`unusable_required_labels`] is which of the two a request's URI-bound parameters get,
//! so the layer split is decided in one place rather than per handler. It is handed ALL of
//! a request's `@required` labels before it words anything, because a sentence names every
//! parameter the request failed on, not merely the first one looked at.
```
and refusal_wording.rs:66-70:
```
/// A request is refused ONCE, so the one sentence names EVERY label unusable in the layer
/// that refused it: ... Either way
/// the names are listed in the order given, which is the order the input binds them.
```
The helper is built to name SEVERAL — refusal_wording.rs:39-53:
```
/// `members` are MODELLED member names; each is rendered through [`parameter_name`] and
/// single-quoted, in the order given.
pub fn invalid_parameters(members: &[&str]) -> String {
    let named: Vec<String> = members
        .iter()
        .map(|member| format!("'{}'", parameter_name(member)))
        .collect();
    format!(
        "The following parameter is null, empty or invalid: [{}]",
        named.join(", ")
    )
}
```

The code that breaks it — src/ops/list_configuration_sets.rs:172-193 (PageSize is looked at first and `?` returns immediately; the NextToken branch below is never reached):
```
        // `PageSize` is modelled as a string, so a value that is not a positive
        // decimal count is a client error, not a count to clamp. ...
        let page_size = match input.page_size.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_PAGE_SIZE,
            Some(raw) => parse_page_size(raw)?,
        };

        // `NextToken` is opaque to the caller but it is OURS: one this service did not
        // mint cannot name a position in the listing, so it is a parameter this
        // service's validation cannot use — and one it names in the refusal ...
        let resume_after = match input.next_token.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(decode_token(raw).ok_or_else(|| {
                ... refusal_wording::invalid_parameters(&["NextToken"]) ...
```
and the refusal it returns instead — list_configuration_sets.rs:251-257:
```
fn parse_page_size(raw: &str) -> Result<usize, ListConfigurationSetsErrorModeled> {
    match raw.parse::<usize>() {
        Ok(size) if size > 0 => Ok(size.min(MAX_PAGE_SIZE)),
        _ => Err(ListConfigurationSetsErrorModeled::BadRequestException(
            BadRequestException { message: None },
        )),
```
What that null is documented to MEAN — src/service.rs:50-57: "The member is reported for EVERY modelled failure, even the one class this service produces no sentence for (`ListConfigurationSets`' unreadable `PageSize`), where it is written as JSON `null`: ... an omitted member would state \"the sentence is empty\" where this service means \"this class carries no sentence\"."

Test coverage: tests/all/parameter_refusal_wording.rs:244-286 sends exactly ONE unusable query parameter per row — the two `NextToken` rows expect `INVALID_NEXT_TOKEN` (:180) and the four `PageSize` rows expect `message: None` — and tests/all/refusal_detail_member.rs:13-20 covers "Seven `PageSize` forms" alone. No row of any suite sends an unusable `NextToken` and an unusable `PageSize` in the same request, so nothing pins the PageSize-wins behaviour.

## Steps to Reproduce

1. Boot the emulator (the entitlement state is irrelevant: ListConfigurationSets is account-scoped and is not gated, src/ops/list_configuration_sets.rs:204-206).
2. Send `GET /v1/sms-voice/configuration-sets?NextToken=not-a-token` — observe HTTP 400 `BadRequestException` with `"message": "The following parameter is null, empty or invalid: ['nextToken']"` (the sentence the crate has for this parameter).
3. Send `GET /v1/sms-voice/configuration-sets?NextToken=not-a-token&PageSize=0` — observe HTTP 400 `BadRequestException` with `"message": null`: the refusal names no parameter at all, and the 'nextToken' sentence is dropped even though `NextToken` is bound first and is still unusable.
4. Compare with the crate's rule (src/ops/types/refusal_wording.rs:22-25, :66-70): one refusal names every parameter the refusing layer cannot use, in the order the input binds them, "not merely the first one looked at".
