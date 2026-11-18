Issue-Source: trace-sweep-divergence
Issue-Trace: service-quotas-catalog-list-services
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-services
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListServices|2xx->2xx|$.Services[*]
Issue-Commit: c5e269aa69c070928f6722aebd119bfd6f867924

# ListServices catalog diverges from the AWS service list: wrong membership and placeholder ServiceName

> _Note: this issue was identified against the code base at commit `c5e269aa69c070928f6722aebd119bfd6f867924`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListServices

## Description
1 net-new cause. ListServices answers 200 but the service catalog it serves is not the AWS one: from position 40 onward roughly 90 of the ~308 entries hold a different ServiceInfo than AWS returns, because the emulator's catalog both OMITS service codes AWS publishes and CONTAINS codes AWS does not list in this page, and because the ServiceName of some entries is not the name AWS publishes for that ServiceCode. Since Services is an ordered list, every omission/insertion shifts all later positions, so the divergence surfaces as a long run of positional mismatches rather than ~90 independent faults — it is one catalog-contents defect.

Cause 1 of 1 — the ListServices catalog (fixture service-quotas-catalog-list-services, step task#1).

AWS: Smithy ListServices (ServiceQuotasV20190624) takes ListServicesRequest {NextToken @length 0..2048 @pattern ^[a-zA-Z0-9/+]*={0,2}$, MaxResults integer @range 1..100} and answers 200 with ListServicesResponse {Services: ServiceInfoListDefinition = list<ServiceInfo>, NextToken}; ServiceInfo has exactly ServiceCode (@length 1..63, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$, doc 'Specifies the service identifier') and ServiceName (doc 'Specifies the service name'). It is @paginated (inputToken NextToken, outputToken NextToken, items Services, pageSize MaxResults); errors AccessDeniedException 403, IllegalArgumentException 400, InvalidPaginationTokenException 400, TooManyRequestsException 429, ServiceException 500. WHICH service codes exist, their ordering, and the ServiceName published for each code are not stated by the model — recorded behavior only: the recording is the reference. The recording shows one ServiceInfo per Service Quotas-registered service, ordered by ServiceCode, each carrying that service's published display ServiceName (e.g. ServiceCode 'health-agent' -> ServiceName 'Amazon Connect Health', 'internetmonitor' -> 'CloudWatch Internet Monitor', 'iot' -> 'AWS IoT', 'iotcore' -> 'AWS IoT Core', 'iotwireless' -> 'AWS IoT Wireless', 'ivschat' -> 'Amazon Interactive Video Service Chat', 'kafkaconnect' -> 'Managed Streaming for Kafka Connect', 'medical-imaging' -> 'AWS Health Imaging', 'mgn' -> 'Application Migration', 'migrationhuborchestrator' -> 'Migration Hub Orchestrator', 'interconnect' -> 'AWS Interconnect'). The requirement is the SET of (ServiceCode, ServiceName) pairs and their order, not any single index: an index in the evidence is only where the two lists first disagree.

Emulator: HTTP 200 (status matches; body-only divergence). The served list is a different catalog. (a) Codes AWS publishes in this page are absent from it — among them interconnect, iot, iotcore, iotwireless, ivschat, kafkaconnect, medical-imaging, mgn, migrationhuborchestrator. (b) It instead carries codes the recorded page does not list at those positions — among them glacier, groundstation, guardduty, healthlake, iotanalytics, iotevents, machinelearning, marketplaceagreement, mediaconnect. (c) For a code that IS served, ServiceName can be wrong: ServiceCode 'health-agent' is returned with ServiceName 'health-agent' — the ServiceCode echoed as a placeholder instead of the published display name 'Amazon Connect Health' — and 'internetmonitor' is returned as 'Amazon CloudWatch Internet Monitor' where AWS publishes 'CloudWatch Internet Monitor'. The net effect is ~90 diverging positions in a ~308-entry list, the first at index 40.

Why: ListServices is the documented discovery call for ServiceCode values (ServiceInfo.ServiceCode's own doc says 'To find the service code value for an Amazon Web Services service, use the ListServices operation'), so a client enumerating it to drive ListAWSDefaultServiceQuotas / ListServiceQuotas / GetServiceQuota never sees the codes the emulator dropped and concludes those services have no quotas at all, while it does see codes that are not in the recorded catalog and issues quota calls for services AWS would not have offered. A ServiceName that echoes the ServiceCode, or that differs from the published display name, breaks any client or console view that keys display text or does a name lookup off this list, and a code-to-name map built from it is wrong for those entries. Because Services is positional and paginated, the misalignment also means a client paging with MaxResults/NextToken gets a different slice than AWS would return for the same token.

Fix: the service-quotas crate's static service catalog that backs ListServices, plus the ListServices handler's page projection over it — the catalog must contain exactly the AWS-registered ServiceCodes in the recorded order, with each entry's ServiceName taken from that same catalog entry's published display name rather than defaulted/echoed from the ServiceCode. The same crate's default-quota catalog has a related family of defects tracked separately (open issues 20261113T222805Z-service-quotas-access-analyzer-acm-acm-pca-and-6-more.md and the other ListAWSDefaultServiceQuotas issues); none of those carries a replay key for ListServices, so this is filed here.

Traces: fixture service-quotas-catalog-list-services, step task#1 (the cluster's only member).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up: /__admin/health 200). Cluster: verdict mismatch, expected_status 200, actual_status 200 — body-only divergence. fixture_id service-quotas-catalog-list-services, step_key task#1, affected_count 1.

divergence_key: service=service-quotas|ListServices|status=200->200|keys=$.Services[106],$.Services[126],$.Services[127],$.Services[131],$.Services[132],$.Services[133],$.Services[134],$.Services[135],$.Services[139],$.Services[140],$.Services[141],$.Services[142],$.Services[144],$.Services[149],$.Services[151],$.Services[167],$.Services[168],$.Services[171],$.Services[173],$.Services[174],$.Services[175],$.Services[181],$.Services[183],$.Services[184],$.Services[185],$.Services[186],$.Services[190],$.Services[191],$.Services[192],$.Services[194],$.Services[195],$.Services[197],$.Services[199],$.Services[200],$.Services[201],$.Services[205],$.Services[206],$.Services[210],$.Services[214],$.Services[215],$.Services[216],$.Services[217],$.Services[219],$.Services[222],$.Services[223],$.Services[224],$.Services[226],$.Services[227],$.Services[230],$.Services[231],$.Services[232],$.Services[233],$.Services[234],$.Services[235],$.Services[236],$.Services[238],$.Services[241],$.Services[242],$.Services[243],$.Services[245],$.Services[246],$.Services[249],$.Services[250],$.Services[251],$.Services[254],$.Services[257],$.Services[260],$.Services[264],$.Services[268],$.Services[270],$.Services[272],$.Services[274],$.Services[277],$.Services[278],$.Services[279],$.Services[280],$.Services[282],$.Services[292],$.Services[294],$.Services[298],$.Services[299],$.Services[303],$.Services[304],$.Services[305],$.Services[307],$.Services[40],$.Services[48],$.Services[50],$.Services[79],$.Services[80]
(~90 diverging positions; first at index 40.)

Expected vs actual, as reported (indices are where the two orderings disagree, not a requirement in themselves):
  $.Services[126]: missing (expected {'ServiceCode': 'health-agent', 'ServiceName': 'Amazon Connect Health'}, got {'ServiceCode': 'glacier', 'ServiceName': 'Amazon Glacier'})
  $.Services[131]: missing (expected {'ServiceCode': 'interconnect', 'ServiceName': 'AWS Interconnect'}, got {'ServiceCode': 'groundstation', 'ServiceName': 'AWS Ground Station'})
  $.Services[132]: missing (expected {'ServiceCode': 'internetmonitor', 'ServiceName': 'CloudWatch Internet Monitor'}, got {'ServiceCode': 'guardduty', 'ServiceName': 'Amazon GuardDuty'})
  $.Services[133]: missing (expected {'ServiceCode': 'iot', 'ServiceName': 'AWS IoT'}, got {'ServiceCode': 'health-agent', 'ServiceName': 'health-agent'})
  $.Services[134]: missing (expected {'ServiceCode': 'iotcore', 'ServiceName': 'AWS IoT Core'}, got {'ServiceCode': 'healthlake', 'ServiceName': 'AWS HealthLake'})
  $.Services[140]: missing (expected {'ServiceCode': 'iotwireless', 'ServiceName': 'AWS IoT Wireless'}, got {'ServiceCode': 'internetmonitor', 'ServiceName': 'Amazon CloudWatch Internet Monitor'})
  $.Services[142]: missing (expected {'ServiceCode': 'ivschat', 'ServiceName': 'Amazon Interactive Video Service Chat'}, got {'ServiceCode': 'iotanalytics', 'ServiceName': 'AWS IoT Analytics'})
  $.Services[144]: missing (expected {'ServiceCode': 'kafkaconnect', 'ServiceName': 'Managed Streaming for Kafka Connect'}, got {'ServiceCode': 'iotevents', 'ServiceName': 'AWS IoT Events'})
  $.Services[171]: missing (expected {'ServiceCode': 'medical-imaging', 'ServiceName': 'AWS Health Imaging'}, got {'ServiceCode': 'machinelearning', 'ServiceName': 'Amazon Machine Learning'})
  $.Services[173]: missing (expected {'ServiceCode': 'mgn', 'ServiceName': 'Application Migration'}, got {'ServiceCode': 'marketplaceagreement', 'ServiceName': 'AWS Marketplace Agreement Service'})
  $.Services[174]: missing (expected {'ServiceCode': 'migrationhuborchestrator', 'ServiceName': 'Migration Hub Orchestrator'}, got {'ServiceCode': 'mediaconnect', 'ServiceName': 'AWS Elemental MediaConnect'})

Two distinct sub-faults are visible in the same cluster: a membership difference (health-agent / interconnect / iot / iotcore / iotwireless / ivschat / kafkaconnect / medical-imaging / mgn / migrationhuborchestrator absent; glacier / groundstation / guardduty / healthlake / iotanalytics / iotevents / machinelearning / marketplaceagreement / mediaconnect present where they are not expected), and a ServiceName fault on a code that IS served — 'health-agent' served as ServiceName 'health-agent' (ServiceCode echoed) instead of 'Amazon Connect Health', and 'internetmonitor' served as 'Amazon CloudWatch Internet Monitor' instead of 'CloudWatch Internet Monitor'. No HTTP status or error-code divergence: 200 expected, 200 returned.

## Steps to Reproduce
1. Boot the emulator from this checkout and confirm /__admin/health answers 200.
2. Replay fixture service-quotas-catalog-list-services, step task#1 — POST / with X-Amz-Target: ServiceQuotasV20190624.ListServices and body {} (no MaxResults/NextToken).
3. The response is 200. Read the Services list and search it by ServiceCode: 'interconnect', 'iot', 'iotcore', 'iotwireless', 'ivschat', 'kafkaconnect', 'medical-imaging', 'mgn' and 'migrationhuborchestrator' are not present, while 'glacier', 'groundstation', 'guardduty', 'healthlake', 'iotanalytics', 'iotevents', 'machinelearning', 'marketplaceagreement' and 'mediaconnect' are returned where the recording does not list them.
4. Look up the entry whose ServiceCode is 'health-agent': its ServiceName is the literal 'health-agent' instead of the published 'Amazon Connect Health'. Look up 'internetmonitor': its ServiceName is 'Amazon CloudWatch Internet Monitor' instead of 'CloudWatch Internet Monitor'.
5. Compare the returned list positionally with the recorded one: the first disagreement is at Services[40] and ~90 of the ~308 positions disagree thereafter, because each omitted/extra entry shifts the remainder.
6. Feed any returned ServiceCode into ListAWSDefaultServiceQuotas to see the downstream effect: codes the catalog dropped can never be discovered, and codes it invents are offered to clients.
