Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudfront-keyvaluestore__s3.function-prefix-routing-follows-putkey-and-falls-back-on-missing-key cloudfront__cloudfront-keyvaluestore__s3.putkey-denied-by-role-policy-leaves-the-edge-serving-the-last-mapped-prefix
Issue-Key: cloudfront__cloudfront-keyvaluestore__s3.function-prefix-routing-follows-putkey-and-falls-back-on-missing-key|run#21|EgressRefused|IfMatch=kvs.describe_key_value_store(KvsARN=store_arn)["ETag"],
Issue-Key: cloudfront__cloudfront-keyvaluestore__s3.putkey-denied-by-role-policy-leaves-the-edge-serving-the-last-mapped-prefix|run#16|EgressRefused|IfMatch=kvs.describe_key_value_store(KvsARN=store_arn)["ETag"],
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# cloudfront__cloudfront-keyvaluestore__s3: the CloudFront KeyValueStore data plane is announced on an account-id-prefixed host the emulator does not serve, so no KVS-backed function check can run

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutKey

## Description
Cause 1 of 1: the cloudfront-keyvaluestore data-plane endpoint the emulator announces is an account-id-prefixed host (<account-id>.<emulator host>) for which the emulator serves no address, so every request to it is refused before it is made and both KVS checks reach no verdict.

AWS: Smithy (cloudfront-keyvaluestore) binds the KVS data plane (DescribeKeyValueStore, PutKey, GetKey, DeleteKey, ListKeys, UpdateKeys) to the key value store's own data-plane host, which is reachable from the caller; CreateKeyValueStore returns that address and PutKey then writes a key that a CloudFront Function reads at the edge. The address form itself is environment-specific and is not a requirement — what is a requirement is that the address the service announces is the address it answers on.

Emulator: the call is refused with 'CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' — the emulator announced a host built by prefixing the account id onto its own host, and then serves nothing there. Nothing of the KVS surface or the function/edge behaviour is exercised.

Why: a client cannot write or read a single key, so prefix routing driven by a KeyValueStore (and the authorization path that should refuse a PutKey denied by a role policy) is unreachable. This is the emulator's own announcement, not a caller mistake: it advertises an endpoint other than the one it is reached on.

Fix: the cloudfront-keyvaluestore endpoint resolution/announcement (the host returned for a key value store must be an address the emulator serves), and the listener that must answer the KVS data-plane operations on it. Sibling open issues record the same account-id-host-prefix defect for other services — 20261116T230421Z-s3-the-account-id-prefixed-s3control-host-is.md and 20261116T222947Z-s3-s3control-the-s3control-account-id-host-prefix.md — but neither carries these cloudfront-keyvaluestore checks.

Traces: cloudfront__cloudfront-keyvaluestore__s3.function-prefix-routing-follows-putkey-and-falls-back-on-missing-key, cloudfront__cloudfront-keyvaluestore__s3.putkey-denied-by-role-policy-leaves-the-edge-serving-the-last-mapped-prefix.

## Evidence
Expected: the KVS data-plane host returned for the key value store is reachable; PutKey stores a key, GetKey reads it back, and a CloudFront Function bound to the store routes a prefix accordingly (falling back when the key is absent).
Actual: 'Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' for both checks — the announced host (account id prefixed onto the emulator host) resolves to nothing the emulator serves. (The literal account id and loopback host are per-run values; the defect is announcing a host other than the served one.)

## Steps to Reproduce
1. CreateKeyValueStore and read the data-plane endpoint the emulator returns for it.
2. Call DescribeKeyValueStore/PutKey against that endpoint.
Observed: the endpoint is <account-id>.<emulator-host>, for which the emulator serves no address, so the request is refused with CapabilityNotOffered before any KVS operation is attempted.
