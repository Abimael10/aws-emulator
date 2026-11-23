Issue-Source: trace-sweep-divergence
Issue-Trace: acm-certificate-dns-explicit-key-algorithm-and-logging-pending-validation-tags-and-not-found
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acm-certificate-dns-explicit-key-algorithm-and-logging-pending-validation-tags-and-not-found acm-certificate-domain-validations-lifecycle acm-certificate-request-and-domain-validations acm-request-dns-certificate-lifecycle
Issue-Shape-Id: com.amazonaws.acm#CertificateManager
Issue-Key: acm|DescribeCertificate|2xx->2xx|$.Certificate.DomainValidationOptions[*].ResourceRecord.Value
Issue-Key: acm|ListCertificateDomainValidations|2xx->2xx|$.DomainValidationSummaryList[*]
Issue-Commit: d320d15df1e8a4ddf926cc8f8c86b65a79910efa

# ACM DNS-validation challenge diverges: loopback CNAME target and a ResourceRecord AWS omits from the list challenge (2 causes)

> _Note: this issue was identified against the code base at commit `d320d15df1e8a4ddf926cc8f8c86b65a79910efa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCertificate

## Description
Two net-new root causes in how this emulator publishes the ACM DNS domain-validation challenge, both in the ACM certificate projection. Both were replayed against the running emulator (score 98.8%, 5 mismatched steps, all ACM) and neither is owned by an open issue.

Cause 1 of 2: the DNS-validation CNAME target is announced in the reserved loopback zone instead of ACM's validation zone.
AWS: DescribeCertificate projects CertificateDetail$DomainValidationOptions[*].ResourceRecord{Name, Type=CNAME, Value} (Smithy: ResourceRecord members Name/Type/Value all @required; ResourceRecord$Value doc 'The value of the CNAME record to add to your DNS database. This is supplied by ACM.'; the shape's own doc adds 'This is used by the DescribeCertificate action.'). The model does not spell the target's grammar out, so this is recorded behavior plus the host rule the replay asserts: the Value is an ACM-owned CNAME target of the form `_<32-hex challenge token>.<short per-validation label>.acm-validations.aws.` — the two leading labels are per-validation VALUES (derived from the certificate/validation, different on every run, never asserted literally), while the trailing zone `acm-validations.aws.` is FIXED TEXT and is the part that makes the target a name the answering validation surface owns. The replay's host check is exactly this: the host a response announces must be one the responder owns/serves.
Emulator: the Value's zone is substituted with the loopback zone, e.g. `_<32-hex>.<label>.acm-validations.localhost.` (replayed value `_06729543f0147e248bac574746b19c27.4253643fee.acm-validations.localhost.`), which the replay flags `host_not_own` at $.Certificate.DomainValidationOptions[0].ResourceRecord.Value — the announced name is neither ACM's validation zone nor the address the request reached this emulator on. HTTP status is 200 on both sides; only the member's value diverges.
Why: a client that reads the challenge and publishes the CNAME points its DNS at a name inside RFC 6761's `.localhost.` zone, which can never be followed by any resolver other than the caller's own loopback, so domain ownership can never be proven; automation that parses or asserts the ACM target's zone (renewal tooling, Terraform/CDK validation-record wiring, the delegation-structure checks ACM's own docs describe) rejects the record outright.
Fix: the validation-record minting in the ACM service state — the zone chosen for the CNAME target, i.e. the `announced_validation_zone` / `acm_validations_cname_target` pair that `domain_validation_record` and `acme_prevalidation_resource_record` both render through — must publish ACM's validation zone rather than the zone of the authority the request arrived on. The DescribeCertificate projection then reports the stored record unchanged. (See Traces for the file/line citations.)
Traces: acm-certificate-dns-explicit-key-algorithm-and-logging-pending-validation-tags-and-not-found#task#2, also #task#11; code: services/service-com-amazonaws-acm-certificatemanager/src/state.rs lines 616-652 (ACM_VALIDATIONS_LABEL, announced_validation_zone) and 3896-3923 (acm_validations_cname_target).

Cause 2 of 2: ListCertificateDomainValidations populates DnsValidationChallenge$ResourceRecord, which AWS leaves out of that response.
AWS: ListCertificateDomainValidations returns DomainValidationSummaryList[*] = DomainValidationSummary{DomainName, ActiveValidationConfiguration, RequestedValidationConfiguration}, and the configuration's ValidationChallenge is a union whose DNS arm is DnsValidationChallenge{ResourceRecord} (Smithy: ValidationChallenge union members EmailValidationChallenge|DnsValidationChallenge; DnsValidationChallenge$ResourceRecord is OPTIONAL). Recorded behavior: for a DNS-validated certificate in PENDING_VALIDATION, this operation answers the DNS arm as an EMPTY structure — `"DnsValidationChallenge": {}` — carrying no ResourceRecord; the CNAME is reported by DescribeCertificate, which is what the ResourceRecord shape's own doc says it is used by. Status 200.
Emulator: the same step answers 200 but fills the arm in, `DnsValidationChallenge: {ResourceRecord: {Name: '_<32-hex>.<domain>.', Type: 'CNAME', Value: '_<32-hex>.<label>.acm-validations.localhost.'}}`, so DomainValidationSummaryList[0] does not match the recorded element (the replay reports it as $.DomainValidationSummaryList[0] missing, expected vs got differing only in that sub-member). Member ordering within the object is not a divergence; the extra ResourceRecord is.
Why: a client that uses the presence of DnsValidationChallenge$ResourceRecord in a list summary as the signal that this list carries the record to publish (rather than calling DescribeCertificate for it) takes a path real ACM never offers, and against AWS reads None; conversely a consumer that round-trips or diffs the list response against ACM's sees an unmodeled extra sub-member. Here the value it leaks is also the loopback target of Cause 1, so the two compound.
Fix: the ListCertificateDomainValidations handler's challenge projection — the DNS arm of `validation_challenge` must emit DnsValidationChallenge with no ResourceRecord, leaving the CNAME to DescribeCertificate's DomainValidationOptions projection. The handler's doc comment currently argues the opposite, so the comment at lines 13-20 / 359-383 has to change with it.
Traces: acm-certificate-domain-validations-lifecycle#task#2, also acm-certificate-request-and-domain-validations#task#2 and acm-request-dns-certificate-lifecycle#task#2; code: services/service-com-amazonaws-acm-certificatemanager/src/ops/list_certificate_domain_validations.rs lines 384-405 (validation_challenge, DNS arm sets resource_record).

Related open issues, none of which cover these: 20261022T143245Z-acm-certificate-representation-dns-validation-and-error-message (its Cause 2 is the MISSING middle label in the CNAME target — that label is now present; the zone is the new defect) and 20261118T091700Z-acm-acme-endpoint-endpointurl-is-published-on-the (the same class of announced-host defect on the ACME EndpointUrl, a different member and operation).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up=true, /__admin/health 200, :36377 data listener accepting): 407 steps, 402 passed, 5 mismatched, 0 blocked, 0 errored, score 98.8%, 2 divergence clusters — both service=acm, both stub_path empty and owned_by empty.

Cause 1 — cluster service=acm|DescribeCertificate|status=200->200|keys=$.Certificate.DomainValidationOptions[0].ResourceRecord.Value, verdict mismatch, expected_status 200, actual_status 200, affected_count 2:
  detail: $.Certificate.DomainValidationOptions[0].ResourceRecord.Value: host_not_own (expected '<shaped-not-asserted>', got '_06729543f0147e248bac574746b19c27.4253643fee.acm-validations.localhost.')
  The expected value is shape-asserted, not literal: the per-validation labels `_06729543f0147e248bac574746b19c27` and `4253643fee` are run-specific values and are NOT the requirement. The requirement is the zone the target is announced in — `acm-validations.aws.` (fixed text) — and the emulator announces `acm-validations.localhost.`, a host it does not own and was not reached on.
  Fixtures: acm-certificate-dns-explicit-key-algorithm-and-logging-pending-validation-tags-and-not-found#task#2 and #task#11.

Cause 2 — cluster service=acm|ListCertificateDomainValidations|status=200->200|keys=$.DomainValidationSummaryList[0], verdict mismatch, expected_status 200, actual_status 200, affected_count 3:
  expected: {'DomainName': '<name under test>', 'RequestedValidationConfiguration': {'ValidationChallenge': {'DnsValidationChallenge': {}}, 'ValidationMethod': 'DNS', 'ValidationStatus': 'PENDING_VALIDATION'}}
  got:      {'DomainName': '<same name>', 'RequestedValidationConfiguration': {'ValidationMethod': 'DNS', 'ValidationChallenge': {'DnsValidationChallenge': {'ResourceRecord': {'Name': '_<32-hex>.<name>.', 'Type': 'CNAME', 'Value': '_<32-hex>.<label>.acm-validations.localhost.'}}}, 'ValidationStatus': 'PENDING_VALIDATION'}}
  The sole structural difference is the added ResourceRecord sub-member inside the DNS arm (the domain name, ValidationMethod and ValidationStatus all agree; member order does not).
  Fixtures: acm-certificate-domain-validations-lifecycle#task#2, acm-certificate-request-and-domain-validations#task#2, acm-request-dns-certificate-lifecycle#task#2.

Smithy citations (services/service-com-amazonaws-acm-certificatemanager/smithy-model/com-amazonaws-acm-certificatemanager.json): DescribeCertificate -> CertificateDetail; DomainValidation{DomainName, ValidationStatus, ResourceRecord, ValidationMethod, ValidationDomain}; ResourceRecord{Name @required, Type @required enum RecordType=CNAME, Value @required} with shape doc 'This is used by the DescribeCertificate action.'; ListCertificateDomainValidations -> DomainValidationSummaryList of DomainValidationSummary{DomainName @required, ActiveValidationConfiguration, RequestedValidationConfiguration}; ValidationConfiguration{ValidationMethod, ValidationChallenge, ValidationStatus}; ValidationChallenge = union{EmailValidationChallenge, DnsValidationChallenge}; DnsValidationChallenge{ResourceRecord} (optional).

## Steps to Reproduce
1. git_clone, then replay the wire corpus (trace set simparity-wire-aws-v1) against the booted emulator.
2. Cause 1: replay fixture acm-certificate-dns-explicit-key-algorithm-and-logging-pending-validation-tags-and-not-found, step task#2 (and task#11) — RequestCertificate with ValidationMethod=DNS, then DescribeCertificate on the returned ARN. Read $.Certificate.DomainValidationOptions[0].ResourceRecord.Value: its trailing zone is `acm-validations.localhost.` instead of ACM's `acm-validations.aws.`, i.e. a host the emulator neither owns nor serves.
3. Cause 2: replay fixture acm-certificate-domain-validations-lifecycle, step task#2 (likewise acm-certificate-request-and-domain-validations#task#2 and acm-request-dns-certificate-lifecycle#task#2) — RequestCertificate with ValidationMethod=DNS, then ListCertificateDomainValidations on that ARN. Read $.DomainValidationSummaryList[0].RequestedValidationConfiguration.ValidationChallenge.DnsValidationChallenge: the emulator returns a ResourceRecord inside it where AWS returns the empty structure {}.
4. After fixing, re-replay all four fixture/step bindings named above (issue_trace/issue_step plus trace_ids) and confirm both clusters pass.
