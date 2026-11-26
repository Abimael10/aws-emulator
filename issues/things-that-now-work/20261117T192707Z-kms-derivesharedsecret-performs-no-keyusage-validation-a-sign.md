Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms.derive-shared-secret-wrong-key-usage-rejected
Issue-Shape-Id: com.amazonaws.kms#TrentService
Issue-Key: kms.derive-shared-secret-wrong-key-usage-rejected|run#9|AssertionError|assert code == "InvalidKeyUsageException", f"expected InvalidKeyUsageException, got {code}"
Issue-Commit: b6e7f4e91c16e0cb38b55fdf0bcb8a9fca13cb8c

# kms DeriveSharedSecret performs no KeyUsage validation: a SIGN_VERIFY key answers 501 NotImplemented instead of 400 InvalidKeyUsageException (and the KEY_AGREEMENT positive control never returns a shared secret)

> _Note: this issue was identified against the code base at commit `b6e7f4e91c16e0cb38b55fdf0bcb8a9fca13cb8c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeriveSharedSecret

## Description
Cause 1 of 1: DeriveSharedSecret never reaches key-usage validation, so neither its refusal path nor its success path exists. Failing check: kms.derive-shared-secret-wrong-key-usage-rejected.

AWS: DeriveSharedSecret is modeled with a required KeyId, a required KeyAgreementAlgorithm (enum KeyAgreementAlgorithmSpec) and a required PublicKey blob, and it declares InvalidKeyUsageException (Smithy: DeriveSharedSecret errors include InvalidKeyUsageException, errorKind client, httpError 400, awsQueryError code InvalidKeyUsage). AWS validates the referenced CMK's KeyUsage before doing any agreement: a key created with KeyUsage=SIGN_VERIFY (KeySpec ECC_NIST_P256) is refused with HTTP 400, error code InvalidKeyUsageException, and the message template `<key-arn> key usage is <KeyUsage> which is not valid for DeriveSharedSecret.` The FIXED text of that message is ` key usage is `, ` which is not valid for DeriveSharedSecret.`; its VARIABLE parts are the resolved key ARN (from the KeyId in the request) and the key's KeyUsage value taken from the emulator's state — never a literal ARN or key id from one run. The same call against a KEY_AGREEMENT key of the same curve, with the identical KeyAgreementAlgorithm and peer PublicKey, must SUCCEED and return KeyAgreementAlgorithm="ECDH" plus a SharedSecret blob (Smithy: output members KeyId, SharedSecret (sensitive blob), KeyAgreementAlgorithm, KeyOrigin, CiphertextForRecipient); for ECC_NIST_P256 the raw ECDH shared secret is 32 bytes (recorded behavior).

Emulator: every DeriveSharedSecret call is refused by the scaffold with HTTP 501, error code `NotImplemented`, message `operation DeriveSharedSecret is scaffolded but not implemented in this emulator`. The check therefore sees code NotImplemented where AWS answers InvalidKeyUsageException (replay: `AssertionError: expected InvalidKeyUsageException, got NotImplemented`), and the KEY_AGREEMENT positive control in the same check can never be reached, so no 32-byte SharedSecret / KeyAgreementAlgorithm=ECDH response is ever produced.

Why: a client that distinguishes a misconfigured key from a transport/feature failure gets the wrong answer. Botocore surfaces NotImplemented (501, a retryable/server-shaped failure) instead of the modeled client error InvalidKeyUsageException (400), so SDK error handling that catches InvalidKeyUsageException to report "this key cannot do ECDH" never fires, retry logic may retry a permanently invalid request, and ECDH key-agreement flows (deriving a shared secret against a peer public key) cannot run at all against the emulator.

Why this is not covered by the open stub issue: 20261027T041855Z-kms-derivesharedsecret-stub.md asks for the operation to be implemented, but its Issue-Trace-Ids carry only kms.derive-shared-secret-is-symmetric and kms.kms-derived-shared-secret-matches-a-locally-computed-ecdh-agreement — it does not carry this check's replay key, and it states no requirement about the KeyUsage refusal (status, code or message). This issue adds that requirement and that replay key.

Fix: the DeriveSharedSecret handler and its input validation in the kms service (services/service-com-amazonaws-kms-trentservice/src/ops/derive_shared_secret.rs): resolve KeyId against key state, reject a key whose KeyUsage is not KEY_AGREEMENT with InvalidKeyUsageException (400) using AWS's message template above, and otherwise perform the ECDH agreement and serialize KeyId (key ARN), KeyAgreementAlgorithm, KeyOrigin and SharedSecret.

Traces: kms.derive-shared-secret-wrong-key-usage-rejected (mismatch).

## Evidence
Check kms.derive-shared-secret-wrong-key-usage-rejected (verdict: mismatch).

Expected (AWS): DeriveSharedSecret(KeyId=<SIGN_VERIFY ECC_NIST_P256 key>, KeyAgreementAlgorithm="ECDH", PublicKey=<DER public key of a KEY_AGREEMENT ECC_NIST_P256 key>) -> HTTP 400, Error.Code="InvalidKeyUsageException", Error.Message="<arn of the SIGN_VERIFY key> key usage is SIGN_VERIFY which is not valid for DeriveSharedSecret." (fixed text " key usage is " / " which is not valid for DeriveSharedSecret."; ARN and the usage word come from the request's key and the stored key's KeyUsage). Then the same algorithm and peer PublicKey against a KEY_AGREEMENT ECC_NIST_P256 key -> HTTP 200 with KeyAgreementAlgorithm="ECDH" and a 32-byte SharedSecret.

Actual (emulator): HTTP 501, Error.Code="NotImplemented", Error.Message="operation DeriveSharedSecret is scaffolded but not implemented in this emulator".

Replay output:
  File "check_kms_derive_shared_secret_wrong_key_usage_rejected.py", line 51, in run
    assert code == "InvalidKeyUsageException", f"expected InvalidKeyUsageException, got {code}"
  AssertionError: expected InvalidKeyUsageException, got NotImplemented

Smithy (services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json): DeriveSharedSecret input KeyId/KeyAgreementAlgorithm/PublicKey all @required; errors include InvalidKeyUsageException (client, httpError 400); output members KeyId, SharedSecret (sensitive blob), KeyAgreementAlgorithm, KeyOrigin.

## Steps to Reproduce
1. Boot the kms emulator.
2. kms:CreateKey with KeyUsage=SIGN_VERIFY, KeySpec=ECC_NIST_P256 -> note its KeyId and Arn.
3. kms:CreateKey with KeyUsage=KEY_AGREEMENT, KeySpec=ECC_NIST_P256; kms:GetPublicKey on it -> peer PublicKey.
4. kms:DeriveSharedSecret(KeyId=<SIGN_VERIFY key id>, KeyAgreementAlgorithm="ECDH", PublicKey=<peer PublicKey>). Observe HTTP 501 / NotImplemented instead of HTTP 400 InvalidKeyUsageException with message "<sign-key-arn> key usage is SIGN_VERIFY which is not valid for DeriveSharedSecret.".
5. kms:DeriveSharedSecret(KeyId=<a KEY_AGREEMENT ECC_NIST_P256 key>, KeyAgreementAlgorithm="ECDH", PublicKey=<peer PublicKey>). Observe the same 501 instead of a 200 carrying KeyAgreementAlgorithm="ECDH" and a 32-byte SharedSecret.
6. Re-replay kms.derive-shared-secret-wrong-key-usage-rejected to confirm.
