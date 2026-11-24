Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 40b4c40578424e9a73c61204c7ab51c695d87313

# A boundary refusal about a value under an all-digit MAP key reports a JSON pointer one less than the key, naming a member the request does not carry

_Filed: 2026-11-16T03:35:44Z_
_Operation: TagResource | Class: error-response_

> _Note: this issue was identified against the code base at commit `40b4c40578424e9a73c61204c7ab51c695d87313`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service.rs::json_pointer` turns the framework's dotted violation path into the JSON pointer this service states in its `ValidationException` refusal envelope. It renumbers a segment from the framework's 1-based list position to RFC 6901's 0-based array index whenever that segment parses as a `usize` AND is followed by the `member` step. Per the function's own documentation, a MAP entry arrives in exactly the same `<position-or-key>.member` shape, with the key kept as the caller sent it — so a map key that happens to be all digits is renumbered as if it were a list position.

The consequence: a `TagResource` request whose `tags` map carries the key `2024` with a value longer than `TagValue`'s modelled `length 0..256` is refused with a pointer naming `/tags/2023`. The request carries no member `2023`, so the pointer resolves against nothing in the body the caller sent — which is the one property this crate states the pointer exists to have.

The gap is admitted in the code itself ("A numeric map key is therefore indistinguishable from a list position here and is renumbered as one"), and the stated reason for the admission is only that the DOTTED form carries nothing that tells the two apart — while the crate does hold the information that tells them apart: each operation's `INPUT_SCHEMA` (`super::types::TAG_RESOURCE_INPUT_SCHEMA`, named by every `impl Operation` in `src/ops/`) says whether the member one step up is a map or a list.

This reaches every caller-keyed map member of the service — `TagResource$tags`, `CreatePolicyStoreInput$tags`, `IsAuthorizedInput$context.contextMap`, an `EntityItem`'s `attributes`/`tags`, `KmsEncryptionSettings$encryptionContext` — through the one shared pointer builder; `TagResource` is named because its `tags` map is the simplest member with a modelled per-entry bound a caller can break.

## Evidence

The gap, admitted in the function's own documentation — services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/service.rs:62-73:

```
/// One member of the request as a JSON pointer rooted at the request body, built
/// from the dotted path the framework walks its constraints with.
///
/// Each dotted segment becomes one pointer segment. A step from a collection into
/// one of its elements arrives as `<position-or-key>.member`, which a pointer
/// spells as the position alone: an element's position is renumbered from the
/// framework's 1-based count to the 0-based index RFC 6901 gives an array
/// element, and a map entry's key — a value the caller chose rather than a model
/// name — is kept as sent, with the two characters a pointer segment escapes
/// (`~` and `/`) escaped. A numeric map key is therefore indistinguishable from a
/// list position here and is renumbered as one; the dotted form carries nothing
/// that tells the two apart.
```

and the code that does it — src/service.rs:77-90:

```
    while let Some(segment) = segments.next() {
        pointer.push('/');
        if segments.peek() == Some(&COLLECTION_STEP) {
            // The step into the element is spelled by the position alone, so the
            // `member` segment that followed it is consumed here.
            segments.next();
            match segment.parse::<usize>() {
                Ok(position) => pointer.push_str(&position.saturating_sub(1).to_string()),
                Err(_) => pointer.push_str(&escaped_segment(segment)),
            }
```

The rule this breaks, stated by the crate's own pointer helper — src/ops/mod.rs:86-100:

```
    /// `ValidationExceptionField$path` is "the path to the specific element that
    /// Verified Permissions found to be not valid", and a member that lives
    /// inside a list element is only addressable through that element's own
    /// position: a pointer numbers an array element from 0 (RFC 6901), which is
    /// also the spelling a refusal of a modelled constraint on the very same
    /// member carries ([`crate::service`] builds it that way). So a check written
    /// for ONE element keeps one implementation, and the walk over the list says
    /// which element it refused — leaving a pointer that resolves against the
    /// request body the caller sent.
    pub fn within_list_element(self, list_path: &str, index: usize) -> Self {
        self.within(&format!("{list_path}/{index}"))
    }
```

and restated for the refusal envelope this pointer lands in — src/service.rs:135-140:

```
/// The count is followed by a PERIOD, where the framework's own default follows
/// it with a colon, and each clause addresses its member as a JSON pointer rather
/// than as a dotted path, because that pointer is the machine-readable half of
/// the message — the same spelling this service's own input checks report in
/// `ValidationException$fieldList` ([`crate::ops::paths`]).
```

The member that makes it reachable — `smithy_query(query="shape", shape="TagMap")`:

```
"TagMap": {"type": "map", "length": {"max": 200},
  "key": {"target": "TagKey", "length": {"min": 1, "max": 128}},
  "value": {"target": "TagValue", "length": {"min": 0, "max": 256}}}
```

and the model sentence the spelling is held to — `smithy_query(query="doc", shape="ValidationExceptionField", member="path")`: "The path to the specific element that Verified Permissions found to be not valid."

Test coverage: tests/all/validation_exception_contents.rs:169-197 pins the LIST case only — `requests` element #2 (framework `requests.2.member.principal.entityId`) must be stated as `/requests/1/principal/entityId` ("A member inside the SECOND element of a list: the pointer names the element's own 0-based position, with no `member` step"). No row in that suite, nor anywhere in tests/all/, submits a map whose key is all digits, so no test pins the renumbered-map-key behaviour and fixing it leaves every existing assertion unchanged.

## Steps to Reproduce

1. CreatePolicyStore with {"validationSettings":{"mode":"OFF"}} and note the returned `arn`.
2. TagResource with {"resourceArn":"<that arn>","tags":{"2024":"<a 300-character string>"}} — the value breaks TagValue's modelled length 0..256, so the framework refuses the request before the handler runs.
3. Read the 400 ValidationException message: it states a clause addressing `/tags/2023`.
4. Observe the request body carries no member `2023` — only `2024` — so the pointer resolves against nothing the caller sent, where the LIST case (tests/all/validation_exception_contents.rs:189-195) is pinned to produce a pointer that does resolve.
