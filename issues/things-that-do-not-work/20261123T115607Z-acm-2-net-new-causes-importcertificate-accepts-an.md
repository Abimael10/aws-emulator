Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm.import-certificate-expired-rejected acm.import-certificate-chain-not-matching-leaf-rejected
Issue-Shape-Id: com.amazonaws.acm#CertificateManager
Issue-Key: acm.import-certificate-expired-rejected|_error_code#12|AssertionError|raise AssertionError(f"expected a rejection, but the call succeeded and returned {result!r}")
Issue-Key: acm.import-certificate-chain-not-matching-leaf-rejected|_error_code#28|AssertionError|raise AssertionError(f"expected a rejection, but the call succeeded and returned {result!r}")
Issue-Commit: 8ed8c28d7ced8b3283a18b17191a631681e35604

# acm: 2 net-new causes — ImportCertificate accepts an already-expired leaf certificate and an expired/non-certificate CertificateChain instead of refusing with ValidationException

> _Note: this issue was identified against the code base at commit `8ed8c28d7ced8b3283a18b17191a631681e35604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportCertificate

## Description
ImportCertificate performs no validation of the X.509 material it is handed: both the leaf certificate's validity window and the supplied CertificateChain are stored and acknowledged with HTTP 200 where real ACM refuses the call. Two distinct causes, both in the ImportCertificate request path; neither is covered by any open issue (the nearest, 20261022T235108Z-ec2-the-whole-ec2-client-vpn-and-enclave-certificate.md, is about EC2 reaching a 501 ACM ImportCertificate stub — here the operation answers 200, so that issue does not cover these).

Cause 1 of 2: an already-expired leaf certificate is ACCEPTED.
AWS: ImportCertificate requires that "The current time must be between the Not Before and Not After certificate fields" (Smithy: ACM ImportCertificate documentation; modeled error ValidationException, errorKind client, httpError 400). A leaf whose notBefore and notAfter both lie in the past is refused with HTTP 400, error code ValidationException and the message "The certificate is expired. You can Import a certificate only during its validity period." That whole message is fixed text for this error class — it carries no value from the request or from state. Nothing is persisted: the refused domain appears in no ListCertificates page, under no CertificateStatuses filter (including EXPIRED), for any keyTypes filter.
Emulator: the call succeeds. It returns HTTP 200 with a minted CertificateArn (shape arn:aws:acm:<region>:<account>:certificate/<uuid>) for a self-signed RSA-2048 leaf minted with notBefore = now-400d and notAfter = now-30d. No error is raised, so the certificate is also stored and becomes describable/listable.
Why: a client that relies on ACM to reject expired material treats a 200 as proof the certificate is usable, and silently publishes an ARN that AWS would never have created. Code that tests its own error handling for expiry (a renewal pipeline importing a stale bundle, a CI check that asserts the refusal) sees success against the emulator and a 400 ValidationException against AWS, so the expiry branch is never exercised and ships broken. The leaked certificate also pollutes ListCertificates/EXPIRED result sets that AWS would leave empty.
Fix: the ImportCertificate handler's request validation for the ACM service — decode the PEM in Certificate, parse the leaf's notBefore/notAfter and refuse with ValidationException before anything is written to certificate state (the current path writes first and validates never).
Traces: acm.import-certificate-expired-rejected (expected a rejection, but the call succeeded and returned {'CertificateArn': 'arn:aws:acm:us-west-2:000000000000:certificate/...', HTTPStatusCode 200}).

Cause 2 of 2: the CertificateChain is never validated — a chain member outside its validity window is ACCEPTED, and the chain's encoding/type is unchecked.
AWS: the same operation requires that "The certificate, private key, and certificate chain must be PEM-encoded" and that the material be inside its validity period (Smithy: ACM ImportCertificate documentation; ImportCertificateRequest.CertificateChain target CertificateChainBlob, @length min 1 max 2097152; modeled error ValidationException, httpError 400). All certificate-material defects share code ValidationException and differ only in the message, each of which is entirely fixed text for its class:
  - a chain containing a certificate whose validity window has passed -> "A certificate in the chain is expired. You can Import a certificate only during its validity period."
  - a chain that is not parseable PEM -> "The certificate chain is not PEM-encoded or is not valid."
  - a well-formed PEM of the wrong type (a private key) passed as the chain -> "Passed PEM is not a certificate chain."
AWS does NOT verify that the chain issues the leaf: a chain naming an unrelated CA is accepted, reaches ISSUED, is returned verbatim by GetCertificate, and Issuer stays parsed from the LEAF's DER — so the fix must reject only the three material defects above and must not add issuer-linkage checking.
Emulator: ImportCertificate with a valid leaf and CertificateChain = a self-signed CA minted with notBefore = now-400d / notAfter = now-10d succeeds with HTTP 200 and a minted CertificateArn instead of a 400 ValidationException. Because the first chain case already passes, the emulator's handling of the other two chain defects (unparseable base64 inside BEGIN/END CERTIFICATE, and a PKCS#8 private key as the chain) is unverified by this replay and must be fixed and re-checked together — they travel the same unvalidated code path.
Why: a caller that supplies a stale or wrong-type chain gets a working-looking certificate from the emulator and a 400 from AWS. The refusal is also an effect-absence guarantee: on AWS nothing is stored for the refused domain, while here the certificate is stored and listed, so tests asserting "no certificate exists for this domain after the refusal" pass against AWS and fail against the emulator (or, worse, pass here and the application never learns its chain is unusable).
Fix: the same ImportCertificate handler's validation step for ACM — parse each PEM block of CertificateChain, refuse non-PEM/undecodable blocks, refuse a PEM that is not a certificate, refuse any chain certificate outside its validity window, each with its own message above, and write nothing on refusal. Keep storing an accepted chain verbatim and keep deriving Issuer from the leaf.
Traces: acm.import-certificate-chain-not-matching-leaf-rejected (the expired-chain import returned HTTPStatusCode 200 with a CertificateArn where a ValidationException was expected).

## Evidence
Suite: acm 244/252 (97%), these 2 checks mismatched.

Cause 1 — check acm.import-certificate-expired-rejected:
  Expected: ClientError, HTTP 400, Code="ValidationException", Message="The certificate is expired. You can Import a certificate only during its validity period."; and the expired domain absent from every ListCertificates page (all keyTypes, and under CertificateStatuses=[EXPIRED]).
  Actual: AssertionError: expected a rejection, but the call succeeded and returned {'CertificateArn': 'arn:aws:acm:us-west-2:000000000000:certificate/ad3c0fcf-49b2-44e5-852b-cc29f8abe09b', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}} (the ARN/uuid is per-run and not part of the requirement).

Cause 2 — check acm.import-certificate-chain-not-matching-leaf-rejected:
  Expected for CertificateChain = an expired self-signed CA: HTTP 400, Code="ValidationException", Message="A certificate in the chain is expired. You can Import a certificate only during its validity period.", and no certificate stored for that domain. Expected for an unparseable-PEM chain: Message="The certificate chain is not PEM-encoded or is not valid." Expected for a private key as the chain: Message="Passed PEM is not a certificate chain." All three with Code="ValidationException".
  Actual: AssertionError: expected a rejection, but the call succeeded and returned {'CertificateArn': 'arn:aws:acm:us-west-2:000000000000:certificate/a99a106d-12ee-4027-a7e1-6fb533ea15e5', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}} — raised on the first (expired-chain) case, so the loop never reached the other two.

The same check confirms the parts the emulator already gets right and that must not regress: an unrelated CA as the chain IS accepted, reaches Status=ISSUED, GetCertificate returns it verbatim, and DescribeCertificate.Issuer still names the leaf's issuer.

## Steps to Reproduce
1. Point botocore at the emulator's acm endpoint.
2. Cause 1: mint a self-signed RSA-2048 certificate with notBefore = now-400d and notAfter = now-30d (so it is expired) plus its unencrypted PKCS#8 key, then call ImportCertificate(Certificate=<pem>, PrivateKey=<key pem>). Observe HTTP 200 + CertificateArn; AWS answers 400 ValidationException "The certificate is expired. You can Import a certificate only during its validity period." Then call ListCertificates with Includes={"keyTypes": [all 7 enum values]} and again with CertificateStatuses=["EXPIRED"] and observe the domain present (AWS: absent).
3. Cause 2: mint a valid CA (CA-A) and a valid leaf signed by it; mint a second self-signed CA with notBefore = now-400d / notAfter = now-10d (expired). Call ImportCertificate(Certificate=<leaf pem>, PrivateKey=<leaf key pem>, CertificateChain=<expired CA pem>). Observe HTTP 200; AWS answers 400 ValidationException "A certificate in the chain is expired. You can Import a certificate only during its validity period."
4. Repeat step 3 with CertificateChain = b"-----BEGIN CERTIFICATE-----\nnot-base64!!!\n-----END CERTIFICATE-----\n" (AWS: ValidationException "The certificate chain is not PEM-encoded or is not valid.") and with CertificateChain = a PKCS#8 private key PEM (AWS: ValidationException "Passed PEM is not a certificate chain.").
5. After each refusal, confirm nothing was stored for the refused domain via ListCertificates, and that an unrelated valid control certificate's Serial and GetCertificate chain are unchanged.
