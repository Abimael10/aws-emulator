Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm.list-certificate-domain-validations-returns-options
Issue-Shape-Id: com.amazonaws.acm#CertificateManager
Issue-Key: acm.list-certificate-domain-validations-returns-options|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Commit: dda52bc4d44c9442bf06859cd046728319609663

# ACM ListCertificateDomainValidations answers the DNS arm as an empty DnsValidationChallenge, so the CNAME the operation documents it returns is never retrievable from the list

> _Note: this issue was identified against the code base at commit `dda52bc4d44c9442bf06859cd046728319609663`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCertificateDomainValidations

## Description
One net-new root cause, grounded in behavioral check acm.list-certificate-domain-validations-returns-options (verdict mismatch — the check never reaches a verdict and is killed at the 60 s budget while polling for the challenge record).

Cause 1 of 1: the DNS arm of a domain-validation summary's ValidationChallenge carries no ResourceRecord, so a DNS-validated certificate's CNAME is absent from every page of this list.

AWS: ListCertificateDomainValidations returns DomainValidationSummaryList[*] = DomainValidationSummary{DomainName, ActiveValidationConfiguration, RequestedValidationConfiguration}; the configuration's ValidationChallenge is a union whose DNS arm is DnsValidationChallenge{ResourceRecord}, and the model states what that arm is for. Smithy (services/service-com-amazonaws-acm-certificatemanager/smithy-model/com-amazonaws-acm-certificatemanager.json): ListCertificateDomainValidations doc — "You can use the results to monitor the progress of an email-to-DNS validation migration and to retrieve the CNAME records required for DNS validation"; DnsValidationChallenge shape doc — "Contains the CNAME record that you must add to your DNS configuration to validate domain ownership using DNS validation"; DnsValidationChallenge$ResourceRecord doc — "The CNAME record that ACM creates for DNS validation. Add this record to your DNS configuration to prove that you own or control the domain" (the member is optional in the model, i.e. absent only where there is no CNAME to report, e.g. the EMAIL/HTTP methods); ValidationConfiguration$ValidationChallenge doc — "for DNS validation, contains a DnsValidationChallenge with the CNAME record to add"; ResourceRecord{Name @required, Type @required (RecordType, CNAME), Value @required}. Recorded behavior: for a certificate requested with ValidationMethod=DNS and still in PENDING_VALIDATION, the single summary's validation configuration carries ValidationMethod=DNS, ValidationStatus=PENDING_VALIDATION and a DNS arm holding a CNAME ResourceRecord with a non-empty Name and a non-empty Value — the check accepts the record under either ActiveValidationConfiguration or RequestedValidationConfiguration and asserts only that it is there, is Type=CNAME, and has non-empty Name/Value (the token/label/zone values themselves are per-run and are not asserted). Status 200 on both sides; only the member's presence diverges.

Emulator: the handler deliberately emits the DNS arm empty — ValidationChallenge::DnsValidationChallenge(DnsValidationChallenge { resource_record: None }) — so the response is 200 with DomainValidationSummaryList[0].RequestedValidationConfiguration.ValidationChallenge.DnsValidationChallenge = {}. The check polls ListCertificateDomainValidations every 2 s for a summary whose challenge contains a {Name, Type, Value} record; because that record can never appear, the predicate is never satisfied and the check is terminated at the 60 s budget ("Check timed out after 60.0s") with no verdict on the rest of its assertions (ValidationMethod, ValidationStatus, DescribeCertificate cross-check), which therefore remain unverified behind this one defect.

Why: ListCertificateDomainValidations is the operation AWS documents as the way to "retrieve the CNAME records required for DNS validation". A client that calls it to obtain the record to publish — rather than calling DescribeCertificate — reads no ResourceRecord at all against this emulator, so it either publishes nothing and the certificate stays PENDING_VALIDATION forever, or it crashes/loops on the absent member. A poller written against AWS (wait for the challenge record, then publish it) never terminates, which is exactly what this check shows.

Note on a conflicting open issue: 20261123T064357Z-acm-dns-validation-challenge-diverges-loopback-cname-target.md (Cause 2) asserts the opposite — that AWS answers this arm as the empty structure and asks for resource_record to be dropped — and the handler's current code and doc comment implement that request. The behavioral recording and the model's own wording of this operation and of DnsValidationChallenge$ResourceRecord say the record IS carried here, so that cause should be withdrawn/reconciled rather than re-applied; it does not cover this check (the suite does not list this check as owned by it). Cause 1 of that issue (the CNAME target announced in acm-validations.localhost. instead of ACM's validation zone) is a DIFFERENT defect in the value of the record and stays with that issue — this issue is only about the record being absent from the list response.

Fix: the ListCertificateDomainValidations handler's challenge projection — the DNS arm of validation_challenge must report the CNAME already stored on the per-domain validation record (the same record DescribeCertificate projects onto CertificateDetail$DomainValidationOptions[*].ResourceRecord, minted in the ACM service state), instead of None. The module's doc comment argues for the empty arm and must change with it. Leave the EMAIL and HTTP arms as they are.

Traces: behavioral check acm.list-certificate-domain-validations-returns-options. Code citations: services/service-com-amazonaws-acm-certificatemanager/src/ops/list_certificate_domain_validations.rs lines 358-403 (validation_challenge; the DNS arm sets resource_record: None) and its module doc at lines 13-19; the stored record is built by services/service-com-amazonaws-acm-certificatemanager/src/state.rs lines 3876-3888 (domain_validation_record).

## Evidence
Behavioral suite run against the booted emulator: 262/283 passed (92.6%), 17 mismatched, 4 errored, 0 skipped. Under ### acm the suite lists [mismatch] acm.list-certificate-domain-validations-returns-options with "Error: Check timed out after 60.0s" and no OWNED-by / ALREADY-FILED marker (every other acm failure in this run is marked owned).

What the check does (verbatim from the report's check source): RequestCertificate(DomainName=<unique>.com, ValidationMethod=DNS), then poll ListCertificateDomainValidations(CertificateArn=arn) every 2 s for up to 60 s until the response has exactly one DomainValidationSummary whose ActiveValidationConfiguration or RequestedValidationConfiguration contains a ValidationChallenge holding a dict with the keys {Name, Type, Value}:

    def _challenge_config(summary):
        for key in ("ActiveValidationConfiguration", "RequestedValidationConfiguration"):
            config = summary.get(key) or {}
            challenge = config.get("ValidationChallenge")
            if challenge and _find_resource_record(challenge) is not None:
                return config
        return None

It then asserts record["Type"] == "CNAME", record["Name"] non-empty, record["Value"] non-empty, ValidationMethod == "DNS", ValidationStatus == "PENDING_VALIDATION", and cross-checks DescribeCertificate Status/DomainValidationOptions[0].ValidationStatus == PENDING_VALIDATION.

Expected (AWS): DomainValidationSummaryList == [ {DomainName: <requested name>, <Active|Requested>ValidationConfiguration: {ValidationMethod: "DNS", ValidationStatus: "PENDING_VALIDATION", ValidationChallenge: {DnsValidationChallenge: {ResourceRecord: {Name: <non-empty>, Type: "CNAME", Value: <non-empty>}}}} } ] — the Name/Value strings are per-validation values, not literals.

Actual (emulator): HTTP 200 with DomainValidationSummaryList == [ {DomainName: <requested name>, RequestedValidationConfiguration: {ValidationMethod: "DNS", ValidationStatus: "PENDING_VALIDATION", ValidationChallenge: {DnsValidationChallenge: {}}}} ] on every poll — the DNS arm is empty, so _find_resource_record returns None forever and the harness kills the check at 60 s.

Source of the behavior, services/service-com-amazonaws-acm-certificatemanager/src/ops/list_certificate_domain_validations.rs:394-398:

        crate::state::VALIDATION_METHOD_DNS => Some(
            ValidationChallenge::DnsValidationChallenge(DnsValidationChallenge {
                resource_record: None,
            }),
        ),

Model citations: ListCertificateDomainValidations doc ("...to retrieve the CNAME records required for DNS validation"); DnsValidationChallenge doc ("Contains the CNAME record that you must add to your DNS configuration..."); DnsValidationChallenge$ResourceRecord doc ("The CNAME record that ACM creates for DNS validation..."); ValidationConfiguration$ValidationChallenge doc ("for DNS validation, contains a DnsValidationChallenge with the CNAME record to add"); ResourceRecord{Name,Type=CNAME,Value} all @required.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. Observe, under ### acm, [mismatch] acm.list-certificate-domain-validations-returns-options — "Check timed out after 60.0s".
3. By hand: acm.request_certificate(DomainName="<unique>.com", ValidationMethod="DNS") and keep the returned CertificateArn.
4. acm.list_certificate_domain_validations(CertificateArn=arn): the single summary's RequestedValidationConfiguration.ValidationChallenge.DnsValidationChallenge is {} — no ResourceRecord — and stays {} however long you poll, while the certificate remains PENDING_VALIDATION.
5. acm.describe_certificate(CertificateArn=arn): Certificate.DomainValidationOptions[0].ResourceRecord IS present with Name/Type=CNAME/Value, showing the record exists in state and is simply not projected into the list response.
6. After fixing, re-replay acm.list-certificate-domain-validations-returns-options and confirm it completes (it also asserts ValidationMethod=DNS, ValidationStatus=PENDING_VALIDATION and the DescribeCertificate cross-check, none of which could be reached while the poll hung).
