Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 202bd31436bfab9fa41ffa446540b750818300e3

# A cursor minted under a long regionCode exceeds nextToken's modelled @length(4, 10000), so the page it names can never be fetched

_Filed: 2026-11-23T05:16:21Z_
_Operation: ListRecommendationResources | Class: pagination_

> _Note: this issue was identified against the code base at commit `202bd31436bfab9fa41ffa446540b750818300e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated read of this service mints its continuation token by base64-encoding an HMAC tag followed by the cursor PLAINTEXT, and that plaintext is the read's cursor scope, which embeds each filter value VERBATIM. `regionCode` is the one filter of the resource lists that carries no modelled length bound (`{"target": "String", "httpQuery": "regionCode"}`), so the token this handler emits grows with the request's own `regionCode` — roughly 4/3 of (32 + scope + key) bytes.

For a `regionCode` of about 7.5 KB (a value the model allows, and the kind of free-form value the crate's own suite already sends through this member), a truncated page returns a `nextToken` LONGER than the 10000 characters the model declares for that member, in the output and in the input alike. Sending that token back is then refused by the centralized request walk on the modelled `@length` before `handle` ever runs: the service hands out a cursor it will itself reject as invalid input, so the second page of that filtered sequence is unreachable and the crate's own paging invariant — "a caller that follows the token until it is absent sees every entry exactly once" — does not hold.

Two further things in the crate rest on the bound the emitted token breaks: `crate::service::validation::page_token_refusal` documents that the sentence it echoes is bounded because "the model's `@length(4, 10000)` bounds [the token] before any handler runs", and the refusal a caller actually receives for a token THIS service issued is the framework's length clause rather than this service's page-token refusal.

`ListOrganizationRecommendationResources` builds its scope from the same unbounded `regionCode` (`src/ops/list_organization_recommendation_resources.rs:197-212`) and mints its cursors through the same helper, so it carries the same defect; no comment, doc or test in the crate bounds what the scope contributes to a token.

## Evidence

Model (smithy_query query="operation" operation="ListRecommendationResources"):
- input  `"nextToken": {"target": "String", "httpQuery": "nextToken", "length": {"min": 4, "max": 10000}}`
- output `"nextToken": {"target": "String", "length": {"min": 4, "max": 10000}}`
- input  `"regionCode": {"target": "String", "httpQuery": "regionCode"}`  (no `length`, no `pattern`)

The request value reaches the scope verbatim — services/.../src/ops/list_recommendation_resources.rs:211-223:
```
        let filters = crate::state::ResourceFilters {
            status: input.status.as_deref(),
            exclusion_status: input.exclusion_status.as_deref(),
            region_code: input.region_code.as_deref(),
        };
        ...
        let scope = crate::state::list_scope(
            &format!("resources/{}", input.recommendation_identifier),
            &filters.scope(),
        );
```
src/state.rs:1769-1780 (`list_scope`) puts the whole value in the document:
```
pub fn list_scope(list: &str, filters: &[(&str, Option<&str>)]) -> String {
    ... ::serde_json::Value::String(value.to_string()),
```
src/state.rs:1806-1810 (`encode_token`) carries that scope INSIDE the token:
```
pub fn encode_token(token_key: &PageTokenKey, scope: &str, last_key: &str) -> String {
    ::aws_smithy_types::base64::encode(
        token_key.mint(&format!("{scope}{TOKEN_SEPARATOR}{last_key}")),
    )
}
```
services/common/src/page_tokens.rs:67-72 — the plaintext travels whole, so token length tracks scope length:
```
    pub fn mint(&self, cursor: &str) -> Vec<u8> {
        let mut token = Vec::with_capacity(AUTHENTICATOR_BYTES + cursor.len());
        token.extend_from_slice(authenticator.as_ref());
        token.extend_from_slice(cursor.as_bytes());
```
The invariant the over-long token breaks — src/state.rs:1840-1847 (`paginate` doc):
"`next_token` resumes strictly after the key it names, and the returned token is present only when entries remain — so a caller that follows the token until it is absent sees every entry exactly once."
And the bound the crate relies on — src/service/validation.rs:302-304:
"the echo is the request's own `nextToken`, whose size the model's `@length(4, 10000)` bounds before any handler runs, so the sentence is bounded whatever the request holds."

Reachability of a free-form `regionCode` is already exercised by the crate's own suite — tests/all/cursor_scope_encoding.rs:207-213 sends a `regionCode` carrying the scope and cursor separators and asserts the read is served 200:
```
    let plain = format!("regionCode={REGION}&affectedAccountId={own}");
    let spliced = format!("regionCode={}", forged(&format!("{REGION}{SCOPE_SEPARATOR}affectedAccountId={own}")));
```
Test coverage of token length today: only the INPUT direction is pinned (tests/all/rejected_value_echo.rs:187-191 "a nextToken one byte past @length(4, 10000)", tests/all/page_token_refusal_sentence.rs:364 "`nextToken` is `@length(4, 10000)`"); no test asserts anything about the length of a token the service MINTS.

## Steps to Reproduce

1. Boot the emulator with EMULATOR_TRUSTEDADVISOR_SUPPORT_PLAN=business (or mount it in-process on SupportPlan::Business) so the support-level gate admits the request.
2. Arrange two resources of one recommendation in one Region (the seeded `low-utilization-ec2-instances` recommendation has three), so a page of size 1 is truncated.
3. GET /v1/recommendations/arn:aws:trustedadvisor::<account>:recommendation/low-utilization-ec2-instances-recommendation/resources?maxResults=1&regionCode=<7500 'a' characters> — the model puts no length on regionCode, so the request is well formed and is served 200.
4. Read `nextToken` from the response body and measure it: it is longer than 10000 characters, outside the `@length(4, 10000)` the model declares for that output member.
5. Re-send the same read with that token appended (&nextToken=<token>): the centralized request walk refuses it 400 ValidationException on the modelled nextToken length before the handler runs, so the second page of this filtered sequence can never be fetched even though the service itself issued the cursor.
6. Repeat against GET /v1/organization-recommendations/{id}/resources?maxResults=1&regionCode=<same value> for the same result.
