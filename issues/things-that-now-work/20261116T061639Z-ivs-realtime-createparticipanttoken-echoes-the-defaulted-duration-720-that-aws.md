Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ivs-realtime.create-participant-token-default-duration-is-720-minutes ivs-realtime.create-participant-token-defaults
Issue-Shape-Id: com.amazonaws.ivsrealtime#AmazonInteractiveVideoServiceRealTime
Issue-Key: ivs-realtime.create-participant-token-default-duration-is-720-minutes|run#7|AssertionError|assert "duration" not in token, f"duration unexpectedly present in response: {token.get('duration')!r}"
Issue-Key: ivs-realtime.create-participant-token-defaults|run#6|AssertionError|assert "duration" not in token, token.get("duration")
Issue-Commit: 9de958fc7b077b99165fa1382cc42efe4e958ebf

# CreateParticipantToken echoes the defaulted duration (720) that AWS omits from ParticipantToken

> _Note: this issue was identified against the code base at commit `9de958fc7b077b99165fa1382cc42efe4e958ebf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateParticipantToken

## Description
One net-new root cause, found by replaying the behavioral suite (2 failing checks, both verdict=mismatch; neither is marked ALREADY FILED nor OWNED by an open issue).

Cause 1 of 1: CreateParticipantToken, called with only stageArn (no `duration` in the request), answers 200 with a participantToken that CARRIES `duration: 720` — the documented default materialised into the response — where AWS answers 200 with `duration` ABSENT from the ParticipantToken it just minted.

AWS: Smithy (com.amazonaws.ivsrealtime#CreateParticipantToken, POST /CreateParticipantToken, 200): input member `duration` -> ParticipantTokenDurationMinutes (integer, @range min 1 max 20160), optional; output `participantToken` -> ParticipantToken, whose `duration` member is documented "Duration (in minutes), after which the participant token expires. Default: 720 (12 hours)." The model marks the member optional and states only the default DURATION, not that the service reports it. Recorded behavior: for a CreateParticipantToken request that omits `duration`, AWS returns the token with `participantId`, `token` and `expirationTime` populated and NO `duration` key at all — the 720-minute default is observable only through `expirationTime` (about 720 minutes after issuance, which the emulator does get right). The same recording shows `capabilities` likewise absent when the request did not set it (the PUBLISH+SUBSCRIBE default is not echoed either); whether the emulator also echoes `capabilities` is masked because the `duration` assertion fails first, so re-replay must clear both assertions. Hypothesis: AWS echoes back on this operation only what the caller supplied plus the minted identity/expiry, and resolves defaults internally.

Emulator: answers 200 but the projected ParticipantToken contains `duration: 720`. Check create-participant-token-default-duration-is-720-minutes fails with `AssertionError: duration unexpectedly present in response: 720`; check create-participant-token-defaults fails with `AssertionError: 720` on `assert "duration" not in token`. The value is the modeled default, not a per-run value: the handler mints through the shared token-record helper, which stamps the 720-minute default onto the stored record, and the record's projection serialises that member unconditionally.

Why: a client cannot distinguish a duration it requested from one the service defaulted. Code that treats presence of `duration` as "the caller sized this token" (e.g. a refresher that only re-mints tokens with an explicit lifetime, or a round-trip assertion that the response mirrors the request) takes the wrong branch against the emulator and the opposite branch against AWS. Checks of the AWS contract that assert the member's ABSENCE fail against the emulator even though status, participantId, token and expirationTime are all correct, so the divergence is invisible to a client that only checks for 200.

Fix: the response projection for the minted ParticipantToken on this path — `crate::ops::create_participant_token` handle() (it calls `crate::state::issue_token_record` and then `record.to_participant_token()`), and `to_participant_token`/`issue_token_record` in the crate's state module, where the 720-minute default is applied at mint time. Keep resolving the effective duration internally (expirationTime must stay ~720 minutes out), but serialise `duration` on a CreateParticipantToken response only when the REQUEST supplied it; apply the same rule to `capabilities`. Note this projection is SHARED with CreateStage, where the open issue 20261106T133006Z-ivs-realtime-createstage-omits-participanttoken-duration-from-the-minted-tokens.md requires the opposite (CreateStage$participantTokens[*].duration must report the effective duration, including the 720 default) — the two operations differ, so the default must be echoed per-operation rather than stamped once into the shared record projection. Fixing them by a single shared switch will re-break one of the two. Related history, neither of which covers these replay keys: the fixed issue 20261029T194619Z-ivs-realtime-refusals-use-the-wrong-error-codes.md (bundle that mentioned CreateParticipantToken echoing defaults AWS omits) and the fixed 20261109T213051Z-ivs-realtime-stage-responses-announce-upstream-live-video.md (which added participantToken duration on the stage path); the behavior is diverging again on CreateParticipantToken now.

Traces: ivs-realtime.create-participant-token-default-duration-is-720-minutes, ivs-realtime.create-participant-token-defaults.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 67/71 passed (94.4%); ivs-realtime 67/69 (97%), 2 failed, both verdict=mismatch.

[mismatch] ivs-realtime.create-participant-token-default-duration-is-720-minutes
  check line 27: assert "duration" not in token, f"duration unexpectedly present in response: {token.get('duration')!r}"
  AssertionError: duration unexpectedly present in response: 720

[mismatch] ivs-realtime.create-participant-token-defaults
  check line 24: assert "duration" not in token, token.get("duration")
  AssertionError: 720

Expected (AWS): CreateParticipantToken(stageArn=<stage>) -> 200, participantToken = {participantId, token, expirationTime} with NO `duration` key and NO `capabilities` key; expirationTime is ~720 minutes after issuance (both checks accept 715.0..720.5 minutes).
Actual (emulator): 200, participantToken includes `duration` = 720 (the modeled default). participantId, token and the ~720-minute expirationTime are correct; only the presence of the defaulted member diverges. 720 here is the model's documented default for an unspecified duration, not an account- or run-specific value. Whether `capabilities` is also echoed is not yet observed: the `duration` assertion in create-participant-token-defaults aborts before the `capabilities` assertion on line 30.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite simparity-behavioral-aws-v1.
2. CreateStage with any name to get a stageArn.
3. POST /CreateParticipantToken with ONLY stageArn set (no duration, no capabilities).
4. Inspect the 200 body: participantToken contains a `duration` member equal to 720.
5. AWS omits `duration` (and `capabilities`) entirely for a request that set neither, reporting the 720-minute default only through expirationTime. Assert `"duration" not in participantToken` and `"capabilities" not in participantToken` while still asserting expirationTime is 715-720.5 minutes in the future to see the divergence.
6. Re-replay checks ivs-realtime.create-participant-token-default-duration-is-720-minutes and ivs-realtime.create-participant-token-defaults to confirm (the second one also clears the `capabilities` assertion that is currently masked).
