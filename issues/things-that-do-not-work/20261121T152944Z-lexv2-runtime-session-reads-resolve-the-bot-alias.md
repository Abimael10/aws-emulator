Issue-Source: trace-sweep-divergence
Issue-Trace: lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers
Issue-Shape-Id: com.amazonaws.lexruntimev2#AWSDeepSenseRunTimeServiceApi2_0
Issue-Key: lexv2-runtime|GetSession|4xx->4xx|$.message
Issue-Key: lexv2-runtime|DeleteSession|4xx->4xx|$.message
Issue-Commit: 26b10df5f12bcf30311c15b2dec769a8c0c49b96

# lexv2-runtime session reads resolve the bot/alias address before the session store, masking the session-absence refusal

> _Note: this issue was identified against the code base at commit `26b10df5f12bcf30311c15b2dec769a8c0c49b96`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSession

## Description
Shared fault, 2 causes: both session-addressed operations of the Lex V2 runtime (GetSession, DeleteSession) resolve the botId/botAliasId PAIR against the control plane BEFORE they touch the session store, and refuse a pair that resolves to no alias of that bot as a bad ADDRESS. AWS does not gate these two session operations on the bot address resolving: it answers the session-absence refusal. So for any botId/botAliasId pair that names nothing (the common case for an unknown, freshly-made-up, or already-deleted bot), the emulator returns the bot-address sentence where AWS returns the session sentence. Both causes are the same wrong branch ordering in two handlers and should be fixed together.

Cause 1 of 2 — GetSession (fixture lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers, step task#2).
AWS: GetSession is GET /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/{sessionId} and models ResourceNotFoundException at HTTP 404 alongside AccessDeniedException 403, InternalServerException 500, ThrottlingException 429 and ValidationException 400 (Smithy: com.amazonaws.lexruntimev2#GetSession errors; input botId @length 10..10 @pattern ^[0-9a-zA-Z]+$, sessionId @length 2..100 @pattern ^[0-9a-zA-Z._:-]+$, all four members @httpLabel @required). The model does NOT state which refusal wins when both the bot address and the session are absent; the operation's prose mentions only a BadRequestException for a non-existent bot/alias/session, which is not what the wire shows. Recorded behavior: with a syntactically valid but non-existent botId/botAliasId pair and a sessionId that was never created, AWS answers HTTP 404 with the session-absence message, whose FIXED text is "The session for user " + <sessionId> + " doesn\u2019t exist or has expired." — the apostrophe is U+2019 RIGHT SINGLE QUOTATION MARK, not ASCII ' — and whose only VARIABLE part is the sessionId path label copied back from the request. Nothing in the message is derived from the bot id, the alias id or the locale. Hypothesis: AWS keys sessions by the four labels and reports the lookup miss without first proving the address exists, so an unknown bot is indistinguishable from an unknown session on this read.
Emulator: HTTP 404 (status and error class agree) but the message is the bot-address sentence, whose fixed text is " is not a valid BotId/BotAliasId combination." with the request's botId and botAliasId interpolated ahead of it — i.e. "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination." The session-absence branch is unreachable whenever the pair does not resolve, because address resolution runs first and short-circuits.

Cause 2 of 2 — DeleteSession (fixture lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers, step task#3).
AWS: DeleteSession is DELETE on the same four-label URI and models the same ResourceNotFoundException at HTTP 404, plus ConflictException 409 (Smithy: com.amazonaws.lexruntimev2#DeleteSession). Its documentation says "If you specify a bot or alias ID that doesn't exist, you receive a BadRequestException", which the recording contradicts for this input: recorded behavior shows the identical session-absence refusal as GetSession — HTTP 404 with fixed text "The session for user " + <sessionId> + " doesn\u2019t exist or has expired." (U+2019), variable part the sessionId label only. A delete of a session that is not stored removes nothing and is reported as the missing session, not as a bad bot address.
Emulator: HTTP 404 with "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination." — again the address refusal, for the same reason and in the same order.

Why: both refusals are 404 ResourceNotFoundException, so the MESSAGE is the only thing a client can branch on. A caller that uses GetSession or DeleteSession to probe whether a conversation exists (session bootstrap, "resume or start", idempotent teardown, negative-path assertions) reads "no such conversation" against AWS and "this bot address is misconfigured" against the emulator, and will report a configuration fault where AWS reports an ordinary cache miss. It also makes session teardown non-idempotent in effect for any bot the runtime cannot resolve: the client cannot confirm the session is gone, only that it cannot see the bot.

Fix: the branch ORDER in each operation's handler, and the choice of refusal message in each. Both handlers must perform the session-store lookup for the four-label key and answer the session-absence refusal when the key is not stored, instead of requiring the bot/alias pair to resolve first; the bot-address refusal belongs only to the turn operations that need a runtime model. The two messages already exist separately in this crate's state module, so the change is which one the session operations select and when — not new text. Keep the existing expiry sweep behavior: an expired session is still the session-absence refusal.

Traces: lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers#task#2 (GetSession) and lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers#task#3 (DeleteSession). Code citations in the checkout: services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/src/ops/get_session.rs:219-236 (addressed_locale resolution ahead of the store read; the session_not_found_message branch at 232-236 it short-circuits); .../src/ops/delete_session.rs:219-241 (the same ordering, with the session_not_found_message branch at 235-240); .../src/state/bot_model.rs:49-51 (bot_alias_not_valid_message, the sentence actually returned).

Related open issues, neither of which covers these two steps (replay reports no owner for either): 20261121T071146Z-lexv2-runtime-getsession-gates-on-a-built-locale-and-refuses.md reports the same misclassification for GetSession but on a different trigger — a locale that EXISTS and is NotBuilt, returning the BotId/BotLocale sentence — and says nothing about an unresolvable bot/alias PAIR or about DeleteSession; 20261121T025607Z-lex-runtime-v2-getsession-wrong-behavior.md asks for the opposite (that GetSession resolve the bot address at all), and the resolution added for it is what over-applies here.

## Evidence
Replay of fixture lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers (trace set simparity-wire-aws-v1), 2 mismatched steps of 49, both status-matched with a diverging $.message.

Step task#2 — GetSession, divergence_key service=lexv2-runtime|GetSession|status=404->404|keys=$.message
  Expected (AWS): HTTP 404, ResourceNotFoundException,
    message = "The session for user <sessionId> doesn\u2019t exist or has expired."
      fixed text: "The session for user " ... " doesn\u2019t exist or has expired." (apostrophe U+2019)
      variable part: <sessionId>, the sessionId @httpLabel from the request (recorded run: sim2real-probe-rt-sess-fixed-1)
  Actual (emulator): HTTP 404, ResourceNotFoundException,
    message = "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination."
      fixed text: " is not a valid BotId/BotAliasId combination."
      variable parts: botId and botAliasId @httpLabels from the request (recorded run: AAAAAAAAAA/BBBBBBBBBB)

Step task#3 — DeleteSession, divergence_key service=lexv2-runtime|DeleteSession|status=404->404|keys=$.message
  Expected (AWS): HTTP 404, ResourceNotFoundException, same session-absence message and same fixed/variable split as above.
  Actual (emulator): HTTP 404, ResourceNotFoundException, the same "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination." address sentence.

The recorded ids above fill the templates for one run and are not themselves requirements; the requirement is the status, the error code and each message's fixed text with its variable parts taken from the request.

Model grounding (smithy_query, services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/smithy-model/com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0.json):
  GetSession    GET    /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/{sessionId}, errors: AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400
  DeleteSession DELETE same URI, errors: AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400
  botId @length 10..10 @pattern ^[0-9a-zA-Z]+$ (so a 10-char alphanumeric unknown id is VALID input and must not be refused as malformed); sessionId @length 2..100 @pattern ^[0-9a-zA-Z._:-]+$.

## Steps to Reproduce
1. Boot the emulator and address the lexv2-runtime data plane.
2. Pick a botId that is 10 alphanumeric characters and names no bot (e.g. ten 'A's) and a botAliasId that names no alias (e.g. ten 'B's), a localeId such as en_US, and a sessionId matching ^[0-9a-zA-Z._:-]+$ that was never created.
3. GetSession(botId, botAliasId, localeId, sessionId) — GET /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/{sessionId}.
   Expected: 404 ResourceNotFoundException, message "The session for user <sessionId> doesn\u2019t exist or has expired." (U+2019 apostrophe, sessionId echoed from the request).
   Actual:   404 ResourceNotFoundException, message "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination."
4. DeleteSession(same four labels) — DELETE on the same URI.
   Expected: 404 ResourceNotFoundException with the same session-absence message.
   Actual:   404 ResourceNotFoundException with the same "<botId>/<botAliasId> is not a valid BotId/BotAliasId combination." address message.
5. Equivalently, replay fixture lex-runtime-v2-runtime-rejects-malformed-and-unknown-identifiers and inspect steps task#2 and task#3; both report $.message value_mismatch at a matching 404.
