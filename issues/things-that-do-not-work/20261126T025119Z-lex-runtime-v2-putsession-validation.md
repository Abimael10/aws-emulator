Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc

# PutSession stores sessionState.runtimeHints naming an intent and slot the locale does not declare, although the model states both must exist and the same handler checks the sibling names

_Filed: 2026-11-26T02:51:19Z_
_Operation: PutSession | Class: validation_

> _Note: this issue was identified against the code base at commit `dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutSession` resolves the addressed bot locale and then validates the dialog state a caller seeds against it — but only HALF of that state. `locale.check_session_state(&input.session_state)` (src/ops/put_session.rs:299-301) looks at exactly two names: `sessionState.intent.name` and `sessionState.dialogAction.slotToElicit` (src/state/bot_definition.rs:70-84). `sessionState.runtimeHints.slotHints` is keyed by an INTENT name at its first level and by a SLOT name of that intent at its second, and the model states flatly that both must exist — yet nothing in this crate ever looks those two names up.

The unchecked value is then stored and reported: `Session::replace_session_state` copies the supplied state with `..supplied` (src/state.rs:691-696), so `runtime_hints` lands on the session verbatim, and `GetSession` reports it back through `visible_session_state` (src/ops/get_session.rs:205), which does not touch the member either. A `PutSession` carrying `runtimeHints.slotHints = {"NoSuchIntent": {"NoSuchSlot": {"runtimeHintValues": [{"phrase": "x"}]}}}` therefore answers HTTP 200 and the session permanently holds hints for a dialog this bot could never be in — the very thing the sibling check exists to prevent ("the state has to name resources that bot declares, else it describes a dialog the bot could never be in", src/state/bot_definition.rs:11-13).

This is not a missing feature: the handler already holds the resolved `BotLocaleModel`, and the two lookups the rule needs are the helpers the sibling check itself uses — `BotLocaleModel::intent` (src/state/bot_model.rs:97-99) and `ModelIntent::slot` (src/state/bot_model.rs:169-173) — and the refusal vocabulary (`SeedRefused::Intent` / `SeedRefused::Slot`, src/state/bot_definition.rs:38-58) is already wired into this operation's error channel via `seed_refused` (src/ops/put_session.rs:170-175). Only the call is missing.

## Evidence

MODEL (kind A) — `smithy_query(query="doc", shape="RuntimeHints", member="slotHints")`:
  "A list of the slots in the intent that should have runtime hints added, and the phrases that should be added for each slot. The first level of the `slotHints` map is the name of the intent. The second level is the name of the slot within the intent. ... **The intent name and slot name must exist.**"
`smithy_query(query="shape", shape="SessionState")` shows `runtimeHints` is a member of the very shape `PutSessionRequest$sessionState` targets (`PutSession` input: `sessionState` (SessionState, required) — OPERATIONS.md:43).

THE CHECK THE SAME HANDLER DOES RUN (kind B) — src/ops/put_session.rs:292-301:
  `let locale = built_locale(&*self.engine, &input.bot_id, &input.bot_alias_id, &input.locale_id)`
  `    .map_err(|error| locale_unavailable(error, &input.bot_id, &input.bot_alias_id))?;`
  `locale`
  `    .check_session_state(&input.session_state)`
  `    .map_err(seed_refused)?;`

AND WHAT THAT CHECK COVERS — src/state/bot_definition.rs:70-84:
  `pub fn check_session_state(&self, state: &SessionState) -> Result<(), SeedRefused> {`
  `    let Some(named) = state.intent.as_ref() else { return Ok(()); };`
  `    let intent = self.intent(&named.name).ok_or(SeedRefused::Intent)?;`
  `    let elicited = state.dialog_action.as_ref()`
  `        .filter(|action| action.r#type == DIALOG_ACTION_ELICIT_SLOT)`
  `        .and_then(|action| action.slot_to_elicit.as_deref());`
  `    if elicited.is_some_and(|slot| intent.slot(slot).is_none()) { return Err(SeedRefused::Slot); }`
  `    Ok(())`
— `runtime_hints` is never read here, and the rule the module states for a seeded state is general, src/state/bot_definition.rs:11-13: "the state has to name resources that bot declares, else it describes a dialog the bot could never be in ([`BotLocaleModel::check_session_state`])".

THE VALUE IS STORED AND REPORTED — src/state.rs:691-696 (`replace_session_state`, called at src/ops/put_session.rs:353 `session.replace_session_state(input.session_state, now, &locale);`):
  `let (supplied, interpretations) = locale.completed_session_state(supplied);`
  `self.session_state = SessionState { active_contexts: None, originating_request_id: None, ..supplied };`
(`..supplied` carries `runtime_hints` verbatim), and src/ops/get_session.rs:205 `session_state: Some(session.visible_session_state(now)),` with `visible_session_state` (src/state.rs:634-656) never touching `runtime_hints`.

THE NAMES ARE NEVER LOOKED UP ANYWHERE — `grep -rn 'runtimeHints|runtime_hints|slotHints'` over the crate returns only src/ops/types/shared_types.rs:135 (`pub runtime_hints: Option<RuntimeHints>,`) and src/state.rs:723-724 (`if let Some(runtime_hints) = supplied.runtime_hints { self.session_state.runtime_hints = Some(runtime_hints); }`) — a store, never a check.

TEST COVERAGE: nothing pins the current acceptance — the same grep matches no file under tests/, so no suite sends `runtimeHints` at all. The contrasting rule IS pinned for the members that are checked: tests/all/bot_addressing.rs:595-650 `a_seeded_session_state_must_name_what_the_locale_defines`, rows "an intent the locale does not declare" -> `Some("The intent is invalid.")` and "eliciting a slot the intent does not declare" -> `Some("The slot is invalid.")`, under that suite's stated rule at tests/all/bot_addressing.rs:26-27 ("locale defines: an intent it does not declare, or an elicited slot that intent does not declare, is HTTP 400 `ValidationException` and stores" nothing).

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane mounted on the same engine (as tests/all/harness.rs `built_bot` does); note botId, botAliasId, localeId and the intent/slot names the locale declares.
2. POST /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/hints-1 (PutSession) with body {"sessionState": {"dialogAction": {"type": "Delegate"}, "runtimeHints": {"slotHints": {"NoSuchIntent": {"NoSuchSlot": {"runtimeHintValues": [{"phrase": "chocolate"}]}}}}}}.
3. Observe HTTP 200: the seed is accepted although `slotHints` names an intent and a slot the locale does not declare (model: "The intent name and slot name must exist.").
4. GET /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/hints-1 — `sessionState.runtimeHints.slotHints` is reported back with the undeclared `NoSuchIntent`/`NoSuchSlot` keys, so the session permanently holds hints for a dialog this bot cannot be in.
5. Contrast the names the same handler DOES check on the same request: POST .../sessions/hints-2 with {"sessionState": {"intent": {"name": "NoSuchIntent"}, "dialogAction": {"type": "Delegate"}}} — HTTP 400 ValidationException "The intent is invalid." and nothing stored (tests/all/bot_addressing.rs:621-628), and with {"intent": {"name": "<declared intent>"}, "dialogAction": {"type": "ElicitSlot", "slotToElicit": "NoSuchSlot"}} — HTTP 400 "The slot is invalid." (tests/all/bot_addressing.rs:642-649). One shape, one resolved locale, two answers depending only on which member carried the undeclared name.
