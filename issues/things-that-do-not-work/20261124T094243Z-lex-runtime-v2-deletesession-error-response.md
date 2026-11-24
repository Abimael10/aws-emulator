Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: e440eb2e6b3cf9d5628e1bfd8cddd7fc88366e52

# DeleteSession never resolves the bot/alias/locale it is addressed by, contradicting the model and the crate's own superseded test file

_Filed: 2026-11-24T09:42:43Z_
_Operation: DeleteSession | Class: error-response_

> _Note: this issue was identified against the code base at commit `e440eb2e6b3cf9d5628e1bfd8cddd7fc88366e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteSession`'s Smithy documentation states a distinct refusal for a bot or alias ID that does not exist, and another for a locale that does not exist in the bot — separate from an absent session. The handler resolves nothing of the address: it calls `remove_session` directly and answers every unresolvable address with the absent-session 404. The crate's own emptied test file `session_reads_need_no_built_model.rs` states that this is wrong and that the correcting rule is asserted in `session_address_resolution.rs`, while that file asserts the opposite.

## Evidence

MODEL — `smithy_query(query="doc", shape="DeleteSession")`: "If you specify a bot or alias ID that doesn't exist, you receive a BadRequestException." and "If the locale doesn't exist in the bot, or if the locale hasn't been enables for the alias, you receive a BadRequestException." (reproduced in `src/ops/delete_session.rs:60-62`).

HANDLER — `src/ops/delete_session.rs:191-202`:
```
if state
    .lock()
    .remove_session(&key, SystemTime::now())
    .is_none()
{
    return Err(DeleteSessionErrorModeled::ResourceNotFoundException(
        ResourceNotFoundException {
            message: crate::state::session_not_found_message(&input.session_id),
        },
    )
    .into());
}
```
No `built_locale(...)` call exists in the file; its module doc asserts the rule the model contradicts — `src/ops/delete_session.rs:8-15`: "What this operation resolves is the SESSION held under those four labels, and nothing else ... The three calls that interpret an utterance or seed a dialog state need the bot's own built model and so resolve the address; removing the session held at an address does not."

CRATE CONTRADICTS ITSELF — `tests/all/session_reads_need_no_built_model.rs:3-11`: "Superseded by `session_address_resolution.rs`. This suite asserted that reading or removing a session needed no model behind the locale it is addressed in, which the model contradicts: `GetSession` and `DeleteSession` document the same resolution of the bot, the alias and the locale that the three calls interpreting input document, so an address the runtime cannot serve is refused AS the address by all five. The rule, and the unbuilt-locale rows that used to live here, are asserted in `session_address_resolution.rs`".

But `tests/all/session_address_resolution.rs:12-15` and `:263-266` pin the opposite for `DeleteSession` (`Call::answers_from_a_stored_session()` -> `session_absent(&session_id)`), as does `tests/all/session_report_resolves_the_session.rs:8-15`.

## Steps to Reproduce

1. Build a bot through the mounted Lex Models V2 control plane.
2. `DELETE /bots/<botId>/botAliases/BBBBBBBBBB/botLocales/en_US/sessions/sess-1` — a well-formed `botAliasId` the bot does not have.
3. Observed: HTTP 404 `ResourceNotFoundException` "The session for user sess-1 doesn\u{2019}t exist or has expired."
4. Also `DELETE .../botLocales/en_US/...` against a bot whose locale was created but never built gives the same absent-session sentence, while the model documents a separate refusal for a locale that does not exist in the bot.
