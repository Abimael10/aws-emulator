Issue-Source: trace-sweep-divergence
Issue-Trace: transfer-certificate-blob-preserved
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: transfer-certificate-blob-preserved
Issue-Shape-Id: com.amazonaws.transfer#TransferService
Issue-Key: transfer|DescribeCertificate|2xx->2xx|$.Certificate.Status
Issue-Commit: 780566efa1a099e163927d46dd50b271247016ff

# Transfer DescribeCertificate reports an in-validity-window imported certificate as INACTIVE instead of ACTIVE

> _Note: this issue was identified against the code base at commit `780566efa1a099e163927d46dd50b271247016ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCertificate

## Description
Cause 1 of 1: the status AWS Transfer derives for an imported AS2/TLS certificate is not derived at all by the emulator — a certificate that is inside its validity window is projected as INACTIVE.

AWS: Smithy model services/service-com-amazonaws-transfer-transferservice — DescribeCertificate returns the required member Certificate: DescribedCertificate, whose member Status targets the enum CertificateStatusType with values ACTIVE, PENDING_ROTATION, INACTIVE. The modeled documentation for DescribedCertificate$Status states the rule exactly: "A certificate's status can be either ACTIVE or INACTIVE. You can set ActiveDate and InactiveDate in the UpdateCertificate call. If you set values for these parameters, those values are used to determine whether the certificate has a status of ACTIVE or INACTIVE. If you don't set values for ActiveDate and InactiveDate, we use the NotBefore and NotAfter date as specified on the X509 certificate to determine when a certificate is active and when it is inactive." The companion members confirm the defaulting: DescribedCertificate$ActiveDate — "If you do not specify a value, ActiveDate takes the same value as NotBeforeDate, which is specified by the CA"; DescribedCertificate$InactiveDate — "If you do not specify a value, InactiveDate takes the same value as NotAfterDate". So the status is a COMPUTED projection over the effective window: ACTIVE when the current time is at or after the effective active date and before the effective inactive date, INACTIVE when it is outside that window. ImportCertificate (input members ActiveDate/InactiveDate, both optional CertDate timestamps) is what seeds that window, falling back to the X509 NotBefore/NotAfter of the supplied Certificate body. The recorded response for this step is HTTP 200 with $.Certificate.Status = "ACTIVE" for a certificate imported and then read back inside its validity window.

Emulator: the same DescribeCertificate call answers HTTP 200 but with $.Certificate.Status = "INACTIVE" — the status is wrong while the rest of the DescribedCertificate projection matches. The replay reports exactly one differing key: "$.Certificate.Status: value_mismatch (expected 'ACTIVE', got 'INACTIVE')". Hypothesis: the emulator stores a fixed/default status at import time rather than computing it from the effective (ActiveDate|NotBeforeDate, InactiveDate|NotAfterDate) window — either it never parses NotBefore/NotAfter out of the imported PEM and leaves the window empty, or its window comparison is inverted so an unset bound reads as "not yet active / already expired".

Why: Status is the field a client uses to decide whether a certificate can be used. An AS2 partner-profile or TLS flow that imports a certificate, then polls DescribeCertificate until Status == ACTIVE before wiring it into a connector/agreement, never sees ACTIVE against the emulator and either loops forever or aborts with "certificate not usable", even though the identical sequence succeeds on AWS. Code that branches on INACTIVE will also take its expiry/rotation path for a perfectly valid certificate.

Fix: the certificate state and its read projection in the transfer service — the status must be derived on read from the stored effective active/inactive dates (ActiveDate/InactiveDate as supplied to ImportCertificate/UpdateCertificate, defaulted to the imported X509 certificate's NotBefore/NotAfter) compared against the current time, and that same derivation must feed DescribeCertificate and ListCertificates. Nothing here is specific to the recorded certificate blob, dates or ids: the requirement is the rule, not the recorded value.

Traces: transfer-certificate-blob-preserved#task#2 (DescribeCertificate, 200 -> 200, $.Certificate.Status).

Related but NOT covering this: the open issues 20261109T165919Z-transfer-sftp-connector-read-paths-diverge-describeconnector-omits.md and 20261109T183437Z-cloudformation-transfer-3-net-new-causes-describeserver-populates.md concern the connector/server read paths, not certificate status derivation.

## Evidence
Fixture transfer-certificate-blob-preserved, step task#2, operation DescribeCertificate.
Expected (recorded AWS): HTTP 200, $.Certificate.Status = "ACTIVE".
Actual (emulator): HTTP 200, $.Certificate.Status = "INACTIVE".
Replay detail: "$.Certificate.Status: value_mismatch (expected 'ACTIVE', got 'INACTIVE')"; divergence_key service=transfer|DescribeCertificate|status=200->200|keys=$.Certificate.Status; verdict mismatch; affected: transfer-certificate-blob-preserved#task#2.
Model evidence: Smithy CertificateStatusType = [ACTIVE, PENDING_ROTATION, INACTIVE]; DescribedCertificate$Status doc specifies ActiveDate/InactiveDate, defaulted to the X509 NotBefore/NotAfter, determine ACTIVE vs INACTIVE. Status is the ONLY diverging key, so the certificate body and the rest of the structure round-trip correctly.

## Steps to Reproduce
1. Replay wire fixture transfer-certificate-blob-preserved against the emulator (trace set simparity-wire-aws-v1).
2. The fixture imports a certificate via Transfer ImportCertificate without explicit ActiveDate/InactiveDate, so the effective window comes from the certificate's X509 NotBefore/NotAfter and the read happens inside that window.
3. At step task#2 call DescribeCertificate with the returned CertificateId (^cert-([0-9a-f]{17})$).
4. Observe HTTP 200 with $.Certificate.Status = "INACTIVE"; AWS returns "ACTIVE" for a certificate inside its validity window.
