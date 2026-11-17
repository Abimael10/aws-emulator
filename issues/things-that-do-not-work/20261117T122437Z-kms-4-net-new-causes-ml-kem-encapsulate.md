Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms.ml-kem-encapsulate-decapsulate-round-trip kms.ml-kem-encapsulate-decapsulate-recovers-the-same-shared-secret kms.ml-kem-disabled-key-blocks-encapsulate-and-decapsulate kms.decapsulate-ciphertext-wrong-length-rejected kms.decapsulate-wrong-length-ciphertext-rejected kms.decapsulate-cross-key-ciphertext-implicit-rejection kms.encapsulate-decapsulate-invalid-key-usage-rejected kms.encapsulate-decapsulate-dry-run-rejected kms.create-key-edwards25519-encrypt-decrypt-leaves-no-key kms.get-public-key-symmetric-key-rejected kms.list-grants-and-list-resource-tags-limit-over-max-rejected
Issue-Shape-Id: com.amazonaws.kms#TrentService
Issue-Key: kms|CreateKey|ValidationException
Issue-Key: kms.get-public-key-symmetric-key-rejected|run#6|AssertionError|assert message == "", f"unexpected message {message!r}"
Issue-Key: kms|ListGrants|ValidationException
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms: 4 net-new causes — ML-KEM/ENCAPSULATE_DECAPSULATE keys and Encapsulate/Decapsulate unsupported, CreateKey and GetPublicKey refusal messages mis-worded, ListGrants/ListResourceTags Limit capped at 100 instead of 1000

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateKey

## Description
Four distinct root causes in the KMS emulator, all grounded in failing behavioral checks.

Cause 1 of 4: CreateKey refuses post-quantum ML-KEM keys, so KMS serves no key-encapsulation surface at all.
  AWS: recorded behavior only — the on-disk Smithy model (services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json) predates ML-KEM: KeySpec enumerates only [RSA_2048, RSA_3072, RSA_4096, ECC_NIST_P256, ECC_NIST_P384, ECC_NIST_P521, ECC_SECG_P256K1, SYMMETRIC_DEFAULT, HMAC_224..512, SM2, ML_DSA_44/65/87, ECC_NIST_EDWARDS25519] and KeyUsageType only [SIGN_VERIFY, ENCRYPT_DECRYPT, GENERATE_VERIFY_MAC, KEY_AGREEMENT], and it binds no Encapsulate/Decapsulate operation. The recordings show real AWS accepting CreateKey with KeyUsage=ENCAPSULATE_DECAPSULATE and KeySpec=ML_KEM_512/ML_KEM_768, then: GetPublicKey returning that key's DER public key; Encapsulate returning a SharedSecret, its Ciphertext and EncapsulationAlgorithm=ML_KEM; Decapsulate on the matching ciphertext recovering the identical shared secret; Decapsulate of a correctly sized ciphertext made under a different ML-KEM key returning 200 with a non-matching 32-byte secret (ML-KEM implicit rejection); a wrong-length ciphertext refused InvalidCiphertextException; a non-ML-KEM key refused InvalidKeyUsageException; a DISABLED key refused DisabledException; and DryRun=true refused DryRunOperationException with message "The request would have succeeded, but the DryRun option is set."
  Emulator: CreateKey answers 400 ValidationException "2 validation errors detected: Value 'ENCAPSULATE_DECAPSULATE' at 'keyUsage' failed to satisfy constraint: Member must satisfy enum value set: [SIGN_VERIFY, ENCRYPT_DECRYPT, GENERATE_VERIFY_MAC, KEY_AGREEMENT]; Value 'ML_KEM_768' at 'keySpec' failed to satisfy constraint: Member must satisfy enum value set: [...]" (same for ML_KEM_512), so every check fails at setup and Encapsulate/Decapsulate are never reachable.
  Why: a client doing post-quantum key establishment through KMS cannot create a key, cannot encapsulate and cannot decapsulate; its KMS-dependent handshake fails outright against the emulator.
  Fix: the CreateKey handler and its key-spec/key-usage validation plus the key-spec catalogue in KMS state, and the Encapsulate/Decapsulate operations (handler + the model surface they are served from).
  Traces: kms.ml-kem-encapsulate-decapsulate-round-trip, kms.ml-kem-encapsulate-decapsulate-recovers-the-same-shared-secret, kms.ml-kem-disabled-key-blocks-encapsulate-and-decapsulate, kms.decapsulate-ciphertext-wrong-length-rejected, kms.decapsulate-wrong-length-ciphertext-rejected, kms.decapsulate-cross-key-ciphertext-implicit-rejection, kms.encapsulate-decapsulate-invalid-key-usage-rejected, kms.encapsulate-decapsulate-dry-run-rejected.

Cause 2 of 4: CreateKey's KeyUsage/KeySpec incompatibility message carries emulator-authored trailing text.
  AWS: recorded behavior only (the model states the enums, not the message). AWS refuses CreateKey(KeyUsage=ENCRYPT_DECRYPT, KeySpec=ECC_NIST_EDWARDS25519) with 400 ValidationException and the message "KeyUsage <keyUsage> is not compatible with KeySpec <keySpec>" — fixed text "KeyUsage ", " is not compatible with KeySpec ", with the two values taken verbatim from the request, and nothing after the key spec.
  Emulator: same code, message "KeyUsage ENCRYPT_DECRYPT is not compatible with KeySpec ECC_NIST_EDWARDS25519, which supports SIGN_VERIFY." — it appends ", which supports <usage>." that AWS never sends.
  Why: a client (or test) matching KMS's refusal text exactly, or logging/forwarding it verbatim, sees a message AWS never produced.
  Fix: the CreateKey handler's key-usage/key-spec compatibility validation message.
  Traces: kms.create-key-edwards25519-encrypt-decrypt-leaves-no-key.

Cause 3 of 4: GetPublicKey's UnsupportedOperationException on a symmetric key carries a message AWS leaves empty.
  AWS: Smithy: GetPublicKey declares UnsupportedOperationException (client error, httpError 400, awsQueryError code UnsupportedOperation). Recorded behavior: calling GetPublicKey on a SYMMETRIC_DEFAULT/ENCRYPT_DECRYPT key is refused 400 UnsupportedOperationException with an EMPTY Error.Message.
  Emulator: refuses with the right code but fills the message with "<keyArn> has KeySpec SYMMETRIC_DEFAULT, which is not an asymmetric KMS key; only an asymmetric KMS key has a public key to download." (the ARN comes from the request, the rest is emulator-authored prose).
  Why: a client that branches or asserts on the refusal text, or surfaces it to an operator, reads wording AWS never sends for this error class.
  Fix: the GetPublicKey handler's unsupported-key-spec refusal (error construction).
  Traces: kms.get-public-key-symmetric-key-rejected.

Cause 4 of 4: the Limit bound on ListGrants and ListResourceTags is enforced at 100 instead of 1000, and the validation message omits the error-count prefix.
  AWS: Smithy: ListGrants.Limit and ListResourceTags.Limit both target LimitType, an integer with @range min 1 max 1000. So Limit=1000 is VALID and must be served; Limit=1001 is refused 400 ValidationException with "1 validation error detected: Value '<value>' at 'limit' failed to satisfy constraint: Member must have value less than or equal to 1000" — fixed text with the rejected value from the request and the modeled bound 1000.
  Emulator: ListGrants(Limit=1000) is REFUSED 400 ValidationException "Value '1000' at 'limit' failed to satisfy constraint: Member must have value less than or equal to 100" — the wrong bound (100, not the modeled 1000) and no "N validation error(s) detected: " prefix.
  Why: a client paginating grants or resource tags at the documented maximum page size gets a hard 400 instead of a page, and a client parsing the refusal sees a limit AWS does not impose.
  Fix: the ListGrants and ListResourceTags handlers' Limit range validation (and the shared validation-message formatter that must emit the counted prefix).
  Traces: kms.list-grants-and-list-resource-tags-limit-over-max-rejected.

## Evidence
kms.ml-kem-encapsulate-decapsulate-round-trip / -recovers-the-same-shared-secret / ml-kem-disabled-key-blocks-encapsulate-and-decapsulate / decapsulate-ciphertext-wrong-length-rejected / decapsulate-wrong-length-ciphertext-rejected / decapsulate-cross-key-ciphertext-implicit-rejection / encapsulate-decapsulate-invalid-key-usage-rejected / encapsulate-decapsulate-dry-run-rejected — all error out at setup: botocore.exceptions.ClientError: An error occurred (ValidationException) when calling the CreateKey operation: 2 validation errors detected: Value 'ENCAPSULATE_DECAPSULATE' at 'keyUsage' failed to satisfy constraint: Member must satisfy enum value set: [SIGN_VERIFY, ENCRYPT_DECRYPT, GENERATE_VERIFY_MAC, KEY_AGREEMENT]; Value 'ML_KEM_768' at 'keySpec' failed to satisfy constraint: Member must satisfy enum value set: [RSA_2048, ..., ECC_NIST_EDWARDS25519]. Expected: the key is created and Encapsulate/Decapsulate round-trip a 32-byte shared secret with EncapsulationAlgorithm=ML_KEM.

kms.create-key-edwards25519-encrypt-decrypt-leaves-no-key — AssertionError: expected ('ValidationException', 'KeyUsage ENCRYPT_DECRYPT is not compatible with KeySpec ECC_NIST_EDWARDS25519'), got ('ValidationException', 'KeyUsage ENCRYPT_DECRYPT is not compatible with KeySpec ECC_NIST_EDWARDS25519, which supports SIGN_VERIFY.')

kms.get-public-key-symmetric-key-rejected — code UnsupportedOperationException matches; AssertionError: unexpected message 'arn:aws:kms:...:key/<id> has KeySpec SYMMETRIC_DEFAULT, which is not an asymmetric KMS key; only an asymmetric KMS key has a public key to download.' Expected message ''.

kms.list-grants-and-list-resource-tags-limit-over-max-rejected — botocore ClientError on the VALID call: An error occurred (ValidationException) when calling the ListGrants operation: Value '1000' at 'limit' failed to satisfy constraint: Member must have value less than or equal to 100. Expected: Limit=1000 accepted; Limit=1001 refused with "1 validation error detected: Value '1001' at 'limit' failed to satisfy constraint: Member must have value less than or equal to 1000".

## Steps to Reproduce
1. Cause 1: kms.create_key(KeyUsage='ENCAPSULATE_DECAPSULATE', KeySpec='ML_KEM_768') -> expect 200 with KeyMetadata; then Encapsulate(KeyId=...) and Decapsulate(KeyId=..., Ciphertext=...) round-trip the same SharedSecret (EncapsulationAlgorithm='ML_KEM'); Decapsulate with a wrong-length ciphertext -> InvalidCiphertextException; on a disabled key -> DisabledException; with DryRun=true -> DryRunOperationException.
2. Cause 2: kms.create_key(KeyUsage='ENCRYPT_DECRYPT', KeySpec='ECC_NIST_EDWARDS25519') -> 400 ValidationException whose message ends at the key spec.
3. Cause 3: kms.create_key(KeySpec='SYMMETRIC_DEFAULT', KeyUsage='ENCRYPT_DECRYPT'), then kms.get_public_key(KeyId=...) -> 400 UnsupportedOperationException with an empty Error.Message.
4. Cause 4: kms.list_grants(KeyId=<key>, Limit=1000) -> 200; kms.list_grants(KeyId=<key>, Limit=1001) and kms.list_resource_tags(KeyId=<key>, Limit=1001) -> 400 ValidationException "1 validation error detected: Value '1001' at 'limit' failed to satisfy constraint: Member must have value less than or equal to 1000".
