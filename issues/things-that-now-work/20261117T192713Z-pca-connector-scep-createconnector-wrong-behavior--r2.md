Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: a1f0ee3bf02657ddb29b8d956a2d1c147a47a855

# CreateConnector holds two contradictory rules for a mis-placed CertificateAuthorityArn: the 404 branch is not guarded by the placement reading the comment beside it says governs it

_Filed: 2026-11-17T19:27:12Z_
_Operation: CreateConnector | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a1f0ee3bf02657ddb29b8d956a2d1c147a47a855`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

CreateConnector resolves `CertificateAuthorityArn` twice: once against the Private CA service (`private_ca::resolve`) and once from the ARN's own partition/Region/account segments (`state::private_ca_failure_reason`). The handler deliberately SUPPRESSES one synchronous refusal when the placement reading already says the CA is out of reach (`if private_ca_failure.is_none() && private_ca.is_not_active()`), so that outcome lands on the connector as `FAILED` + a `ConnectorStatusReason`. The sibling refusal immediately above it — `if private_ca.is_absent() { return Err(...404...) }` — carries no such guard, even though the comment between the two says a mis-placed CA "is reported on the connector rather than refused here".

Consequence: whether the same request answers 404 or 202-then-FAILED depends only on whether the Amazon Web Services Private CA service happens to be mounted on the router. With Private CA mounted — which `missing_resource_status_class.rs` states is "what the assembled emulator does" — a CA ARN naming another Region/account/partition is one that service holds no record under, so `resolve` answers `Absent` and the unguarded branch refuses the call 404 with no connector created. The whole placement path (`private_ca_failure_reason`, and with it the `PRIVATECA_RESOURCE_NOT_FOUND` / `PRIVATECA_ACCESS_DENIED` outcomes it is the only producer of) is then unreachable, contradicting both the comment and the suite that pins it.

The crate's own two test suites assert OPPOSITE answers for the same request class (same REGION `eu-west-2`, same emulator ACCOUNT, only the CA uuid differs), and both pass only because one of them builds a router WITHOUT Private CA, which makes `resolve` take its "dependency did not answer -> Active" fallback. One of the two rules is wrong, and nothing in the crate says which.

## Evidence

Handler, the guarded sibling vs the unguarded branch — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/create_connector.rs:245-249 and :251-253 and :263:
```
245:        if private_ca.is_absent() {
246:            return Err(certificate_authority_not_found(
247:                &input.certificate_authority_arn,
248:            ));
249:        }
251:        // Where the ARN PLACES the CA, decided from the request alone
252:        // (`crate::state::private_ca_failure_reason`): a CA that sits in another partition,
253:        // Region or account is not one this endpoint and principal resolve, which is reported
254:        // on the connector rather than refused here (see below).
263:        if private_ca_failure.is_none() && private_ca.is_not_active() {
```
What makes `Absent` fire for a mis-placed ARN — src/service/private_ca.rs:88-89:
```
88:    match error.as_service_error() {
89:        Some(service_error) if service_error.is_resource_not_found_exception() => Resolved::Absent,
```
and the owning service answers exactly that for an ARN it holds no record under — services/service-com-amazonaws-acmpca-acmprivateca/src/ops/describe_certificate_authority.rs:192-199: `let Some(record) = state.certificate_authority_mut(&arn, ...) else { return Err(... ResourceNotFoundException ...) }`.

Side A of the repo contradiction — tests/all/missing_resource_status_class.rs:304-306 ("An emulator holding this service AND Amazon Web Services Private CA over one shared engine, which is what the assembled emulator mounts"), row :374-377 `("a CA of another account", ca_arn("aws", REGION, "123456789012", "99999999-..."))`, asserted at :399-405:
```
399:        assert_eq!(
400:            (answer.status, answer.error_type.as_str()),
401:            (404, "ResourceNotFoundException"),
```
with REGION = "eu-west-2" (:46) and ACCOUNT = "000000000000" (:43), and :425-431 asserting no connector was registered.

Side B — tests/all/connector_private_ca_reachability.rs row :148-157 `("a CA of another account in this connector's own Region", ca_arn("aws", REGION, "123456789012", "22222222-..."), reason: Some(REASON_ACCESS_DENIED))` with REGION = "eu-west-2" (:35), asserted at :225-230 `(created.status, created.error_type.as_str()) == (202, "")` and :284-298 `Status == FAILED`, `StatusReason == PRIVATECA_ACCESS_DENIED`. Its rows for another Region (:98-127) and another partition (:128-147) assert the same 202 + FAILED. That suite's router registers only this service — tests/all/harness.rs:95-103:
```
101:    router.register_service(scep::EmulatedService::new(Arc::clone(&engine)));
```
so no CA in it is ever created in Private CA and `resolve` returns `Resolved::Active` through its documented "the question could not be put" fallback (src/service/private_ca.rs:42-45, :91-98).

Dead-in-practice reason constant on the same side: state.rs:80-83 documents `REASON_PRIVATECA_RESOURCE_NOT_FOUND` / `REASON_PRIVATECA_ACCESS_DENIED` as statuses a created connector carries, and `private_ca_failure_reason` (state.rs:745-759) is their only producer.

## Steps to Reproduce

1. Build a router holding this service AND `service_com_amazonaws_acmpca_acmprivateca` on one shared engine, exactly as tests/all/missing_resource_status_class.rs:306-317 does.
2. POST /connectors signed for eu-west-2 with body {"CertificateAuthorityArn": "arn:aws:acm-pca:eu-west-2:123456789012:certificate-authority/22222222-3333-4444-5555-666677778888"} (another account, this Region).
3. Observe 404 ResourceNotFoundException and no connector created: `resolve` -> Absent via create_connector.rs:245.
4. Run the same request against a router registering only this service (tests/all/harness.rs:95-103) and observe 202, then after releasing the pending transition a connector with Status FAILED and StatusReason PRIVATECA_ACCESS_DENIED.
5. Repeat with another Region (ap-northeast-1) and another partition (aws-us-gov): the two suites assert 404 and 202+FAILED for the same class of request.
