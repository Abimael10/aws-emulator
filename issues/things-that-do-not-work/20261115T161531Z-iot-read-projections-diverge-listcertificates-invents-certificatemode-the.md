Issue-Source: trace-sweep-divergence
Issue-Trace: iot-certificate-list-lifecycle
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-certificate-list-lifecycle iot-domain-configuration-inspection iot-topic-rule-destination-http-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|ListCertificates|2xx->2xx|$.certificates[*]
Issue-Key: iot|ListDomainConfigurations|2xx->2xx|$.domainConfigurations[*]
Issue-Key: iot|GetTopicRuleDestination|2xx->2xx|$.topicRuleDestination.statusReason
Issue-Commit: 4167e7cf5381813eabd70d9b700d048404e5c90a

# iot read projections diverge: ListCertificates invents certificateMode, the built-in domain configurations are absent and destination statusReason is authored

> _Note: this issue was identified against the code base at commit `4167e7cf5381813eabd70d9b700d048404e5c90a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCertificates

## Description
Three successful (200) reads project members the recorded AWS responses do not, or omit resources AWS always serves. 3 causes.

Cause 1 of 3: ListCertificates emits certificateMode 'DEFAULT' on summaries where AWS sends no certificateMode.
AWS: Smithy: ListCertificates (GET /certificates) returns certificates: Certificates; the summary's certificateMode member is optional. For the certificates registered in this trajectory AWS's summaries carry certificateArn, certificateId, status and creationDate and no certificateMode at all.
Emulator: adds certificateMode: 'DEFAULT' to every summary.
Why: a client that branches on the presence of certificateMode (DEFAULT vs SNI_ONLY handling, or 'unknown mode' fallbacks) takes a branch it would not take on AWS, and strict response comparisons fail.
Fix: the ListCertificates projection — emit certificateMode only when the stored certificate has one.
Note: the same cluster also reports creationDate differing (recorded 1787052770.334 vs the replay's own clock). That is a per-run value and is NOT part of this finding; only the certificateMode member is.
Traces: iot-certificate-list-lifecycle#task#3.

Cause 2 of 3: ListDomainConfigurations omits the two built-in domain configurations every account has.
AWS: Smithy: ListDomainConfigurations (GET /domainConfigurations) returns domainConfigurations: DomainConfigurations, paginated over marker/nextMarker. Recorded behavior: every account is served two service-owned configurations with no customer action — domainConfigurationName 'iot:Data-ATS' with serviceType 'DATA', and 'iot:CredentialProvider' with serviceType 'CREDENTIAL_PROVIDER', each with domainConfigurationArn arn:aws:iot:<region>:<account>:domainconfiguration/<domainConfigurationName> and no ipAddressType.
Emulator: returns neither entry (both positions are absent from domainConfigurations).
Why: a client that discovers its data or credential-provider domain by listing domain configurations finds nothing and cannot resolve an endpoint, and a customer configuration cannot be distinguished from the built-ins. (Open issue 20261114T091403Z-iot-domain-configuration-plane-7-causes-an-untrusted.md describes a related absence of the built-in iot:Data-ATS configuration but does not carry this step's replay key.)
Fix: seed the two service-owned domain configurations in the IoT domain-configuration state so the ListDomainConfigurations handler returns them.
Traces: iot-domain-configuration-inspection#task#2.

Cause 3 of 3: GetTopicRuleDestination serves an authored statusReason for an HTTP destination awaiting confirmation.
AWS: recorded behavior only — the recorded step asserts topicRuleDestination.statusReason by shape, and the replay reports the emulator's value as diverging from that projection. Smithy: GetTopicRuleDestination returns topicRuleDestination: TopicRuleDestination, whose statusReason explains the destination's status; for an HTTP destination in IN_PROGRESS AWS's statusReason reports that a confirmation request was sent to the endpoint and is awaiting confirmation.
Emulator: serves the fixed sentence 'Awaiting confirmation. The confirmation message has not been sent to the destination endpoint yet.' — prose the service does not use, and which asserts the opposite of AWS (that nothing was sent). Hypothesis: the emulator never dispatches a confirmation request to the destination endpoint, so it has no真 status to report.
Why: an operator reading statusReason concludes the service failed to send the confirmation and starts debugging the wrong thing; a client waiting for the confirmation token never receives one.
Fix: the GetTopicRuleDestination projection's statusReason, and the topic-rule-destination state that should record the confirmation dispatch.
Traces: iot-topic-rule-destination-http-lifecycle#task#2.

## Evidence
Cause 1 (iot-certificate-list-lifecycle#task#3, 200 vs 200): $.certificates[0] expected certificateMode None, got 'DEFAULT'; same for $.certificates[1]. (The cluster's creationDate difference — expected 1787052770.334, got 1794758606.9355242 — is a per-run clock value and excluded from this finding.)
Cause 2 (iot-domain-configuration-inspection#task#2, 200 vs 200): $.domainConfigurations[0] missing — expected {'domainConfigurationArn': 'arn:aws:iot:<region>:<account>:domainconfiguration/iot:Data-ATS', 'domainConfigurationName': 'iot:Data-ATS', 'ipAddressType': None, 'serviceType': 'DATA'}, got None; $.domainConfigurations[1] missing — expected the same shape for 'iot:CredentialProvider' with serviceType 'CREDENTIAL_PROVIDER', got None.
Cause 3 (iot-topic-rule-destination-http-lifecycle#task#2, 200 vs 200): $.topicRuleDestination.statusReason value_mismatch — expected the recorded shaped projection, got 'Awaiting confirmation. The confirmation message has not been sent to the destination endpoint yet.'

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: register two certificates, then GET /certificates (fixture iot-certificate-list-lifecycle, step task#3) and inspect certificates[].certificateMode.
3. Cause 2: GET /domainConfigurations on a fresh account (fixture iot-domain-configuration-inspection, step task#2) and look for 'iot:Data-ATS' (DATA) and 'iot:CredentialProvider' (CREDENTIAL_PROVIDER).
4. Cause 3: create an HTTP topic-rule destination, then GET /destinations/{arn+} (fixture iot-topic-rule-destination-http-lifecycle, step task#2) and inspect topicRuleDestination.statusReason.
