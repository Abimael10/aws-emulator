Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: be68e03a3b164dfd55295662738d8b5c0a02662c

# DeleteSession never resolves the bot/alias pair or the locale, so it deletes a session addressed through a bot address that resolves to nothing

_Filed: 2026-11-21T02:56:07Z_
_Operation: DeleteSession | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `be68e03a3b164dfd55295662738d8b5c0a02662c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

DeleteSession's own documentation makes a nonexistent bot or alias ID, and a locale that does not exist in the bot or is not enabled for the alias, failure conditions of this operation. The handler checks neither: it builds a `SessionKey` from the four path labels and removes the entry. So a delete addressed to a bot/alias pair that is not an alias of that bot, or to a locale with no built model, is answered as if only the session were missing (the session-not-found 404) — and when the session does exist, the removal succeeds with 200 after the control plane has dropped the alias or the locale, where the three sibling handlers of this crate refuse the identical address with the modelled ResourceNotFoundException. The crate's own `built_locale` helper, which those siblings call before touching the store, is never called here.

## Evidence

Smithy (smithy_query query="doc" shape="DeleteSession"): "Removes session information for a specified bot, alias, and user ID. … If you specify a bot or alias ID that doesn't exist, you receive a BadRequestException. If the locale doesn't exist in the bot, or if the locale hasn't been enables for the alias, you receive a BadRequestException."
Handler, which only removes from the map — src/ops/delete_session.rs:170-191:
```
let key = SessionKey::new(&input.bot_id, &input.bot_alias_id, &input.locale_id, &input.session_id);
// Deleting a session that is not there ... is the modelled 404 ...
if state.lock().remove_session(&key, SystemTime::now()).is_none() {
    return Err(DeleteSessionErrorModeled::ResourceNotFoundException(ResourceNotFoundException { message: crate::state::session_not_found_message(&input.session_id) }).into());
}
```
Siblings resolve first: src/ops/put_session.rs:315-321 `built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id).map_err(|error| locale_unavailable(error, …))?` with the comment at :309-314 "the three that address it have to resolve to a model Amazon Lex V2 has built — the bot/alias PAIR, then that version's locale"; src/ops/recognize_utterance.rs:481-487 is the same call.
The only thing stating the exemption is a test comment, not the model — tests/all/bot_addressing.rs:75-84: "`PutSession` seeds a dialog state the locale has to define … the read and the delete address the stored session alone" / `Self::GetSession | Self::DeleteSession => false` — contradicted by the DeleteSession sentence quoted above.

## Steps to Reproduce

1. Create a bot and build locale en_US through the Lex Models V2 control plane on the same engine; PutSession .../sessions/s1 -> 200.
2. Call DeleteBotLocale for that bot version's en_US locale.
3. RecognizeText .../sessions/s1/text -> 404 ResourceNotFoundException (built_locale refuses the address).
4. DELETE /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/s1 -> 200, the session is removed although the address resolves to no bot locale.
5. Also: DELETE /bots/{botId}/botAliases/XXXXXXXXXX/botLocales/en_US/sessions/s1 answers the session-not-found refusal instead of the bot-alias refusal its siblings answer for the same pair.
