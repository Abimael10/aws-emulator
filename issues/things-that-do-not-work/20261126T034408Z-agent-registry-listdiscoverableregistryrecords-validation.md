Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 41be3a2659be45277be7049b64517ef95e5ebd6f

# ListDiscoverableRegistryRecords honours a nextToken the caller assembled itself, though its own doc and test say such a token is refused

_Filed: 2026-11-26T03:44:08Z_
_Operation: ListDiscoverableRegistryRecords | Class: validation_

> _Note: this issue was identified against the code base at commit `41be3a2659be45277be7049b64517ef95e5ebd6f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`decode_next_token` is documented to answer `None` for any token this service did not mint, and the refusal it feeds says the token "is not a pagination token this service issued for this registry". The implementation checks only the token's SHAPE: hex-decodable, carrying the `v1:` prefix, and naming the registry of the request. Those three properties are all public knowledge (the prefix is a crate constant, the registry id is in the request, the record-id form is in every response), so any caller can hex-encode `v1:<registryId>:<anyRecordId>` and the handler accepts it as a resume position the service never offered, returning 200 with a page that starts strictly after a record id of the caller's choosing. The documented rule ("`None` when the token was not issued by `encode_next_token`") and the rule the test asserts by its very name (`a_next_token_this_service_did_not_issue_is_refused`) are therefore not implemented: the test only drives values that fail the shape check ("not-a-token", "rec000000001", "ffff"), so the forged-but-well-shaped case is both unimplemented and untested. The crate already depends on `service-common`, whose `page_tokens` module exists for exactly this distinction and says so in its own module doc.

## Evidence

The documented rule, services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs:351-353:
```
/// The record id a `nextToken` resumes after, or `None` when the token was not
/// issued by [`encode_next_token`] for `registry_id`.
```
and :345-346: "The token encodes a position in one registry's record-id order, and the registry it was issued for, so it cannot be replayed against another registry."

The code under that doc, same file :356-360 — it authenticates nothing, it only parses:
```
fn decode_next_token(token: &str, registry_id: &str) -> Option<String> {
    let decoded = String::from_utf8(hex::decode(token).ok()?).ok()?;
    let (named_registry, record_id) = decoded.strip_prefix(TOKEN_PREFIX)?.split_once(':')?;
    (named_registry == registry_id).then(|| record_id.to_string())
}
```
with `const TOKEN_PREFIX: &str = "v1:";` (:332) and the mint being a plain hex of that same string (:347-349: `hex::encode(format!("{TOKEN_PREFIX}{registry_id}:{last_record_id}"))`).

The refusal the handler words for the case it cannot actually detect, same file :262-269:
```
crate::state::cannot_parse(
    "nextToken is not a pagination token this service issued for this registry.",
)
```

The test that claims the rule, services/service-com-amazonaws-agentregistry-agentregistry/tests/all/discovery_read_path.rs:1341-1343:
```
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let router = seeded_router();
    for token in ["not-a-token", "rec000000001", "ffff"] {
```
— every row is refused by the hex/prefix parse alone; no row carries a value the caller BUILT in the minted shape.

The facility this crate already has available (service-common is a dependency: services/service-com-amazonaws-agentregistry-agentregistry/Cargo.toml:38 `service-common = { path = "../common" }`, already used for `arn`, `iam_authorization`, `date_time_json`), services/common/src/page_tokens.rs:6-11:
```
//! The position the token names travels INSIDE it ... so a read that only checks the
//! token's SHAPE honours any value of that shape, and a caller can name a position the
//! service never offered: the first page of a report it was paging past, a position far
//! in the future, a position under another filter. What the services' APIs document for
//! such a value is a refusal (an invalid-token error), not a page.
```

## Steps to Reproduce

1. Build the seeded router of tests/all/discovery_read_path.rs (`seeded_router()`), which holds registry `abc123def456` with discoverable records rec000000001, rec000000002, rec000000004.
2. Hex-encode the string `v1:abc123def456:rec000000001` yourself — no call to the service — giving `76313a6162633132336465663435363a726563303030303030303031`.
3. POST /registries/abc123def456/discoverable-records-list with body {"nextToken": "76313a6162633132336465663435363a726563303030303030303031"}.
4. Observe HTTP 200 and a page resuming after rec000000001, although the service issued no such token; the handler doc at list_discoverable_registry_records.rs:351-353 and the test name at discovery_read_path.rs:1341 both state this token is refused with the 400 ValidationException of line 262-269.
