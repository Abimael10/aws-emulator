Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f89783cb4c796b94ed6e2a4b91d09cf066e599c6

# A caller-supplied sessionState on a turn is stored unchecked: neither the locale check PutSession applies nor the shape's own modelled constraints are enforced on the turn path

_Filed: 2026-11-25T11:49:12Z_
_Operation: RecognizeText | Class: validation_

> _Note: this issue was identified against the code base at commit `f89783cb4c796b94ed6e2a4b91d09cf066e599c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both turn operations let the caller supply a `sessionState` that is merged into the stored session (`take_turn` -> `open_turn` -> `merge_session_state`). Nothing on that path validates it, so the session store ends up holding, through the turn door, exactly the dialog state the crate refuses at the `PutSession` door, and a later `GetSession` reports it back.

Symptom 1 — the locale check is skipped. `BotLocaleModel::check_session_state` exists in this crate and is documented as the rule for a caller-seeded state: "the state has to name resources that bot declares, else it describes a dialog the bot could never be in". `PutSession` calls it right after resolving the locale (`put_session.rs:299-301`) and answers HTTP 400 `ValidationException` "The intent is invalid." / "The slot is invalid.". `RecognizeText` (`recognize_text.rs:258-289`) and `RecognizeUtterance` (`recognize_utterance.rs:475-504`) resolve the SAME `BotLocaleModel` through the same `built_locale` helper and then hand the caller's `session_state` straight to `take_turn` without ever calling it. So `RecognizeText` with `sessionState.intent.name = "NoSuchIntent"` is 200, the undeclared intent is persisted as the session's active intent, and `GetSession` reports it — while the identical state through `PutSession` on the same session is 400 and stores nothing. The crate's own `nlu.rs:335-345` branch records that this state does arrive ("An intent the locale does not declare — a name a caller put in the session itself"), i.e. the code knows about the hole the sibling write path closes. The reason the crate gives for refusing such a state is a property of the STATE, not of the operation that delivered it.

Symptom 2 — on `RecognizeUtterance` the shape's own modelled constraints are never applied either. That operation's `sessionState` is modelled as a plain header STRING, so the framework's constraint walk (`constraints::validate(&input, O::INPUT_SCHEMA)`) only ever sees a string; the structure is materialised inside the handler by `decode_struct`, which deserializes and nothing more. Every constraint the model puts inside `SessionState` is therefore unenforced on this encoding: `ActiveContextTimeToLive$timeToLiveInSeconds` `@range(5..86400)`, `turnsToLive` `@range(1..20)`, `ActiveContextsList` `@length(0..20)`, `ActiveContextName`'s length/pattern, `originatingRequestId`'s `@length(min:1)`. The values are stored and reported: `set_active_contexts` copies `turnsToLive` verbatim into `turns_remaining` and `live_contexts` writes it back into the reported `turnsToLive`, and `merge_session_state` assigns a supplied `originatingRequestId` with no emptiness test, after which `visible_session_state` reports it on every state — an empty string for a member the model declares `NonEmptyString`. The identical values in a body are refused 400 (the crate's own test pins exactly that for `PutSession`), and the crate's sibling header decoder `decode_string_map` deliberately re-applies the one modelled constraint of its own shape after decoding, so the crate already knows a header-decoded payload must be re-checked — it just does not do it for the struct.

## Evidence

Smithy — `smithy_query(query="shape", shape="ActiveContextTimeToLive")`: `timeToLiveInSeconds` `"required": true, "range": {"min": 5, "max": 86400}`; `turnsToLive` `"required": true, "range": {"min": 1, "max": 20}`. `smithy_query(query="shape", shape="SessionState")`: `activeContexts` `"length": {"min": 0, "max": 20}`, `originatingRequestId` -> `NonEmptyString` `"length": {"min": 1}`.

The helper and the rule it states — src/state/bot_definition.rs:11-13: "the state has to name resources that bot declares, else it describes a dialog the bot could never be in ([`BotLocaleModel::check_session_state`])"; src/state/bot_definition.rs:62-64: "/// Refuse a `sessionState` a caller seeded that names something this locale /// does not define: the intent it names has to be an intent of the locale, /// and the slot its dialog action elicits has to be a slot of that intent."

The sibling write path calls it — src/ops/put_session.rs:299-301:
  `locale`
  `    .check_session_state(&input.session_state)`
  `    .map_err(seed_refused)?;`

The turn path does not — src/ops/recognize_text.rs:258-289 resolves the same model and goes straight to the turn:
  `let model: BotLocaleModel = built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id)`
  `    .map_err(|error| locale_unavailable(error, &input.bot_id, &input.bot_alias_id))?;`
  ... `let turn = crate::state::take_turn(&state, key, now, &input.text, &model, input.session_state, input.request_attributes)`
and src/ops/recognize_utterance.rs:475-504 does the same with the decoded header value. `grep -rn check_session_state` over the crate returns only src/state/bot_definition.rs:12,70,279 (its own unit test) and src/ops/put_session.rs:300 — no turn handler calls it.

The code records that the unchecked state arrives — src/state/nlu.rs:335-339: "// An intent the locale does not declare — a name a caller put in the // session itself — has no slots here to elicit, so this turn elicits // nothing, the dialog closes, and the state the session carries for it // stands".

Symptom 2, the constraint walk cannot see inside the header member — OPERATIONS.md:57 lists RecognizeUtterance's input as `sessionState` (string, @httpHeader(x-amz-lex-session-state)), and the only constraint pass is vendor/smithy-server-runtime/src/operation.rs:481: `let violations = aws_smithy_schema_server::constraints::validate(&input, O::INPUT_SCHEMA);`. The handler materialises the structure itself — src/ops/recognize_utterance.rs:453-458:
  `let session_state = input.session_state.as_deref()`
  `    .map(|value| decode_struct::<SessionState>("sessionState", value))`
  `    .transpose().map_err(rejected)?;`
and `decode_struct` claims only shape-level checks — src/state/headers.rs:156-159: "The structure is read with the service's own `restJson1` codec and its own schema, so a payload that is JSON but not this shape — a required member missing, a member of the wrong type — is refused here rather than silently dropped." (no constraint check, and none is performed: src/state/headers.rs:164-169 is `decode_bytes` + `T::deserialize`).

The crate's sibling header decoder DOES re-apply its shape's modelled constraint — src/state/headers.rs:176-179 and :191-196: "So the payload is read straight into the map, a value that is not a string is refused, and so is the one constraint the shape does carry — an empty key." / `if decoded.keys().any(String::is_empty) { return Err(HeaderRejected::new(member, "its keys must each be at least 1 character long")); }`

The unvalidated values are then stored and reported — src/state.rs:586-595 `set_active_contexts` (`turns_remaining: context.time_to_live.turns_to_live`), src/state.rs:619-628 `live_contexts` (`context.time_to_live.turns_to_live = stored.turns_remaining;`), src/state.rs:717-722 (`self.originating_request_id = originating_request_id;`), src/state.rs:645-648 (`state.originating_request_id = Some(self.originating_request_id.clone());`).

Test coverage: the crate pins the refusal for the BODY encoding of the very same values — tests/all/session_lifecycle.rs:758-779 rows `("context time to live below the range", ... "timeToLiveInSeconds": 4 ... , 400)` and `("context turns to live above the range", ... "turnsToLive": 21 ... , 400)`, under the suite's stated rule 4 at tests/all/session_lifecycle.rs:20-21 "A value outside a modelled constraint is refused with HTTP 400 `ValidationException`". The locale check is pinned only through PutSession — tests/all/bot_addressing.rs:605-650 (row "an intent the locale does not declare" -> `Some("The intent is invalid.")`, posted to `bot.session_path(&session_id)`), and its suite rule is written for that operation (tests/all/bot_addressing.rs:25-28). No test drives either check through `/text` or `/utterance`, so nothing pins the current acceptance.

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane mounted on the same engine (as tests/all/harness.rs `built_bot` does); note botId, botAliasId, localeId and a declared intent name.
2. Symptom 1: POST /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/s1/text with body {"text": "hello", "sessionState": {"dialogAction": {"type": "ElicitSlot", "slotToElicit": "NoSuchSlot"}, "intent": {"name": "NoSuchIntent", "state": "InProgress"}}}. Observe HTTP 200 and `sessionState.intent.name` = "NoSuchIntent" in the response.
3. GET the same session path: the stored session reports the undeclared intent as its active intent.
4. Contrast the sibling door: POST /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/s2 (PutSession) with {"sessionState": {"dialogAction": {"type": "ElicitSlot", "slotToElicit": "NoSuchSlot"}, "intent": {"name": "NoSuchIntent", "state": "InProgress"}}} — HTTP 400 ValidationException "The intent is invalid." and nothing stored (tests/all/bot_addressing.rs:621-628).
5. Symptom 2: POST .../sessions/s3/utterance with `Content-Type: text/plain; charset=utf-8`, body `hello`, and header `x-amz-lex-session-state` = gzip-then-base64 of {"dialogAction":{"type":"Delegate"},"originatingRequestId":"","activeContexts":[{"name":"c","timeToLive":{"timeToLiveInSeconds":4,"turnsToLive":1000},"contextAttributes":{}}]}. Observe HTTP 200.
6. GET .../sessions/s3 within 4 seconds: `sessionState.activeContexts[0].timeToLive.turnsToLive` is 1000 (model max 20), `timeToLiveInSeconds` is 4 (model min 5), and `sessionState.originatingRequestId` is the empty string (model: NonEmptyString, @length min 1).
7. Send the SAME structure as the body of PutSession on a fresh session id ({"sessionState": {... "timeToLiveInSeconds": 4 ...}}): HTTP 400 ValidationException, per tests/all/session_lifecycle.rs:758-768 — one shape, two answers depending only on which encoding carried it.
