Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 91f93a1bde5a801c679c246d110e0d3f16ec179b

# UntagResource accepts a reserved aws: tag key that its two sibling tag paths refuse, although both members carry the same TagKey shape

_Filed: 2026-11-26T03:42:32Z_
_Operation: UntagResource | Class: validation_

> _Note: this issue was identified against the code base at commit `91f93a1bde5a801c679c246d110e0d3f16ec179b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UntagResource$TagKeys` targets `TagKeyList`, whose member is the `TagKey` shape — the very shape whose `@pattern(^(?!aws:)[a-zA-Z+-=._:/]+$)` this crate quotes as the SOURCE of its reserved-prefix refusal. That pattern uses a negative lookahead, so the framework's constraint walk does not enforce it (proved below by the class of the answer the crate's own test pins), and the crate therefore re-implements the reservation by hand: `crate::state::reserved_tag_key` is called by `TagResource` and by `CreateLifecyclePolicy` before either touches the store.

`UntagResource` calls it nowhere. Its handler goes straight from the region lookup to `policy.tags.remove(key)` for every key the request names, so `DELETE /tags/{arn}?tagKeys=aws:reserved` is answered 200 (and advances the policy's `DateModified`) while the identical key sent to `POST /tags/{arn}` is refused 400.

That contradicts the crate's own stated rationale for the refusal, which is explicitly about the request's SHAPE and explicitly independent of what the store holds: state.rs says a key under the prefix "is input no request may carry, whatever the store holds, and not a rule of the policy the request addresses", and that the helper is "[g]eneric over the operation's modeled error enum, so every operation that sets tags refuses the same key with the same class, status and prose". `UntagResource` is the one operation whose input carries `TagKey`-constrained keys and is never asked.

This is not the already-filed `UntagResource` DateModified finding (20261029T180848Z, gap_class `wrong-behavior`), which is about keys the policy never held being accepted as a no-op write; this is about a key the model's own shape constraint forbids any request from carrying reaching the store at all.

## Evidence

Smithy — the member's shape constraint (`smithy_query(query="shape", shape="TagKeyList")`):
```
"TagKeyList": {"type": "list", "length": {"min": 1, "max": 200},
  "member": {"target": "TagKey", "length": {"min": 1, "max": 128},
             "pattern": "^(?!aws:)[a-zA-Z+-=._:/]+$"}}
```
and `smithy_query(query="operation", operation="UntagResource")`: `"TagKeys": {"target": "TagKeyList", "httpQuery": "tagKeys", "required": true, ...}` — i.e. every entry of `TagKeys` is a `TagKey`, the same shape `TagMap`'s key targets.

The crate's own rule, services/service-com-amazonaws-dlm-dlm-20180112/src/state.rs:1880-1896:
```
/// The reservation is a constraint of the tag key SHAPE — `TagKey` is
/// `@pattern(^(?!aws:)[a-zA-Z+-=._:/]+$)` (`smithy_query(query="shape",
/// shape="TagKey")`) — so a key under the prefix is input no request may carry,
/// whatever the store holds, and not a rule of the policy the request addresses.
...
/// Generic over the operation's modeled error enum, so every operation that sets
/// tags refuses the same key with the same class, status and prose.
pub fn reserved_tag_key_refused<Modeled>() -> crate::scaffolded::Scaffolded<Modeled> {
    crate::scaffolded::Scaffolded::bad_request(RESERVED_TAG_KEY_MESSAGE)
}
```

The sibling that applies it — src/ops/tag_resource.rs:144-150:
```
        // A caller may not set a tag key under the reserved `aws:` prefix. The
        // refusal precedes the lookup and the write, so nothing is tagged, ...
        if crate::state::reserved_tag_key(input.tags.keys().map(String::as_str)).is_some() {
            return Err(crate::state::reserved_tag_key_refused());
        }
```
and the second sibling — src/ops/create_lifecycle_policy.rs:397-409:
```
        // A caller may not SET a tag key under the reserved `aws:` prefix, here
        // or through `TagResource`, and the refusal is the same class, status and
        // prose on both paths (`crate::state::reserved_tag_key_refused`).
        if crate::state::reserved_tag_key(... input.tags ...).is_some() {
            return Err(crate::state::reserved_tag_key_refused());
        }
```

The path that skips it — src/ops/untag_resource.rs:138-153 (the whole body between the store lock and the write; no call to `reserved_tag_key`, and the module header at untag_resource.rs:43-57 mentions no such rule):
```
        let region = crate::state::request_region(ctx.parts());
        let now = ::std::time::SystemTime::now();
        let mut store = state.lock();
        let policy = store.policy_mut_by_arn(&region, &input.resource_arn)?;

        // Removing a key the policy does not hold is not an error: the request
        // asks for those keys to be absent, and afterwards they are.
        for key in &input.tag_keys {
            policy.tags.remove(key);
        }
```

Proof that the modelled `@pattern` is NOT enforced ahead of the handler (so this is not a "the framework already checks it" case): the crate's test pins the HANDLER's refusal class, not the constraint walk's. tests/all/reserved_tag_key_refusal_class.rs:41-44 and :228-258:
```
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;
...
    assert_eq!(answer.error_shape(), BAD_REQUEST_EXCEPTION, ...);
    assert_eq!(answer.body["message"], json!(RESERVED_TAG_KEY_MESSAGE), ...);
    assert_eq!(answer.body["Code"], Value::Null,
        "case {case:?}: this class declares no Code member, got {:?}", answer.body);
```
Had the walk flagged the key, `TagResource` (wrapped in `InModelledEnvelope`) would have answered the modelled `InvalidRequestException` carrying `Code`/`Message` instead — src/service.rs:542-581 `fn invalid_request(...)` builds exactly that from `constraints::validate`. The test's module doc states the rule it asserts, tests/all/reserved_tag_key_refusal_class.rs:6-12:
```
//! One rule, asserted over every operation that sets tags ...
//! > A tag key outside the reservation its SHAPE declares — `TagKey` is
//! > `@pattern(^(?!aws:)[a-zA-Z+-=._:/]+$)` ..., so no request may carry such a
//! > key whatever the store holds — is refused as malformed input: HTTP 400 under
//! > the error type `BadRequestException` ...
```
Test coverage: that suite exercises only `TagResource` and `CreateLifecyclePolicy` ("every operation that SETS tags", test at :267-330); no test sends a reserved key to `UntagResource`, so nothing pins today's 200.

## Steps to Reproduce

1. POST /policies (signed for us-east-2) with a valid ExecutionRoleArn, Description, State: "ENABLED" and a complete custom PolicyDetails; note the PolicyId, then GET /policies/{PolicyId} and note the PolicyArn.
2. POST /tags/{PolicyArn} with body {"Tags": {"aws:reserved": "nope"}} — observe HTTP 400, error type BadRequestException, message "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)" (the handler check at src/ops/tag_resource.rs:148).
3. DELETE /tags/{PolicyArn}?tagKeys=aws:reserved — observe HTTP 200, although every entry of TagKeys carries the same TagKey shape whose @pattern the crate quotes as the source of the refusal in step 2.
4. Repeat step 3 with mixed case (?tagKeys=AWS:reserved) and with the key beside an allowed one (?tagKeys=aws:reserved&tagKeys=team) — observe HTTP 200 in each case, where TagResource refuses the identical key set whole.
5. GET /policies/{PolicyId} and observe DateModified advanced by the accepted call of step 3, i.e. the request the model's shape constraint forbids reached the store.
