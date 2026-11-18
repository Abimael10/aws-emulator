Issue-Source: trace-sweep-divergence
Issue-Trace: mq-catalog-describe-broker-instance-options
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-catalog-describe-broker-instance-options
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|DescribeBrokerInstanceOptions|2xx->2xx|$.brokerInstanceOptions[*]
Issue-Commit: e418955b25a2864490121ffd52c84ac505af05d5

# mq DescribeBrokerInstanceOptions omits the published RABBITMQ host-instance-option combinations

> _Note: this issue was identified against the code base at commit `e418955b25a2864490121ffd52c84ac505af05d5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBrokerInstanceOptions

## Description
One net-new root cause in the mq DescribeBrokerInstanceOptions handler: the instance-option catalog it serves is shorter than Amazon MQ's published one and ends before the RabbitMQ host-instance-type entries, so the tail of the list the recording expects is absent entirely.

Cause 1 of 1 - the brokerInstanceOptions catalog is truncated and carries no entry for the RABBITMQ m7g host instance types.
AWS: DescribeBrokerInstanceOptions (Smithy: GET /v1/broker-instance-options, code 200; output DescribeBrokerInstanceOptionsResponse.brokerInstanceOptions -> __listOfBrokerInstanceOption, plus maxResults -> __integerMin5Max100 and nextToken; input filters engineType / hostInstanceType / storageType / maxResults @range min 1 max 100; errors BadRequestException 400, ForbiddenException 403, InternalServerErrorException 500). Each element is a BrokerInstanceOption {engineType: EngineType, hostInstanceType: string, storageType: BrokerStorageType [EBS, EFS] - 'EFS is not supported for RabbitMQ engine type', supportedEngineVersions: list<string>, supportedDeploymentModes: list<DeploymentMode> [SINGLE_INSTANCE, ACTIVE_STANDBY_MULTI_AZ, CLUSTER_MULTI_AZ], availabilityZones: list<AvailabilityZone>}. The model does not enumerate the host instance types, so the specific combinations are recorded behavior only: the recording, with no filters applied, returns a catalog whose entries at positions 12 and beyond are the RABBITMQ host instance types (the mq.m7g.* family, e.g. mq.m7g.12xlarge / .16xlarge / .2xlarge / .4xlarge / .8xlarge), each with engineType RABBITMQ, storageType 'ebs', supportedEngineVersions ['4.3', '4.2', '3.13'] and supportedDeploymentModes ['SINGLE_INSTANCE', 'CLUSTER_MULTI_AZ'], each listing the region's availability zones. The requirement is that the unfiltered catalog enumerate every engineType/hostInstanceType/storageType combination the service offers - including a RABBITMQ entry per supported RabbitMQ host instance type - and that each entry's supportedEngineVersions agree with the engine catalog the same service publishes for that engine.
Emulator: the response is 200 but the list is shorter than the published one: eight expected entries (positions 12 through 19 of the recording) have no counterpart. Position 12 is served as an ACTIVEMQ entry (hostInstanceType mq.t3.micro, storageType 'efs', supportedEngineVersions ['5.19','5.18'], supportedDeploymentModes ['SINGLE_INSTANCE','ACTIVE_STANDBY_MULTI_AZ']) where AWS serves the first RABBITMQ m7g entry, and positions 13 onward are absent (got None) - the emulator's list simply ends. No RABBITMQ m7g option is discoverable at all, and no error or nextToken signals the truncation.
Why: DescribeBrokerInstanceOptions is the discovery call a client (console, CDK/Terraform provider, sizing or capacity script) makes to choose a hostInstanceType - and the deployment mode and storage type legal for it - before CreateBroker. Against the emulator a client enumerating RabbitMQ options finds none of the m7g instance types, so it either picks a smaller/wrong instance type, concludes RabbitMQ supports no multi-AZ CLUSTER_MULTI_AZ sizing, or rejects a user-supplied mq.m7g.* value that real AWS accepts. Pagination logic is equally affected: a client that pages until the catalog is exhausted sees a different population than AWS returns, so any assertion made against the emulator does not hold on AWS and the reverse.
Fix: the static instance-option catalog backing the DescribeBrokerInstanceOptions handler - it must enumerate the full published set of engineType/hostInstanceType/storageType combinations in the order the service publishes them, including one entry per supported RabbitMQ host instance type with the RabbitMQ-legal storage type and deployment modes, and its supportedEngineVersions must come from the same source of truth DescribeBrokerEngineTypes publishes for that engine. (The open issue 20261115T144250Z-mq-describebrokerenginetypes-serves-a-stale-rabbitmq-version-catalog.md covers the separate DescribeBrokerEngineTypes engine catalog and the engine-version list it publishes, not this instance-option catalog; the two DescribeBrokerInstanceOptions issues that previously existed are both resolved and do not cover this divergence.)
Traces: mq-catalog-describe-broker-instance-options#task#1 ($.brokerInstanceOptions[12] through $.brokerInstanceOptions[19] reported missing).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 68 steps, 52 passed, 16 mismatched, score 76.5%).

Fixture mq-catalog-describe-broker-instance-options, step task#1, DescribeBrokerInstanceOptions, expected_status 200 / actual_status 200, verdict mismatch. Diverging keys: $.brokerInstanceOptions[12], [13], [14], [15], [16], [17], [18], [19] (eight entries).

  $.brokerInstanceOptions[12]: missing
    expected {'hostInstanceType': 'mq.m7g.12xlarge', 'engineType': 'RABBITMQ', 'supportedEngineVersions': ['4.3', '4.2', '3.13'], 'availabilityZones': [{'name': 'us-west-2c'}, {'name': 'us-west-2d'}, {'name': 'us-west-2b'}, {'name': 'us-west-2a'}], 'storageType': 'ebs', 'supportedDeploymentModes': ['SINGLE_INSTANCE', 'CLUSTER_MULTI_AZ']}
    got      {'hostInstanceType': 'mq.t3.micro', 'engineType': 'ACTIVEMQ', 'supportedEngineVersions': ['5.19', '5.18'], 'storageType': 'efs', 'supportedDeploymentModes': ['SINGLE_INSTANCE', 'ACTIVE_STANDBY_MULTI_AZ'], 'availabilityZones': [{'name': 'us-west-2a'}, {'name': 'us-west-2b'}, {'name': 'us-west-2c'}, {'name': 'us-west-2d'}]}
  $.brokerInstanceOptions[13]: missing (expected {'hostInstanceType': 'mq.m7g.16xlarge', 'engineType': 'RABBITMQ', 'supportedEngineVersions': ['4.3', '4.2', '3.13'], 'storageType': 'ebs', 'supportedDeploymentModes': ['SINGLE_INSTANCE', 'CLUSTER_MULTI_AZ'], ...}, got None)
  $.brokerInstanceOptions[14]: missing (expected mq.m7g.2xlarge / RABBITMQ / ebs, got None)
  $.brokerInstanceOptions[15]: missing (expected mq.m7g.4xlarge / RABBITMQ / ebs, got None)
  $.brokerInstanceOptions[16]: missing (expected mq.m7g.8xlarge / RABBITMQ / ebs, got None)
  $.brokerInstanceOptions[17], [18], [19]: missing, same shape (RABBITMQ host-instance-option entries, got None)

The emulator's list ends at index 12; the recording carries at least 20 entries. The zone ORDER inside an entry and the generated per-run values are not the requirement here - the absent entries are.

Smithy consulted (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json): DescribeBrokerInstanceOptions http {GET /v1/broker-instance-options, code 200}; output members brokerInstanceOptions -> __listOfBrokerInstanceOption, maxResults -> __integerMin5Max100 (range 5..100), nextToken; BrokerInstanceOption {engineType: EngineType, hostInstanceType: __string, storageType: BrokerStorageType, supportedEngineVersions: __listOf__string, supportedDeploymentModes: __listOfDeploymentMode, availabilityZones: __listOfAvailabilityZone}; BrokerStorageType enum [EBS, EFS] documented 'EFS is not supported for RabbitMQ engine type'; DeploymentMode enum [SINGLE_INSTANCE, ACTIVE_STANDBY_MULTI_AZ, CLUSTER_MULTI_AZ]; errors BadRequestException (400), ForbiddenException (403), InternalServerErrorException (500).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture mq-catalog-describe-broker-instance-options, step task#1 (GET /v1/broker-instance-options with no engineType / hostInstanceType / storageType filter).
2. Read $.brokerInstanceOptions and count the entries, then filter them by engineType.
3. Observe the list ends at index 12 and contains no entry whose engineType is RABBITMQ with a mq.m7g.* hostInstanceType; index 12 is an ACTIVEMQ mq.t3.micro entry and indices 13..19 are absent.
4. AWS returns the full published catalog, whose entries at those positions are the RABBITMQ host-instance-type options (mq.m7g.12xlarge, .16xlarge, .2xlarge, .4xlarge, .8xlarge, ...), each with storageType 'ebs', supportedEngineVersions ['4.3', '4.2', '3.13'] and supportedDeploymentModes ['SINGLE_INSTANCE', 'CLUSTER_MULTI_AZ'].
5. Re-replay that fixture step after the fix to confirm; the same step is part of the simparity-wire-aws-v1 corpus.
