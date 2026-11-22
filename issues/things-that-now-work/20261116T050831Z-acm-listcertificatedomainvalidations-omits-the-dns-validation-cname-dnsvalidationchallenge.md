Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm.list-certificate-domain-validations-returns-options
Issue-Shape-Id: com.amazonaws.acm#CertificateManager
Issue-Key: acm.list-certificate-domain-validations-returns-options|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Commit: 7b48dd1812adb90a036957570722499f0872b4d2

# ACM ListCertificateDomainValidations omits the DNS validation CNAME: DnsValidationChallenge is emitted empty, so the documented way to retrieve the validation record returns nothing

> _Note: this issue was identified against the code base at commit `7b48dd1812adb90a036957570722499f0872b4d2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCertificateDomainValidations

## Description
One net-new root cause in ACM's per-domain validation projection.

Cause 1 of 1: ListCertificateDomainValidations reports a DNS-validated domain with an EMPTY DnsValidationChallenge — the summary's ValidationChallenge union is populated with the DNS arm, but that arm carries no ResourceRecord, so the CNAME the caller must publish is never reported by this operation.

AWS: Smithy com.amazonaws.acm#ListCertificateDomainValidations -> ListCertificateDomainValidationsResponse{DomainValidationSummaryList: DomainValidationSummary[]}; DomainValidationSummary{DomainName required, ActiveValidationConfiguration, RequestedValidationConfiguration} -> ValidationConfiguration{ValidationMethod(EMAIL|DNS|HTTP), ValidationChallenge, ValidationStatus(PENDING_VALIDATION|SUCCESS|FAILED)}; ValidationChallenge is a union with DnsValidationChallenge{ResourceRecord} and ResourceRecord{Name required, Type required (enum CNAME), Value required}. The operation's own model documentation states the results are how a caller monitors validation and "retrieve the CNAME records required for DNS validation" — i.e. for a DNS-validated name the challenge carries a fully-populated ResourceRecord (Name, Type=CNAME, Value) once the record has been minted, which for a certificate requested with ValidationMethod=DNS is from the moment the request is accepted. Recorded behavior (the check's own comment: "ValidationChallenge appears empty before its CNAME record lands; wait for the record") is that the challenge becomes populated with that record within seconds and the configuration reads ValidationMethod=DNS, ValidationStatus=PENDING_VALIDATION.

Emulator: for a certificate requested with ValidationMethod=DNS, ListCertificateDomainValidations answers HTTP 200 with one summary whose RequestedValidationConfiguration.ValidationChallenge is DnsValidationChallenge with NO ResourceRecord member at all — permanently, not transiently. The handler builds the DNS arm as an all-default DnsValidationChallenge and documents this as intentional, deferring the record to DescribeCertificate's CertificateDetail$DomainValidationOptions[*].ResourceRecord. Consequently the check polled this operation for 60s waiting for a challenge containing a {Name, Type, Value} record and was killed at the 60.0s budget with no verdict ("Check timed out after 60.0s"); the assertions after the poll (DomainName echo, ValidationMethod=DNS, ValidationStatus=PENDING_VALIDATION, Type=CNAME, non-empty Name/Value, and the DescribeCertificate cross-check) were never reached.

Why: a client that follows the operation's documented purpose — list the domain validations and publish the CNAME each challenge hands it — gets an empty challenge and can never complete DNS validation through this API; code that reads summary['RequestedValidationConfiguration']['ValidationChallenge']['DnsValidationChallenge']['ResourceRecord'] raises KeyError, and any poll loop keyed on the record appearing hangs until its own timeout. It also makes the two readers of one store disagree: DescribeCertificate reports a record for the same name while this operation reports none.

Fix: the ListCertificateDomainValidations response projection — the challenge builder for the DNS arm must carry this domain's stored validation record as DnsValidationChallenge.ResourceRecord{Name, Type=CNAME, Value} (the same record the certificate state minted and DescribeCertificate projects), rather than an all-default DnsValidationChallenge. File and line citations under Traces.

Traces: acm.list-certificate-domain-validations-returns-options. Handler: services/service-com-amazonaws-acm-certificatemanager/src/ops/list_certificate_domain_validations.rs — validation_challenge() (lines 381-400) returns ValidationChallenge::DnsValidationChallenge(DnsValidationChallenge::default()) for VALIDATION_METHOD_DNS, with the module doc (lines 12-18) and the function doc (lines 357-380) stating the DNS arm deliberately carries no record. Related but distinct open issues (they do NOT cover this operation): 20261022T143245Z-acm-certificate-representation-dns-validation-and-error-message.md (DescribeCertificate's projection and the CNAME value shape) and 20261110T181625Z-acm-requested-certificates-never-settle-terminally-describecertificate-stays.md (certificate status settling).

## Evidence
acm.list-certificate-domain-validations-returns-options — [mismatch] "Error: Check timed out after 60.0s". The check requests a certificate with ValidationMethod='DNS', then polls acm.list_certificate_domain_validations(CertificateArn=arn)['DomainValidationSummaryList'] every 2s for up to 60s for a predicate of: exactly one summary whose ActiveValidationConfiguration or RequestedValidationConfiguration has a ValidationChallenge containing a dict with the keys {Name, Type, Value}. The predicate is never satisfied because the emulator's DNS challenge is an empty DnsValidationChallenge (no ResourceRecord), so the 60s budget expires and the check yields no verdict.
Expected (AWS): DomainValidationSummaryList = [ {DomainName: <the requested name>, RequestedValidationConfiguration: {ValidationMethod: 'DNS', ValidationStatus: 'PENDING_VALIDATION', ValidationChallenge: {DnsValidationChallenge: {ResourceRecord: {Name: '_<token>.<domain>.', Type: 'CNAME', Value: '_<token>.<label>.acm-validations.aws.'}}}} } ] — the Name/Value strings are values derived from the request and the service's own minted validation token, not literals from any one run; the requirement is that all three required ResourceRecord members are present and Type is the single modeled enum value CNAME.
Actual (emulator): one summary with RequestedValidationConfiguration = {ValidationMethod: 'DNS', ValidationStatus: 'PENDING_VALIDATION', ValidationChallenge: {DnsValidationChallenge: {}}} — the required ResourceRecord sub-structure is absent on every poll. Status is 200 and no error code is involved; the divergence is a missing member, not a refusal.
Smithy citations: ListCertificateDomainValidationsResponse.DomainValidationSummaryList; DomainValidationSummary{DomainName, ActiveValidationConfiguration, RequestedValidationConfiguration}; ValidationConfiguration{ValidationMethod, ValidationChallenge, ValidationStatus}; ValidationChallenge union{EmailValidationChallenge, DnsValidationChallenge}; DnsValidationChallenge{ResourceRecord}; ResourceRecord{Name required, Type required enum[CNAME], Value required}; operation doc: "...retrieve the CNAME records required for DNS validation".

## Steps to Reproduce
1. Boot the emulator and point an ACM client at it.
2. arn = acm.request_certificate(DomainName='<a fresh public name>.com', ValidationMethod='DNS')['CertificateArn'].
3. acm.list_certificate_domain_validations(CertificateArn=arn) — read DomainValidationSummaryList[0]['RequestedValidationConfiguration']['ValidationChallenge']['DnsValidationChallenge']: it is an empty structure, with no 'ResourceRecord' member, and stays empty however long it is polled.
4. acm.describe_certificate(CertificateArn=arn)['Certificate']['DomainValidationOptions'][0] — a ResourceRecord IS reported there, showing the record exists in state but is not projected by this operation.
5. Re-replay acm.list-certificate-domain-validations-returns-options to confirm the fix.
