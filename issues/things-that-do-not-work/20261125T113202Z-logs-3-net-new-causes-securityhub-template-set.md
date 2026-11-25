Issue-Source: trace-sweep-divergence
Issue-Trace: logs-catalog-describe-configuration-templates
Issue-Step: task#26
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-catalog-describe-configuration-templates logs-insights-start-query-and-describe-workflow logs-log-group-query-discovery logs-osdocstat-traj-insights-query-audit-group logs-osdocstat-traj-log-group-arn-forms logs-integration-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|StartQuery|4xx->4xx|$.message
Issue-Key: logs|GetIntegration|2xx->2xx|$.integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint
Issue-Key: logs|GetIntegration|2xx->2xx|$.integrationDetails.openSearchIntegrationDetails.collection.collectionEndpoint
Issue-Related: 20261125T113202Z-logs-describeconfigurationtemplates-serves-an-emulator-authored-template-catalog.md
Issue-Commit: 2bc3ea78efb0f9a362e61bf32f07370289f9309a

# logs: 3 net-new causes — securityhub template set invented, StartQuery MalformedQueryException message carries SDK decoration, GetIntegration publishes unserved OpenSearch endpoints

> _Note: this issue was identified against the code base at commit `2bc3ea78efb0f9a362e61bf32f07370289f9309a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQuery

## Description
Three independent logs causes, each its own divergence cluster, all served with a 2xx/4xx status whose BODY diverges. All three must be closed.

Cause 1 of 3 — DescribeConfigurationTemplates invents securityhub templates and gives the real one the wrong destination type.
AWS: recorded behavior only for the catalog content (the content is not modelled); the shape is logs#ConfigurationTemplate with deliveryDestinationType of enum logs#DeliveryDestinationType = S3|CWL|FH|XRAY. For the recorded securityhub query AWS returns a SHORT list whose first element is service 'securityhub', logType 'SECURITY_FINDING_LOGS', resourceType 'hubv2', deliveryDestinationType 'CWL', allowedFields [{name 'event', mandatory true}], allowedOutputFormats ['plain'], defaultDeliveryConfigValues {recordFields ['event'], fieldDelimiter ''} and NO allowedSuffixPathFields and NO s3DeliveryConfiguration (a CWL destination has no S3 suffix path).
Emulator: element [0] is served as deliveryDestinationType 'S3' with an s3DeliveryConfiguration {suffixPath '', enableHiveCompatiblePath false} and allowedSuffixPathFields ['accountid','region','resource_id','yyyy','MM','dd','HH'], and the list carries additional elements AWS does not return at all — [2] (resourceType 'hubv2', deliveryDestinationType 'FH'), [3] (resourceType 'hub', deliveryDestinationType 'S3') and [5] are all reported 'extra (expected None)'.
Why: a client enumerating where Security Hub findings may be delivered is told it can deliver to S3 and Firehose destinations AWS does not support for that log type, and is given an S3 suffix-path contract for a destination that has none; the delivery it then creates is impossible on real AWS.
Fix: the configuration-template catalog data seeded in the logs service's state, read by the DescribeConfigurationTemplates handler (same catalog as the companion bundle 'logs DescribeConfigurationTemplates serves an emulator-authored template catalog...', but a distinct cluster: that one is about missing/incorrect entries, this one about invented extra entries and a wrong deliveryDestinationType).
Traces: logs-catalog-describe-configuration-templates#task#26.

Cause 2 of 3 — StartQuery's MalformedQueryException message embeds an AWS-SDK diagnostic clause in the wire payload.
AWS: Smithy logs#StartQuery declares error MalformedQueryException (client, HTTP 400) for a queryString that does not parse. The serialized error body carries only the parse diagnostic as its message; the fixed text is 'unexpected symbol found ' / ' at line ' / ' and position ', and the variable parts are the offending symbol, the line number and the character position, all taken from the submitted queryString. The '(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: ...)' clause is appended CLIENT-SIDE by the AWS SDK when it renders an exception; it is never part of the message AWS puts on the wire.
Emulator: the 400 body's $.message is 'unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: 1d97a86c-42e5-48f9-95fa-0ae6d73ad7cd; Proxy: null)' — the parse diagnostic plus the whole SDK decoration clause, which fails the fixture's shape assertion on $.message. The divergence is the presence of the clause, not the per-run request id inside it.
Why: a client that matches, logs or re-presents the error message sees SDK framing duplicated inside the message (and an SDK that renders the error appends the clause a second time); any assertion on the service's message text fails.
Fix: the StartQuery handler's MalformedQueryException construction / the logs error serializer — serialize only the diagnostic text as `message` and leave Service/Status Code/Error Code/Request ID/Proxy framing to the protocol headers and the client.
Traces: logs-insights-start-query-and-describe-workflow#task#4 (also logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7). The open issue '20261117T115022Z-logs-5-causes-generic-not-found-invalid-token.md' covers StartQuery's wrong error CLASS for a missing log-group selector; it does not cover this message-decoration defect, and this cluster was not reported as owned by it.

Cause 3 of 3 — GetIntegration publishes OpenSearch application/collection endpoints on hosts the emulator does not serve.
AWS: Smithy logs#GetIntegration returns integrationDetails.openSearchIntegrationDetails with application.applicationEndpoint and collection.collectionEndpoint. On AWS those are live HTTPS endpoints of the managed OpenSearch application/collection the integration created, reachable by the caller; the host names themselves are account/region-generated and are not a requirement.
Emulator: it returns endpoints under the real AWS domains ('https://<name>-application.us-west-2.opensearch.amazonaws.com' and 'https://<name>-collection.us-west-2.aoss.amazonaws.com') — addresses other than the one the emulator is reached on, so the replay flags both as host_not_own. Nothing answers at them.
Why: the documented next step after creating an OpenSearch integration is to call the returned applicationEndpoint/collectionEndpoint; a client that does so leaves the emulator entirely (name resolution to, or egress toward, real AWS), so the integration is announced as usable but is unreachable and the whole dashboards/collection flow dead-ends.
Fix: the GetIntegration/PutIntegration handler's endpoint minting in the logs service's integration state — mint endpoints on the host:port the emulator actually serves, as the other served endpoints in this emulator are minted.
Traces: logs-integration-lifecycle#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1, emulator up (/__admin/health 200, data listener accepting).

Cause 1 — DescribeConfigurationTemplates, logs-catalog-describe-configuration-templates#task#26, expected 200 / actual 200:
  $.configurationTemplates[0]: extra — expected {deliveryDestinationType 'CWL', allowedFields [{name 'event', mandatory true}], allowedOutputFormats ['plain'], defaultDeliveryConfigValues {recordFields ['event'], fieldDelimiter ''}, resourceType 'hubv2', service 'securityhub'}, got deliveryDestinationType 'S3' with s3DeliveryConfiguration {suffixPath '', enableHiveCompatiblePath false} and allowedSuffixPathFields ['accountid','region','resource_id','yyyy','MM','dd','HH'].
  $.configurationTemplates[2]: extra (expected None, got resourceType 'hubv2' / deliveryDestinationType 'FH').
  $.configurationTemplates[3]: extra (expected None, got resourceType 'hub' / deliveryDestinationType 'S3').
  $.configurationTemplates[5]: extra (expected None).

Cause 2 — StartQuery, logs-insights-start-query-and-describe-workflow#task#4, expected 400 / actual 400:
  $.message: value_mismatch — expected the shaped parse diagnostic alone, got 'unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: 1d97a86c-42e5-48f9-95fa-0ae6d73ad7cd; Proxy: null)'. Fixed text that belongs on the wire: 'unexpected symbol found <symbol> at line <line> and position <position>'; everything from ' (Service:' onward is SDK-side framing. Same cluster at logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7.

Cause 3 — GetIntegration, logs-integration-lifecycle#task#4, expected 200 / actual 200:
  $.integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint: host_not_own — got 'https://sim2real-probe-logs-integ-lifecycle-cb534092-application.us-west-2.opensearch.amazonaws.com'.
  $.integrationDetails.openSearchIntegrationDetails.collection.collectionEndpoint: host_not_own — got 'https://sim2real-probe-logs-integ-lifecycle-cb534092-collection.us-west-2.aoss.amazonaws.com'.
  Both are hosts the emulator does not serve; the generated name component itself is environment-specific and not the defect.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Cause 1: replay logs-catalog-describe-configuration-templates step task#26 (DescribeConfigurationTemplates filtered to the securityhub log source) and compare the configurationTemplates list to the recorded one — check element count, each element's deliveryDestinationType, and whether CWL elements carry s3DeliveryConfiguration/allowedSuffixPathFields.
3. Cause 2: replay logs-insights-start-query-and-describe-workflow step task#4 (StartQuery with a queryString that does not parse) and read $.message of the 400 body — it must contain only the parse diagnostic, with no '(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: ...)' clause.
4. Cause 3: replay logs-integration-lifecycle through step task#4 (CreateIntegration/PutIntegration then GetIntegration) and attempt an HTTPS request to the returned integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint and .collection.collectionEndpoint — both must resolve to, and be served by, the emulator's own listener.
