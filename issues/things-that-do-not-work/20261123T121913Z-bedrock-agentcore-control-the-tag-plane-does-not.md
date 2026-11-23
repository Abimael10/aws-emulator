Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.agent-runtime-tag-roundtrip-and-endpoint-tag-isolation
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|ListTagsForResource|NotFoundException
Issue-Commit: 10a6073fd096e836d3399e1065e42f6a20fdae56

# bedrock-agentcore-control: the tag plane does not resolve an AgentRuntimeEndpoint ARN (404 NotFoundException for an endpoint that exists), and the service-managed DEFAULT endpoint carries no create-time tag snapshot (2 causes)

> _Note: this issue was identified against the code base at commit `10a6073fd096e836d3399e1065e42f6a20fdae56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Two net-new root causes in this service's tagging plane, both reached through an agent runtime's endpoint ARNs. Not covered by the open stub issue 20261029T212239Z-bedrock-agentcore-control-listtagsforresource-stub.md (ListTagsForResource is implemented now: it answers, it answers wrongly), nor by 20261118T170231Z (BatchPutGatewayRateLimits) or any other open bedrock-agentcore-control issue.

Cause 1 of 2: ListTagsForResource / TagResource / UntagResource refuse an existing AgentRuntimeEndpoint ARN with HTTP 404.
AWS: ListTagsForResource takes a `resourceArn` of shape `TaggableResourcesArn`, whose modelled pattern explicitly admits nested sub-resource ARNs — `^arn:(?:[^:]+)?:bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:([a-z-]+/[^/]+)(?:/[a-z-]+/[^/]+)*$` (Smithy: ListTagsForResource.resourceArn -> TaggableResourcesArn, @length 20..1011, @pattern as quoted; errors AccessDeniedException 403 / ResourceNotFoundException 404 / ValidationException 400). An agent runtime endpoint is published under exactly such an ARN, `arn:aws:bedrock-agentcore:<region>:<account>:runtime/<agentRuntimeId>/runtime-endpoint/<endpointName>` (AgentRuntimeEndpointArn). Recorded behavior: ListTagsForResource on an endpoint ARN returned by ListAgentRuntimeEndpoints answers HTTP 200 with a `tags` map — the DEFAULT endpoint's ARN reporting the runtime's creation-time tags and a freshly created named endpoint reporting an empty map. The operation's own documentation names the taggable families ("AgentCore Runtime, Browser, Browser Profile, Code Interpreter tool, and Gateway") without listing endpoints, so for the endpoint ARN this is recorded behavior only: the 200 is what the recording shows.
Emulator: the call is refused HTTP 404 with error code `NotFoundException` and message `Resource was not found` — `botocore.exceptions.ClientError: An error occurred (NotFoundException) when calling the ListTagsForResource operation: Resource was not found`. The whole message is fixed text with no variable part; the code `NotFoundException` is not even one of the operation's modelled error shapes (the model declares `ResourceNotFoundException`), so botocore raises the generic ClientError rather than the modelled exception class. The cause is the tag plane's single ARN classification: it splits the ARN's resource part at the FIRST `/` only, so `runtime/<id>/runtime-endpoint/<name>` is read as resource type `runtime` with resource id `<id>/runtime-endpoint/<name>`, which matches no stored agent runtime and so falls to `TagTarget::Absent`. The taggable-type list and the tag-set lookups likewise have no runtime-endpoint arm at all, so TagResource and UntagResource refuse the same ARN the same way: the endpoint tag surface is entirely unusable.
Why: a client that reads or writes tags on a runtime endpoint ARN — exactly the ARN the service hands it back from CreateAgentRuntimeEndpoint and ListAgentRuntimeEndpoints — is told the resource does not exist, and, because the served error code is not a modelled shape, a client branching on the modelled `ResourceNotFoundException` cannot even catch it as that error.
Fix: the tag plane's ARN classification and tag-set resolution (the shared `tag_target` / `resource_tags` / `resource_tags_mut` classification the three tagging handlers branch on) must recognise the nested runtime-endpoint resource type and reach the endpoint record's own tag map; and the not-found answer this plane serves should be the operation's modelled error shape.
Traces: bedrock-agentcore-control.agent-runtime-tag-roundtrip-and-endpoint-tag-isolation (first ListTagsForResource call made on an endpoint ARN).

Cause 2 of 2: the service-managed DEFAULT endpoint is created holding no tags, where AWS gives it a snapshot of the runtime's creation-time tags.
AWS: recorded behavior only — the DEFAULT endpoint is auto-created inside CreateAgentRuntime, and ListTagsForResource on that endpoint's OWN ARN reports the tag set the runtime was created with at that moment, and keeps reporting it unchanged after later TagResource/UntagResource calls made against the runtime's own ARN. A named endpoint created afterwards with no tags of its own reports an empty map and does not inherit the runtime's current tags.
Emulator: the managed DEFAULT endpoint record is built with an empty tag map ("A caller that named no endpoint tagged none either"), so once cause 1 is fixed ListTagsForResource on the DEFAULT endpoint ARN would report `{}` instead of the runtime's create-time tag set. Nothing in the create path copies the runtime's `tags` onto that endpoint record, and nothing freezes it against later runtime tag mutations.
Why: a client that keys deployment metadata off the DEFAULT endpoint's ARN (the address of "whatever this runtime runs now") sees an empty tag set, and cost-allocation / discovery that relies on the endpoint carrying the runtime's creation-time tags finds nothing.
Fix: CreateAgentRuntime's construction of the service-managed DEFAULT endpoint record should seed that record's own tag map from the tags the create request carried, and leave it untouched by later tag writes against the runtime's ARN (the endpoint record already has a `tags` field).
Traces: bedrock-agentcore-control.agent-runtime-tag-roundtrip-and-endpoint-tag-isolation.

## Evidence
Check bedrock-agentcore-control.agent-runtime-tag-roundtrip-and-endpoint-tag-isolation [error].

Expected (AWS, per the recording the check replays):
- create_agent_runtime(..., tags={'env': 'dev'}) -> runtime READY
- list_tags_for_resource(resourceArn=<runtimeArn>) -> 200 {'env': 'dev'}; tag_resource/untag_resource round-trip on the runtime ARN
- list_agent_runtime_endpoints(agentRuntimeId=<id>) -> 'DEFAULT' and the named endpoint, with distinct ARNs
- list_tags_for_resource(resourceArn=<defaultEndpointArn>) -> HTTP 200, tags == {'env': 'dev'} (creation-time snapshot, unaffected by the later TagResource/UntagResource on the runtime ARN)
- list_tags_for_resource(resourceArn=<namedEndpointArn>) -> HTTP 200, tags == {} (no inheritance)

Actual (emulator):
- HTTP 404, Error.Code = 'NotFoundException', Error.Message = 'Resource was not found' (fixed text, no variable part), raised as botocore.exceptions.ClientError rather than the modelled ResourceNotFoundException:
  botocore.exceptions.ClientError: An error occurred (NotFoundException) when calling the ListTagsForResource operation: Resource was not found

Source grounding for the two causes:
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/tags.rs: TAGGABLE_ARN_RESOURCES lists only runtime/gateway/browser/browser-profile/code-interpreter types; tag_target() does `resource.split_once('/')` (first separator only) and then `agent_runtimes.get(resource_id)`, so `runtime/<id>/runtime-endpoint/<name>` resolves to TagTarget::Absent -> tag_target_not_found() -> Scaffolded::not_found("Resource was not found"); resource_tags()/resource_tags_mut() have no runtime-endpoint arm.
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/list_tags_for_resource.rs lines 170-197 (and the identical branch in ops/tag_resource.rs lines 179-201) serve that refusal.
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/agent_runtime_default_endpoint.rs line 78: managed_default_endpoint() sets `tags: BTreeMap::new()`.
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state.rs line 3786: runtime_endpoint_arn() publishes `runtime/{id}/runtime-endpoint/{name}`; line 870: AgentRuntimeEndpointRecord already carries a `tags` map.

## Steps to Reproduce
1. CreateAgentRuntime with a code artifact, a usable execution role, networkConfiguration={'networkMode':'PUBLIC'} and tags={'env':'dev'}; poll GetAgentRuntime until status READY.
2. ListTagsForResource(resourceArn=<agentRuntimeArn>) -> 200 {'env':'dev'} (this works); TagResource({'env':'prod','team':'core'}) then UntagResource(['env']) on the same ARN.
3. CreateAgentRuntimeEndpoint(agentRuntimeId=<id>, name=<name>, agentRuntimeVersion='1'); poll until it leaves CREATING.
4. ListAgentRuntimeEndpoints(agentRuntimeId=<id>) and take agentRuntimeEndpointArn for 'DEFAULT' and for <name>.
5. ListTagsForResource(resourceArn=<defaultEndpointArn>) — the emulator answers HTTP 404 NotFoundException 'Resource was not found'; AWS answers 200 with the runtime's creation-time tags {'env':'dev'}. Same 404 for the named endpoint ARN (AWS: 200 with {}), and for TagResource/UntagResource against either endpoint ARN.
6. Cause 2 remains visible after the ARN resolution is fixed: the DEFAULT endpoint's record is created with an empty tag map, so it reports {} instead of the create-time snapshot.
