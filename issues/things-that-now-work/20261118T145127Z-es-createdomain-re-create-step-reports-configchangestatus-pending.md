Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-domain-lifecycle-and-config-inspection
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-domain-lifecycle-and-config-inspection
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|CreateDomain|2xx->2xx|$.DomainStatus.ChangeProgressDetails.ConfigChangeStatus
Issue-Related: 20261114T061447Z-es-domain-configuration-projection-diverges-modifyingproperties-entries-pendingvalue.md
Issue-Commit: c1209de2c0ccb8d10bbfdb930ff746048be9a58f

# es CreateDomain re-create step reports ConfigChangeStatus Pending and a wrong ModifyingProperties projection

> _Note: this issue was identified against the code base at commit `c1209de2c0ccb8d10bbfdb930ff746048be9a58f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
ONE net-new cause, on the CreateDomain step that re-creates a domain late in the opensearch-domain-lifecycle-and-config-inspection fixture (task#14). The emulator answers 200 as AWS does, but the DomainStatus body diverges in three linked ways: the freshly-initiated configuration change is reported with ConfigChangeStatus 'Pending' instead of 'Initializing'; the ModifyingProperties list is built from the emulator's internal option struct instead of AWS's property enumeration, so entries land at the wrong indices, AWS entries are absent and emulator-only entries are extra, and the per-entry PendingValue/ValueType encoding is wrong; and the submitted access policy's principal has been rewritten before being echoed.

Cause 1 of 1: CreateDomain's response for a newly created domain misreports the change status and mis-projects DomainStatus.ModifyingProperties (including rewriting the access-policy principal).
AWS: Smithy (com.amazonaws.opensearch#AmazonOpenSearchService) ChangeProgressDetails.ConfigChangeStatus -> ConfigChangeStatus enum = [Pending, Initializing, Validating, ValidationFailed, ApplyingChanges, Completed, PendingUserInput, Cancelled], documented 'The current status of the configuration change'; ModifyingProperties{Name, ActiveValue, PendingValue: String, ValueType: PropertyValueType}, PropertyValueType enum = [PLAIN_TEXT, STRINGIFIED_JSON] where PLAIN_TEXT 'Contain direct values such as "1", "True", or "c5.large.search"' and STRINGIFIED_JSON 'Contain content in JSON format'. Recorded behavior: on the CreateDomain response AWS reports ChangeProgressDetails.ConfigChangeStatus 'Initializing' for the change the create just initiated; the ModifyingProperties list is positionally stable with one entry per AWS-enumerated domain property, the AccessPolicies entry's PendingValue is the policy document EXACTLY as submitted (a principal supplied as the bare account id stays the bare account id), each AdvancedSecurityOptions sub-property carries the literal string 'false' when fine-grained access control is off, the TAGS entry is ValueType PLAIN_TEXT with an empty PendingValue when no tags were supplied, and ClusterConfig.ColdStorageOptions / AutomatedSnapshotPauseOptions / NodeToNodeEncryptionOptions are present as STRINGIFIED_JSON {"Enabled":false}.
Emulator: ConfigChangeStatus is 'Pending'; the AccessPolicies PendingValue has had the submitted Principal {"AWS":"<account-id>"} canonicalized to {"AWS":"arn:aws:iam::<account-id>:root"}; the AdvancedSecurityOptions sub-property entries carry an EMPTY PendingValue instead of 'false'; the entry at AWS's TAGS position is CognitoOptions with PendingValue {"Enabled":false} and ValueType STRINGIFIED_JSON where AWS has TAGS/PLAIN_TEXT/empty; the entry at AWS's ClusterConfig.ColdStorageOptions position is AutoTuneOptions; and where AWS reports AutomatedSnapshotPauseOptions {"Enabled":false} the emulator reports DeploymentStrategyOptions {"DeploymentStrategy":"CapacityOptimized"}, with the same class of displacement continuing at later indices.
Why: a client that polls the change status after creating a domain and waits for the documented Initializing -> ... -> Completed progression never observes the initializing state and can mis-sequence its wait; a client that indexes ModifyingProperties by position, or looks up an AWS property by name, reads the wrong property or finds none, and must parse JSON where AWS hands it plain text; and most seriously the access policy the service echoes is not the document the caller submitted, so a read-modify-write of the domain access policy writes back a security document the caller never authored.
Fix: the CreateDomain handler and response serializer for this service - derive ChangeProgressDetails.ConfigChangeStatus from the newly initiated change's real state instead of a hard-coded 'Pending', build ModifyingProperties from AWS's property enumeration (entry set, order, PendingValue stringification and ValueType selection) rather than from the emulator's option struct, and store/echo the AccessPolicies document verbatim with no principal canonicalization.
Traces: opensearch-domain-lifecycle-and-config-inspection#task#14.

Related but NOT covering this: the open issue 20261114T061447Z-es-domain-configuration-projection-diverges-modifyingproperties-entries-pendingvalue.md describes the same family of ModifyingProperties faults on OTHER steps (esdef-create-domain-gp3-server-defaults#task#1, opensearch-domain-lifecycle-and-config-inspection#task#2/#task#4/#task#5, opensearch-domain-lifecycle-workflow#setup#1, opensearch-domain-version-capacity-and-software-update-management#setup#1, opensearch-tag-domain-lifecycle#setup#1) and does NOT carry this step's replay key, nor does it mention the ChangeProgressDetails.ConfigChangeStatus divergence at all.

## Evidence
Fixture opensearch-domain-lifecycle-and-config-inspection, step task#14, operation CreateDomain. Status matched (expected 200, got 200); all divergences are in the response body:

  $.DomainStatus.ChangeProgressDetails.ConfigChangeStatus: expected 'Initializing', got 'Pending'
  $.DomainStatus.ModifyingProperties[3].PendingValue: expected '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"<account-id>"},"Action":"es:*","Resource":"arn:aws:es:<region>:<account-id>:domain/<domain-name>/*"}]}', got the same document with "Principal":{"AWS":"arn:aws:iam::<account-id>:root"}
  $.DomainStatus.ModifyingProperties[5].PendingValue: expected 'false', got ''
  $.DomainStatus.ModifyingProperties[7].PendingValue: expected 'false', got ''
  $.DomainStatus.ModifyingProperties[9].PendingValue: expected 'false', got ''
  $.DomainStatus.ModifyingProperties[10].PendingValue: expected 'false', got ''
  $.DomainStatus.ModifyingProperties[11].PendingValue: expected 'false', got ''
  $.DomainStatus.ModifyingProperties[26].PendingValue: expected '', got '{"Enabled":false}'
  $.DomainStatus.ModifyingProperties[26].ValueType: expected 'PLAIN_TEXT', got 'STRINGIFIED_JSON'
  $.DomainStatus.ModifyingProperties[27].PendingValue: expected '{"Enabled":false}', got '{"DeploymentStrategy":"CapacityOptimized"}'
  $.DomainStatus.ModifyingProperties[12]: extra - expected {Name: 'ClusterConfig.ColdStorageOptions', ActiveValue: '', PendingValue: '{"Enabled":false}', ValueType: 'STRINGIFIED_JSON'}, got {Name: 'AutoTuneOptions', ActiveValue: '', PendingValue: '{"DesiredState":"DISABLED","UseOffPeakWindow":false}', ValueType: 'STRINGIFIED_JSON'}
  $.DomainStatus.ModifyingProperties[26]: extra - expected {Name: 'TAGS', ActiveValue: '', PendingValue: '', ValueType: 'PLAIN_TEXT'}, got {Name: 'CognitoOptions', ActiveValue: '', PendingValue: '{"Enabled":false}', ...}
  same class of entry-set/order divergence at $.DomainStatus.ModifyingProperties[31], [34], [35] and [41]

NOTE on values: the account id (recorded as 000000000000) and the domain name (recorded as sim2real-e6cbd272) are this run's values and are not part of the requirement. The requirements are: ConfigChangeStatus is the enum member for a just-initiated change ('Initializing'); the AccessPolicies PendingValue is the policy document byte-for-byte as the request supplied it, with the principal in the submitted form; an AdvancedSecurityOptions sub-property with FGAC off has PendingValue the literal text 'false'; the TAGS entry is PLAIN_TEXT with PendingValue the stringified request tag map (empty when no tags were supplied); and the entry set and order follow AWS's domain-property enumeration.

Smithy cited (com.amazonaws.opensearch#AmazonOpenSearchService): ConfigChangeStatus enum [Pending, Initializing, Validating, ValidationFailed, ApplyingChanges, Completed, PendingUserInput, Cancelled]; ChangeProgressDetails{ChangeId, Message, ConfigChangeStatus, InitiatedBy, StartTime, LastUpdatedTime}; ModifyingProperties{Name, ActiveValue, PendingValue: String, ValueType: PropertyValueType}; PropertyValueType enum [PLAIN_TEXT, STRINGIFIED_JSON].

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Replay fixture opensearch-domain-lifecycle-and-config-inspection up to step task#14 - the CreateDomain call that re-creates the domain with an access policy whose Principal is the bare account id and with no tags.
4. In the 200 response, read $.DomainStatus.ChangeProgressDetails.ConfigChangeStatus: it must be 'Initializing', the emulator returns 'Pending'.
5. Read $.DomainStatus.ModifyingProperties element by element: entry 3 (AccessPolicies) must echo the submitted policy with the principal in the submitted bare-account-id form, not rewritten to arn:aws:iam::<account-id>:root; the AdvancedSecurityOptions sub-property entries (indices 5, 7, 9, 10, 11) must carry PendingValue 'false', not ''; index 12 must be ClusterConfig.ColdStorageOptions, not AutoTuneOptions; index 26 must be TAGS with ValueType PLAIN_TEXT and an empty PendingValue, not CognitoOptions/STRINGIFIED_JSON; index 27 must be AutomatedSnapshotPauseOptions {"Enabled":false}, not DeploymentStrategyOptions; indices 31, 34, 35 and 41 must likewise match AWS's property enumeration.
6. The step should pass against the recorded response.

## Disposition

Closed with the ConfigChangeStatus cause DECLINED as a per-run value, the access-policy
symptom verified gone and pinned by a test, and the ModifyingProperties symptoms left with the
issue that owns their replay keys.

* `$.DomainStatus.ChangeProgressDetails.ConfigChangeStatus` (this report's only replay key) —
  DECLINED. Which of the early states of its own change workflow AWS has reached when it
  serialises a create response is a per-run race, not a rule: the SAME key is recorded as
  'Pending' on five other creates (see
  `issues/things-that-now-work/20261029T190009Z-es-domainstatus-and-domainconfig-responses-omit-modeled-option.md`,
  cause 1 and its evidence line "expected 'Pending', got 'ApplyingChanges'"), and the earlier
  create in THIS fixture is not among this report's traces either, so one run served two
  creates two different values. The emulator reports a `ConfigChangeStatus` member for the
  state the change is really in, the same value on every path that reports that change, and
  advances it when the change advances; answering 'Initializing' on the create would fit this
  one recording and break those five.
* The access policy is NOT rewritten by this code base any more: a submitted document is held
  with only the insignificant whitespace between its JSON tokens dropped, and
  `tests/all/change_progress/change_status_reported_consistently.rs` now pins that over a
  table of principal forms (bare account id, account root ARN, wildcard, role ARN in a
  one-element list).
* The `ModifyingProperties` entry set, order, `PendingValue` and `ValueType` divergences on
  this step carry the replay keys of the OPEN issue
  `20261114T061447Z-es-domain-configuration-projection-diverges-modifyingproperties-entries-pendingvalue.md`,
  which holds the recorded rows for seven fixtures; they are that issue's to fix and are
  untouched here.
