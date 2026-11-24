Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 35d7082131f9491ad89c2e4b5f0079c864ac9dfa

# Nothing in src/ ever stores a billing-view resource policy, so GetResourcePolicy answers 404 for every request a client can make

_Filed: 2026-11-23T07:46:06Z_
_Operation: GetResourcePolicy | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `35d7082131f9491ad89c2e4b5f0079c864ac9dfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

GetResourcePolicy reads `BillingView::resource_policy` and answers the modelled `ResourceNotFoundException` (404) whenever it is `None`. No code path under `src/` ever sets that field to anything other than `None`: `CreateBillingView` writes `resource_policy: None`, the seeded default PRIMARY view is created with `None` (and is additionally filtered out as managed by the read), and no other handler or state helper assigns it. The only writer in the whole repository is this crate's own test suite, which reaches into the engine's state and assigns the field directly. The consequence is that the operation's success branch is unreachable over the wire: whatever a client creates, shares or tags, every GetResourcePolicy request is answered 404, and the `policy` output member can never carry a document. This is the same unreachable-gate shape the crate's own comments admit ("nothing in the API writes it"), and it leaves a modelled read with no state behind it: the emulator has no path by which the document a shared view carries ever comes into existence.

## Evidence

The field's own doc admits no API writes it — services/service-com-amazonaws-billing-awsbilling/src/state.rs:466-472:
```
    /// `None` for every view this emulator creates: only a CUSTOM billing view
    /// can carry one, and this service's model declares no operation that
    /// attaches a policy (there is no `PutResourcePolicy` — on AWS the document
    /// appears when the view is shared), so nothing in the API writes it.
    /// `GetResourcePolicy` reports the stored document when a view carries one
    /// and answers `ResourceNotFoundException` when it does not.
    pub resource_policy: Option<String>,
```
The only creator writes `None` — src/ops/create_billing_view.rs:353-355:
```
            // No operation in this service's model attaches a resource-based
            // policy, so a new view carries none: see `BillingView::resource_policy`.
            resource_policy: None,
```
The read therefore always takes the 404 branch — src/ops/get_resource_policy.rs:188-201:
```
        let policy = state
            .billing_views
            .get(&input.resource_arn)
            .filter(|view| !view.is_managed())
            .and_then(|view| view.resource_policy.clone())
            .ok_or_else(|| {
                GetResourcePolicyErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: "The custom billing view does not exist.".to_string(),
```
The single writer in the repo is the test suite, going around the API — tests/all/source_views_tags_and_policy.rs:912-920:
```
            let state = service_com_amazonaws_billing_awsbilling::state::service_state(&*engine)
                .expect("the billing state is reachable from the shared engine");
            let mut state = state.lock();
            state
                .billing_views
                .get_mut(&target)
                .expect("the view just created is stored")
                .resource_policy = Some(document.to_string());
```
`grep -r resource_policy` over the crate finds assignments only at state.rs:814 (`None`), create_billing_view.rs:355 (`None`), and the two test files (source_views_tags_and_policy.rs:920, cross_account_existence_signal.rs:76). Test coverage: tests/all/source_views_tags_and_policy.rs::get_resource_policy_reports_a_stored_policy_and_404s_without_one exercises both branches, but reaches the 200 branch only by writing state directly.

## Steps to Reproduce

1. Start the emulator and create a CUSTOM billing view: CreateBillingView {"name":"shared","sourceViews":["arn:aws:billing::<account>:billingview/primary"]} -> 200 with an arn
2. Call GetResourcePolicy {"resourceArn":"<that arn>"} -> 404 ResourceNotFoundException "The custom billing view does not exist."
3. Exercise every other operation of the service (tagging, association, update) and call GetResourcePolicy again -> still 404; no API request can make the handler's success branch run
4. Confirm by grep: the only assignments of `resource_policy` outside tests are `None` (src/state.rs:814, src/ops/create_billing_view.rs:355)
