Issue-Source: trace-sweep-divergence
Issue-Trace: qconnect-ai-agent-lifecycle
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: qconnect-ai-agent-lifecycle
Issue-Shape-Id: com.amazonaws.qconnect#WisdomService
Issue-Key: wisdom|CreateAIAgentVersion|2xx->2xx|$.aiAgent.tags.resourceArn
Issue-Key: wisdom|ListAIAgentVersions|2xx->2xx|$.aiAgentVersionSummaries[*]
Issue-Commit: 6e06957da8d63bf47f50e8aab29b9e886ef2aec8

# qconnect AI Agent version snapshots drop the agent's tags map

> _Note: this issue was identified against the code base at commit `6e06957da8d63bf47f50e8aab29b9e886ef2aec8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAIAgentVersion

## Description
Two causes, one shared fault: when an AI Agent that carries tags is versioned, the emulator materializes the version snapshot WITHOUT copying the agent's tags, so every read path that exposes a version returns an absent or empty `tags` map. The tag keys/values themselves are request-supplied (the fixture happens to tag the agent with a single key whose value is the agent ARN) — the requirement is that whatever tags the agent carries at version time are echoed on the versioned resource, not those particular literals.

Cause 1 of 2: CreateAIAgentVersion omits `aiAgent.tags`.
  AWS: Smithy (com.amazonaws.qconnect) CreateAIAgentVersion -> CreateAIAgentVersionResponse.aiAgent : AIAgentData, and AIAgentData.tags : Tags (map<TagKey,TagValue>), documented "The tags used to organize, track, or control access for this resource." CreateAIAgent accepts `tags` on input, so an agent created/tagged with a tag map is a tagged resource and the AIAgentData returned for its new version carries that same map. Recorded response: HTTP 200 with aiAgent.tags containing the agent's tag entry (key `resourceArn`, value = the unqualified ai-agent ARN the fixture tagged with).
  Emulator: HTTP 200 (status matches) but `$.aiAgent.tags.resourceArn` is missing — the tags map is not populated from the source agent at all.
  Why: a client that versions an agent and then reads `aiAgent.tags` to confirm propagation, to drive tag-based access control, or to copy tags onto a downstream resource sees an untagged agent. Tag-driven authorization and cost/ownership tooling silently treat the version as having no tags.
  Fix: the CreateAIAgentVersion handler / its version-snapshot state in the qconnect (wisdom) service crate — when cloning the live AI Agent into a version record, carry the agent's tag map onto the snapshot, and have the serializer emit `aiAgent.tags` from it.
  Traces: qconnect-ai-agent-lifecycle#task#6.

Cause 2 of 2: ListAIAgentVersions returns version summaries with an empty tags map.
  AWS: Smithy ListAIAgentVersions -> ListAIAgentVersionsResponse.aiAgentVersionSummaries : AIAgentVersionSummariesList of AIAgentVersionSummary{aiAgentSummary: AIAgentSummary, versionNumber: Version}; AIAgentSummary.tags : Tags, same doc text. The recorded response is HTTP 200 whose single summary carries versionNumber 1 and an aiAgentSummary whose `tags` map holds the agent's tag entry.
  Emulator: HTTP 200 and the summary matches on every other member (name, assistantId/assistantArn, aiAgentId/aiAgentArn with the `:1` version qualifier, type, configuration, origin CUSTOMER, description, status ACTIVE, visibilityStatus PUBLISHED, versionNumber 1) but serves `tags: {}` instead of the agent's tags, so the element compares as mismatched.
  Why: listing versions is the discovery path for an agent's history; with an empty tags map a client cannot tell a tagged version from an untagged one and any tag-based filtering over the list returns nothing.
  Fix: the ListAIAgentVersions handler / summary projection in the qconnect (wisdom) service crate — project the stored version snapshot's tag map into AIAgentSummary.tags instead of emitting an empty map. Same underlying state defect as Cause 1: fixing the snapshot to retain tags must also be reflected by this read path.
  Traces: qconnect-ai-agent-lifecycle#task#7.

Not covered by any open wisdom issue: the open DeleteMessageTemplateAttachment issue (20261122T090437Z-wisdom-qconnect-deletemessagetemplateattachment-...) owns the two attachment divergences in the other fixture, the open message-template issue (20261111T125533Z-wisdom-qconnect-message-template-write-and-version-paths-omit) concerns message-template members, and neither touches AI Agent version tags.

## Evidence
Replay of fixture qconnect-ai-agent-lifecycle (trace set simparity-wire-aws-v1):

Cause 1 — step task#6, CreateAIAgentVersion, expected status 200, actual status 200:
  $.aiAgent.tags.resourceArn: missing (expected 'arn:aws:wisdom:<region>:<account>:ai-agent/<assistantId>/<aiAgentId>', got None)
  divergence_key: service=wisdom|CreateAIAgentVersion|status=200->200|keys=$.aiAgent.tags.resourceArn

Cause 2 — step task#7, ListAIAgentVersions, expected status 200, actual status 200:
  $.aiAgentVersionSummaries[0]: value differs only in the tags member —
    expected aiAgentSummary.tags = {'resourceArn': 'arn:aws:wisdom:<region>:<account>:ai-agent/<assistantId>/<aiAgentId>'}
    actual   aiAgentSummary.tags = {}
  (all other members equal: name, assistantId, assistantArn, aiAgentId, aiAgentArn '<agent-arn>:1', type MANUAL_SEARCH, configuration.manualSearchAIAgentConfiguration.locale en_US, origin CUSTOMER, description, status ACTIVE, visibilityStatus PUBLISHED, versionNumber 1; modifiedTime not asserted)
  divergence_key: service=wisdom|ListAIAgentVersions|status=200->200|keys=$.aiAgentVersionSummaries[0]

Model evidence: com.amazonaws.qconnect#AIAgentData.tags -> Tags; com.amazonaws.qconnect#AIAgentSummary.tags -> Tags; Tags = map<TagKey (1..128, ^(?!aws:)[a-zA-Z+-=._:/]+$), TagValue (1..256)>; CreateAIAgent input accepts `tags`.

## Steps to Reproduce
1. Replay fixture qconnect-ai-agent-lifecycle from trace set simparity-wire-aws-v1 against the emulator.
2. Setup steps create an assistant and then an AI Agent (type MANUAL_SEARCH, visibilityStatus PUBLISHED) carrying a tag map, and update its description.
3. Step task#6: POST /assistants/{assistantId}/aiagents/{aiAgentId}/versions (CreateAIAgentVersion). Observe HTTP 200 whose $.aiAgent.tags omits the agent's tag entry (Cause 1).
4. Step task#7: GET /assistants/{assistantId}/aiagents/{aiAgentId}/versions (ListAIAgentVersions). Observe HTTP 200 whose $.aiAgentVersionSummaries[0].aiAgentSummary.tags is {} rather than the agent's tag map (Cause 2).
5. Expected in both: the tag map the AI Agent carried at version time, echoed verbatim.
