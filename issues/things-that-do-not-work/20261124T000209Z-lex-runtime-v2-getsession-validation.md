Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 2af414c51a998bccb0ceedc9ab36026ce1caaf60

# GetSession never resolves the bot/alias/locale it is addressed to, so a stored session is reported for a locale that no longer exists

_Filed: 2026-11-24T00:02:09Z_
_Operation: GetSession | Class: validation_

> _Note: this issue was identified against the code base at commit `2af414c51a998bccb0ceedc9ab36026ce1caaf60`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler resolves ONLY the session held under the four `@httpLabel` values: it locks the store and looks the key up, with no call to `crate::state::bot_model::built_locale`, which every sibling that is addressed by the same four labels calls before touching the store. Consequence: when the `botId`/`botAliasId` pair or the `localeId` names nothing the runtime can serve, GetSession answers from the session map alone — HTTP 200 with the stored conversation if a session happens to be stored under those labels (the crate's own test asserts exactly that for a locale the control plane has deleted), and otherwise the generic absent-session 404 that names a condition the request did not hit. The model documents the opposite: a bot, alias or locale that does not exist is itself an error condition for this operation. The crate is self-contradictory about which rule it follows — the superseded suite `session_reads_need_no_built_model.rs` states that the "reads resolve no address" rule is what the MODEL contradicts and claims the corrected rule is asserted in `session_address_resolution.rs`, while that file asserts the un-corrected rule instead.

## Evidence

Smithy doc for the operation (`smithy_query(query="doc", shape="GetSession")`, reproduced in `src/ops/get_session.rs:58-60`): "If the bot, alias, or session identifier doesn't exist, Amazon Lex V2 returns a BadRequestException. If the locale doesn't exist or is not enabled for the alias, you receive a BadRequestException."

Handler, `src/ops/get_session.rs:194-199` — the whole of the resolution it performs:
```
let mut store = state.lock();
let session = store.session(&key, now).ok_or_else(|| {
    GetSessionErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
        message: crate::state::session_not_found_message(&input.session_id),
```
No `built_locale(..)` call anywhere in the file. Contrast the sibling `src/ops/recognize_text.rs:258-264`: "let model: BotLocaleModel = built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id)" and `src/ops/put_session.rs:292-298`, which do resolve the address first.

The crate contradicts itself about the rule. `tests/all/session_reads_need_no_built_model.rs:5-10`: "This suite asserted that reading or removing a session needed no model behind the locale it is addressed in, which the model contradicts: `GetSession` and `DeleteSession` document the same resolution of the bot, the alias and the locale that the three calls interpreting input document, so an address the runtime cannot serve is refused AS the address by all five. The rule, and the unbuilt-locale rows that used to live here, are asserted in `session_address_resolution.rs`". But `tests/all/session_address_resolution.rs:12-15` asserts the inverse: "The two calls that only REPORT or REMOVE the session held under those labels resolve no address: the session is stored under the whole address, so a lookup that finds none is reported as the session that is not there, whichever label of the address names nothing."

The 200 is pinned by `tests/all/bot_addressing.rs:523-533`: "// And the READ still reports the session while the locale is dropped … let while_dropped = emulator.send(\"GET\", &path, None).await; assert_eq!(while_dropped.status, 200, …)" — i.e. after a successful `DELETE /bots/{id}/botversions/DRAFT/botlocales/{locale}` on the control plane, GetSession answers 200 for a locale that no longer exists.

## Steps to Reproduce

1. Create and build a bot locale through the mounted Lex Models V2 control plane (as tests/all/harness.rs `built_bot` does)
2. PutSession on /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/sess-001 with {"sessionState":{"dialogAction":{"type":"Delegate"}}} — 200
3. DELETE the locale through the control plane: DELETE /bots/{botId}/botversions/DRAFT/botlocales/en_US
4. GET /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/sess-001 — the handler answers 200 with the stored session although the locale does not exist; the model documents an error for a locale that doesn't exist, and PutSession/RecognizeText/RecognizeUtterance on the same address are refused 404 with the locale's own sentence (bot_addressing.rs:495-521)
5. Also: GET the same path under a botAliasId that is no alias of the bot — the answer is the absent-session sentence rather than any report of the unresolvable address
