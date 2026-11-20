Issue-Source: trace-sweep-divergence
Issue-Trace: iam-credential-not-found-errors
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-credential-not-found-errors iam-ssh-public-key-guard-clauses iam-ssh-public-key-lifecycle iam-emr-roles-policy-path-and-rerun-mistakes
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|DeleteSSHPublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: iam|UpdateSSHPublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: iam|AddRoleToInstanceProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 602156962253e64136e9b0eeef8fba5e51605e54

# IAM NoSuchEntity messages for SSH public keys and instance-profile role lookups use emulator wording instead of AWS's

> _Note: this issue was identified against the code base at commit `602156962253e64136e9b0eeef8fba5e51605e54`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteSSHPublicKey

## Description
Three IAM NoSuchEntity refusals return the right HTTP status and error code but a message the emulator authored rather than the sentence AWS words for that error class. The status (404) and the awsQuery error code (NoSuchEntity) match in every case; only ErrorResponse/Error/Message diverges. All three share one root cause: the not-found sentence is composed in the handler's own words instead of from the service's message template for that entity kind.

Cause 1 of 3: DeleteSSHPublicKey on an SSHPublicKeyId that does not exist. AWS: Smithy IAM (awsidentitymanagementv20100508) models DeleteSSHPublicKey with the single error NoSuchEntityException, errorKind client, httpError 404, awsQueryError code NoSuchEntity. The message AWS words for a missing SSH public key is the fixed text "The Public Key with id " + <the SSHPublicKeyId from the request> + " cannot be found." — the only variable part is the key id echoed from SSHPublicKeyId (publicKeyIdType, pattern ^[\w]+$). Emulator: answers 404/NoSuchEntity with "The SSH Public Key with id <id> cannot be found." — the fixed text carries an extra "SSH " before "Public Key". Why: a client (or an assertion harness) that matches IAM's documented not-found sentence for SSH keys never matches, so a correctly-handled missing-key case is reported as an unknown failure. Fix: the DeleteSSHPublicKey handler's not-found path (the NoSuchEntity message template for the SSH-public-key entity kind). Traces: iam-credential-not-found-errors#task#3, and the cluster's other member iam-ssh-public-key-guard-clauses#task#4.

Cause 2 of 3: UpdateSSHPublicKey on an SSHPublicKeyId that does not exist. AWS: Smithy models UpdateSSHPublicKey errors InvalidInputException (400/InvalidInput) and NoSuchEntityException (404/NoSuchEntity); the missing-key sentence is the same template as Cause 1 — fixed text "The Public Key with id " ... " cannot be found.", variable part the request's SSHPublicKeyId. Emulator: 404/NoSuchEntity with "The SSH Public Key with id <id> cannot be found." Why: same as Cause 1 — the status-update path for a deleted/unknown key is indistinguishable from an unexpected error to a client keyed on AWS's wording. Fix: the UpdateSSHPublicKey handler's not-found path, which should share one SSH-public-key NoSuchEntity template with DeleteSSHPublicKey. Traces: iam-ssh-public-key-lifecycle#task#10.

Cause 3 of 3: AddRoleToInstanceProfile naming a RoleName that does not exist. AWS: Smithy models AddRoleToInstanceProfile with NoSuchEntityException (404/NoSuchEntity) among EntityAlreadyExists/LimitExceeded/UnmodifiableEntity/ServiceFailure. The sentence AWS words for a missing role is the fixed text "The role with name " + <the RoleName from the request> + " cannot be found." — only the role name varies (roleNameType, pattern ^[\w+=,.@-]+$). Emulator: 404/NoSuchEntity with "Role <name> cannot be found." — a different fixed text ("Role " ... " cannot be found."), dropping "The role with name". Why: the role-not-found case on instance-profile association is a common bootstrap error for EMR/EC2 role setup; a client that distinguishes a missing role from a missing instance profile by message text cannot. Fix: the AddRoleToInstanceProfile handler's role-resolution path (the NoSuchEntity message template for the role entity kind). Traces: iam-emr-roles-policy-path-and-rerun-mistakes#task#11.

Related but not covering: the open issue 20261119T085411Z-iam-every-accessdenied-refusal-message-stops-at-action.md concerns AccessDenied trailers, not NoSuchEntity wording, and does not carry these replay keys.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator (233 fixtures, 1799 steps, 74 mismatched).

Cause 1 — fixture iam-credential-not-found-errors step task#3, DeleteSSHPublicKey, expected 404 / actual 404:
  $.ErrorResponse.Error.Message: value_mismatch (expected 'The Public Key with id APKAABCDEFGHIJKLMNOP cannot be found.', got 'The SSH Public Key with id APKAABCDEFGHIJKLMNOP cannot be found.')
  Fixed text expected: "The Public Key with id " ... " cannot be found."; variable part: the SSHPublicKeyId sent in the request (APKAABCDEFGHIJKLMNOP in this recording — a request value, not part of the template). Same cluster: iam-ssh-public-key-guard-clauses#task#4.

Cause 2 — fixture iam-ssh-public-key-lifecycle step task#10, UpdateSSHPublicKey, expected 404 / actual 404:
  $.ErrorResponse.Error.Message: value_mismatch (expected 'The Public Key with id APKABGXEQES5NRCCJJWAN cannot be found.', got 'The SSH Public Key with id APKABGXEQES5NRCCJJWAN cannot be found.')
  Same template; the id is the request's SSHPublicKeyId.

Cause 3 — fixture iam-emr-roles-policy-path-and-rerun-mistakes step task#11, AddRoleToInstanceProfile, expected 404 / actual 404:
  $.ErrorResponse.Error.Message: value_mismatch (expected 'The role with name sim2real-probe-emrmm-ghost-267d012e cannot be found.', got 'Role sim2real-probe-emrmm-ghost-267d012e cannot be found.')
  Fixed text expected: "The role with name " ... " cannot be found."; variable part: the RoleName sent in the request.

In all three the HTTP status and the NoSuchEntity error code agree with the recording; only the message text diverges.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (replay_wire_corpus) and look at the iam clusters.
3. Cause 1: replay fixture iam-credential-not-found-errors step task#3 — call DeleteSSHPublicKey with a UserName that exists and an SSHPublicKeyId that does not; compare ErrorResponse/Error/Message against "The Public Key with id <SSHPublicKeyId> cannot be found."
4. Cause 2: replay fixture iam-ssh-public-key-lifecycle step task#10 — call UpdateSSHPublicKey with Status=Active for an SSHPublicKeyId already deleted; compare the same template.
5. Cause 3: replay fixture iam-emr-roles-policy-path-and-rerun-mistakes step task#11 — call AddRoleToInstanceProfile with an existing InstanceProfileName and a RoleName that does not exist; compare the message against "The role with name <RoleName> cannot be found."
6. Each returns 404 with code NoSuchEntity but the emulator's own sentence.
