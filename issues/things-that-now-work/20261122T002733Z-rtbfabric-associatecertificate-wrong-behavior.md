Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 05866a96998b37f437bd411ae9396664f070e9d9

# AssociateCertificate rewrites an association whose removal is pending, silently cancelling the accepted DisassociateCertificate — the crate's own definition says such a certificate is "still serving"

_Filed: 2026-11-22T00:27:33Z_
_Operation: AssociateCertificate | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `05866a96998b37f437bd411ae9396664f070e9d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The certificate surface decides what a repeat of `AssociateCertificate` does through one helper, `CertificateAssociation::association_in_force()`. That helper's doc says it answers "Whether the certificate is serving, or on its way to serving: the two statuses a repeat of the association asks for rather than starting afresh" — but its body lists only PENDING_ASSOCIATION and ASSOCIATED, while the constant for the third status states in the same file that a PENDING_DISASSOCIATION certificate "is still serving". The two statements contradict each other, and the handler follows the narrow one.

Consequence: a certificate whose removal this service has already ACCEPTED (DisassociateCertificate answered 200 with status PENDING_DISASSOCIATION, and the clock owes DISASSOCIATED at `now + PROVISIONING_DELAY`) is NOT treated as a record that is already there. The handler falls past the in-force branch and constructs a brand-new `CertificateAssociation`, which `store.certificate_associations.insert(key, association)` writes over the existing record: `status` goes back to PENDING_ASSOCIATION, `settles_at` is re-armed, and `associated_at` is reset to now. The accepted removal therefore never completes — state the caller was told was pending is destroyed by an unrelated call — and the record's `associatedAt` moves for a record that was in force moments earlier, which the surface's own repeat rule is written to avoid.

The crate has the guard for exactly this shape of race one module away: `LinkRoutingRule::deleting()` exists so `DeleteLinkRoutingRule` does not restart a deletion that is already outstanding, and `DisassociateCertificate` itself only moves a record when `association_in_force()` holds "so it answers the record as it stands rather than starting the removal again". `AssociateCertificate` is the one path on this surface that rewrites a record mid-transition instead of answering it. (`AssociateCertificate` also declares the modelled `ConflictException` (409) and never constructs it, so the refusal channel for a status that does not admit the transition is unused.)

## Evidence

The crate's own definition of the status, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state/certificates.rs:20-22:
```
/// `CertificateAssociationStatus` member: the removal is accepted, the certificate is
/// still serving.
pub const CERTIFICATE_STATUS_PENDING_DISASSOCIATION: &str = "PENDING_DISASSOCIATION";
```

The helper that answers "is it serving" and says no, src/state/certificates.rs:83-88:
```
    /// Whether the certificate is serving, or on its way to serving: the two statuses
    /// a repeat of the association asks for rather than starting afresh.
    pub fn association_in_force(&self) -> bool {
        self.status == CERTIFICATE_STATUS_PENDING_ASSOCIATION
            || self.status == CERTIFICATE_STATUS_ASSOCIATED
    }
```

The handler that rewrites the record when the helper says no, src/ops/associate_certificate.rs:204-217 and 242-255:
```
        // This operation mints no id: the (gateway, certificate) pair IS the
        // association's identity, so a retry — with the caller's `clientToken` or
        // without it — addresses the record already there rather than making a
        // second one. ...
        let key = (input.gateway_id.clone(), input.acm_certificate_arn.clone());
        if let Some(existing) = store.certificate_associations.get(&key) {
            if existing.association_in_force() {
                return Ok(AssociateCertificateOutput { ... });
            }
        } else if store
```
```
        let association = crate::state::CertificateAssociation {
            gateway_id: input.gateway_id,
            acm_certificate_arn: input.acm_certificate_arn,
            status: crate::state::CERTIFICATE_STATUS_PENDING_ASSOCIATION.to_string(),
            settles_at: Some(now + crate::state::PROVISIONING_DELAY),
            associated_at: now,
            updated_at: now,
        };
        ...
        store.certificate_associations.insert(key, association);
```

The sibling path on the same surface that does NOT restart a transition already outstanding, src/ops/disassociate_certificate.rs:197-205:
```
        // Removal is asynchronous, and terminal once settled: a repeat of this
        // `@idempotent` call asks for the state the association is already in, so it
        // answers the record as it stands rather than starting the removal again.
        if association.association_in_force() {
```
and the same guard for the other asynchronously-deleted resource, src/state/routing_rules.rs:79-83:
```
    /// Whether this rule's deletion is already outstanding, so a repeat of the
    /// delete asks for the state the rule is in rather than for a new one.
    pub fn deleting(&self) -> bool {
        self.status == RULE_STATUS_DELETION_IN_PROGRESS
    }
```

Test coverage: tests/all/certificate_association.rs:365-384 pins the repeat only for an ASSOCIATED record ("expected the repeat to leave the record alone", asserting `associatedAt` is unchanged), and tests/all/certificate_association.rs:400-419 only associates again AFTER the removal has settled to DISASSOCIATED ("a certificate that was removed can be associated again"). No test associates while the removal is still PENDING_DISASSOCIATION, so nothing pins the rewrite.

## Steps to Reproduce

1. POST /responder-gateway with gatewayType EXTERNAL (plus the required vpcId/subnetIds/securityGroupIds/port/protocol/domainName/clientToken) and note the gatewayId.
2. POST /responder-gateway/{gatewayId}/certificate with a valid acmCertificateArn and a clientToken — answers 200, status PENDING_ASSOCIATION.
3. Poll GET /responder-gateway/{gatewayId}/certificate?acmCertificateArn=... until status is ASSOCIATED; note associatedAt.
4. DELETE /responder-gateway/{gatewayId}/certificate?acmCertificateArn=... — answers 200 with status PENDING_DISASSOCIATION (the removal is accepted; the crate's own comment says the certificate is still serving).
5. Within PROVISIONING_DELAY (500 ms), POST /responder-gateway/{gatewayId}/certificate again with the same acmCertificateArn.
6. Observed: 200 with status PENDING_ASSOCIATION — the stored record was overwritten, associatedAt moved to now, and the accepted removal is gone: a later GET reports ASSOCIATED instead of DISASSOCIATED. Expected (per the surface's own repeat rule and the crate's definition that a PENDING_DISASSOCIATION certificate is still serving) the call to answer the record that is already there rather than rewrite it mid-transition.
