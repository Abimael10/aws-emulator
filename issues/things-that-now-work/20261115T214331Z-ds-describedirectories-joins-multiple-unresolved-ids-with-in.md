Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds.describe-directories-absent-id-rejected
Issue-Shape-Id: com.amazonaws.directoryservice#DirectoryService_20150416
Issue-Key: ds.describe-directories-absent-id-rejected|run#15|AssertionError|assert both_message == expected_both, (
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# ds DescribeDirectories joins multiple unresolved ids with ", " in the "Invalid directory ID(s)" refusal where AWS joins them with a bare comma

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDirectories

## Description
One net-new root cause in the Directory Service control plane.

Cause 1 of 1: DescribeDirectories' EntityDoesNotExistException message separates the unresolved directory ids with a comma AND a space, where AWS separates them with a bare comma.

AWS: Smithy (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json): DescribeDirectories takes the optional list member DirectoryIds (entries matching ^d-[0-9a-f]{10}$) and models EntityDoesNotExistException (client, HTTP 400) alongside ClientException / InvalidNextTokenException / InvalidParameterException. A filter naming well-formed ids this account never held is refused (not served as an empty page). Message template — fixed text: "Invalid directory ID(s): " <the unresolved ids> ".", then the service envelope's " : RequestId: " <request id>. Variable part: the unresolved ids, taken from the request in the order the request listed them, joined by a SINGLE COMMA with NO space. The single-id form of the same template is right in the emulator today (that leg of the check passes), so the only diverging part of the fixed text is the separator used between two or more ids. The status (400) and the error code (EntityDoesNotExistException) are correct; so is the sibling InvalidNextTokenException leg ("Invalid value passed for the NextToken parameter.").

Emulator: DescribeDirectories over two absent ids answers the right code and status, with "Invalid directory ID(s): <id1>, <id2>. : RequestId: <rid>" — an extra space after the comma. The separator is hard-wired in the shared diagnostic: crate::state::invalid_directory_ids joins with ", " (services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/diagnostics.rs lines 59-64, whose own doc comment asserts AWS separates such values with "a comma and a space"), called from DescribeDirectories' unresolved-id branch (src/ops/describe_directories.rs, around line 266).

Why: the message is the only place that reports WHICH of the filtered ids the account does not hold. A client that splits the message on "," to recover the offending ids gets values with a leading space and fails to match them against the ids it sent; a client asserting on AWS's sentence by equality never matches at all once more than one id is unresolved. The divergence is invisible for a single unresolved id and appears on every multi-id filter, so it is exactly the case a caller reconciling an inventory list hits.

Fix: the shared ds diagnostic that renders the unresolved-id list for DescribeDirectories (the join in crate::state::invalid_directory_ids) — it must emit a bare comma between ids, keeping the filter order and the single-naming of a repeated id that are already correct. No handler change is needed in DescribeDirectories itself.

Related open issue that does NOT cover this (its Issue-Trace-Ids do not carry this check): 20261102T102235Z-ds-eight-refusal-messages-are-emulator-authored-instead.md, whose cause 6 made DescribeDirectories stop reusing the LDAPS "Directory: '<id>' does not exist." wording and adopt "Invalid directory ID(s): ..." — that opener is now right; only the multi-id separator inside it diverges.

Traces: ds.describe-directories-absent-id-rejected.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (ds: 131/157 passed).

Check ds.describe-directories-absent-id-rejected, DescribeDirectories(DirectoryIds=[<absent id 1>, <absent id 2>]):
  code:     EntityDoesNotExistException (matches AWS), HTTP 400 (matches)
  expected: 'Invalid directory ID(s): d-ac25483372,d-c368a4106a. : RequestId: <rid>'
  actual:   'Invalid directory ID(s): d-ac25483372, d-c368a4106a. : RequestId: <rid>'
AssertionError (check_ds_describe_directories_absent_id_rejected.py line 73): expected message '...d-ac25483372,d-c368a4106a. : RequestId: ...', got '...d-ac25483372, d-c368a4106a. : RequestId: ...'.

Fixed text of the template: "Invalid directory ID(s): ", the "." that closes it, and the " : RequestId: " envelope. Values from the request: the two directory ids (the particular ids and the RequestId above are per-run and are not part of the requirement). The ONLY diverging character is the space the emulator inserts after the comma between ids.

The same check's other legs pass and show the rest of the surface is right: the single-absent-id form yields exactly 'Invalid directory ID(s): <id>. : RequestId: <rid>'; the failed lookups register nothing; an unminted NextToken yields InvalidNextTokenException with 'Invalid value passed for the NextToken parameter. : RequestId: <rid>'.

Code evidence: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/diagnostics.rs lines 59-64 — format!("Invalid directory ID(s): {ids}.", ids = directory_ids.join(", ")); call site services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/describe_directories.rs line ~266 (crate::state::refusal(crate::state::invalid_directory_ids(&unresolved))).

## Steps to Reproduce
1. Clone this checkout and boot the emulator; replay suite simparity-behavioral-aws-v1.
2. Run check ds.describe-directories-absent-id-rejected, or by hand:
   a. ds DescribeDirectories() with no filter, to learn the inventory.
   b. Pick TWO ids matching ^d-[0-9a-f]{10}$ that no directory in the inventory holds.
   c. Call ds DescribeDirectories(DirectoryIds=[id1]) — observe HTTP 400 EntityDoesNotExistException with 'Invalid directory ID(s): id1. : RequestId: <rid>' (correct).
   d. Call ds DescribeDirectories(DirectoryIds=[id1, id2]) — observe HTTP 400 EntityDoesNotExistException with 'Invalid directory ID(s): id1, id2. : RequestId: <rid>'.
3. AWS answers 'Invalid directory ID(s): id1,id2. : RequestId: <rid>' — ids joined by a bare comma, in request order. Compare Error.Message character for character.
