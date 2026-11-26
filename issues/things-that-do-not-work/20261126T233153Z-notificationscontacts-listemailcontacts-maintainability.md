Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# ListEmailContacts hand-rolls a page-token authenticator that service_common::page_tokens already provides

_Filed: 2026-11-26T23:31:53Z_
_Operation: ListEmailContacts | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListEmailContacts` carries its own page-token construction: a domain separator, an unkeyed SHA-256 digest truncated to 8 bytes, and a hex rendering, written in `src/ops/list_email_contacts.rs` (and the `sha2` dependency exists in this crate only to compute it). The workspace already has exactly this span as a shared helper — `services/common/src/page_tokens.rs` — whose own module doc calls itself "The ONE way a service tells a continuation token it ISSUED from one a caller built" and which hands back and takes BYTES precisely so a service keeps its own wire rendering. `service-common` is already a direct dependency of this crate (Cargo.toml:32), so no new dependency is needed.

Beyond the duplication, the two copies of the rule do not agree on what the tag IS: the shared helper authenticates the cursor with HMAC-SHA256 under a per-store secret drawn from system randomness ("no caller can produce a cursor the key authenticates"), while the local copy digests a PUBLIC constant together with the ARN, so the local tag is reproducible by anyone who has the construction. The local doc nevertheless claims the tag is what lets the handler "tell a token this operation cut from a value a caller assembled" — a claim only the keyed form supports.

The fix is a swap, not a behaviour change: hold a `PageTokenKey` on the store and build the token bytes with `PageTokenKey::mint` / read them back with `PageTokenKey::cursor_of`, keeping the existing `hex` rendering and the existing `contact_id_from_arn` cursor check. Responses stay opaque hex words and the tag-width the tests derive from the token itself adapts, so every existing test keeps passing unchanged.

## Evidence

Hand-rolled copy — services/.../src/ops/list_email_contacts.rs:100-108:
```
/// The integrity tag of the position `arn`: the leading [`PAGE_TOKEN_TAG_LEN`] bytes of
/// SHA-256 over [`PAGE_TOKEN_DOMAIN`] and the ARN itself.
fn page_token_tag(arn: &str) -> Vec<u8> {
    use ::sha2::Digest;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(PAGE_TOKEN_DOMAIN);
    hasher.update(arn.as_bytes());
    hasher.finalize()[..PAGE_TOKEN_TAG_LEN].to_vec()
}
```
and its claim at list_email_contacts.rs:116-118: "The tag is what makes the token SELF-VERIFYING, so [`decode_page_token`] can tell a token this operation cut from a value a caller assembled". `PAGE_TOKEN_TAG_LEN = 8` (list_email_contacts.rs:93), `PAGE_TOKEN_DOMAIN` a public constant (list_email_contacts.rs:98).

The helper that already does it — services/common/src/page_tokens.rs:3: "//! The ONE way a service tells a continuation token it ISSUED from one a caller built." and page_tokens.rs:19-23: "The authenticator is HMAC-SHA256 from the cryptography library this workspace already uses, not a keyed digest written here; ... How a service RENDERS the authenticated bytes as the string on the wire is the service's own ... so this module hands back bytes and takes bytes." API: `PageTokenKey::mint(&self, cursor: &str) -> Vec<u8>` (page_tokens.rs:67) and `PageTokenKey::cursor_of(&self, token: &[u8]) -> Option<String>` (page_tokens.rs:81), the latter documented as answering "`None` for any value THIS key did not mint".

Already a dependency — services/service-com-amazonaws-notificationscontacts-notificationscontacts/Cargo.toml:32: `service-common = { path = "../common" }`; and Cargo.toml:26-28 shows `sha2` is pulled in solely "Computes the integrity tag that `nextToken` carries".

Tests that must stay unchanged (they derive the tag width from the token rather than restating it, so a wider authenticator still passes): tests/all/list_email_contacts_page_token.rs — `only_a_next_token_this_listing_issued_resumes_the_walk` (line 141: `let tag_len = bytes.len().checked_sub(last_reported.len())`), `every_issued_token_of_a_whole_walk_is_accepted_at_every_page_size`, `a_token_this_listing_issued_resumes_even_after_its_contact_is_deleted`; tests/all/email_contacts.rs `the_list_pages_over_every_stored_contact_exactly_once`. The hex form the suite relies on (line 137: `hex::decode(&issued).expect("an issued token is hex")`) is kept by rendering the helper's bytes with `hex` exactly as today.

## Steps to Reproduce

1. Open services/service-com-amazonaws-notificationscontacts-notificationscontacts/src/ops/list_email_contacts.rs and read lines 88-150: the domain separator, `page_token_tag`, `encode_page_token` and `issued_position` are a complete issued-token authenticator written in this crate.
2. Open services/common/src/page_tokens.rs and read lines 1-124: the same rule, as `PageTokenKey::mint` / `PageTokenKey::cursor_of`, declared there as the one way a service tells its own token from a caller's, rendering left to the service.
3. Confirm the helper is already reachable: services/.../Cargo.toml:32 lists `service-common` as a direct dependency (the crate already calls `service_common::validation` and `service_common::no_content_responses` from src/state.rs and src/service.rs).
4. Note the two copies disagree on the tag's nature — unkeyed digest of a public constant locally, HMAC under a per-store secret in the helper — so the local doc's self-verifying claim holds only for the helper's version.
