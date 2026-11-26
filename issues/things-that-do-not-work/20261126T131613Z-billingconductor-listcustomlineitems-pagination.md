Issue-Severity: HIGH
Issue-Gap-Class: pagination
Issue-Commit: c3aab41312baa47e690c4afe8f7c7d51a974747b

# A NextToken carries its scope and cursor in the clear behind an UNKEYED digest, so a caller can mint one the service never issued and be served an arbitrary cut point

_Filed: 2026-11-26T13:16:13Z_
_Operation: ListCustomLineItems | Class: pagination_

> _Note: this issue was identified against the code base at commit `c3aab41312baa47e690c4afe8f7c7d51a974747b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated read of this service mints and reads its `NextToken` through `crate::state::encode_next_token` / `decode_next_token` (`ListCustomLineItems` reaches them via `crate::state::paginate` at `src/ops/list_custom_line_items.rs:313`; so do the nine other paginated reads).

The token is `hex(tag || scope || 0x1f || cursor)`, where `scope` is the operation name (plus the parent ARN and/or canonical billing period) and `cursor` is the store key the page resumes at (an ARN, an account id, a fixed-width version ordinal, a composite report key). `tag` is the first 8 bytes of a plain `SHA-256` of that same payload — computed with no secret and over bytes the caller already knows.

Two consequences, both of which contradict rules this crate states about its own tokens:

1. The cursor does NOT travel opaquely. `::hex::encode` is a transport encoding, not concealment: hex-decoding an issued token yields the operation name, the billing period and the store key verbatim. The module's own doc says the opposite, and the suite that pins the rule asserts opacity only by checking the token string does not *contain* `"arn:"` — which hex-encoding defeats while the ARN is still right there.

2. `decode_next_token` cannot tell a token this service minted from one a caller assembled, because reproducing the tag requires nothing the caller lacks. A caller can build the payload for ANY cursor, hash it, hex it, and hand it in; `decode_next_token` re-mints the same bytes and accepts it. The listing then resumes from a cut point the service never offered (silently dropping every element sorting below it, or skipping straight to an arbitrary key), where `invalid_next_token`'s own doc says such a value must be refused with the modelled 400 `ValidationException` / `Reason` `CANNOT_PARSE` rather than "a listing to serve from an arbitrary cut point". A documented error is turned into a 200.

The repository already states the correct rule and already carries the machinery for it in `services/common/src/page_tokens.rs`, which exists precisely because "a read that only checks the token's SHAPE honours any value of that shape"; this crate instead hand-rolled an unkeyed digest (adding `hex` and `sha2` as direct dependencies for it, per `Cargo.toml:24-27`).

Note this is the same rule a FIXED issue already defended one step further out (`20261029T140633Z-billingconductor-listaccountassociations-pagination.md`: "a NextToken this service never issued is served as a cut point") — that fix closed the re-cased-hex spelling; the unkeyed tag leaves the general case open.

## Evidence

The minting, with the cursor placed verbatim in the payload — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state/pagination.rs:58-63`:

```rust
pub fn encode_next_token(scope: &str, cursor: &str) -> String {
    let payload = format!("{scope}{PAGE_TOKEN_SEPARATOR}{cursor}");
    let mut encoded = page_token_tag(payload.as_bytes());
    encoded.extend_from_slice(payload.as_bytes());
    ::hex::encode(encoded)
}
```

The rule the doc directly above it states — `src/state/pagination.rs:46-57`:

> "A pagination token is OPAQUE — the only thing a caller may do with one is hand it back, unchanged, to the listing that returned it — **so the cursor does not travel in the clear as the store key it is**, and the scope travels with it"
> "The tag is what makes a token that was not minted whole DECIDABLE rather than read as some other cut point: **a value that does not agree with its own tag was not produced here**, so it is refused instead of resuming the listing somewhere the caller never asked for."

The tag, which takes no secret — `src/state/pagination.rs:108-115`:

```rust
/// An integrity check over a value this service produced and the caller hands back
/// unchanged, not a secret: it tells a token that was minted whole from one that was not.
pub(super) fn page_token_tag(payload: &[u8]) -> Vec<u8> {
    use ::sha2::Digest;
    ::sha2::Sha256::digest(payload)[..PAGE_TOKEN_TAG_BYTES].to_vec()
}
```

The refusal that is bypassed — `src/state/pagination.rs:200-205` (doc of `invalid_next_token`):

> "A token the service did not issue (or one a client corrupted) carries no page boundary at all, so it is a fault in the INPUT **rather than a listing to serve from an arbitrary cut point**: `ValidationExceptionReason` enumerates `CANNOT_PARSE` for exactly a value the service cannot read"

...and the acceptance path that re-mints instead of authenticating — `src/state/pagination.rs:101-105`:

```rust
    let (minted_for, cursor) = decoded.split_once(PAGE_TOKEN_SEPARATOR)?;
    if minted_for != scope {
        return None;
    }
    (encode_next_token(scope, cursor) == token).then(|| cursor.to_string())
```

The crate's own test states the opacity rule but can only check it as a substring — `tests/all/pagination_token_opacity.rs:639-641` and `:653-657`:

> "/// THE RULE: a token a listing minted is opaque — **it carries no ARN, no account id and no index a caller could read or edit** — and handing it back resumes exactly where the page stopped"

```rust
        assert!(
            !token.contains("arn:") && token != world.stored_arn && token != world.stored_account,
            "case {}: a token must not be a store key, got {token:?}",
```

and `tests/all/pagination_token_opacity.rs:6-13` lists the classes it refuses — "a word, an empty value, a store key, a value minted by another listing or for another parent resource, a minted value one character of which changed" — none of which is a correctly-tagged payload a caller computed, so no existing test pins the current behaviour as intended.

The rule, already written in this repository — `services/common/src/page_tokens.rs:6-17`:

> "The position the token names travels INSIDE it — there is no page kept on the side to look up — so **a read that only checks the token's SHAPE honours any value of that shape, and a caller can name a position the service never offered** ... What the services' APIs document for such a value is a refusal (an invalid-token error), not a page. So a token carries an AUTHENTICATOR over its own cursor, computed under a secret the issuing store draws from the system's randomness and never reveals ... **no caller can produce a cursor the key authenticates**."

This crate took the dependencies for the hand-rolled version instead — `Cargo.toml:24-27`:

```toml
# Hex codec for the opaque pagination tokens the paginated reads mint.
hex = { workspace = true }
# SHA-256, for the integrity tag those tokens carry.
sha2 = { workspace = true }
```

## Steps to Reproduce

1. As the payer account, create three custom line items in one billing group (`POST /create-custom-line-item`).
2. Call `POST /list-custom-line-items` with `{"MaxResults": 1}` and keep the `NextToken` from the response.
3. Hex-decode the token: bytes 0..8 are the digest, and the remainder is the literal payload `ListCustomLineItems` + 0x1E + `<YYYY-MM>` + 0x1F + `arn:aws:billingconductor::<account>:customlineitem/<id>` — the scope and the store key in the clear, contradicting `pagination.rs:46-48` ("the cursor does not travel in the clear as the store key it is") and the opacity rule `tests/all/pagination_token_opacity.rs:639-641` states.
4. Now forge one: pick any cursor of your own (for example the ARN of the LAST item, so the first two are silently skipped), build `payload = "ListCustomLineItems" + "\u{1e}" + "<the same YYYY-MM the listing used>" + "\u{1f}" + <cursor>`, take `tag = SHA-256(payload)[..8]`, and send `NextToken = hex(tag || payload)`.
5. Call `POST /list-custom-line-items` with that `NextToken`. Expected: the modelled 400 `ValidationException` with `Reason` `CANNOT_PARSE` and `Fields` naming `NextToken`, which `invalid_next_token` (`pagination.rs:200-205`) documents for a token the service did not issue. Actual: HTTP 200, the listing resumed at the caller-chosen cut point — `decode_next_token` re-mints the same bytes at `pagination.rs:105` and accepts them, because the tag at `pagination.rs:112-115` is computed with no secret over bytes the caller already knows.
6. Repeat against any other paginated read (`/list-billing-groups`, `/list-pricing-rules`, `/list-resources-associated-to-custom-line-item`, `/get-billing-group-cost-report`, ...): all ten share `encode_next_token` / `decode_next_token`, so all ten accept a forged cursor.
