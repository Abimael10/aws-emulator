Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 39d351fb1e8d061b1c0616e3b1effc9c5ba40bfd

# The tag-refusal assertion rule (status + wire code + message) is written five times across this crate's tag suites

_Filed: 2026-11-17T10:16:31Z_
_Operation: UntagResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `39d351fb1e8d061b1c0616e3b1effc9c5ba40bfd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate's tag-surface suites each carry a private copy of the SAME verification rule: "a refusal of the tagging surface is its HTTP status, its wire error code and its message, and it must not name a code the operation does not model". Three of the copies are byte-for-byte identical three-statement helpers (`assert_refused(reply, case, (status, code, message))`); two more are the same helper plus one extra assertion, and those two also each declare their own constant for the single undeclared code (`UNMODELLED_CODE` and `UNDECLARED_CODE`, both `"ValidationException"`).

The home for the rule already exists and is already the one place the two halves it is built from live: `crate::harness::Reply::error` (status + code) and `crate::harness::Reply::message`. The crate has already consolidated exactly this shape of duplication once — the `ListTagsForResource` reply->tag-set projection was moved onto `harness::reported_tags` / `harness::reported_tags_of` and pinned by `tests/all/shared_tag_projection.rs`, whose own header records that "The suites that used to carry a private copy of that projection now read it from here". The refusal assertion is the same rule in the same suites and has not been collapsed, so the five copies can drift: a sixth tag suite, or a change to how a refusal is read off a reply (the `message` member, the `x-amzn-errortype` suffix match), has to be made in five places or the copies disagree about what "this reply is that refusal" means.

The fix is confined to the suites: give `Reply` one method that asserts a `(status, code, message)` refusal (and one that also asserts the undeclared code is absent), and have the five call sites use it. No handler, no response and no assertion outcome changes — every existing test keeps asserting exactly what it asserts today.

## Evidence

Copy 1 — `tests/all/resource_tags.rs:68-78`:
```
/// Assert `reply` is the HTTP status, wire error code and message of `refusal`.
fn assert_refused(reply: &Reply, case: &str, refusal: (u16, &str, &str)) {
    let (status, code, message) = refusal;
    reply.error(case, status, code);
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected message {message:?}, got body {:?}",
        reply.body
    );
}
```

Copy 2 — `tests/all/tag_not_found_status.rs:78-88`:
```
/// Assert `reply` is the HTTP status, wire error code and message of `refusal`.
fn assert_refused(reply: &Reply, case: &str, refusal: (u16, &str, &str)) {
    let (status, code, message) = refusal;
    reply.error(case, status, code);
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected message {message:?}, got {:?}",
        reply.message()
    );
}
```

Copy 3 — `tests/all/tag_plane_authorization.rs:166-177`:
```
/// Assert `reply` is the refusal `expected` states: its HTTP status, its wire error code and
/// its message.
fn assert_refused(reply: &Reply, case: &str, expected: (u16, &str, &str)) {
    let (status, code, message) = expected;
    reply.error(case, status, code);
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected message {message:?}, got {:?}",
        reply.message()
    );
}
```

Copy 4 (same rule plus the undeclared-code assertion) — `tests/all/tag_api_refusal_code.rs:175-196`:
```
fn assert_refusal(
    reply: &Reply,
    case: &str,
    operation: &str,
    expected: (u16, &str, &str),
) {
    let (status, code, message) = expected;
    reply.error(case, status, code);
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected {message:?}, got {:?}",
        reply.message()
    );
    let answered = reply.error_type.clone().unwrap_or_default();
    assert!(
        operation != "UntagResource" || !answered.contains(UNDECLARED_CODE),
```

Copy 5 (same again) — `tests/all/tag_selection_limit.rs:185-198`:
```
fn assert_tag_limit(reply: &Reply, case: &str) {
    let (status, code, message) = TAG_LIMIT;
    reply.error(case, status, code);
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the limit stated as {message:?}, got {:?}",
        reply.message()
    );
    let answered = reply.error_type.clone().unwrap_or_default();
    assert!(
        !answered.contains(UNMODELLED_CODE),
```
and the one code, named twice: `tests/all/tag_selection_limit.rs:58` `const UNMODELLED_CODE: &str = "ValidationException";` beside `tests/all/tag_api_refusal_code.rs:61` `const UNDECLARED_CODE: &str = "ValidationException";`.

The existing shared home the copies are built on — `tests/all/harness.rs:227-245`:
```
    /// Assert this reply is the client error `code` with HTTP `status`.
    pub fn error(&self, case: &str, status: u16, code: &str) {
...
    /// The `message` member of an error body, or `""` when there is none.
    pub fn message(&self) -> &str {
```

The precedent in this same crate, `tests/all/shared_tag_projection.rs:10-11`:
```
//! (`harness::reported_tags` over a reply, `harness::reported_tags_of` over an ARN) yields
//! the same set on both forms. The suites that used to carry a private copy of that
//! projection now read it from here
```

Test coverage that must stay unchanged: `tests/all/resource_tags.rs:316` `an_arn_addressing_no_resource_is_refused_by_both_tag_operations` and `:386` `a_write_past_the_tag_maximum_is_refused_and_stores_nothing`; `tests/all/tag_not_found_status.rs:143` `an_unregistered_resource_arn_is_reported_absent_with_http_404` and `:209` `a_bad_request_on_the_tag_apis_keeps_http_400`; `tests/all/tag_plane_authorization.rs:191` `a_resource_arn_in_another_account_is_denied_by_every_tag_operation`, `:249` and `:292`; `tests/all/tag_api_refusal_code.rs:202`, `:249`, `:276`, `:298`; `tests/all/tag_selection_limit.rs:210` and `:267` — all of them call one of the five copies.

## Steps to Reproduce

1. Open tests/all/resource_tags.rs:68-78, tests/all/tag_not_found_status.rs:78-88 and tests/all/tag_plane_authorization.rs:166-177 side by side and observe the three bodies are the same three statements over the same `(u16, &str, &str)` tuple.
2. Open tests/all/tag_api_refusal_code.rs:175-196 and tests/all/tag_selection_limit.rs:185-198 and observe the same three statements again, each followed by the same "the surface must not answer ValidationException" assertion over a locally declared constant (UNDECLARED_CODE / UNMODELLED_CODE, both "ValidationException").
3. Note that the two halves each copy is built from already live in one place: harness.rs:227-245 (`Reply::error`, `Reply::message`), which is where tests/all/shared_tag_projection.rs:10-11 records the crate already moved the equivalent tag-set projection.
4. Change how a refusal is read off a reply (e.g. the body member the message is taken from) and observe five call sites must be edited in step for the suites to keep agreeing.
