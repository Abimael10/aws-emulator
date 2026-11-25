Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: dc6d632c08aad82c0d642cbd411db86477f8bbd7

# A NextToken this listing never issued is served, because the token is unauthenticated hex of a guessable string

_Filed: 2026-11-23T23:53:13Z_
_Operation: ListAppInstances | Class: pagination_

> _Note: this issue was identified against the code base at commit `dc6d632c08aad82c0d642cbd411db86477f8bbd7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated listing in this crate (`ListAppInstances`, `ListAppInstanceUsers`, `ListAppInstanceBots`, `ListAppInstanceUserEndpoints`, `ListAppInstanceAdmins`) states, in a comment above its own token decode, that a `NextToken` the listing never issued is a bad request. The decode cannot make that decision: `encode_page_token` renders the token as the HEX of the plaintext string `"v1\n<scope>\n<cursor>"`, and `decode_page_token` accepts any value whose plaintext carries the compile-time version marker `v1` and a scope string equal to the listing's own. Both halves are known to the caller — the version marker is a constant in the source and the scope is the operation name (plus, for the per-parent listings, the parent ARN the caller just supplied) — so a caller can hand-build a well-formed token for ANY cursor and the listing resumes at it with 200.

So the refusal the handlers describe only catches two of the three cases the crate's own test RULE names: a value that is not hex / does not decode to a token, and a token issued by a DIFFERENT listing (the scope check). A token that was issued by nobody at all — the third, and the one the RULE sentence actually words — is served. Observable: `GET /app-instances?next-token=<hex of "v1\nListAppInstances\n<any string>">` answers 200 with the page after that fabricated cursor (and an empty page with no `NextToken` for a cursor past every key), i.e. a page position the service never offered.

This is not a feature the crate never attempts: it attempts exactly this rule (version marker + scope + the comment stating the refusal), and the workspace already carries the one helper that completes it — `service_common::page_tokens`, whose `PageTokenKey::mint`/verify authenticate the cursor under an in-memory secret, and `service_common` is already a dependency of this crate (used for `chime_tags`, `chime_app_instance_admins`, `chime_identity_names`, `arn`). The two tokens this crate's tests exercise (ones it minted, and ones another listing minted) keep answering exactly as they do today under an authenticated token.

## Evidence

The token is plaintext hex with no authenticator — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/state.rs:1044-1048:
```
pub fn encode_page_token(scope: &str, key: &str) -> String {
    // `\n` separates the fields: a scope is an operation name or an ARN and a key is
    // an ARN, so neither can contain one.
    ::hex::encode(format!("{TOKEN_VERSION}\n{scope}\n{key}"))
}
```
with the marker a source constant — state.rs:64:
```
const TOKEN_VERSION: &str = "v1";
```
and the decode checking only that marker and the scope — state.rs:1052-1062:
```
pub fn decode_page_token(scope: &str, token: &str) -> Option<String> {
    let raw = ::hex::decode(token).ok()?;
    let decoded = String::from_utf8(raw).ok()?;
    let mut fields = decoded.splitn(3, '\n');
    let (Some(TOKEN_VERSION), Some(token_scope), Some(key)) =
        (fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    (token_scope == scope).then(|| key.to_string())
}
```

The rule the handler states over that decode — src/ops/list_app_instances.rs:154-168 (the scope is literally `Self::NAME`, i.e. `"ListAppInstances"`):
```
        // `NextToken` is opaque and scoped to the listing that issued it, so one this
        // listing never issued is a bad request rather than an empty page.
        let after = match input.next_token.as_deref() {
            Some(token) => Some(
                crate::state::decode_page_token(Self::NAME, token).ok_or_else(|| {
                    ListAppInstancesErrorModeled::BadRequestException(BadRequestException {
                        code: Some("BadRequest".to_string()),
                        message: Some(
                            "NextToken is not a token this listing issued".to_string(),
                        ),
```
The same comment and the same guessable scope in the per-parent listings: src/ops/list_app_instance_users.rs:189-204 (`let scope = format!("{}:{}", Self::NAME, instance.arn);`), src/ops/list_app_instance_bots.rs:190-203, src/ops/list_app_instance_user_endpoints.rs:193-210, src/ops/list_app_instance_admins.rs:201-216.

The crate's test states the rule in full but exercises only the two catchable cases — tests/all/app_instance_lifecycle.rs:755-756:
```
/// RULE: a `MaxResults` outside the modelled range, and a `NextToken` this service
/// never issued, are refused with a 4xx client error rather than served.
```
with rows at tests/all/app_instance_lifecycle.rs:793-795 (`"not an encoded token at all"`, `"an encoding of something that is not a token"` = `"68656c6c6f"`) and 820-823 (`"a ListAppInstances token"`, `"another instance's user token"` — the scope check). No row hands the listing a well-formed token of its OWN scope that it never minted; the identical gap is in tests/all/app_instance_admin_listing.rs:419-425.

The completion already exists in a crate this one depends on — services/common/src/page_tokens.rs:13-17:
```
//! So a token carries an AUTHENTICATOR over its own cursor, computed under a secret the
//! issuing store draws from the system's randomness and never reveals ([`PageTokenKey`]):
//! the cursor stays readable by the service that minted it, and no caller can produce a
//! cursor the key authenticates. A token minted by another store — another emulator
//! process, a restarted one — is refused for the same reason.
```
and this crate already reaches into `service_common` for the family's shared rules — src/state.rs:585-588:
```
        ::service_common::chime_tags::addresses_taggable_resource(
            arn,
            names_identity_resource(arn).then(|| self.resource_exists(arn)),
        )
```

## Steps to Reproduce

1. Start the emulator and create three app instances: POST /app-instances with {"Name":"a","ClientRequestToken":"t1"} (repeat for t2, t3).
2. GET /app-instances?max-results=1 -> 200 with one summary and a NextToken the listing issued (hex of "v1\nListAppInstances\n<first ARN>").
3. Build a token the listing never issued: hex-encode the ASCII bytes of "v1\nListAppInstances\narn:aws:chime:us-east-1:000000000000:app-instance/00000000-0000-4000-8000-000000000000" (e.g. `printf 'v1\nListAppInstances\n<arn>' | xxd -p | tr -d '\n'`).
4. GET /app-instances?next-token=<that hex> -> 200, with the page of instances whose ARNs sort after the fabricated cursor, instead of the 400 BadRequest/"NextToken is not a token this listing issued" the handler comment at list_app_instances.rs:154-156 states for a token this listing never issued.
5. Hex-encode "v1\nListAppInstances\n~" (a cursor past every ARN) and pass it as next-token -> 200 with an empty AppInstances list and no NextToken: a page position the service never offered.
6. The same works for the per-parent listings, whose scope the caller also knows: hex-encode "v1\nListAppInstanceUsers:<AppInstanceArn>\n<any string>" and GET /app-instance-users?app-instance-arn=<AppInstanceArn>&next-token=<hex> -> 200.
