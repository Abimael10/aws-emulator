Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 7c268c75f20ad8b8ff92b3c02bd5037e54f1d3fa

# A change's details are held to Json's modelled @length in BYTES, so a details value the boundary admitted is refused 422

_Filed: 2026-11-16T20:14:27Z_
_Operation: StartChangeSet | Class: validation_

> _Note: this issue was identified against the code base at commit `7c268c75f20ad8b8ff92b3c02bd5037e54f1d3fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::details_json_length` is the one place this crate measures a details value against the length the model declares for it, and it measures UTF-8 BYTES (`String::len`) while the modelled `@length` it is compared against counts characters. Its own doc sentence says it answers characters, and every other modelled length in the crate is measured with `chars().count()`.

Two paths use it, and both diverge for any details value that is not pure ASCII:

1. `StartChangeSet` (src/ops/start_change_set.rs:531-542) refuses the change with the modelled `ValidationException` when the BYTE length passes `DETAILS_JSON_LIMIT` (16384). `Change$Details` targets `Json` (`@length(min: 2, max: 16384)`) and is NOT in `skip_validation` for `Change` (src/ops/types/start_change_set_types.rs:46 names only `ChangeName` and `ChangeType`), so the boundary validator has already accepted the member against that modelled length — the handler's own comment two lines above says so: "`Change$Details` states that length and the boundary enforces it". The same value is then re-measured in a different unit and refused. ~5500 CJK characters (3 bytes each) is enough: well inside the 16384 characters the model allows, past 16384 bytes. The refusal sentence (`details_too_long`) even reports the byte count as "characters".

2. `judge_change_set` (src/state.rs:2041-2050) measures the MERGED entity details the same way, so a change set whose merge is reportable in characters is settled to `FAILED` with a `LIMIT_EXCEEDED` `ErrorDetail` naming a byte count.

The effect is that a request the model allows is turned into an error (and, on path 2, into a failed change set that applies nothing), and the bound the crate publishes in its messages is not the bound it enforces.

## Evidence

The model (smithy_query query="shape" shape="Json"): `Json` is `"length": {"min": 2, "max": 16384}` — a Smithy string `@length`, i.e. a count of characters; `Change$Details` targets it with the same bound.

The measurement, in bytes — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/state.rs:3745-3755:
```
/// How many characters the `Json` string a details value is reported as takes —
/// [`EntityRecord::details_json`]'s own serialization, which writes no
/// insignificant whitespace, so the count is the length of the string a reader
/// receives.
...
pub fn details_json_length(members: &Map<String, Value>) -> usize {
    serde_json::to_string(members).map_or(usize::MAX, |text| text.len())
}
```
(`text.len()` is the UTF-8 byte length; the doc above it says "How many characters".)

The bound it is compared against — state.rs:3733-3743: "`Json` — the shape of every member that carries a details value as a string ... is `@length(min: 2, max: 16384)`" / `pub const DETAILS_JSON_LIMIT: usize = 16_384;`

The refusal, on a value the boundary already accepted — src/ops/start_change_set.rs:526-541:
```
                // length `Json` declares is judged on the value itself,
                // whichever member carried it: `Change$Details` states that
                // length and the boundary enforces it, while
                // `Change$DetailsDocument` carries the same details as a
                // document the model bounds in no way at all.
                let length = crate::state::details_json_length(members);
                if length > crate::state::DETAILS_JSON_LIMIT {
                    ...
                        crate::state::details_too_long(field, length),
```
and the sentence it builds — state.rs:3802-3809: "the details of a change are at most {DETAILS_JSON_LIMIT} characters of JSON, not {length}" (where `length` is the byte count).

The same unit mistake on the apply path — state.rs:2041-2048:
```
            let length = details_json_length(merged).saturating_add(applied_details_growth(merged));
            if length > DETAILS_JSON_LIMIT {
                verdict.change_errors[index].push(ErrorDetail {
                    error_code: Some(ERROR_CODE_LIMIT_EXCEEDED.to_string()),
                    error_message: Some(format!(
                        "Details: an entity holds at most {DETAILS_JSON_LIMIT} characters of \
                         details, and this change would leave {entity_id} with {length}"
```

Every other modelled length in this crate is measured in CHARACTERS, which is the inconsistency: state.rs:4570 `if value.chars().count() > ENTITY_MEMBER_MAX` (`is_entity_id`), state.rs:3588 `if !(1..=RESOURCE_ARN_MAX).contains(&value.chars().count())` (`is_resource_arn`), state.rs:4330 `is_entity_identifier`, state.rs:4488 `is_change_set_name`, state.rs:2886-2887 `let (min, max) = self.length; (min..=max).contains(&value.chars().count())` (`DetailContract::admits_text`), state.rs:3951-3953 `quoting` (`prefix.chars().count() + suffix.chars().count()`), state.rs:3298-3302 `cut_to` (`text.char_indices()`).

Test coverage: tests/all/details_length_bound.rs exercises this bound only with ASCII payloads, where bytes and characters coincide — tests/all/details_length_bound.rs:263-269 `assert!(details.len() <= DETAILS_JSON_LIMIT ...)` and the generators at :251-257 (`"p".repeat(...)`), so no test pins the byte semantics or states a reason for them. The crate's own length tests measure modelled lengths in characters: tests/all/modelled_member_lengths.rs:460-464 `reported.chars().count() <= ENTITY_MEMBER_MAX`.

## Steps to Reproduce

1. POST /StartChangeSet with Catalog=AWSMarketplace and one change whose Details is a JSON object carrying ~6000 multi-byte characters, e.g. {"ChangeType":"CreateProduct","Entity":{"Type":"AmiProduct@1.0"},"Details":"{\"ProductTitle\":\"鍵鍵…\"}"} with 6000 '鍵' characters — about 6020 characters (inside Json's @length(2, 16384)) and about 18020 UTF-8 bytes.
2. Observe the boundary validator accepts the member (it is held to the modelled @length, which counts characters, and Details is not in skip_validation for Change).
3. Observe the handler answers 422 ValidationException "[ChangeSet.Details: the details of a change are at most 16384 characters of JSON, not 18020]" — the quoted count is bytes, not characters, and the request the model allows is refused.
4. Repeat with the same object sent as DetailsDocument: same refusal, from the same byte count.
5. For the apply path, submit two changes against one entity whose merged details are ~6000 multi-byte characters: the change set settles to FAILED with ErrorDetail LIMIT_EXCEEDED naming a byte count, although DescribeEntity could report the merge inside Json's modelled character length.
