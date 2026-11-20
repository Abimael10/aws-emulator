Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: payment-cryptography-data.e2e-card-issuance-transaction-settlement-pipeline
Issue-Shape-Id: com.amazonaws.paymentcryptographydata#PaymentCryptographyDataPlane
Issue-Key: payment-cryptography-data|EncryptData|NotImplemented
Issue-Commit: 8876c286e7810e099eb80215dccfcc7b46232ae7

# payment-cryptography-data: 2 causes — EncryptData answers 501 NotImplemented for Dukpt/Asymmetric/Emv encryption attributes, and ReEncryptData is unimplemented

> _Note: this issue was identified against the code base at commit `8876c286e7810e099eb80215dccfcc7b46232ae7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EncryptData

## Description
Two root causes in the payment-cryptography-data data plane block the DUKPT settlement leg of the card-issuance pipeline. Both surface on the same failing check, which aborts at the first one.

Cause 1 of 2: EncryptData refuses every non-Symmetric member of EncryptionDecryptionAttributes with HTTP 501 NotImplemented.
AWS: Smithy model — EncryptData.EncryptionAttributes targets the union EncryptionDecryptionAttributes with members Symmetric | Asymmetric | Dukpt | Emv, all four modeled and selectable. DukptEncryptionAttributes members: KeySerialNumber (HexLength16Or20Or24, required, @pattern ^(?:[0-9a-fA-F]{16}|[0-9a-fA-F]{20}|[0-9a-fA-F]{24})$), Mode (DukptEncryptionMode, default CBC), DukptKeyDerivationType, DukptKeyVariant, InitializationVector. EncryptData's own modeled documentation states "Encrypts plaintext data to ciphertext using a symmetric (TDES, AES), asymmetric (RSA), or derived (DUKPT or EMV) encryption key scheme" and "To encrypt using DUKPT, you must already have a BDK (Base Derivation Key) key in your account with KeyModesOfUse set to DeriveKey". A valid DUKPT request against a TR31_B0_BASE_DERIVATION_KEY with KeyModesOfUse.DeriveKey therefore succeeds with HTTP 200 and returns CipherText, KeyArn and KeyCheckValue. EncryptData models only AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429) and ValidationException(400) — 501/NotImplemented is not in its error set at all, so no well-formed request can legitimately receive it.
Emulator: a DUKPT request returns the non-modeled error code `NotImplemented` with the message "operation EncryptData with Asymmetric, Dukpt or Emv encryption attributes is scaffolded but not implemented in this emulator". The handler matches only EncryptionDecryptionAttributes::Symmetric and short-circuits every other variant to a scaffold 501; it also 501s whenever WrappedKey is present ("EncryptData with a WrappedKey"). The fixed text here is the emulator's own scaffold wording — AWS never emits it; there is no AWS-side counterpart message for this request, because AWS serves it.
Why: a client doing terminal-side DUKPT encryption (the normal point-of-sale path: derive the transaction key from BDK+KSN, encrypt the record) gets an unmodeled error code its SDK cannot map to any modeled exception, so retry/branching logic keyed on ValidationException/ResourceNotFoundException never fires and the whole DUKPT acquiring flow is unreachable. The EMV and asymmetric (RSA) schemes, and the TR-31 dynamic-key WrappedKey form, are unreachable for the same reason.
Fix: the EncryptData handler (services/service-com-amazonaws-paymentcryptographydata-paymentcryptographydataplane/src/ops/encrypt_data.rs) plus a DUKPT key-derivation component alongside the existing emv/pin modules in src/state/crypto.rs — derive the transaction key from the BDK material and KeySerialNumber per ANSI X9.24, honouring DukptKeyDerivationType/DukptKeyVariant and the Mode/InitializationVector, then run the existing symmetric mode machinery under the derived key. The same derivation must back DecryptData's Dukpt variant so an encrypt/decrypt round-trip under one KSN recovers the plaintext. Requests the model does allow must never leave this handler as 501; a genuinely bad request belongs in ValidationException (400).
Traces: payment-cryptography-data.e2e-card-issuance-transaction-settlement-pipeline (stage 5, encrypt_data with EncryptionAttributes={"Dukpt": {...}} against a TR31_B0_BASE_DERIVATION_KEY).

Cause 2 of 2: ReEncryptData is an unimplemented scaffold and answers 501 NotImplemented for every request.
AWS: Smithy model — ReEncryptData takes IncomingKeyIdentifier, OutgoingKeyIdentifier, CipherText, IncomingEncryptionAttributes and OutgoingEncryptionAttributes (ReEncryptionAttributes union: Symmetric | Asymmetric | Dukpt) and returns CipherText, KeyArn (the OUTGOING key's ARN) and KeyCheckValue with HTTP 200. It decrypts under the incoming key/attributes and re-encrypts under the outgoing key/attributes without exposing plaintext, so the answered CipherText differs from the input whenever the keys differ, and a DecryptData under the outgoing key and the outgoing attributes recovers the original plaintext. Its modeled error set does not include 501/NotImplemented.
Emulator: the handler body is the generated scaffold — it binds input/ctx/engine and returns `Scaffolded::not_implemented("ReEncryptData")`, i.e. error code `NotImplemented` with the message "operation ReEncryptData is scaffolded but not implemented in this emulator" (again the emulator's own scaffold wording, not any AWS message). No request form reaches any logic.
Why: network settlement cannot move a record from a terminal/acquirer key to a network key at all, and a client gets an error code outside the operation's modeled set. This cause is downstream of Cause 1 in the failing check, so it stays hidden until Cause 1 is fixed — fixing only Cause 1 will not make the check pass.
Fix: the ReEncryptData handler (services/service-com-amazonaws-paymentcryptographydata-paymentcryptographydataplane/src/ops/re_encrypt_data.rs) — resolve both keys through crate::state::resolve_key, check the incoming key's Decrypt and the outgoing key's Encrypt modes of use, decrypt under IncomingEncryptionAttributes (including the Dukpt variant from Cause 1's derivation), re-encrypt under OutgoingEncryptionAttributes, and answer KeyArn/KeyCheckValue of the OUTGOING key.
Traces: payment-cryptography-data.e2e-card-issuance-transaction-settlement-pipeline (stage 5, re_encrypt_data from the BDK Dukpt attributes to the network key's Symmetric attributes, asserting KeyArn == outgoing key and CipherText != input).

No open issue covers either cause; the open payment-cryptography issues concern the control plane (UpdateAlias stub, alias refusal status, GetResourcePolicy, CloudFormation registration) and the already-filed data-plane stub issue names TranslatePinData only.

## Evidence
Failing check payment-cryptography-data.e2e-card-issuance-transaction-settlement-pipeline, verdict [error].

Cause 1 — EncryptData with DUKPT attributes:
  Request: EncryptData{KeyIdentifier=<BDK key ARN, KeyUsage TR31_B0_BASE_DERIVATION_KEY, KeyModesOfUse.DeriveKey=true>, PlainText=<hex, multiple of 8 bytes for TDES>, EncryptionAttributes={"Dukpt": {"KeySerialNumber": <20 hex chars>, "Mode": "ECB"}}}
  Expected (AWS): HTTP 200 with {CipherText: <hex ciphertext of the plaintext under the key derived from the BDK and the KSN>, KeyArn: <the BDK's canonical key ARN>, KeyCheckValue: <that key's KCV>}.
  Actual (emulator): botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the EncryptData operation: operation EncryptData with Asymmetric, Dukpt or Emv encryption attributes is scaffolded but not implemented in this emulator
  Error code `NotImplemented` is not in EncryptData's modeled error set (AccessDeniedException, InternalServerException, ResourceNotFoundException, ThrottlingException, ValidationException). The whole message is emulator-authored scaffold text with no AWS counterpart; no part of it is an AWS template.
  Source: src/ops/encrypt_data.rs lines 227-232 — `let EncryptionDecryptionAttributes::Symmetric(attributes) = &input.encryption_attributes else { return Err(Scaffolded::not_implemented(...)) }`; lines 222-226 do the same for any WrappedKey. src/state/crypto.rs declares only `pub mod emv;` and `pub mod pin;` (line 44-45) — there is no DUKPT derivation component.

Cause 2 — ReEncryptData:
  Request: ReEncryptData{IncomingKeyIdentifier=<BDK ARN>, OutgoingKeyIdentifier=<network TR31_D0 key ARN>, CipherText=<DUKPT ciphertext>, IncomingEncryptionAttributes={"Dukpt": {...}}, OutgoingEncryptionAttributes={"Symmetric": {"Mode": "ECB"}}}
  Expected (AWS): HTTP 200 with KeyArn == the OUTGOING key's ARN and CipherText != the incoming ciphertext, such that DecryptData under the outgoing key and Symmetric attributes recovers the original plaintext.
  Actual (emulator): error code `NotImplemented`, message "operation ReEncryptData is scaffolded but not implemented in this emulator" — again outside the operation's modeled error set.
  Source: src/ops/re_encrypt_data.rs lines 200-212 — handler binds `let _ = (&input, ctx, &self.engine);` then `Err(Scaffolded::not_implemented("ReEncryptData"))`.

The check's earlier stages (GenerateCardValidationData/VerifyCardValidationData, GeneratePinData/VerifyPinData, GenerateMac/VerifyMac) all succeed, so the failure is isolated to the DUKPT encrypt and re-encrypt legs.

## Steps to Reproduce
1. Control plane: CreateKey with KeyAttributes{KeyAlgorithm=TDES_2KEY, KeyClass=SYMMETRIC_KEY, KeyUsage=TR31_B0_BASE_DERIVATION_KEY, KeyModesOfUse={DeriveKey:true}} -> note its KeyArn (the BDK). CreateKey again with KeyUsage=TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY and KeyModesOfUse={Encrypt,Decrypt,Wrap,Unwrap} -> the network key.
2. Cause 1: call EncryptData{KeyIdentifier=<BDK ARN>, PlainText="AABBCCDD11223344", EncryptionAttributes={"Dukpt": {"KeySerialNumber": "FFFF9876543210E00001", "Mode": "ECB"}}}. Observe HTTP 501 with error code NotImplemented instead of a 200 carrying CipherText/KeyArn/KeyCheckValue. Repeating with EncryptionAttributes={"Emv": {...}} or {"Asymmetric": {...}}, or with any WrappedKey present, reproduces the same refusal.
3. Cause 2: call ReEncryptData{IncomingKeyIdentifier=<BDK ARN>, OutgoingKeyIdentifier=<network key ARN>, CipherText=<any hex ciphertext>, IncomingEncryptionAttributes={"Dukpt": {"KeySerialNumber": "FFFF9876543210E00001", "Mode": "ECB"}}, OutgoingEncryptionAttributes={"Symmetric": {"Mode": "ECB"}}}. Observe error code NotImplemented for every request form, including an all-Symmetric one.
4. Re-replay payment-cryptography-data.e2e-card-issuance-transaction-settlement-pipeline to confirm both legs: the DUKPT EncryptData returns ciphertext, ReEncryptData reports the outgoing KeyArn with a changed CipherText, and DecryptData under the network key recovers "AABBCCDD11223344".
