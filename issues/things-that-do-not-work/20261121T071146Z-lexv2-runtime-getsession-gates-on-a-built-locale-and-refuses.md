Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lexv2-runtime.recognize-text-rejects-an-unbuilt-locale lexv2-models__lexv2-runtime.unbuilt-bot-locale-recognize-text-is-rejected
Issue-Key: lexv2-runtime.recognize-text-rejects-an-unbuilt-locale|run#12|AssertionError|assert absent_message == (
Issue-Key: lexv2-models__lexv2-runtime.unbuilt-bot-locale-recognize-text-is-rejected|run#15|AssertionError|assert message == (
Issue-Commit: 78bdb50d943f4d05c6435838052575fa81ce08df

# GetSession gates on a BUILT locale and refuses an absent session with the bot-address message instead of "The session for user <sessionId> doesn’t exist or has expired."

> _Note: this issue was identified against the code base at commit `78bdb50d943f4d05c6435838052575fa81ce08df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSession

## Description
Cause 1 of 1: GetSession resolves the addressed bot/alias pair to a BUILT bot-locale model before it touches the session store, so every GetSession addressed to a locale that exists but was never built is refused as a bad bot address rather than as a missing session. Both failing checks observe it on the same path: a bot whose DRAFT en_US locale is authored and reports botLocaleStatus 'NotBuilt', then GetSession for a sessionId that was never created.

AWS: GetSession is modeled with ResourceNotFoundException (HTTP 404) among AccessDeniedException (403), InternalServerException (500), ThrottlingException (429) and ValidationException (400) (Smithy, com-amazonaws-lexruntimev2 model, operation GetSession). Its operation documentation says "Returns session information for a specified bot, alias, and user. ... If the bot, alias, or session identifier doesn't exist, Amazon Lex V2 returns a BadRequestException. If the locale doesn't exist or is not enabled for the alias, you receive a BadRequestException." — i.e. the locale must EXIST and be enabled for the alias; the model says nothing about the locale having been BUILT. Recorded behavior shows the distinction: against a locale that exists with botLocaleStatus 'NotBuilt', the sibling data-plane call RecognizeText on the identical four path labels is refused HTTP 404 ResourceNotFoundException with the fixed, variable-free sentence "The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request.", while GetSession for a session that does not exist is refused HTTP 404 ResourceNotFoundException with the session-absence sentence whose fixed text is "The session for user " + <sessionId> + " doesn’t exist or has expired." — the only variable part is the sessionId path label copied back from the request, and the apostrophe is U+2019 (RIGHT SINGLE QUOTATION MARK), not ASCII '. So AWS distinguishes 'this bot/locale cannot serve traffic' (RecognizeText) from 'there is no such session' (GetSession) on exactly the same address.

Emulator: GetSession answers the bot-address refusal for that input — code ResourceNotFoundException, message 'The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request.' — where AWS answers ResourceNotFoundException with 'The session for user <sessionId> doesn’t exist or has expired.'. The status and error code match; only the message class is wrong, and it is wrong because the wrong branch is taken: the built-locale resolution runs first and short-circuits, so the session-not-found message is unreachable whenever the addressed locale has no built model. Both failing checks assert the GetSession message after a refused RecognizeText and both get the bot-address sentence.

Why: a client cannot tell 'no such conversation' from 'this bot address cannot serve traffic'. Both are 404 ResourceNotFoundException, so the message is the only discriminator, and callers (and the AWS-documented contract this suite records) use GetSession's session-absence refusal to assert that a refused turn opened no session. Code that branches on that wording — session bootstrap logic, idempotent 'resume or start' flows, negative-path tests — takes the bot-misconfiguration branch against an emulator and the session branch against AWS. It also means GetSession cannot be used to probe session existence at all on any bot whose locale is not currently built, including after a locale is deleted or un-built underneath an existing session.

Fix: the GetSession handler's address-resolution step and the ordering between it and the session lookup. The handler must not require a BUILT runtime model to read a session: resolution for this read is the locale EXISTING/enabled for the alias (which does not imply built), and an addressed session that is not in the store must still produce the session-absence refusal. The bot/alias-pair refusal and the session-absence refusal are two distinct messages in this crate's state module and GetSession must select the one matching what actually failed. No change is needed to the sibling turn operations, which correctly require a built model.

Related (not a duplicate): open issue 20261121T025607Z-lex-runtime-v2-getsession-wrong-behavior.md asks for the opposite — that GetSession resolve the bot address at all. The checkout now resolves it via the built-locale helper, which over-applies that fix: it also gates on the locale being BUILT and so loses the session-absence refusal recorded here.

Traces: lexv2-runtime.recognize-text-rejects-an-unbuilt-locale (assert absent_message, check line 227) and lexv2-models__lexv2-runtime.unbuilt-bot-locale-recognize-text-is-rejected (assert message, check line 289). The second check is listed by the suite under the cross-service heading lexv2-models__lexv2-runtime, but the diverging call and the defect are both lexv2-runtime's GetSession.

## Evidence
Check lexv2-runtime.recognize-text-rejects-an-unbuilt-locale, line 227:
  assert absent_message == f"The session for user {before_session} doesn\u2019t exist or has expired."
  AssertionError: got message='The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request.'

Check lexv2-models__lexv2-runtime.unbuilt-bot-locale-recognize-text-is-rejected, line 289:
  assert message == f"The session for user {session_id} doesn\u2019t exist or has expired."
  AssertionError: got message='The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request.'

Expected (AWS), GetSession on an existing-but-NotBuilt locale with no such session:
  HTTP 404, Error.Code = ResourceNotFoundException
  Error.Message = "The session for user <sessionId> doesn\u2019t exist or has expired."
    fixed text: "The session for user " ... " doesn\u2019t exist or has expired." (U+2019 apostrophe)
    variable part: <sessionId>, the sessionId path label from the request

Actual (emulator), same call:
  HTTP 404, Error.Code = ResourceNotFoundException
  Error.Message = "The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request." (no variable part)

Both checks confirm RecognizeText on the identical address IS correctly refused with the BotId/BotLocale sentence; only GetSession's refusal is misclassified.

Code citations in the checkout:
  services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/src/ops/get_session.rs:208-220 — built_locale(...) resolution runs before the store read and maps every failure through locale_unavailable().
  same file:104-118 — locale_unavailable() returns the AddressRefused::NotFound message for a locale with no built model.
  same file:226-231 — the session_not_found_message(&input.session_id) branch, unreachable when the locale is not built.
  services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/src/state/bot_model.rs:37-39 — BOT_LOCALE_NOT_VALID_MESSAGE, the sentence actually returned.

## Steps to Reproduce
1. Through lexv2-models on the same emulator: CreateBot with an assumable role, wait for botStatus Available.
2. CreateBotLocale botVersion=DRAFT localeId=en_US; wait for botLocaleStatus NotBuilt. Author a slot type, an intent with sample utterances and a required slot (enough to build, but do NOT call BuildBotLocale).
3. Discover the service-created DRAFT test alias via ListBotAliases.
4. Confirm DescribeBotLocale still reports botLocaleStatus 'NotBuilt'.
5. lexv2-runtime RecognizeText(botId, botAliasId, localeId=en_US, sessionId=<fresh id>, text="i would like a chocolate shake") -> 404 ResourceNotFoundException "The provided BotId/BotLocale combination is not valid. ..." (correct).
6. lexv2-runtime GetSession(botId, botAliasId, localeId=en_US, sessionId=<the same fresh id>).
   Expected: 404 ResourceNotFoundException, message "The session for user <that sessionId> doesn\u2019t exist or has expired."
   Actual:   404 ResourceNotFoundException, message "The provided BotId/BotLocale combination is not valid. Please update the BotId/BotLocale combination and retry your request."
