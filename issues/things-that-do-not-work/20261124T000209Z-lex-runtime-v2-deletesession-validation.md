Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 2af414c51a998bccb0ceedc9ab36026ce1caaf60

# DeleteSession never resolves the bot/alias/locale it is addressed to, so it removes a session under an address the runtime cannot serve

_Filed: 2026-11-24T00:02:09Z_
_Operation: DeleteSession | Class: validation_

> _Note: this issue was identified against the code base at commit `2af414c51a998bccb0ceedc9ab36026ce1caaf60`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Like `GetSession`, the handler resolves only the session map entry: it calls `remove_session` on the four-label key and never calls `crate::state::bot_model::built_locale`, which the three siblings addressed by the same four labels call before touching the store. So a DELETE addressed to a bot that does not exist, an alias that is not an alias of that bot, or a locale that was never built (or has since been deleted from the control plane) is answered purely from the session map: the stored session is removed and 200 returned when one happens to be held under those labels, and otherwise the absent-session 404 is returned for a condition the request did not hit. The model documents a bot, alias or locale that does not exist as its own error condition for this operation, and the crate's own superseded suite says exactly that the implemented rule is the one the model contradicts.

## Evidence

Smithy doc for the operation (`smithy_query(query="doc", shape="DeleteSession")`, reproduced in `src/ops/delete_session.rs:60-62`): "If you specify a bot or alias ID that doesn't exist, you receive a BadRequestException. If the locale doesn't exist in the bot, or if the locale hasn't been enables for the alias, you receive a BadRequestException."

Handler, `src/ops/delete_session.rs:191-202` — the whole of the resolution it performs:
```
if state
    .lock()
    .remove_session(&key, SystemTime::now())
    .is_none()
{
    return Err(DeleteSessionErrorModeled::ResourceNotFoundException(
        ResourceNotFoundException {
            message: crate::state::session_not_found_message(&input.session_id),
```
No `built_locale(..)` call anywhere in the file. Contrast `src/ops/put_session.rs:292-298`: "let locale = built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id).map_err(|error| locale_unavailable(error, &input.bot_id, &input.bot_alias_id))?;".

The crate contradicts itself about the rule: `tests/all/session_reads_need_no_built_model.rs:5-10` — "This suite asserted that reading or removing a session needed no model behind the locale it is addressed in, which the model contradicts: `GetSession` and `DeleteSession` document the same resolution of the bot, the alias and the locale that the three calls interpreting input document, so an address the runtime cannot serve is refused AS the address by all five" — versus `tests/all/session_address_resolution.rs:12-15`, which asserts the opposite and is what the live table (`session_address_resolution.rs:263-271`, `answers_from_a_stored_session`) pins: for every unresolvable address row, DeleteSession is expected to answer the absent-session sentence rather than the address refusal.

## Steps to Reproduce

1. Create and build a bot locale through the mounted Lex Models V2 control plane, then PutSession on /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/sess-001 — 200
2. DELETE the locale through the control plane: DELETE /bots/{botId}/botversions/DRAFT/botlocales/en_US
3. DELETE /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/sess-001 — the handler resolves nothing about the address and removes the stored session, although the model documents an error for a locale that doesn't exist and the sibling PutSession on the same address is refused 404 with the locale's own sentence
4. Also: DELETE the same path under a botAliasId that is no alias of that bot — the answer is the absent-session sentence, never a report of the unresolvable address
