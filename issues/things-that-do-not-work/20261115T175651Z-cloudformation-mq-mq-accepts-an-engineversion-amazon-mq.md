Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified cloudformation__mq.deploy-amazonmq-configuration-invalid-engine-version-rejected
Issue-Key: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Key: cloudformation__mq.deploy-amazonmq-configuration-invalid-engine-version-rejected|run#9|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Commit: 7159003333263c232287cf539a8a92dd610dcec6

# cloudformation__mq: mq accepts an EngineVersion Amazon MQ does not offer on CreateBroker and CreateConfiguration, so stacks that must roll back reach CREATE_COMPLETE (2 causes)

> _Note: this issue was identified against the code base at commit `7159003333263c232287cf539a8a92dd610dcec6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBroker

## Description
Two net-new causes, both in mq's create-path validation of EngineVersion. Number them when closing: Cause 1 of 2 and Cause 2 of 2.

Cause 1 of 2 — mq CreateBroker does not validate EngineVersion against the versions Amazon MQ offers for the requested EngineType. Failing check: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified
AWS: Smithy (com-amazonaws-mq-mq): CreateBroker (POST /v1/brokers) takes EngineType (@required, enum) and EngineVersion (string) and declares BadRequestException (errorKind client, httpError 400). Recorded behavior: an EngineVersion that is not in the catalog DescribeBrokerEngineTypes serves for that EngineType is refused before any broker is created, with HTTP 400 BadRequestException whose message is "Broker engine version [<EngineVersion>] is invalid for broker engine type [<EngineType>]. Valid values: [<comma-separated offered versions>]." The sentence shape, the bracketed slots and "Valid values:" are fixed text; the version, the engine type and the version list are taken from the request and from the service's own catalog. Through CloudFormation the AWS::AmazonMQ::Broker handler reports that service error on the resource's single CREATE_FAILED row as 'Resource handler returned message: "<that message> (Service: Mq, Status Code: 400, Request ID: <id>)" (HandlerErrorCode: InvalidRequest)', the stack ends ROLLBACK_COMPLETE, and no broker with that name is listed.
Emulator: the create is accepted. The stack reaches CREATE_COMPLETE (replay: "rejected stack ended 'CREATE_COMPLETE'"), so there is no CREATE_FAILED row, no refusal message and no rollback — a broker is provisioned with a version Amazon MQ does not offer.

Cause 2 of 2 — mq CreateConfiguration does not validate EngineVersion against the versions Amazon MQ offers for the requested EngineType. Failing check: cloudformation__mq.deploy-amazonmq-configuration-invalid-engine-version-rejected
AWS: Smithy (com-amazonaws-mq-mq): CreateConfiguration (POST /v1/configurations) takes EngineType (@required, enum) and EngineVersion (string) and declares BadRequestException (errorKind client, httpError 400). Recorded behavior: an unoffered EngineVersion is refused with HTTP 400 and the same message shape, "Broker engine version [<EngineVersion>] is invalid for broker engine type [<EngineType>]. Valid values: [<comma-separated offered versions>]." Through CloudFormation the AWS::AmazonMQ::Configuration handler reports it with HandlerErrorCode GeneralServiceException, the stack ends ROLLBACK_COMPLETE, and no configuration with that name is listed. Note the refusal is made by the resource handler, not by CloudFormation's pre-provisioning template validation: the check asserts there are no early validation rows.
Emulator: the create is accepted and the stack reaches CREATE_COMPLETE (replay: "rejected stack ended 'CREATE_COMPLETE'"), leaving a configuration whose EngineVersion Amazon MQ never offered.

Why (both): a client relying on AWS refusing an unsupported engine version gets the opposite answer — a stack that must fail reports success, and the account is left holding a broker/configuration pinned to a version that cannot exist. Infrastructure pipelines that treat CREATE_COMPLETE as proof the declared properties were accepted propagate the bad version downstream, and negative tests that expect ROLLBACK_COMPLETE plus the handler's exact reason never see one.

Fix (both): the EngineVersion validation in the mq CreateBroker and CreateConfiguration handlers — reject with BadRequestException (400) when the requested version is not in the per-EngineType catalog the emulator's DescribeBrokerEngineTypes serves, using the message shape above with the offered list rendered from that same catalog. The surrounding CloudFormation handler wrapping (Service: Mq, Status Code: 400, HandlerErrorCode InvalidRequest for Broker / GeneralServiceException for Configuration) then follows from the service error.

Related already-open issues that do NOT carry these check ids: 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md (mq refusals generally) and 20261115T144250Z-mq-describebrokerenginetypes-serves-a-stale-rabbitmq-version-catalog.md (the catalog's own content). Neither covers the two checks above.

Traces: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified; cloudformation__mq.deploy-amazonmq-configuration-invalid-engine-version-rejected. In both, the positive control (the same template with the newest version DescribeBrokerEngineTypes offers) is what the check compares against.

## Evidence
Cause 1 — cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified, [mismatch]:
AssertionError: rejected stack ended 'CREATE_COMPLETE'  (assert status == "ROLLBACK_COMPLETE")
Expected (AWS): stack ROLLBACK_COMPLETE; no early-validation rows; the Broker resource's CREATE_FAILED ResourceStatusReason = 'Resource handler returned message: "Broker engine version [99.99] is invalid for broker engine type [ACTIVEMQ]. Valid values: [<versions DescribeBrokerEngineTypes offers for ACTIVEMQ>]. (Service: Mq, Status Code: 400, Request ID: <id>)" (HandlerErrorCode: InvalidRequest)'; the broker name is not listed by MQ.
Actual (emulator): stack CREATE_COMPLETE — no CREATE_FAILED row at all. ("99.99" is the check's stand-in for any version outside the offered catalog; the version list is whatever the catalog holds, not a literal.)

Cause 2 — cloudformation__mq.deploy-amazonmq-configuration-invalid-engine-version-rejected, [mismatch]:
AssertionError: rejected stack ended 'CREATE_COMPLETE'  (assert status == "ROLLBACK_COMPLETE")
Expected (AWS): stack ROLLBACK_COMPLETE; no early-validation rows; the Cfg resource's CREATE_FAILED handler reason message "Broker engine version [99.99] is invalid for broker engine type [ACTIVEMQ]. Valid values: [<offered ACTIVEMQ versions>]. (Service: Mq, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" with HandlerErrorCode GeneralServiceException; ListConfigurations shows no configuration with that name.
Actual (emulator): stack CREATE_COMPLETE, configuration created.

## Steps to Reproduce
Cause 1:
1. mq DescribeBrokerEngineTypes(EngineType="ACTIVEMQ") and note the offered EngineVersions; pick a version not in that list (e.g. "99.99").
2. CreateStack with a template declaring a VPC/subnet/security group plus AWS::AmazonMQ::Broker {EngineType: ACTIVEMQ, EngineVersion: <unoffered version>, DeploymentMode: SINGLE_INSTANCE, HostInstanceType: mq.t3.micro, PubliclyAccessible: false}.
3. Wait for a terminal stack status. Expected ROLLBACK_COMPLETE with the Broker resource CREATE_FAILED and the handler reason quoted above (HandlerErrorCode InvalidRequest). Observed CREATE_COMPLETE.
4. Direct call: mq CreateBroker with the same unoffered EngineVersion — expected 400 BadRequestException, observed success.

Cause 2:
1. Same DescribeBrokerEngineTypes step to pick an unoffered ACTIVEMQ version.
2. CreateStack with a template declaring AWS::AmazonMQ::Configuration {Name: <name>, EngineType: ACTIVEMQ, EngineVersion: <unoffered version>, Data: <base64 ActiveMQ broker xml>}.
3. Expected ROLLBACK_COMPLETE, no validation rows, the Cfg CREATE_FAILED handler reason quoted above (HandlerErrorCode GeneralServiceException), and no configuration of that name in ListConfigurations. Observed CREATE_COMPLETE and the configuration created.
4. Direct call: mq CreateConfiguration with the same unoffered EngineVersion — expected 400 BadRequestException, observed success.
