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
Issue-Commit: f672b96071627a2f926b4576526626783c2d131c

# wisdom: published AI Agent version responses report an empty tags map, so an agent's tags vanish from version reads

> _Note: this issue was identified against the code base at commit `f672b96071627a2f926b4576526626783c2d131c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAIAgentVersion

## Description
Two causes, one shared fault: every operation that renders a PUBLISHED AI Agent version reports the version's `tags` as an empty map, although the AI Agent the version was published from carries a tag set at that point in the lifecycle. Both causes are net-new; the only open qconnect AI-Agent issue (20261031T004936Z-qconnect-updateaiagent-answers-501-notimplemented-so-an-ai.md, UpdateAIAgent 501) is a different fault and does not cover either — the update path now answers and its description/visibilityStatus are reported correctly in this same fixture.

Cause 1 of 2: CreateAIAgentVersion omits the agent's tags from the published version (fixture qconnect-ai-agent-lifecycle, step task#6).
AWS: Smithy (com.amazonaws.qconnect) CreateAIAgentVersion output member `aiAgent` targets AIAgentData, whose `tags` member targets the `Tags` map and is documented "The tags used to organize, track, or control access for this resource." (`Tags` constrains only key pattern ^(?!aws:)[a-zA-Z+-=._:/]+$ and value length 1..256). The recorded 200 response for the publish reports `aiAgent.tags` holding the tag set the agent carries at publish time — one entry, whose key and value are whatever the fixture tagged the agent with (in this recording key `resourceArn`, value the agent's unqualified ARN; both are request-supplied values, not part of the requirement). Everything else in that response matches.
Emulator: answers 200 with the same identity, configuration, origin, status and visibilityStatus, but `aiAgent.tags` is present and EMPTY — `$.aiAgent.tags.resourceArn: missing (expected 'arn:aws:wisdom:<region>:<account>:ai-agent/<assistantId>/<aiAgentId>', got None)`.
Why: a client that publishes a version and reads the response's `tags` to drive tag-based access control, cost allocation or its own reconciliation sees an untagged resource and concludes the agent's tags were never applied (or re-applies them in a loop). Tags are a property of the AI Agent resource, so losing them on a version read makes the published version indistinguishable from an untagged one.
Fix: the CreateAIAgentVersion handler's publish/snapshot path and the AIAgentData projection it renders through — whatever the publish takes a snapshot of must carry the agent record's tag map through to the rendered version.
Hypothesis (mechanism, not asserted by the model or the recording): the record the publish snapshots holds no tags at that instant, i.e. the tag write the fixture performed earlier never reached the AI Agent record this crate stores (this endpoint's prefix `wisdom` is served by two crates, the qconnect and the wisdom model, each with its own store, so a tag write resolved against the other store would leave this one's agent untagged).

Cause 2 of 2: ListAIAgentVersions reports the same empty tags map in each version summary (fixture qconnect-ai-agent-lifecycle, step task#7).
AWS: Smithy (com.amazonaws.qconnect) ListAIAgentVersions output `aiAgentVersionSummaries` is a required list of AIAgentVersionSummary; its `aiAgentSummary` member targets AIAgentSummary, which declares the same `tags` member (`Tags` map, "The tags used to organize, track, or control access for this resource."). The recorded 200 listing reports each summary with the agent's tag set populated, alongside the version-qualified `aiAgentArn` (`<agentArn>:<versionNumber>`) and `versionNumber`.
Emulator: answers 200 with the correct version count, versionNumber, version-qualified ARN, name, type, configuration, origin, description, status and visibilityStatus, but `aiAgentSummary.tags` is `{}` — the replay reports the whole summary element as mismatched because that one member differs.
Why: the listing is the enumeration path a client uses to find a version by tag; with every summary reporting no tags, tag-based selection over AI Agent versions never matches, and a client comparing the listing against its own desired tag state sees permanent drift.
Fix: the same projection seam, reached from the ListAIAgentVersions handler — the AIAgentSummary rendered for a version must carry the agent's tag map, exactly as the working-copy summary does.

Traces: qconnect-ai-agent-lifecycle#task#6 (Cause 1), qconnect-ai-agent-lifecycle#task#7 (Cause 2). Code seams for reference: services/service-com-amazonaws-qconnect-wisdomservice/src/ops/create_ai_agent_version.rs:238 (snapshot clone) and :330-341 (response rendered from the stored version), services/service-com-amazonaws-qconnect-wisdomservice/src/state/ai_agent_versions.rs:112-126 (to_ai_agent_data / to_summary), services/service-com-amazonaws-qconnect-wisdomservice/src/state.rs:1852 and :1884 (tags_out(&self.tags) in the AIAgentData / AIAgentSummary projections), services/service-com-amazonaws-qconnect-wisdomservice/src/ops/tag_resource.rs:194 and src/state.rs:3554-3572 (the nested-resource tag-write seam).

## Evidence
Fixture qconnect-ai-agent-lifecycle, step task#6, CreateAIAgentVersion: expected 200, got 200, but `$.aiAgent.tags.resourceArn: missing (expected 'arn:aws:wisdom:us-west-2:000000000000:ai-agent/4f8e9c43-0e70-53f3-9cfa-e87e0b7a638c/e6d95fa7-8d0d-ea78-8c1e-17eaedc59efb', got None)` — the emulator's `aiAgent.tags` is an empty map. (The ARN and tag key shown are this recording's values; the requirement is that the agent's tag set is reported, whatever it contains.)

Fixture qconnect-ai-agent-lifecycle, step task#7, ListAIAgentVersions: expected 200, got 200, `$.aiAgentVersionSummaries[0]` mismatched. Expected summary tags `{'resourceArn': 'arn:aws:wisdom:us-west-2:000000000000:ai-agent/4f8e9c43-0e70-53f3-9cfa-e87e0b7a638c/e6d95fa7-8d0d-ea78-8c1e-17eaedc59efb'}`; emulator returned `'tags': {}`. Every other member of the summary agrees, including versionNumber 1, the version-qualified aiAgentArn ending `:1`, name, type MANUAL_SEARCH, origin CUSTOMER, status ACTIVE, visibilityStatus PUBLISHED and description 'Updated probe manual search agent'.

Model references: smithy_query(query="operation", operation="CreateAIAgentVersion,ListAIAgentVersions") and smithy_query(query="shape", shape="AIAgentData,AIAgentVersionSummary,AIAgentSummary,Tags") on services/service-com-amazonaws-qconnect-wisdomservice/smithy-model/com-amazonaws-qconnect-wisdomservice.json.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture qconnect-ai-agent-lifecycle. Manually: CreateAssistant; CreateAIAgent (type MANUAL_SEARCH, visibilityStatus SAVED) inside it; apply a tag to the returned aiAgentArn with TagResource; UpdateAIAgent to PUBLISHED with a new description.
3. Call CreateAIAgentVersion for that assistantId/aiAgentId (step task#6) and inspect `aiAgent.tags` in the 200 response — the emulator returns an empty map where AWS returns the agent's tag set.
4. Call ListAIAgentVersions for the same assistantId/aiAgentId (step task#7) and inspect `aiAgentVersionSummaries[0].aiAgentSummary.tags` — again empty where AWS reports the agent's tags.
