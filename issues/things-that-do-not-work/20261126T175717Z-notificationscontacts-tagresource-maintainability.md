Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e7528d3e52a8616792d558e251041b8f9fac92e1

# Inline frame_empty_body duplicates service_common::no_content_responses::without_content, which names this very span as a copy to migrate

_Filed: 2026-11-26T17:57:17Z_
_Operation: TagResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e7528d3e52a8616792d558e251041b8f9fac92e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate inlines the 204 no-content framing rule in its own service entry: a `NO_CONTENT_STATUS`/`NO_CONTENT_LENGTH` pair plus `fn frame_empty_body`, called from both `serialize_output` and `serialize_error`. `service-common` — already a dependency of this crate — owns that exact rule as `no_content_responses::without_content`, and its module documentation names `frame_empty_body` as one of the inlined copies to be migrated and says nothing should inline the rule again.

The two copies are the same logic (status comparison against 204, empty `SdkBody`, `content-length: 0`), so they can drift: a correction made in the shared home — including anything that comes out of the open content-length issue — would not reach this crate, and vice versa. The shared function is behaviourally identical for every response this service produces (it additionally removes a stale `content-length` on an insert failure that cannot happen today), so swapping `frame_empty_body(&mut response)` for `Ok(no_content_responses::without_content(response))` leaves every status, header and body byte as it is.

## Evidence

The inlined copy — `src/service.rs:87-109`:
```
/// The status whose answer carries NO content, whatever a renderer wrote for it, and the
/// length such an answer states. See the module docs.
const NO_CONTENT_STATUS: u16 = 204;
const NO_CONTENT_LENGTH: &str = "0";
...
fn frame_empty_body(response: &mut WireResponse) {
    if response.status().as_u16() != NO_CONTENT_STATUS {
        return;
    }
    *response.body_mut() = aws_smithy_types::body::SdkBody::empty();
    let _ = response
        .headers_mut()
        .try_insert("content-length", NO_CONTENT_LENGTH);
}
```
called at `src/service.rs:175` (`serialize_output`) and `src/service.rs:193` (`serialize_error`).

The shared home, which names this span — `services/common/src/no_content_responses.rs:35-45`:
```
//! # Services still carrying their own copy
//!
//! This correction predates this module and is inlined in the service crates that needed it
//! before there was anywhere shared to put it. Search for the SPAN rather than for a list
//! that would drift: the body is a status comparison against 204 followed by an empty
//! `SdkBody` and a `content-length` of `0`, and it appears both as a function of its own
//! (spelled `frame_empty_body` in some crates and `frame_empty_answer` in others, each beside
//! a `NO_CONTENT_STATUS` of its own) ... Those copies are to be MIGRATED onto this module ...
//! Nothing new should inline the rule again — call this module.
```
and the replacement itself — `services/common/src/no_content_responses.rs:62-79`:
```
pub fn without_content(mut resp: RtResponse) -> RtResponse {
    if resp.status().as_u16() != NO_CONTENT_STATUS {
        return resp;
    }
    *resp.body_mut() = SdkBody::empty();
    if resp
        .headers_mut()
        .try_insert("content-length", NO_CONTENT_LENGTH)
        .is_err()
    {
        resp.headers_mut().remove("content-length");
    }
    resp
}
```

The crate already depends on it — `Cargo.toml:32`:
```
service-common = { path = "../common" }
```

Test coverage that pins the framing and must stay green unchanged — `tests/all/tag_mutation_success_status.rs:7-9`:
```
//! RULE: a successful tag mutation answers 204 No Content with zero body bytes, stating
//! the length it has
```
with the body and `content-length` assertions at `tests/all/tag_mutation_success_status.rs:162-176`; `tests/all/resource_tag_writes.rs` exercises the same 204 answers for both tag mutations.

## Steps to Reproduce

1. Read `src/service.rs:87-109` — the local `NO_CONTENT_STATUS`, `NO_CONTENT_LENGTH` and `fn frame_empty_body`.
2. Read `services/common/src/no_content_responses.rs:35-45` and `:62-79` — the same rule, in the module that names `frame_empty_body` as a copy to be migrated onto it, and says nothing should inline the rule again.
3. Note `Cargo.toml:32`: `service-common` is already a dependency, so the replacement is reachable without adding one.
4. Observe that the two bodies are the same logic, so a change to one does not reach the other; replacing the local function with `no_content_responses::without_content` leaves `tests/all/tag_mutation_success_status.rs` and `tests/all/resource_tag_writes.rs` passing with byte-identical responses.
