Issue-Source: trace-sweep-divergence
Issue-Trace: logs-catalog-describe-configuration-templates
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-catalog-describe-configuration-templates
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|DescribeConfigurationTemplates|2xx->2xx|$.configurationTemplates[*]
Issue-Commit: 2bc3ea78efb0f9a362e61bf32f07370289f9309a

# logs DescribeConfigurationTemplates serves an emulator-authored template catalog instead of AWS's published one (10 causes)

> _Note: this issue was identified against the code base at commit `2bc3ea78efb0f9a362e61bf32f07370289f9309a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeConfigurationTemplates

## Description
DescribeConfigurationTemplates is a read-only catalog operation: it returns the AWS-published, service-wide set of vended-log configuration templates, keyed by (service, logType, resourceType, deliveryDestinationType). The emulator answers 200 for every filter but the catalog it serves is its own synthesized data, so the per-service template set, the per-template field lists and the per-template defaults all differ from AWS. There are TEN distinct causes below (ten separate filter queries, each its own divergence cluster); all ten must be closed.

Smithy basis used throughout: logs#DescribeConfigurationTemplates (input filters service / logTypes / resourceTypes / deliveryDestinationTypes / nextToken / limit; output configurationTemplates + nextToken, paginated items=configurationTemplates) and logs#ConfigurationTemplate (members service, logType, resourceType, deliveryDestinationType enum DeliveryDestinationType = S3|CWL|FH|XRAY, defaultDeliveryConfigValues, allowedFields (list of RecordField {name, mandatory}), allowedOutputFormats, allowedActionForAllowVendedLogsDeliveryForResource, allowedFieldDelimiters, allowedSuffixPathFields, deliverySourceConfiguration, s3TablesIntegration). The CONTENT of the catalog is not in the model, so for the content each cause below is grounded in "recorded behavior only" — the recorded AWS response for that exact filter.

Cause 1 of 10 — service filter selecting the 'aidevops' log source.
AWS: recorded behavior only — the filtered query returns a non-empty configurationTemplates list; its first element is the aidevops template with allowedActionForAllowVendedLogsDeliveryForResource 'aidevops:AllowVendedLogDeliveryForResource', allowedFieldDelimiters ['\t',' ',','], allowedOutputFormats ['plain','json','w3c','parquet'], allowedSuffixPathFields ['accountid','region','resource_id','yyyy','MM','dd','HH'], an allowedFields list in which resource_arn and event_timestamp are mandatory=true and timestamp/resource_id/the optional_* fields are mandatory=false, and defaultDeliveryConfigValues.recordFields listing resource_arn, event_timestamp and the optional_* fields.
Emulator: six expected elements ($.configurationTemplates[0]..[5]) are absent from the 200 response — the aidevops log source is not in the served catalog at all.
Why: a client that enumerates templates to discover which recordFields/output formats/suffix-path fields a delivery may use, or which IAM action it must be granted, sees that log source as unsupported and cannot build a valid CreateDelivery/UpdateDeliveryConfiguration request.
Fix: the configuration-template catalog seeded in the logs service's state/static catalog data, read by the DescribeConfigurationTemplates handler.
Traces: logs-catalog-describe-configuration-templates#task#1 (also task#6, task#13, task#17, task#21, task#24).

Cause 2 of 10 — service filter selecting 'autoloop'.
AWS: recorded behavior only — the query returns 21 templates; element [0] is service 'autoloop', logType 'ERROR_LOGS', resourceType 'subscription', deliveryDestinationType 'S3', with allowedSuffixPathFields ['accountid','region','yyyy','MM','dd','HH'] and allowedFields timestamp/resource_arn/event_timestamp/log_timestamp/transformer_arn/object_identifier/event_identifier/trace_identifier/log_message.
Emulator: all 21 elements ($.configurationTemplates[0]..[20]) diverge from the recorded templates at the same positions.
Why: the template set a client paginates is not the AWS set, so field/format discovery for this log source yields values AWS will reject.
Fix: same catalog data + the handler's ordering of the returned list.
Traces: logs-catalog-describe-configuration-templates#task#3 (also task#22).

Cause 3 of 10 — service filter selecting 'bedrock'.
AWS: recorded behavior only — element [0] is service 'bedrock', logType 'EVENT_LOGS', resourceType 'agent-alias', deliveryDestinationType 'S3', and its allowedFields list contains an entry {name 'resource_id', mandatory false} alongside the mandatory timestamp/resourceId/traceId/spanId/sessionId/requestId/operation/attributes/body/eventType/eventVersion/eventName/level entries.
Emulator: 17 elements diverge; for [0] the allowedFields list omits the non-mandatory 'resource_id' entry that AWS lists.
Why: allowedFields is the contract for which recordFields a delivery may name; a field AWS allows is reported as disallowed, so a correct client refuses a valid configuration.
Fix: the per-template allowedFields in the catalog data.
Traces: logs-catalog-describe-configuration-templates#task#4.

Cause 4 of 10 — service filter selecting 'bedrock-agentcore'.
AWS: recorded behavior only — the query returns 48 templates; element [0] is logType 'APPLICATION_LOGS', resourceType 'workload-identity', deliveryDestinationType 'S3', allowedSuffixPathFields ['accountid','region','workload_identity_id','yyyy','MM','dd','HH'], with a 23-entry allowedFields list in which resource_arn and event_timestamp are mandatory.
Emulator: all 48 elements diverge at their recorded positions.
Why: as cause 2 — the served set for this log source is not AWS's.
Fix: same catalog data.
Traces: logs-catalog-describe-configuration-templates#task#5.

Cause 5 of 10 — service filter selecting 'ec2' resourceType 'route-server-peer'.
AWS: recorded behavior only — three templates are returned for this resource type: deliveryDestinationType 'S3' (allowedOutputFormats ['plain','json','w3c','parquet'], allowedSuffixPathFields ['accountid','region','resource_id','yyyy','MM','dd','HH']), 'CWL' (allowedOutputFormats ['plain','json'], no suffix-path fields) and a third element, each with allowedFields timestamp/resource_arn/resource_id/event_timestamp/type/status/message and resource_arn, event_timestamp, type mandatory.
Emulator: $.configurationTemplates[0], [1] and [2] are all absent ("got None") — the 200 response carries none of the three.
Why: the EC2 route-server-peer vended-log source looks unsupported, so a delivery for it can never be configured against the emulator.
Fix: same catalog data.
Traces: logs-catalog-describe-configuration-templates#task#8 (also task#9, task#12, task#15, task#16, task#23, task#25, task#28, task#29).

Cause 6 of 10 — service filter selecting 'eks'.
AWS: recorded behavior only — 33 templates; element [0] is logType 'AUTO_MODE_LOAD_BALANCING_LOGS', resourceType 'cluster', deliveryDestinationType 'S3', allowedSuffixPathFields ['accountid','cluster_id','region','yyyy','MM','dd','HH'], allowedFields resource_arn (mandatory) / event_timestamp (mandatory) / level / stream / message / err / error / controller / controllerGroup / controllerKind / reconcileID / worker-count / attempt / delay / totalItems.
Emulator: all 33 elements diverge at their recorded positions.
Why: as cause 2.
Fix: same catalog data.
Traces: logs-catalog-describe-configuration-templates#task#10.

Cause 7 of 10 — service filter selecting 'elasticloadbalancing' (ALB_CONNECTION_LOGS).
AWS: recorded behavior only — element [0] is logType 'ALB_CONNECTION_LOGS', resourceType 'loadbalancer', deliveryDestinationType 'S3', defaultDeliveryConfigValues.recordFields beginning with 'time' (then client_ip, client_port, listener_port, tls_protocol, ...), s3DeliveryConfiguration.suffixPath '{region}/{yyyy}/{MM}/{dd}/' (no {HH} segment), and an s3TablesIntegration block {datasourceName 'aws_alb', datasourceType 'connection'}.
Emulator: 12 elements diverge; for [0] defaultDeliveryConfigValues.recordFields begins with 'timestamp' instead of AWS's 'time', i.e. the default record-field list names a field that is not the ALB connection-log field.
Why: a client that accepts defaultDeliveryConfigValues verbatim builds a CreateDelivery with a recordField name AWS does not accept for this log type, and the s3TablesIntegration metadata an S3-Tables consumer needs is absent.
Fix: the per-template defaultDeliveryConfigValues and s3TablesIntegration in the catalog data.
Traces: logs-catalog-describe-configuration-templates#task#11 (also task#20).

Cause 8 of 10 — service filter selecting 'events'.
AWS: recorded behavior only — element [3] of the returned list is service 'events', logType 'ERROR_LOGS', resourceType 'event-bus', deliveryDestinationType 'S3', with allowedSuffixPathFields ['accountid','event_bus_name','region','yyyy','MM','dd','HH'] and recordFields naming event_bus_name.
Emulator: element [3] is instead logType 'INFO_LOGS', resourceType 'subscriber', with recordFields naming subscriber_name; 15 positions ([3]..[17]) diverge — the list the emulator serves is a different, differently ordered set.
Why: a client reading a page by index, or filtering client-side for the event-bus ERROR_LOGS template, gets the wrong template and configures the wrong log source.
Fix: same catalog data + the handler's list ordering.
Traces: logs-catalog-describe-configuration-templates#task#14.

Cause 9 of 10 — service filter selecting 'mediatailor'.
AWS: recorded behavior only — 9 templates; element [0] has allowedActionForAllowVendedLogsDeliveryForResource 'mediatailor:AllowVendedLogDeliveryForResource' and a large allowedFields list in which eventTimestamp and awsAccountId are mandatory and timestamp/originId/customerId/eventType/sessionType/requestId/... are not.
Emulator: all 9 elements diverge at their recorded positions.
Why: as cause 2.
Fix: same catalog data.
Traces: logs-catalog-describe-configuration-templates#task#18 (also task#19, task#27).

Cause 10 of 10 — service filter selecting 'workmail'.
AWS: recorded behavior only — element [0] is logType 'ACCESS_CONTROL_LOGS', resourceType 'organization', deliveryDestinationType 'S3', and in its allowedFields list organization_arn, event_timestamp, request_id, user_id, protocol, source_ip, scope and access_granted are mandatory=true while timestamp, organization_id, impersonator_id, user_agent and rule_id are mandatory=false.
Emulator: 15 elements diverge; for [0] the allowedFields entries carry the wrong mandatory flags — e.g. organization_arn is served as mandatory=false where AWS reports mandatory=true.
Why: RecordField.mandatory tells a caller which recordFields it MUST include; reporting a mandatory field as optional makes a client omit it and AWS then rejects the delivery, while the emulator accepts it.
Fix: the per-template allowedFields mandatory flags in the catalog data.
Traces: logs-catalog-describe-configuration-templates#task#30.

Note: no open issue covers DescribeConfigurationTemplates; none of these ten clusters was reported as owned_by an existing issue or blocked by a stub. Generated resource names, ids and timestamps in the recorded payloads are environment-specific and are not part of the requirement — the requirement is the catalog's membership, ordering and per-template field/default/flag content.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200): 10 DescribeConfigurationTemplates mismatch clusters on fixture logs-catalog-describe-configuration-templates, every one expected_status 200 / actual_status 200 with diverging configurationTemplates members.

Representative (task#1, service filter 'aidevops'): $.configurationTemplates[0]..[5] all reported 'missing' — expected the aidevops S3/CWL templates (allowedActionForAllowVendedLogsDeliveryForResource 'aidevops:AllowVendedLogDeliveryForResource', allowedFieldDelimiters ['\t',' ',','], allowedOutputFormats ['plain','json','w3c','parquet'], allowedSuffixPathFields ['accountid','region','resource_id','yyyy','MM','dd','HH']), got nothing at those positions.

Other clusters, expected vs actual:
- task#3 autoloop: 21/21 elements diverge.
- task#4 bedrock: [0].allowedFields missing {'name':'resource_id','mandatory':false}; 17 elements diverge.
- task#5 bedrock-agentcore: 48/48 elements diverge.
- task#8 ec2 route-server-peer: [0],[1],[2] expected (S3 and CWL variants), got None for each.
- task#10 eks: 33/33 elements diverge.
- task#11 elasticloadbalancing: [0].defaultDeliveryConfigValues.recordFields expected to start 'time', got 'timestamp'; expected s3TablesIntegration {datasourceName 'aws_alb', datasourceType 'connection'}; 12 elements diverge.
- task#14 events: [3] expected logType 'ERROR_LOGS'/resourceType 'event-bus', got logType 'INFO_LOGS'/resourceType 'subscriber'; 15 elements diverge.
- task#18 mediatailor: 9/9 elements diverge.
- task#30 workmail: [0].allowedFields organization_arn expected mandatory true, got mandatory false; 15 elements diverge.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture logs-catalog-describe-configuration-templates.
2. For each cause, issue the DescribeConfigurationTemplates call recorded at that step (step task#1 = service filter 'aidevops', task#3 = 'autoloop', task#4 = 'bedrock', task#5 = 'bedrock-agentcore', task#8 = 'ec2' resourceType 'route-server-peer', task#10 = 'eks', task#11 = 'elasticloadbalancing', task#14 = 'events', task#18 = 'mediatailor', task#26 handled in the companion issue, task#30 = 'workmail').
3. Compare the returned configurationTemplates list, element by element, against the recorded response: membership (elements present at all), order, allowedFields names and mandatory flags, allowedOutputFormats, allowedSuffixPathFields, allowedActionForAllowVendedLogsDeliveryForResource, defaultDeliveryConfigValues.recordFields/fieldDelimiter/s3DeliveryConfiguration and s3TablesIntegration.
