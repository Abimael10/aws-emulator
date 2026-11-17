Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 1461f90f45b201746a1342e698d42d830a2c1617

# A whitespace-only NextToken is silently honoured as "no cursor" and restarts the listing, though the handler states a token this service did not mint cannot be used

_Filed: 2026-11-17T07:18:35Z_
_Operation: ListConfigurationSets | Class: pagination_

> _Note: this issue was identified against the code base at commit `1461f90f45b201746a1342e698d42d830a2c1617`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListConfigurationSets` trims both query parameters and then folds the trimmed-empty case into the ABSENT case, so a whitespace-only value never reaches the validation that is written for it:

* `?NextToken=%20` (or any run of spaces/tabs) is treated as "no cursor" and the handler answers HTTP 200 with the FIRST page of the listing, even though such a value is not a token this service ever minted — `encode_token` only ever produces hex of `TOKEN_MARKER` + a name, never whitespace. A paginating client that passes a blanked-out cursor is silently restarted at the beginning of the collection (it can re-walk names it already consumed, and can loop) instead of being told its token is unusable. The very same handler refuses `?NextToken=0g`, which is equally never-minted, with the modelled `BadRequestException` (400).
* `?PageSize=%20` likewise silently takes the server's own `DEFAULT_PAGE_SIZE` instead of the refusal `?PageSize=0` and `?PageSize=two` get.

The defect is the layer collapse, not the chosen bound: the handler's own comment states the rule it then does not apply to this class of value, and the crate states ONE rule elsewhere for telling a value that bound NOTHING (absent/empty) from one that bound a BLANK value (present but unusable) — `refusal_wording::unusable_required_labels` implements exactly that split for path labels and refuses the blank one as invalid, while this handler maps blank onto absent.

## Evidence

The rule the handler states for a token, immediately above the code that breaks it — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/list_configuration_sets.rs:180-183:
```
// `NextToken` is opaque to the caller but it is OURS: one this service did not
// mint cannot name a position in the listing, so it is a parameter this
// service's validation cannot use — and one it names in the refusal, because the
// caller can only have got a usable token from this service.
```
The code, list_configuration_sets.rs:184-193 — the `str::trim` turns a blank value into `Some("")`, which the first arm swallows so `decode_token` is never called:
```
let resume_after = match input.next_token.as_deref().map(str::trim) {
    None | Some("") => None,
    Some(raw) => Some(decode_token(raw).ok_or_else(|| { ... BadRequestException ... })?),
};
```
Same collapse for the page bound, list_configuration_sets.rs:175-178:
```
let page_size = match input.page_size.as_deref().map(str::trim) {
    None | Some("") => DEFAULT_PAGE_SIZE,
    Some(raw) => parse_page_size(raw)?,
};
```
Why a never-minted value must not be honoured, from this file's own `decode_token` doc, list_configuration_sets.rs:269-274: "The inverse of [`encode_token`]; `None` for anything this service did not mint. The marker is checked, not just the encoding: without it any even-length hex string that happens to decode to UTF-8 would be honoured as a cursor, and a caller that invented one would silently get a listing starting from a position the service never handed out." A whitespace token bypasses that check entirely.

The crate's ONE stated rule for a blank value, which this handler does not follow — src/ops/types/refusal_wording.rs:60-66: "* an EMPTY value bound nothing at all, so the request never named a target and the request-binding layer reports it ([`missing_request_parameters`]); * a value that is present but BLANK did bind, so it reached this service's own validation, which cannot use it ([`invalid_parameters`])." Implemented at refusal_wording.rs:84-91 (`let invalid = named(|value| value.trim().is_empty()); if !invalid.is_empty() { return Some(invalid_parameters(&invalid)); }`).

The suite that states the rule as spanning query parameters AND the blank form, with no row for their intersection — tests/all/parameter_refusal_wording.rs:16-19 ("a parameter that reached this service's own validation carrying a value it cannot use is `The following parameter is null, empty or invalid: ['<parameter>']`") and :35-38 ("The rows differ along what the rule generalises over: ... where the parameter travels (a URI path label, a query parameter, a body member), and the FORM of the unusable value (absent, empty, blank, unparseable, out of range, contradictory)"). Existing coverage: the blank (`%20`) rows exist only for PATH labels (parameter_refusal_wording.rs:225-247, legacy_surface_authorization.rs:583-590); the query-parameter rows are all non-blank (parameter_refusal_wording.rs:247-283; tests/all/configuration_set_lifecycle.rs:811-831 refuses `"zzz"`, `"0g"`, `hex::encode("set-a")`, `"6161"`). No test pins a whitespace-only `NextToken` or `PageSize`, so nothing justifies the collapse.

## Steps to Reproduce

1. Boot the emulator and create two configuration sets (`set-a`, `set-b`) as tests/all/configuration_set_lifecycle.rs:787-789 does; `ListConfigurationSets` is account-scoped so it needs no entitlement.
2. GET /v1/sms-voice/configuration-sets?NextToken=0g — observe HTTP 400 `BadRequestException` with message "The following parameter is null, empty or invalid: ['nextToken']" (the control: a value this service did not mint).
3. GET /v1/sms-voice/configuration-sets?NextToken=%20 — observe HTTP 200 and the FIRST page (`set-a`, `set-b`), although a whitespace value is just as impossible an output of `encode_token`: the cursor is silently ignored instead of refused.
4. GET /v1/sms-voice/configuration-sets?PageSize=0 — observe HTTP 400 `BadRequestException`; then GET /v1/sms-voice/configuration-sets?PageSize=%20 — observe HTTP 200 with the server default bound applied instead of the same refusal.
