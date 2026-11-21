Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: bab32ab3ecd51bb55a27809c2e18220867c710f4

# A NextToken this call issued is refused as "not a token this call issued" once its resume-at routing control is deleted

_Filed: 2026-11-21T06:09:34Z_
_Operation: ListRoutingControls | Class: pagination_

> _Note: this issue was identified against the code base at commit `bab32ab3ecd51bb55a27809c2e18220867c710f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListRoutingControls encodes its cursor as (the ControlPanelArn filter, the ARN of the routing control the next page starts at). When the next page is requested, the handler decodes the cursor and then looks the resume-at ARN up by position in the current listing, collapsing TWO different outcomes into one refusal:

1. the token was not issued by this operation (or was issued for another filter) — `decode_cursor` answers `None`, and
2. the token WAS issued by this operation, decodes cleanly, carries the right version tag and the right filter, but the routing control it resumes at has since been deleted through the control plane — `decode_cursor` answers `Some(arn)` and `controls.iter().position(..)` answers `None`.

Both land in the same `ok_or_else` and are answered HTTP 400 `ValidationException` with the message "NextToken is not a token this call issued for the requested ControlPanelArn" — a statement that is simply untrue of case 2, and which aborts a legitimate pagination walk: the client cannot continue the listing at all, it must restart it, so the pages do NOT walk the whole set.

This contradicts the file's own stated rationale for naming an ITEM rather than an offset (robustness against a routing control being created or deleted between two pages), the documented contract of `decode_cursor` (which reports `None` only for a token this operation did not issue for this filter), and the module header's claim that the pages walk the whole set without repeats or omissions. The deleting of a routing control mid-walk is a reachable input: the crate's own test proves a control the control plane deletes stops being listed.

## Evidence

The rationale the code breaks — services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/src/ops/list_routing_controls.rs:133-136:
"The cursor names the next ITEM rather than an offset into the listing, because the set it walks is the control plane's registry: a routing control created or deleted between two pages would shift every offset after it, but it cannot shift the ARN this page is to resume at."

The module header, same file, lines 9-12:
"Paging honours `MaxResults` and `NextToken`: the token is an opaque, versioned encoding of the filter it was issued for plus the routing control the next page starts at, so issuing a page writes no state and the pages walk the whole set without repeats or omissions."

The decoder's documented contract — same file, lines 144-145:
"/// The routing control ARN `token` resumes at, or `None` when it is not a token this
/// operation issued for this same `ControlPanelArn` filter."

The code that breaks them — same file, lines 289-312:
```
        let offset = match input.next_token.as_deref() {
            None => 0,
            Some(token) => decode_cursor(token, filter)
                .and_then(|resume_at| {
                    controls
                        .iter()
                        .position(|control| control.routing_control_arn == resume_at)
                })
                .ok_or_else(|| {
                    ... ValidationException {
                            message: "NextToken is not a token this call issued for the \
                                      requested ControlPanelArn"
```
So a token this operation DID issue, whose resume-at control the control plane has since deleted, is answered with the sentence saying it was not issued by this call.

The input is reachable and already exercised in the crate — services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/tests/all/control_plane_registry.rs:134 `delete_routing_control(&router, &late.arn).await;` and :158-161 asserting "case a deleted routing control: it must not be listed".

Test coverage of the pagination path today: tests/all/routing_control_toggle.rs:386 `a_next_token_is_only_valid_for_the_call_that_issued_it` pins only tokens that were never issued ("not-a-token", `cursor_for(&unknown_arn_like(..))`) and tokens replayed under a different filter — no test covers a token this service issued whose resume-at control was deleted between pages, so this behaviour is pinned by nothing.

## Steps to Reproduce

1. Create a cluster, a control panel and three routing controls through the recovery-control config (control) plane, as tests/all/common.rs `fleet` does.
2. Call ListRoutingControls with MaxResults=1 (no ControlPanelArn) and keep the returned NextToken; note which routing control the next page would start at (the second ARN in ARN order).
3. Delete THAT routing control through the control plane (DeleteRoutingControl), as tests/all/control_plane_registry.rs:134 does.
4. Call ListRoutingControls again with the kept NextToken.
5. Observed: HTTP 400 ValidationException "NextToken is not a token this call issued for the requested ControlPanelArn" — about a token this call did issue — and the pagination walk cannot be continued, so the remaining routing controls are never listed.
6. Compare with src/ops/list_routing_controls.rs:133-136, which gives deletion-robustness as the reason the cursor names an item rather than an offset, and lines 9-12, which claim the pages walk the whole set without omissions.
