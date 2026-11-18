Issue-Source: trace-sweep-divergence
Issue-Trace: application-insights-component-configuration-lifecycle
Issue-Step: setup#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: application-insights-component-configuration-lifecycle application-insights-workload-lifecycle
Issue-Shape-Id: com.amazonaws.applicationinsights#EC2WindowsBarleyService
Issue-Key: applicationinsights|DescribeComponentConfiguration|2xx->4xx|$
Issue-Key: applicationinsights|DescribeComponentConfiguration|2xx->4xx|status
Issue-Key: applicationinsights|DescribeComponentConfiguration|2xx->4xx|$.ComponentConfiguration
Issue-Key: applicationinsights|DescribeComponentConfiguration|2xx->4xx|$.Monitor
Issue-Key: applicationinsights|DescribeComponentConfiguration|2xx->4xx|$.Tier
Issue-Key: applicationinsights|UpdateComponentConfiguration|2xx->4xx|$
Issue-Key: applicationinsights|UpdateComponentConfiguration|2xx->4xx|status
Issue-Commit: 0685ecf4ce56006659cf4e34833f0a1282c52a8c

# application-insights: a discovered component is unknown to DescribeComponentConfiguration and UpdateComponentConfiguration

> _Note: this issue was identified against the code base at commit `0685ecf4ce56006659cf4e34833f0a1282c52a8c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeComponentConfiguration

## Description
FOUR causes, all in applicationinsights, all net-new wire divergences from this replay (none marked stub_path, none reported owned_by an open issue). Causes 1-3 are the same monitoring-configuration surface seen from three different calls; cause 4 is the status the refusal itself is served with. A fixer must close all four before submitting.

Prior art, so this is not read as a re-file: the open BEHAVIORAL issue 20261117T113145Z-application-insights-component-onboarding-still-yields-nothing-a.md reports that a TAG_FILTERS_1_0 resource group resolves to no members so ListComponents stays empty, and the open issue 20261110T060637Z-application-insights-an-ec2-instance-in-the-application.md describes the EC2/custom-component surface. Neither carries the replay key of any step below, and the replay reports these clusters unowned, so they are filed here; causes 1-3 are the WIRE-visible consequence on DescribeComponentConfiguration/UpdateComponentConfiguration and cause 4 is not described by either.

Cause 1 of 4: DescribeComponentConfiguration refuses the DEFAULT (standalone) component an application onboarded from its resource group, instead of reporting that component's monitoring configuration. Fixture application-insights-component-configuration-lifecycle step setup#5 (cluster also covers application-insights-workload-lifecycle#setup#5).
AWS: Smithy com.amazonaws.applicationinsights#DescribeComponentConfiguration - input ResourceGroupName @required @length 1..256 @pattern ^[a-zA-Z0-9\.\-_]*$ and ComponentName @required @length 1..1011 @pattern ^(?:^[\d\w\-_\.+]*$)|(?:^arn:aws(-\w+)*:[\w\d-]+:([\w\d-]*)?:[\w\d_-]*([:/].+)*$)$ (the pattern admits a bare ARN precisely because a discovered component is named by its resource's ARN); output Monitor (boolean), Tier (enum, member DEFAULT), ComponentConfiguration (@length 1..10000); success HTTP 200; errors ValidationException 400 / ResourceNotFoundException 404 / InternalServerException 500. Creating an application over a resource group onboards every supported resource that group resolves to as a standalone component named by that resource's own ARN, in tier DEFAULT and unmonitored, so DescribeComponentConfiguration for that ARN answers 200 with Monitor=false and Tier=DEFAULT. The rule is the requirement - one standalone component per resolved resource, addressable by its ARN - not any particular ARN or bucket name.
Emulator: answers HTTP 400 with {"__type": "ResourceNotFoundException", "Message": "Custom component <component-name> does not exist for <resource-group-name>"}. In the recorded step the variable parts are filled with the request's ComponentName (the tagged bucket's ARN) and ResourceGroupName; the fixed text is "Custom component " ... " does not exist for " (crate::state::component_missing_message, services/.../src/state.rs:52-54). The step is a poll and it never flips: 24 attempts, 400 every time, because the addressed application's `components` map is empty - onboarding resolves a group membership that comes back empty, so no discovered component is ever keyed by the resource ARN the caller addresses.
Why: a client that creates an application over a resource group and then reads or configures one of its resources - the documented onboarding sequence - gets a hard 400 ResourceNotFoundException for a component AWS does have, and it is told the component is a "Custom component", which is a different concept from the standalone component it asked about. Every monitoring-configuration workflow (read current config, turn monitoring on, verify) stops at the first call, and a client that retries sees no eventual-consistency convergence either.
Fix: the application's component state and the onboarding that populates it - the group-membership resolution the create handler depends on (services/service-com-amazonaws-applicationinsights-ec2windowsbarleyservice/src/state/resource_groups.rs::group_resources and crate::state::onboarded_components, invoked from src/ops/create_application.rs) - must key a component by each resolved resource's ARN, and keep that set in step with the group over the application's life rather than computing it once at create time. The two handlers themselves already look the component up correctly.
Traces: application-insights-component-configuration-lifecycle#setup#5, application-insights-workload-lifecycle#setup#5.

Cause 2 of 4: the same refusal on DescribeComponentConfiguration after the flow has configured the component, so the response carries no Monitor/Tier/ComponentConfiguration at all. Fixture application-insights-component-configuration-lifecycle steps task#6 and task#8.
AWS: as cited in cause 1. A DescribeComponentConfiguration that follows an UpdateComponentConfiguration must report back exactly what was stored: Monitor as last set, Tier as last set (DEFAULT in this flow), and ComponentConfiguration as the escaped JSON the update supplied. HTTP 200 with those three members present.
Emulator: HTTP 400 with the same ResourceNotFoundException body, so the response has none of the three members: $.ComponentConfiguration missing (expected the stored escaped JSON, got None), $.Monitor missing (expected false, got None), $.Tier missing (expected "DEFAULT", got None). Same root as cause 1 - the component was never onboarded, so there was nothing for the earlier configure call to write and nothing for this read to report.
Why: a client cannot read back a configuration it just wrote, and cannot observe the Monitor flag that the application's LifeCycle (NOT_CONFIGURED -> ACTIVE) is derived from. Read-after-write on component configuration is unusable.
Fix: same component as cause 1 (the onboarded-component state); the read path in src/ops/describe_component_configuration.rs:192-210 is correct once the component exists.
Traces: application-insights-component-configuration-lifecycle#task#6, application-insights-component-configuration-lifecycle#task#8.

Cause 3 of 4: UpdateComponentConfiguration refuses the same discovered component instead of storing the configuration. Fixture application-insights-component-configuration-lifecycle step task#7.
AWS: Smithy com.amazonaws.applicationinsights#UpdateComponentConfiguration - input ResourceGroupName @required, ComponentName @required (same 1..1011/ARN-admitting pattern), Monitor, Tier (enum), ComponentConfiguration (@length 1..10000, "the escaped JSON of the configuration"), AutoConfigEnabled; output is an EMPTY structure; success HTTP 200 with an empty JSON body; errors ValidationException 400 / ResourceInUseException 400 / ResourceNotFoundException 404 / InternalServerException 500. Configuring a standalone component the application onboarded succeeds and persists Monitor/Tier/ComponentConfiguration.
Emulator: HTTP 400 with {"__type": "ResourceNotFoundException", "Message": "Custom component <component-name> does not exist for <resource-group-name>"} where the expected body is {} - same fixed text and same request-derived variable parts as cause 1 (src/ops/update_component_configuration.rs:214-226 via crate::state::component_missing_message).
Why: monitoring can never be turned on for a discovered resource, so the application never leaves NOT_CONFIGURED and no downstream monitoring behavior is reachable. The write is not merely unreported - it is refused, so nothing in the emulator's state changes.
Fix: same component as cause 1 (the onboarded-component state the update handler mutates).
Traces: application-insights-component-configuration-lifecycle#task#7.

Cause 4 of 4: when a component genuinely is not found, the modeled ResourceNotFoundException is served with HTTP 400 instead of HTTP 404. Observed in the actual responses of all three clusters above.
AWS: Smithy com.amazonaws.applicationinsights#ResourceNotFoundException is errorKind client with @httpError 404, and it is the declared not-found error of both DescribeComponentConfiguration and UpdateComponentConfiguration (ValidationException/ResourceInUseException are the 400s). So a not-found component must come back as HTTP 404 with __type ResourceNotFoundException; the status is a requirement, not a detail.
Emulator: every refusal in this replay carried HTTP 400 with __type ResourceNotFoundException - the error code is right, the status is a client-error 400. Hypothesis: the modeled-error status mapping for this service does not take the status from the shape's @httpError and falls back to a generic 400 for client errors.
Why: SDK and client retry/branching logic keys on the HTTP status as well as the code - a 400 is a non-retryable "bad request" while a 404 is "absent", so a client that branches on status (or any HTTP-level middleware, cache or proxy) misclassifies a missing application/component as a malformed request.
Fix: the service's modeled-error serialization - the httpError mapping used when a handler returns ResourceNotFoundException through the generated error channel (crate::scaffolded / the smithy_model!-generated error schema for this service), not the handlers, which already pick the right error shape.
Traces: application-insights-component-configuration-lifecycle#setup#5, application-insights-component-configuration-lifecycle#task#6, application-insights-component-configuration-lifecycle#task#7 (the actual_status of each is 400 while the body is ResourceNotFoundException). Note for verification: these recorded steps expect 200, so this cause is confirmed by addressing a component that really does not exist - the status must then be 404.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, 79 steps, 31 passed, score 39.2%). Three divergence clusters, all service=applicationinsights, all verdict=mismatch, all with stub_path="" and owned_by=[]:

1) fixture application-insights-component-configuration-lifecycle, step setup#5, operation DescribeComponentConfiguration, expected_status 200, actual_status 400, affected_count 2 (application-insights-component-configuration-lifecycle#setup#5, application-insights-workload-lifecycle#setup#5):
   detail: "status: expected 200, got 400\n$: poll: status 400 not in [200] after 24 attempt(s) (expected None, got {'__type': 'ResourceNotFoundException', 'Message': 'Custom component arn:aws:s3:::sim2real-probe-ai-compcfg-bkt-698c3fa6 does not exist for sim2real-probe-ai-compcfg-16a01092'})"
   (the bucket ARN and the group name are this run's generated values; the requirement is the template "Custom component <name> does not exist for <group>" NOT being produced at all for a component the application does own.)

2) fixture application-insights-component-configuration-lifecycle, step task#6, operation DescribeComponentConfiguration, expected_status 200, actual_status 400, affected_count 2 (task#6, task#8):
   detail: "status: expected 200, got 400\n$.ComponentConfiguration: missing (expected '', got None)\n$.Monitor: missing (expected False, got None)\n$.Tier: missing (expected 'DEFAULT', got None)"

3) fixture application-insights-component-configuration-lifecycle, step task#7, operation UpdateComponentConfiguration, expected_status 200, actual_status 400, affected_count 1:
   detail: "status: expected 200, got 400\n$: value_mismatch (expected {}, got {'__type': 'ResourceNotFoundException', 'Message': 'Custom component arn:aws:s3:::sim2real-probe-ai-compcfg-bkt-698c3fa6 does not exist for sim2real-probe-ai-compcfg-16a01092'})"

Model evidence (smithy_query on services/service-com-amazonaws-applicationinsights-ec2windowsbarleyservice/smithy-model/com-amazonaws-applicationinsights-ec2windowsbarleyservice.json):
  DescribeComponentConfiguration: ComponentName @required @length 1..1011 @pattern ^(?:^[\d\w\-_\.+]*$)|(?:^arn:aws(-\w+)*:...)$; output {Monitor, Tier(enum 1..50, includes DEFAULT), ComponentConfiguration(1..10000)}; errors ValidationException(400), ResourceNotFoundException(404), InternalServerException(500).
  UpdateComponentConfiguration: same ComponentName constraints, output shape EMPTY (expected body {}); errors ValidationException(400), ResourceInUseException(400), ResourceNotFoundException(404), InternalServerException(500).
  ResourceNotFoundException: errorKind client, httpError 404.

Where it bottoms out in the checkout:
  src/ops/describe_component_configuration.rs:192-204 - refuses with ResourceNotFoundException(component_missing_message(...)) when application.components holds no entry under the requested ComponentName.
  src/ops/update_component_configuration.rs:214-226 - the same lookup and refusal before anything is written.
  src/state.rs:52-54 - component_missing_message(): "Custom component {component_name} does not exist for {resource_group_name}".
  src/state.rs:378-394 / 396-410 - onboarded_components()/discovered_component(): one component per resolved group member, keyed and named by the member's resource_arn, tier DEFAULT, monitor=false, origin=Discovered. With an empty membership this map is empty, which is exactly what the replay shows.
  src/state/resource_groups.rs::group_resources - the membership seam the onboarding reads.

## Steps to Reproduce
Against the emulator (region = the request's signing region, account = the emulator account), with s3, resource-groups and application-insights mounted — this is the recorded shape of application-insights-component-configuration-lifecycle:

1. s3.create_bucket(Bucket=<bucket>) and s3.put_bucket_tagging(Bucket=<bucket>, Tagging={"TagSet":[{"Key":<tag>,"Value":"1"}]}).
2. resource-groups.create_group(Name=<group>, ResourceQuery={"Type":"TAG_FILTERS_1_0","Query":json.dumps({"ResourceTypeFilters":["AWS::AllSupported"],"TagFilters":[{"Key":<tag>,"Values":["1"]}]})}).
3. application-insights.create_application(ResourceGroupName=<group>) -> 200.
4. Poll application-insights.describe_component_configuration(ResourceGroupName=<group>, ComponentName="arn:aws:s3:::<bucket>").
   Expected: HTTP 200 with Monitor=false and Tier="DEFAULT" (ComponentConfiguration absent until something configures it).
   Observed: HTTP 400, {"__type":"ResourceNotFoundException","Message":"Custom component arn:aws:s3:::<bucket> does not exist for <group>"}, on every attempt (24 polls in the fixture). [cause 1]
5. application-insights.update_component_configuration(ResourceGroupName=<group>, ComponentName="arn:aws:s3:::<bucket>", Monitor=False, Tier="DEFAULT", ComponentConfiguration=json.dumps({"subComponents":[]})).
   Expected: HTTP 200 with body {}.
   Observed: HTTP 400 with the same ResourceNotFoundException body. [cause 3]
6. application-insights.describe_component_configuration(ResourceGroupName=<group>, ComponentName="arn:aws:s3:::<bucket>") again.
   Expected: HTTP 200 reporting back what step 5 stored - Monitor, Tier="DEFAULT", ComponentConfiguration.
   Observed: HTTP 400; the response carries no Monitor, no Tier and no ComponentConfiguration. [cause 2]
7. For cause 4, address a component that really does not exist: application-insights.describe_component_configuration(ResourceGroupName=<group>, ComponentName="does-not-exist").
   Expected: HTTP 404 with __type ResourceNotFoundException (the shape's modeled @httpError).
   Observed: HTTP 400 with __type ResourceNotFoundException.

Re-replay application-insights-component-configuration-lifecycle (steps setup#5, task#6, task#7, task#8) and application-insights-workload-lifecycle (step setup#5) to confirm a fix.
