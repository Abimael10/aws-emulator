Issue-Severity: LOW
Issue-Gap-Class: validation
Issue-Commit: 5a91070757730c0ac9289dd2b7ac94b796bd322a

# ListConfigurationSets honours any NextToken carrying its public marker, contradicting the handler's "one this service did not mint" rule

_Filed: 2026-11-22T17:05:43Z_
_Operation: ListConfigurationSets | Class: validation_

> _Note: this issue was identified against the code base at commit `5a91070757730c0ac9289dd2b7ac94b796bd322a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler states that a `NextToken` it did not mint is a parameter its validation cannot use, and refuses such a value with `BadRequestException`. The check it actually performs is a SHAPE check: hex-decode, then `strip_prefix(TOKEN_MARKER)`, where `TOKEN_MARKER` is a fixed string declared in the same file. Any caller can therefore assemble a value that passes — `hex("pinpointsmsvoice:configuration-sets:1:" + <any name>)` — and get a 200 page resumed after a position this service never handed out, which is precisely the outcome `decode_token`'s own doc says the marker exists to prevent.

The crate's test suite demonstrates how cheap the assembly is: it builds a token-shaped value by hand from the marker string, differing only in the version digit (`:0:` instead of `:1:`), to produce a REFUSED row. Swapping that digit for the live one turns the same hand-made string into an accepted cursor.

This crate already depends on `service-common`, whose `page_tokens` module is the workspace's existing answer for exactly this distinction (it is used by ~20 sibling service crates); the hand-rolled marker in `list_configuration_sets.rs` re-implements that distinction in a form that cannot make it.

## Evidence

The rule the handler states — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/list_configuration_sets.rs:180-183:

        // `NextToken` is opaque to the caller but it is OURS: one this service did not
        // mint cannot name a position in the listing, so it is a parameter this
        // service's validation cannot use — and one it names in the refusal, because the
        // caller can only have got a usable token from this service.

What the marker is claimed to do — list_configuration_sets.rs:93-98:

    /// The marker every `NextToken` this operation mints carries, inside the encoding.
    ///
    /// It is what distinguishes a token this service handed out from a string a caller made
    /// up that merely decodes cleanly; ...
    const TOKEN_MARKER: &str = "pinpointsmsvoice:configuration-sets:1:";

What the code under those comments actually accepts — list_configuration_sets.rs:269-277:

    /// The inverse of [`encode_token`]; `None` for anything this service did not mint.
    ...
    fn decode_token(token: &str) -> Option<String> {
        let decoded = String::from_utf8(hex::decode(token).ok()?).ok()?;
        decoded.strip_prefix(TOKEN_MARKER).map(str::to_string)
    }

The crate's own test assembles a value of that shape by hand — tests/all/configuration_set_lifecycle.rs:811-818:

    for token in [
        "zzz".to_string(),
        ...
        hex::encode("pinpointsmsvoice:configuration-sets:0:set-a"),

(refused only because of the `0`; with the live `1` the identical construction is honoured.)

The helper this crate already has available — services/common/src/page_tokens.rs:3-11:

    //! The ONE way a service tells a continuation token it ISSUED from one a caller built.
    ...
    //! the token — there is no page kept on the side to look up — so a read that only checks
    //! the token's SHAPE honours any value of that shape, and a caller can name a position
    //! the service never offered ... What the services' APIs document for such a value is a
    //! refusal (an invalid-token error), not a page.

and services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/Cargo.toml:28: `service-common = { path = "../common" }` (already used at src/ops/types/authorization.rs:108 and src/service.rs:124).

Test coverage of the surrounding behaviour: tests/all/configuration_set_lifecycle.rs:726-779 (walking real tokens), :784-842 (`invalid_pagination_arguments_are_bad_requests`), tests/all/parameter_refusal_wording.rs:244-258 (the two refusal rows for a non-minted token). None of them exercises a hand-made token carrying the LIVE marker.

## Steps to Reproduce

1. Start the emulator with the Pinpoint SMS Voice service registered.
2. Create sets "set-a" .. "set-e" via POST /v1/sms-voice/configuration-sets (ListConfigurationSets is not entitlement-gated, so no store tweak is needed for the read).
3. Compute the forged cursor off the public constant: hex("pinpointsmsvoice:configuration-sets:1:set-c") = 70696e706f696e74736d73766f6963653a636f6e66696775726174696f6e2d736574733a313a7365742d63.
4. GET /v1/sms-voice/configuration-sets?NextToken=<that hex> -> HTTP 200 with ConfigurationSets ["set-d", "set-e"], i.e. the listing resumed from a position this service never minted a token for.
5. Compare with GET /v1/sms-voice/configuration-sets?NextToken=<hex of the same string with :0: instead of :1:> -> HTTP 400 BadRequestException (the behaviour pinned at tests/all/configuration_set_lifecycle.rs:816), which is what list_configuration_sets.rs:180-183 says every non-minted token gets.
