Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ivs-realtime.delete-active-ingest-configuration-without-force-rejected ivs-realtime.list-ingest-configurations-filter-by-state ivs-realtime.rtmp-publish-full-participant-lifecycle ivs-realtime.rtmp-publish-lifecycle-and-disconnect ivs-realtime.stage-active-session-absent-until-participant
Issue-Shape-Id: com.amazonaws.ivsrealtime#AmazonInteractiveVideoServiceRealTime
Issue-Key: ivs-realtime.delete-active-ingest-configuration-without-force-rejected|run#15|AssertionError|assert active_ingest is not None and active_ingest["state"] == "ACTIVE", active_ingest
Issue-Key: ivs-realtime.list-ingest-configurations-filter-by-state|_poll_until#17|AssertionError|raise AssertionError(f"timed out waiting for predicate; last observed {last!r}")
Issue-Key: ivs-realtime.rtmp-publish-full-participant-lifecycle|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: ivs-realtime.rtmp-publish-lifecycle-and-disconnect|run#14|AssertionError|assert active_ingest is not None and active_ingest["state"] == "ACTIVE", active_ingest
Issue-Key: ivs-realtime.stage-active-session-absent-until-participant|_poll_until#20|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last value={last!r}")
Issue-Commit: bc1bc4adeee7633b8641d1a5dd09706e034a0d08

# ivs-realtime stage RTMP/RTMPS ingest data plane is inert: a live publish never turns an IngestConfiguration ACTIVE, so no StageSession or Participant is ever created

> _Note: this issue was identified against the code base at commit `bc1bc4adeee7633b8641d1a5dd09706e034a0d08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIngestConfiguration

## Description
Cause 1 of 1: a real RTMP/RTMPS publish to a stage's announced ingest endpoint has no effect on ivs-realtime state. The IngestConfiguration stays INACTIVE forever, no StageSession is opened, no Participant appears, and consequently every downstream real-time fact (activeSessionId, ListStageSessions, GetParticipant, ListParticipantEvents, the ACTIVE-ingest delete refusal) is unreachable.
Failing check ids for this cause: ivs-realtime.delete-active-ingest-configuration-without-force-rejected, ivs-realtime.list-ingest-configurations-filter-by-state, ivs-realtime.rtmp-publish-full-participant-lifecycle, ivs-realtime.rtmp-publish-lifecycle-and-disconnect, ivs-realtime.stage-active-session-absent-until-participant.

AWS: Smithy (com.amazonaws.ivsrealtime) models IngestConfiguration.state as the required enum IngestConfigurationState with values ACTIVE|INACTIVE, documented as "State of the ingest configuration. It is ACTIVE if a publisher currently is publishing to the stage associated with the ingest configuration." Stage.endpoints (StageEndpoints) carries rtmp and rtmps endpoints, documented as "The endpoint to be used for IVS real-time streaming using the RTMP protocol" / "...RTMPS protocol", and IngestConfiguration.streamKey is the "Ingest-key value for the RTMP(S) protocol". So on real AWS a publisher that connects to that endpoint with that stream key joins the stage: the configuration's state flips to ACTIVE, Stage.activeSessionId (pattern ^st-[a-zA-Z0-9]+$) becomes populated, ListStageSessions lists that session, the IngestConfiguration's participantId becomes a Participant in state CONNECTED with published=true and protocol RTMP/RTMPS, ListParticipantEvents records JOINED, and the mirror transitions happen when the publisher stops or is disconnected (state back to INACTIVE, participant DISCONNECTED, a LEFT event, the stage session gaining an endTime). While a publisher is live, DeleteIngestConfiguration without force=true is refused — Smithy binds ConflictException (HTTP 409) to DeleteIngestConfiguration — with the fixed text "Unable to mutate resource <arn> while participant is actively publishing", where <arn> is the request's ingest-configuration ARN; after a successful delete, GetIngestConfiguration answers ResourceNotFoundException (HTTP 404) with the fixed text "Resource: <arn> not found", <arn> again taken from the request. ListIngestConfigurations(filterByState=ACTIVE|INACTIVE) must partition configurations by that live state.

Emulator: the publish is a no-op against emulator state. Every check that drives a publish with ffmpeg to ctx.endpoint_url(stage.endpoints.rtmp|rtmps)/<streamKey> (both the documented "<endpoint>/app/<streamKey>" form and the bare "<endpoint>/<streamKey>" fallback are tried) then polls GetIngestConfiguration and never observes state ACTIVE within the poll window; the last observed ingestConfiguration is always state=INACTIVE with a populated participantId, e.g. {... 'ingestProtocol': 'RTMP', 'state': 'INACTIVE', 'participantId': <id>, 'stageArn': <stage arn> ...}. rtmp-publish-full-participant-lifecycle (RTMPS, 60 s budget) does not even complete inside the check timeout. Because the ingest never goes ACTIVE, nothing downstream is reached, and ListIngestConfigurations(filterByState=ACTIVE) therefore never contains a publishing configuration. Hypothesis (neither the model nor the recordings state which): the emulator announces rtmp/rtmps endpoints but either serves no RTMP/RTMPS listener behind them or accepts the connection without binding the stream key to the IngestConfiguration, so the state machine is never driven.

Why: a client that uses IVS real-time the way it is meant to be used — publish to a stage, then observe the session — sees nothing happen. Ingest never reports ACTIVE, so publisher-health polling never succeeds; Stage.activeSessionId stays absent, so session discovery fails; GetParticipant/ListParticipantEvents never show a participant, so presence, moderation and analytics code paths cannot run; and the protective ConflictException on deleting a live ingest configuration never fires, so tooling that relies on AWS refusing a destructive delete while a publisher is live gets a silent success here. The emulator reads as a stage that nobody can ever join.

Fix: the ivs-realtime crate's RTMP/RTMPS stage-ingest listener and the state machine that joins it to control-plane state — the stream-key-to-IngestConfiguration lookup on connect, the IngestConfiguration.state transition (INACTIVE->ACTIVE on publish, ACTIVE->INACTIVE on publisher stop or DisconnectParticipant), StageSession creation/activeSessionId publication and endTime on last-participant-leave, Participant creation with state CONNECTED/published=true/protocol RTMP|RTMPS and the JOINED/LEFT participant events, plus the filterByState branch of ListIngestConfigurations and the ConflictException guard in the DeleteIngestConfiguration handler that depends on the ACTIVE state.

Related but not covering: open issue 20261126T045531Z-ivs-native-rtmps-live-path-still-never-goes.md and 20261124T065254Z-ivs-reopens-ivs-channel-rtmps-ingest-remains-undialable-createchannel.md describe the ivs (channel) RTMPS path, a different service and a different resource; neither covers ivs-realtime stage ingest.

Traces: the five ivs-realtime checks listed above.

## Evidence
Expected (AWS): after ffmpeg publishes to <stage.endpoints.rtmp>/app/<streamKey>, GetIngestConfiguration(arn) returns ingestConfiguration.state == "ACTIVE"; GetStage(arn).stage.activeSessionId is populated (^st-[a-zA-Z0-9]+$) and appears in ListStageSessions(stageArn); GetParticipant(stageArn, sessionId, participantId) returns state == "CONNECTED", published == true, protocol == "RTMP"/"RTMPS"; DeleteIngestConfiguration(arn) without force fails 409 ConflictException "Unable to mutate resource <ingest arn> while participant is actively publishing" (fixed text; <ingest arn> from the request) and leaves the configuration ACTIVE; ListIngestConfigurations(filterByState="ACTIVE") contains that arn and filterByState="INACTIVE" does not; after the publisher stops/DisconnectParticipant, state returns to INACTIVE, the participant is DISCONNECTED, ListParticipantEvents contains JOINED and LEFT, the stage session gains an endTime, and a forced or post-INACTIVE delete makes GetIngestConfiguration fail 404 ResourceNotFoundException "Resource: <ingest arn> not found".

Actual (emulator): GetIngestConfiguration keeps returning state == "INACTIVE" for the whole poll window while ffmpeg publishes.
- ivs-realtime.delete-active-ingest-configuration-without-force-rejected: AssertionError on `active_ingest["state"] == "ACTIVE"`, last value state='INACTIVE' (ingestProtocol RTMP, participantId populated).
- ivs-realtime.list-ingest-configurations-filter-by-state: AssertionError "timed out waiting for predicate; last observed {... 'state': 'INACTIVE' ...}" after 20 s.
- ivs-realtime.rtmp-publish-full-participant-lifecycle: "Check timed out after 60.0s" while polling for state ACTIVE over RTMPS.
- ivs-realtime.rtmp-publish-lifecycle-and-disconnect: AssertionError on `active_ingest["state"] == "ACTIVE"`, last value state='INACTIVE'.
- ivs-realtime.stage-active-session-absent-until-participant: AssertionError "condition not met within 20.0s; last value={... 'state': 'INACTIVE' ...}" — the fresh-stage assertions (no activeSessionId, empty ListStageSessions) pass, only the positive control fails, confirming the emulator models absence but never presence.
(The ARNs, stream keys, participant ids and stage ids in those payloads are per-run values, not requirements.)

## Steps to Reproduce
1. Boot the emulator and point an ivs-realtime client at it.
2. CreateStage(name=<any>); read stage.endpoints.rtmp (and .rtmps) from the response or GetStage.
3. CreateIngestConfiguration(name=<any>, stageArn=<stage arn>, ingestProtocol="RTMP", insecureIngest=true); note arn, streamKey and participantId. Confirm state == "INACTIVE".
4. Publish a live stream to the announced endpoint with the stream key, e.g. `ffmpeg -re -f lavfi -i testsrc=size=640x360:rate=30 -f lavfi -i sine=frequency=1000 -pix_fmt yuv420p -c:v libx264 -b:v 800k -c:a aac -t 60 -f flv <rtmp endpoint>/app/<streamKey>` (also try `<rtmp endpoint>/<streamKey>`), with the endpoint host rewritten to the emulator's reachable address.
5. Poll GetIngestConfiguration(arn=<ingest arn>) every 2 s for 20-60 s: the emulator keeps returning state "INACTIVE"; AWS returns "ACTIVE".
6. While the publish is running, call GetStage(arn=<stage arn>) and ListStageSessions(stageArn=<stage arn>): no activeSessionId and no sessions on the emulator.
7. While the publish is running, call ListIngestConfigurations(filterByState="ACTIVE"): the publishing configuration is absent.
8. While the publish is running, call DeleteIngestConfiguration(arn=<ingest arn>) with no force: AWS refuses with 409 ConflictException "Unable to mutate resource <ingest arn> while participant is actively publishing"; the emulator has no ACTIVE state to refuse from.
9. Repeat steps 3-5 with ingestProtocol="RTMPS" against stage.endpoints.rtmps for the same result.
