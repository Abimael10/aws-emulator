Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 2caadaf466728c72d3718afcc2311eff917f0cf8

# A PageSize above usize::MAX is refused with BadRequestException instead of being clamped, contradicting the handler's own stated clamp rule

_Filed: 2026-11-20T01:08:31Z_
_Operation: ListConfigurationSets | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `2caadaf466728c72d3718afcc2311eff917f0cf8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListConfigurationSets` states — twice, in the handler and in the helper's doc — that a `PageSize` which is a positive decimal count is CLAMPED to `MAX_PAGE_SIZE` and that only a value which is "not a positive decimal count" is refused. The implementation delegates the decision to `raw.parse::<usize>()`, so the split is not "positive count vs. not a count" but "fits in a `usize` vs. does not": a positive decimal count larger than `usize::MAX` (e.g. `PageSize=99999999999999999999`) makes `parse` return `Err(PosOverflow)` and the request is answered with the modelled `BadRequestException` (HTTP 400, and with no detail text at all), while `PageSize=99999` — equally above the maximum — is clamped to `MAX_PAGE_SIZE` and answered 200.

Two values that are both "a count above the maximum" therefore get opposite answers, and the boundary between them is the host's pointer width rather than any rule this crate states: on a 32-bit build `PageSize=5000000000` is refused, on a 64-bit build the same request is clamped and succeeds. Nothing in the crate justifies the divergence — the refusal rows that exist for `PageSize` are non-numeric, zero, negative and fractional values, all of which the stated rule does cover.

This is not a complaint about the value of `MAX_PAGE_SIZE`: the defect is that the documented clamp is not applied to the whole class of values the doc says it applies to.

## Evidence

The rule the handler states, immediately above the code — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/list_configuration_sets.rs:172-178:
```
// `PageSize` is modelled as a string, so a value that is not a positive
// decimal count is a client error, not a count to clamp. Absent, the server's
// own `DEFAULT_PAGE_SIZE` applies; present, it is clamped to `MAX_PAGE_SIZE`.
let page_size = match input.page_size.as_deref().map(str::trim) {
    None | Some("") => DEFAULT_PAGE_SIZE,
    Some(raw) => parse_page_size(raw)?,
};
```

The same rule restated in the helper's own doc — list_configuration_sets.rs:234-240:
```
/// Read `PageSize` as the page bound, refusing anything that is not a positive count
/// and clamping what is left to [`MAX_PAGE_SIZE`].
///
/// A page of zero items would make a paginating client loop forever, so it is refused
/// alongside a negative, fractional or non-numeric value. A count above the maximum is
/// clamped rather than refused: the caller asked for at most that many, and it gets a
/// `NextToken` for the remainder.
```

The code that breaks it — list_configuration_sets.rs:251-258 (a 20-digit positive decimal count is a positive count, yet `parse::<usize>()` fails on it and the `_` arm refuses it):
```
fn parse_page_size(raw: &str) -> Result<usize, ListConfigurationSetsErrorModeled> {
    match raw.parse::<usize>() {
        Ok(size) if size > 0 => Ok(size.min(MAX_PAGE_SIZE)),
        _ => Err(ListConfigurationSetsErrorModeled::BadRequestException(
            BadRequestException { message: None },
        )),
    }
}
```

The bound that the clamp is supposed to make unreachable — list_configuration_sets.rs:81-91: "The largest page the server will serve, whatever `PageSize` asks for. A caller-chosen page size is clamped to this rather than refused: it bounds how much one request can amplify into, so the response size is capped no matter what is stored."

Test coverage: the refusal rows for this parameter are only non-numeric, zero, negative and fractional — tests/all/parameter_refusal_wording.rs:259-286 ("ListConfigurationSets with a non-numeric PageSize" `PageSize=not-a-number`, "...with a PageSize of zero" `PageSize=0`, "...with a negative PageSize" `PageSize=-5`, "...with a fractional PageSize" `PageSize=2.5`). No test pins an overflowing count, so nothing justifies refusing it.

## Steps to Reproduce

1. Boot the emulator and create two configuration sets (`set-a`, `set-b`); `ListConfigurationSets` is account-scoped, so it needs no entitlement (src/ops/list_configuration_sets.rs:204-206).
2. GET /v1/sms-voice/configuration-sets?PageSize=99999 — observe HTTP 200: the count is above `MAX_PAGE_SIZE` (1000) and is clamped, exactly as the handler's comment says.
3. GET /v1/sms-voice/configuration-sets?PageSize=99999999999999999999 — observe HTTP 400 `BadRequestException` with no message, although this is also "a positive decimal count above the maximum" and the handler states such a count is clamped rather than refused.
4. Note that the boundary is the host's `usize` width, not a stated rule: on a 32-bit build `?PageSize=5000000000` is refused while the same request is clamped and answered 200 on a 64-bit build.
