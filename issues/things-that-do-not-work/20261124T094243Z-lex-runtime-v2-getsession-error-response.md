Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: e440eb2e6b3cf9d5628e1bfd8cddd7fc88366e52

# GetSession never resolves the bot/alias/locale it is addressed by, contradicting the model and two of the crate's own test files

_Filed: 2026-11-24T09:42:43Z_
_Operation: GetSession | Class: error-response_

> _Note: this issue was identified against the code base at commit `e440eb2e6b3cf9d5628e1bfd8cddd7fc88366e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetSession`'s Smithy documentation states a distinct refusal for an address whose bot, alias or locale does not exist, separate from an absent session. The handler resolves nothing of the address: it goes straight to the session store and answers EVERY unresolvable address with the absent-session 404, so a `GetSession` on a `botId` nothing was ever created under, on an alias the bot does not have, or on a locale that was never built is indistinguishable from a correctly addressed call for a session id nobody stored. The crate contradicts itself about this: two of its own (now emptied) test files state that the model requires the address to be resolved by all five session operations and claim the rule is asserted in `session_address_resolution.rs`, while that file asserts precisely the opposite, and `bot_addressing.rs` carries both statements in the same file.

## Evidence

MODEL — `smithy_query(query="doc", shape="GetSession")`: "Returns session information for a specified bot, alias, and user. ... If the bot, alias, or session identifier doesn't exist, Amazon Lex V2 returns a BadRequestException. If the locale doesn't exist or is not enabled for the alias, you receive a BadRequestException." (reproduced in `src/ops/get_session.rs:56-60`). The model therefore names the bot, the alias and the locale as separately resolvable, each with its own refusal, distinct from the session identifier.

HANDLER — `src/ops/get_session.rs:194-199`:
```
let mut store = state.lock();
let session = store.session(&key, now).ok_or_else(|| {
    GetSessionErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
        message: crate::state::session_not_found_message(&input.session_id),
    })
})?;
```
There is no `built_locale(...)` call anywhere in the file, unlike the siblings (`src/ops/recognize_text.rs:258-264`, `src/ops/put_session.rs:292-298`, `src/ops/recognize_utterance.rs:475-481`), each of which resolves the pair and the locale first.

CRATE CONTRADICTS ITSELF — `tests/all/session_store_resolves_the_address.rs:3-11`: "Superseded by `session_address_resolution.rs`. This suite asserted that a session read or delete resolved the session against this runtime's own store and nothing else, which the model contradicts: both operations document that the bot, the alias and the locale a call addresses are resolved, so an address naming no alias of that bot or no locale the alias's version can serve is refused AS the address rather than as a session that is not there. That rule, the address rows that used to live here and the round trip through a resolving address are asserted in `session_address_resolution.rs`".

But `tests/all/session_address_resolution.rs:12-15` asserts the reverse: "The two calls that only REPORT or REMOVE the session held under those labels resolve no address: the session is stored under the whole address, so a lookup that finds none is reported as the session that is not there, whichever label of the address names nothing." — pinned at lines 263-266: `let expected = match call.answers_from_a_stored_session() { true => session_absent(&session_id), false => expected.clone(), };`

And `tests/all/bot_addressing.rs:84-85` states the opposite of its own rule 1 (lines 21-23) in the same file: "Every one of them resolves the bot ADDRESS first, whichever side of this it is on."

`src/state/bot_model.rs:22-25` takes the third position again: "every operation that interprets an utterance or seeds a dialog state resolves it here ([`built_locale`]). The two operations that only report or remove the session held under those labels resolve no address at all".

## Steps to Reproduce

1. Create and build a bot through the mounted Lex Models V2 control plane (as `tests/all/session_address_resolution.rs` does via `emulator.built_bot(...)`).
2. `GET /bots/AAAAAAAAAA/botAliases/TSTALIASID/botLocales/en_US/sessions/sess-1` — a well-formed `botId` nothing was created under.
3. Observed: HTTP 404 `ResourceNotFoundException` with "The session for user sess-1 doesn\u{2019}t exist or has expired." — the same answer a fully resolving address gives for a session nobody stored.
4. The model documents a distinct refusal here ("If the bot, alias, or session identifier doesn't exist ..."), so the caller cannot tell "no such bot/alias" from "no such session".
