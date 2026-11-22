Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: be68e03a3b164dfd55295662738d8b5c0a02662c

# GetSession never resolves the botId/botAliasId pair or the locale, so a session is read back for a bot address that resolves to nothing

_Filed: 2026-11-21T02:56:07Z_
_Operation: GetSession | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `be68e03a3b164dfd55295662738d8b5c0a02662c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model documents, in GetSession's own operation documentation, that the bot, the alias and the locale a request addresses are resolved: a bot/alias that does not exist, and a locale that does not exist or is not enabled for the alias, are refused. The handler resolves none of them. It builds a `SessionKey` out of the four path labels and looks the map up, so:

1. a `GetSession` addressed to a botId/botAliasId pair that is not an alias of that bot is answered with the session-not-found refusal ("The session for user … doesn\u2019t exist or has expired."), i.e. the absence of a session rather than the absence of the bot address — while the three sibling handlers in this same crate answer the bot-address refusal for exactly that input; and
2. once a session exists, a later control-plane change that removes the addressed alias or un-builds/deletes the locale leaves `GetSession` answering HTTP 200 with the whole session, while `RecognizeText`, `RecognizeUtterance` and `PutSession` on the identical path answer the modelled 404. The read fails open on addressing: the crate's own `crate::state::bot_model::built_locale` helper is never called from this handler.

## Evidence

Smithy (smithy_query query="doc" shape="GetSession"): "Returns session information for a specified bot, alias, and user. … If the bot, alias, or session identifier doesn't exist, Amazon Lex V2 returns a BadRequestException. If the locale doesn't exist or is not enabled for the alias, you receive a BadRequestException."
Handler, which touches only the session map — src/ops/get_session.rs:171-188:
```
let key = SessionKey::new(&input.bot_id, &input.bot_alias_id, &input.locale_id, &input.session_id);
// A read: the store is only looked up ...
let mut store = state.lock();
let session = store.session(&key, now).ok_or_else(|| { ... ResourceNotFoundException { message: crate::state::session_not_found_message(&input.session_id) } })?;
```
Sibling handlers in the SAME crate do resolve the pair and the locale first — src/ops/recognize_text.rs:268-274: `let model: BotLocaleModel = built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id).map_err(|error| locale_unavailable(error, &input.bot_id, &input.bot_alias_id))?;` with the comment at :261-267 "the PAIR is resolved, not the two ids separately, and resolution happens BEFORE the session store is touched"; src/ops/put_session.rs:315-321 is the same call.
The exemption is only asserted by a comment, not by the model — tests/all/bot_addressing.rs:79-84: "fn needs_the_runtime_model(&self) -> bool { match self { Self::GetSession | Self::DeleteSession => false, …" with the stated reason at :75-78 "the read and the delete address the stored session alone" — which the GetSession documentation sentence quoted above contradicts. No test asserts a 200 for a GetSession whose alias or locale does not resolve (tests/all/bot_addressing.rs:129-134 filters GetSession out of the rule-1 table).

## Steps to Reproduce

1. Through the Lex Models V2 control plane on the same engine, create a bot, create and build locale en_US (tests/all/harness.rs `built_bot`).
2. PutSession /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/s1 with a valid sessionState -> 200, session stored.
3. Call DeleteBotLocale for that bot version's en_US locale (implemented in service-com-amazonaws-lexmodelsv2-lexmodelbuildingservicev2).
4. RecognizeText on .../sessions/s1/text -> 404 ResourceNotFoundException "The provided BotId/BotLocale combination is not valid. …" (built_locale refuses).
5. GET .../sessions/s1 -> 200 with the full session: GetSession resolved nothing.
6. Also: GET /bots/0123456789/botAliases/XXXXXXXXXX/botLocales/en_US/sessions/s9 answers the session-not-found refusal, where the siblings answer "0123456789/XXXXXXXXXX is not a valid BotId/BotAliasId combination."
