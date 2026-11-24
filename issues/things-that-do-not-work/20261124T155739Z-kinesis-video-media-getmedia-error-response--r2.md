Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: fd9a72c97341bdd57013a660f551057e879b5f36

# GetMedia's fragment-number refusal echoes the caller's fragment number and the stream's registered name into an error channel this crate states carries neither

_Filed: 2026-11-24T15:57:39Z_
_Operation: GetMedia | Class: error-response_

> _Note: this issue was identified against the code base at commit `fd9a72c97341bdd57013a660f551057e879b5f36`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler refuses a `FRAGMENT_NUMBER` selector whose `AfterFragmentNumber` is not on the resolved stream with an `InvalidArgumentException` whose message interpolates TWO values the crate says this channel never carries: the fragment number the request sent, and `stream.name` — the `StreamName` the control-plane registry holds for the resolved stream. The second is state, not request input: a caller that addressed the stream by `StreamARN` is told the stream's registered NAME, which is exactly what the sibling refusals in the same file and the same error channel are documented to withhold.

Three places in this crate state the rule for this channel:
* `src/ops/get_media.rs:191-195` — the undecodable-continuation-token refusal is fixed text precisely so no token and no stream name leak out of `invalid_argument(...)`;
* `src/service/wrong_endpoint.rs:36-42` — the `InvalidEndpointException` sentence is fixed text so a refusal "tells a caller nothing about which streams exist";
* `src/service/validation.rs:12-19` — a clause of this family states the requirement and never the value, "so a media identifier, a cursor or a fragment number a caller sent is not read back out of this error channel".

The fragment-number refusal is the one refusal of this handler that breaks all three. Note the contrast with the companion-member refusals in the same file (`missing_companion`, `src/ops/get_media.rs:226-280`), which DO render the caller's selector but carry an explicit stated reason for it and are pinned by `tests/all/request_rejection_wording.rs:58`; the fragment-number message carries no such reason and no test pins it, so the crate's own stated rule is the only standard it can be judged against — and it fails it.

## Evidence

Offending code — `src/ops/get_media.rs:665-674`:
```
                        .ok_or_else(|| {
                            // The stream exists, so this is an invalid argument and
                            // not the one condition `ResourceNotFoundException`
                            // documents ("The stream with the given name does not
                            // exist").
                            invalid_argument(format!(
                                "Fragment number {after} is not on stream {}.",
                                stream.name
                            ))
                        })?
```
(`after` is `selector.after_fragment_number` as the request sent it, `src/ops/get_media.rs:660`; `stream.name` is the registry's registered `StreamName`, `src/state.rs:512-516` / `src/state.rs:421-423`.)

The rule it breaks, same file, same error constructor — `src/ops/get_media.rs:191-195`:
```
/// A continuation token is an opaque value the service itself mints and encodes, so a token
/// it did not mint is a token it cannot decode, whatever text it carries. The whole message
/// is fixed: no part of it comes from the request or from this service's state, which is
/// what keeps a token and a stream name out of an error channel that states neither.
pub const UNDECODABLE_CONTINUATION_TOKEN_MESSAGE: &str = "Illegal embedded sign character";
```

`src/service/wrong_endpoint.rs:36-38`:
```
/// The whole message is fixed text, as this family's other refusals are: no stream name,
/// ARN or address from the request or from this service's state reaches it, so a refusal
/// tells a caller nothing about which streams exist or where their endpoints are.
```

`src/service/validation.rs:12-19`:
```
//! * THE VALUE HALF. A clause of this family states the REQUIREMENT a value had to satisfy
//!   and never the value that missed it, whatever constraint class it broke: a request
//!   refused at the boundary is answered a sentence that carries none of its own bytes, so
//!   a media identifier, a cursor or a fragment number a caller sent is not read back out
//!   of this error channel.
```

Test coverage: `tests/all/refusal_message_fidelity.rs:3-27` pins the fixed-sentence rule for exactly two state-level refusals ("The WORDING of `GetMedia`'s two state-level refusals"), asserting per row that "the identifier it sent does NOT appear in the answer". The fragment-number refusal is a third state-level refusal and is covered by no wording assertion: `tests/all/get_media.rs:817-828` only checks the row "a fragment number that is not on the stream" answers 400 `InvalidArgumentException`, never the message, so the leak is unpinned either way.

## Steps to Reproduce

1. Create a stream through the control plane and ingest at least one fragment, so the stream resolves and holds a log (as `tests/all/get_media.rs` `seed` does).
2. POST /getMedia at the announced read endpoint with {"StreamARN":"<the minted ARN>","StartSelector":{"StartSelectorType":"FRAGMENT_NUMBER","AfterFragmentNumber":"987654321"}} — addressing the stream by ARN only, so the caller supplies no stream name.
3. Read the `Message` member of the 400 `InvalidArgumentException` body: it is "Fragment number 987654321 is not on stream <registered StreamName>." — it carries the fragment number the request sent and the stream name held in state.
4. Compare with the two refusals the crate documents for the same channel: an unissued continuation token answers the fixed `UNDECODABLE_CONTINUATION_TOKEN_MESSAGE` and an unresolvable identifier answers the fixed `STREAM_NOT_FOUND_MESSAGE`, neither carrying a request or state value (`tests/all/refusal_message_fidelity.rs`).
