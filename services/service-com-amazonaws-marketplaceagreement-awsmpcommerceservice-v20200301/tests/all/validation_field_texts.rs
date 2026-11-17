// SPDX-License-Identifier: CC0-1.0

//! What a `ValidationExceptionField` entry SAYS about the member it names.
//!
//! The rule under test: `ValidationExceptionField$message` is the fixed text of
//! the class the member failed in, reproduced exactly — one text for a member
//! that carried a value this service does not admit, one for a member whose
//! well-formed identifier names no resource of the account, one for a member
//! that was absent and is required of every request, and one for a member that
//! was absent and became required only through the `intent` the request stated.
//! The variable part of the entry is `name`, the rejected member's dotted
//! request path taken from the request. So the text depends on WHAT was wrong
//! with the member and not on which member, which operation, which depth it sits
//! at, or whether the modelled constraint walk or a rule inside a handler is
//! what refused it.
//!
//! Each row differs from the others in what the rule generalises over: the class
//! of the failure, the constraint missed or the rule broken, the member, the
//! depth (top level, inside a structure, a member of a list element), the
//! operation, the reason, and whether the boundary walk or a rule inside a
//! handler refused the request — including the SAME member refused both ways, so
//! the sentence is shown not to report where the refusal was raised. The
//! recorded cases — an unacceptable party type on `ListAgreementPaymentRequests`
//! and on `ListAgreementCancellationRequests`, a `SearchAgreements` filter
//! refusal, an `agreementId` that resolves to no agreement on
//! `SendAgreementPaymentRequest`, the absent per-element agreement identifier on
//! `UpdatePurchaseOrders`, and the intent-conditional proposal identifier on
//! `CreateAgreementRequest` — are rows among them.
//!
//! The tables at the end carry the unresolved-reference class on its own, over
//! every operation of this service that RESOLVES an identifier: the five calls
//! of the payment-request family and the three listings scoped to one agreement
//! for `agreementId`, the send for the `termId` of an agreement that does
//! resolve, and an accept for a `chargeId` one level down inside a list element —
//! each contrasted with a member of the same operation, and at the same depth,
//! refused for carrying a value it does not admit.
//!
//! The harnesses (`emulator`, `call`, `cancel`, `term`, `agreement_type_filter`,
//! the agreement carrying a `VariablePaymentTerm`, the quote and purchase-order
//! builders) are the ones `agreement_lifecycle`, `payment_requests` and
//! `purchase_order_associations` already own.

use serde_json::{json, Value};

use crate::agreement_lifecycle::{agreement_type_filter, call, cancel, emulator, term, Answer};
// The `AgreementId` forms no minter of this emulator produces (they span the
// published pattern and both published length bounds), the readers of a refused
// member's body, and the two class sentences: the catalog and the readers
// `error_bodies` already owns, rather than second copies here.
use crate::error_bodies::{
    assert_input_fault, assert_unresolvable_reference_fault, unresolvable_agreement_ids,
    FIELD_INVALID_REFERENCE,
};
use crate::payment_requests::agreement_with_variable_term;
use crate::purchase_order_associations::{order, quote};

/// The fixed text for a member that carried a value this service does not
/// admit, whichever part of the service refused it: a constraint the model
/// declares, or a rule of the service itself. Spelled out here rather than
/// imported, so a change of wording in the service fails this test instead of
/// silently agreeing with it.
const FIELD_INVALID: &str = "Member is invalid.";
/// The fixed text for a member that was absent from a request every one of which
/// has to carry it.
const FIELD_MISSING: &str = "Member is required.";
/// The fixed text for a member that was absent from a request whose `intent` is
/// what made it required.
const FIELD_MISSING_FOR_INTENT: &str = "Member must not be null for the provided intent.";

/// The class a refused member failed in, which is what decides the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The value misses a constraint the model declares.
    InvalidConstraint,
    /// The value is well formed, and a rule of the service does not admit it.
    Unacceptable,
    /// The value is a well-formed identifier naming no resource of the account,
    /// so there is nothing for the call to act on.
    UnresolvedReference,
    /// The member was absent, and every request has to carry it.
    Absent,
    /// The member was absent, and the request's `intent` is what required it.
    AbsentForIntent,
}

impl Class {
    fn text(self) -> &'static str {
        match self {
            // ONE text for both: a member whose VALUE this service does not
            // admit is one class of failure however the service came to refuse
            // it — a constraint the model declares, or a rule of its own.
            Self::InvalidConstraint | Self::Unacceptable => FIELD_INVALID,
            Self::UnresolvedReference => FIELD_INVALID_REFERENCE,
            Self::Absent => FIELD_MISSING,
            Self::AbsentForIntent => FIELD_MISSING_FOR_INTENT,
        }
    }
}

/// One refused request, and the single `fields` entry it must be answered with.
struct Row {
    /// What this row varies, for the assertion messages.
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The `ValidationExceptionReason` the body must carry.
    reason: &'static str,
    /// The message the body must carry.
    message: &'static str,
    /// The dotted request path the entry must name.
    field: &'static str,
    /// The class that path's entry must be worded in.
    class: Class,
}

/// The `fields` entries of an answer, as `(name, message)` pairs in the order
/// they were reported.
fn fields(answer: &Answer) -> Vec<(String, String)> {
    answer.body["fields"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["name"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `SendAgreementPaymentRequest` whose every member satisfies its modelled
/// constraints (`PaymentRequestName` is `@length(5..64)`,
/// `PositiveAmountUpto8Decimals` is `^[0-9]*(\.[0-9]{0,8})?$`,
/// `smithy_query(query="operation", operation="SendAgreementPaymentRequest")`),
/// so the only member that can be refused is the `agreementId` it charges under.
fn payment_request(agreement_id: &str) -> Value {
    json!({
        "agreementId": agreement_id,
        "termId": "VariablePaymentTerm",
        "name": "October usage",
        "chargeAmount": "10.00",
    })
}

fn rows() -> Vec<Row> {
    vec![
        // A value that misses a constraint the model declares, over the
        // constraints, the depths and the operations.
        Row {
            case: "a pattern violation at the top level",
            operation: "DescribeAgreement",
            request: json!({"agreementId": "not an agreement id!"}),
            reason: "INVALID_AGREEMENT_ID",
            message: "Provided agreement id is invalid.",
            field: "agreementId",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a value outside a modelled enum, nested in a structure",
            operation: "SearchAgreements",
            request: json!({"sort": {"sortOrder": "SIDEWAYS"}}),
            reason: "INVALID_SORT_ORDER",
            message: "Provided sort order is invalid.",
            field: "sort.sortOrder",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a number above the range its member admits",
            operation: "GetAgreementTerms",
            request: json!({"agreementId": "agmt-1", "maxResults": 51}),
            reason: "INVALID_MAX_RESULTS",
            message: "Provided max results is invalid.",
            field: "maxResults",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a string below the length its member admits",
            operation: "SendAgreementPaymentRequest",
            request: json!({
                "agreementId": "agmt-1",
                "termId": "VariablePaymentTerm",
                "name": "four",
                "chargeAmount": "10.00",
            }),
            reason: "INVALID_NAME",
            message: "Provided name is invalid.",
            field: "name",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a pattern violation on a member of a list element",
            operation: "UpdatePurchaseOrders",
            request: json!({"purchaseOrders": [{"chargeId": "not a charge id!"}]}),
            reason: "INVALID_CHARGE_ID",
            message: "Provided charge id is invalid.",
            field: "purchaseOrders.1.member.chargeId",
            class: Class::InvalidConstraint,
        },
        Row {
            // The SAME member and the same reason as the service-rule row
            // below, failing the constraint its shape declares instead
            // (`PartyType` is `@pattern ^[A-Za-z]+$`): the text follows what
            // refused the value, not the member it was about.
            case: "a party type outside the pattern its shape declares",
            operation: "ListAgreementCancellationRequests",
            request: json!({"partyType": "Not A Party!"}),
            reason: "INVALID_PARTY_TYPE",
            message: "Provided party type is invalid.",
            field: "partyType",
            class: Class::InvalidConstraint,
        },
        // A well-formed value a rule of the service itself does not accept.
        Row {
            case: "a party type naming neither party",
            operation: "ListAgreementCancellationRequests",
            request: json!({"partyType": "Bystander"}),
            reason: "INVALID_PARTY_TYPE",
            message: "Provided party type is invalid.",
            field: "partyType",
            class: Class::Unacceptable,
        },
        Row {
            case: "a catalog this marketplace does not expose",
            operation: "ListAgreementCancellationRequests",
            request: json!({"partyType": "Acceptor", "catalog": "AnotherCatalog"}),
            reason: "INVALID_CATALOG",
            message: "Provided catalog is invalid.",
            field: "catalog",
            class: Class::Unacceptable,
        },
        Row {
            case: "an id naming no agreement the caller can charge under",
            operation: "SendAgreementPaymentRequest",
            request: payment_request("agmt-nothing-holds-this"),
            reason: "INVALID_AGREEMENT_ID",
            message: "Provided agreement id is invalid.",
            field: "agreementId",
            class: Class::UnresolvedReference,
        },
        Row {
            case: "a member the stated intent does not admit at all",
            operation: "CreateAgreementRequest",
            request: json!({
                "intent": "NEW",
                "agreementProposalIdentifier": "ap-offer1",
                "sourceAgreementIdentifier": "agmt-1",
                "requestedTerms": [term("eula.v1")],
            }),
            reason: "INVALID_SOURCE_AGREEMENT_IDENTIFIER",
            message: "Provided source agreement identifier is invalid.",
            field: "sourceAgreementIdentifier",
            class: Class::Unacceptable,
        },
        Row {
            case: "the intent-conditional member present but outside its pattern",
            operation: "CreateAgreementRequest",
            request: json!({
                "intent": "NEW",
                "agreementProposalIdentifier": "not a proposal!",
                "requestedTerms": [term("eula.v1")],
            }),
            reason: "INVALID_AGREEMENT_PROPOSAL_IDENTIFIER",
            message: "Provided agreement proposal identifier is invalid.",
            field: "agreementProposalIdentifier",
            class: Class::InvalidConstraint,
        },
        // A member absent from a request every one of which has to carry it.
        Row {
            case: "a required scalar left out",
            operation: "DescribeAgreement",
            request: json!({}),
            reason: "MISSING_AGREEMENT_ID",
            message: "Agreement id is required.",
            field: "agreementId",
            class: Class::Absent,
        },
        Row {
            case: "a required list left out",
            operation: "UpdatePurchaseOrders",
            request: json!({}),
            reason: "MISSING_PURCHASE_ORDERS",
            message: "Purchase orders is required.",
            field: "purchaseOrders",
            class: Class::Absent,
        },
        Row {
            case: "a required member of a list element left out",
            operation: "SearchAgreements",
            request: json!({"filters": [{"values": ["x"]}]}),
            reason: "MISSING_FILTER_NAME",
            message: "Filter name is required.",
            field: "filters.1.member.name",
            class: Class::Absent,
        },
        Row {
            // Two defects at once: the element names a charge nothing holds AND
            // leaves out the agreement the association belongs to. The absent
            // member is what the refusal is about, so the entry is the
            // absent-member one at that member's own path.
            case: "an absent per-element member of a request with a second defect",
            operation: "UpdatePurchaseOrders",
            request: json!({
                "purchaseOrders": [
                    {"chargeId": "chg-1", "agreementId": "agmt-1", "chargeRevision": 1,
                     "purchaseOrderReference": "po-1"},
                    {"chargeId": "chg-nothing-holds-this", "chargeRevision": 1,
                     "purchaseOrderReference": "po-2"},
                ],
            }),
            reason: "MISSING_AGREEMENT_ID",
            message: "Agreement id is required.",
            field: "purchaseOrders.2.member.agreementId",
            class: Class::Absent,
        },
        // A member absent from a request whose intent is what required it:
        // "required if the intent is not AMEND" and "required for all non-NEW
        // intents" (`smithy_query(query="operation",
        // operation="CreateAgreementRequest")`), over every intent each rule
        // covers.
        Row {
            case: "the proposal identifier left out under the NEW intent",
            operation: "CreateAgreementRequest",
            request: json!({"intent": "NEW", "requestedTerms": [term("eula.v1")]}),
            reason: "MISSING_AGREEMENT_PROPOSAL_IDENTIFIER",
            message: "Agreement proposal identifier is required.",
            field: "agreementProposalIdentifier",
            class: Class::AbsentForIntent,
        },
        Row {
            case: "the same member left out under the REPLACE intent",
            operation: "CreateAgreementRequest",
            request: json!({
                "intent": "REPLACE",
                "sourceAgreementIdentifier": "agmt-1",
                "requestedTerms": [term("eula.v1")],
            }),
            reason: "MISSING_AGREEMENT_PROPOSAL_IDENTIFIER",
            message: "Agreement proposal identifier is required.",
            field: "agreementProposalIdentifier",
            class: Class::AbsentForIntent,
        },
        Row {
            case: "the source agreement left out under the AMEND intent",
            operation: "CreateAgreementRequest",
            request: json!({"intent": "AMEND", "requestedTerms": [term("eula.v1")]}),
            reason: "MISSING_SOURCE_AGREEMENT_IDENTIFIER",
            message: "Source agreement identifier is required.",
            field: "sourceAgreementIdentifier",
            class: Class::AbsentForIntent,
        },
        Row {
            case: "the same member left out under the REPLACE intent",
            operation: "CreateAgreementRequest",
            request: json!({
                "intent": "REPLACE",
                "agreementProposalIdentifier": "ap-offer1",
                "requestedTerms": [term("eula.v1")],
            }),
            reason: "MISSING_SOURCE_AGREEMENT_IDENTIFIER",
            message: "Source agreement identifier is required.",
            field: "sourceAgreementIdentifier",
            class: Class::AbsentForIntent,
        },
        // A member that carried a value a RULE OF THE SERVICE does not accept,
        // on the operations whose refusals are almost entirely of that kind:
        // the party type naming neither role, the selectors whose value sets
        // this service fixes, the sort attribute, the filter name, the filter
        // value, the two filter-combination rules, and a page token minted for
        // another query.
        Row {
            case: "a party type naming neither role, on the payment-request listing",
            operation: "ListAgreementPaymentRequests",
            request: json!({"partyType": "Bystander"}),
            reason: "INVALID_PARTY_TYPE",
            message: "Provided party type is invalid.",
            field: "partyType",
            class: Class::Unacceptable,
        },
        Row {
            case: "a catalog this marketplace does not expose, on a listing",
            operation: "ListAgreementPaymentRequests",
            request: json!({"partyType": "Acceptor", "catalog": "AnotherCatalog"}),
            reason: "INVALID_CATALOG",
            message: "Provided catalog is invalid.",
            field: "catalog",
            class: Class::Unacceptable,
        },
        Row {
            // `agreementType` is documented as `PurchaseAgreement` or
            // `VendorInsightsAgreement` (`smithy_query(query="doc",
            // shape="DescribeAgreementOutput", member="agreementType")`) and is
            // a plain `^[A-Za-z]+$` string in the model, so a third word is a
            // rule of the service to refuse.
            case: "an agreement type outside the documented pair",
            operation: "ListAgreementPaymentRequests",
            request: json!({"partyType": "Proposer", "agreementType": "SubscriptionAgreement"}),
            reason: "INVALID_AGREEMENT_TYPE",
            message: "Provided agreement type is invalid.",
            field: "agreementType",
            class: Class::Unacceptable,
        },
        Row {
            case: "a sort attribute this service does not sort on",
            operation: "SearchAgreements",
            request: json!({
                "filters": [agreement_type_filter()],
                "sort": {"sortBy": "Whatever"},
            }),
            reason: "INVALID_SORT_BY",
            message: "Provided sort by is invalid.",
            field: "sort.sortBy",
            class: Class::Unacceptable,
        },
        Row {
            case: "a filter name this service does not publish",
            operation: "SearchAgreements",
            request: json!({"filters": [{"name": "NotAFilter", "values": ["x"]}]}),
            reason: "INVALID_FILTER_NAME",
            message: "Provided filter name is invalid.",
            field: "filters.1.member.name",
            class: Class::Unacceptable,
        },
        Row {
            case: "a filter value outside the set its attribute publishes",
            operation: "SearchAgreements",
            request: json!({"filters": [{"name": "PartyType", "values": ["Bystander"]}]}),
            reason: "INVALID_PARTY_TYPE",
            message: "Provided party type is invalid.",
            field: "filters.1.member.values.1.member",
            class: Class::Unacceptable,
        },
        Row {
            case: "a catalog this marketplace does not expose, on a search",
            operation: "SearchAgreements",
            request: json!({
                "catalog": "AnotherCatalog",
                "filters": [agreement_type_filter()],
            }),
            reason: "INVALID_CATALOG",
            message: "Provided catalog is invalid.",
            field: "catalog",
            class: Class::Unacceptable,
        },
        Row {
            // The unsupported-combination refusal has a body of its own, which
            // states the combination in `message` because no single member of
            // such a request is the wrong one — its `fields` entry is still the
            // invalid-member sentence.
            case: "a filter combination the acceptor party type does not support",
            operation: "SearchAgreements",
            request: json!({
                "filters": [
                    agreement_type_filter(),
                    {"name": "PartyType", "values": ["Acceptor"]},
                    {"name": "AcceptorAccountId", "values": ["000000000000"]},
                ],
            }),
            reason: "UNSUPPORTED_FILTERS",
            message: "Provided combination of filters is not supported.",
            field: "filters",
            class: Class::Unacceptable,
        },
        Row {
            case: "a sort attribute the search's party type does not support",
            operation: "SearchAgreements",
            request: json!({
                "filters": [
                    agreement_type_filter(),
                    {"name": "PartyType", "values": ["Acceptor"]},
                ],
                "sort": {"sortBy": "StartTime"},
            }),
            reason: "UNSUPPORTED_FILTERS",
            message: "Provided combination of filters is not supported.",
            field: "sort.sortBy",
            class: Class::Unacceptable,
        },
        Row {
            case: "a page token no search of this kind minted",
            operation: "SearchAgreements",
            request: json!({
                "filters": [agreement_type_filter()],
                "nextToken": "bm90LWEtdG9rZW4=",
            }),
            reason: "INVALID_NEXT_TOKEN",
            message: "Provided next token is invalid.",
            field: "nextToken",
            class: Class::Unacceptable,
        },
        // The same members refused by a constraint the MODEL declares instead,
        // at the boundary: the same sentence, so the wording does not report
        // where the refusal was raised.
        Row {
            case: "a party type outside the pattern its shape declares, on the same listing",
            operation: "ListAgreementPaymentRequests",
            request: json!({"partyType": "Party1"}),
            reason: "INVALID_PARTY_TYPE",
            message: "Provided party type is invalid.",
            field: "partyType",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a page size one past the range its member admits",
            operation: "SearchAgreements",
            request: json!({"filters": [agreement_type_filter()], "maxResults": 51}),
            reason: "INVALID_MAX_RESULTS",
            message: "Provided max results is invalid.",
            field: "maxResults",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a filter list one past the length its shape admits",
            operation: "SearchAgreements",
            request: json!({"filters": vec![agreement_type_filter(); 11]}),
            reason: "INVALID_FILTERS",
            message: "Provided filters is invalid.",
            field: "filters",
            class: Class::InvalidConstraint,
        },
        Row {
            case: "a nested filter value outside the pattern its shape declares",
            operation: "SearchAgreements",
            request: json!({
                "filters": [{"name": "Status", "values": ["not a filter value!"]}],
            }),
            reason: "INVALID_FILTER_VALUES",
            message: "Provided filter values is invalid.",
            field: "filters.1.member.values.1.member",
            class: Class::InvalidConstraint,
        },
    ]
}

/// Every refused member is named in one `fields` entry whose message is the
/// fixed text of the class it failed in, at the modelled 400
/// `ValidationException` carrying that class's reason and message.
#[tokio::test]
async fn a_field_entry_states_the_fixed_text_of_its_class() {
    let router = emulator();
    for row in rows() {
        let case = row.case;
        let operation = row.operation;
        let answer = call(&router, operation, row.request.clone()).await;

        assert_eq!(
            answer.status, 400,
            "case {case:?} ({operation}): expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?} ({operation}): expected error code ValidationException, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.reason(),
            row.reason,
            "case {case:?} ({operation}): expected reason {:?}, got {:?}",
            row.reason,
            answer.body
        );
        assert_eq!(
            answer.str_at("/message"),
            row.message,
            "case {case:?} ({operation}): expected message {:?}, got {:?}",
            row.message,
            answer.body
        );
        assert_eq!(
            fields(&answer),
            vec![(row.field.to_string(), row.class.text().to_string())],
            "case {case:?} ({operation}): expected one entry at {:?} reading {:?}, got {:?}",
            row.field,
            row.class.text(),
            answer.body
        );
    }
}

/// The texts are each other's contrast: ONE member, refused in four ways, reads
/// in the text of the CLASS it failed in — the same unacceptable-value sentence
/// whether a constraint the model declares or a rule of the service refused the
/// value it carried, the unresolved-reference sentence when the value named no
/// agreement of the account at all, and the absent-member sentence when it
/// carried no value.
#[tokio::test]
async fn one_member_refused_in_four_ways_reads_the_text_of_its_class() {
    let router = emulator();
    let member = "agreementId";

    // Present, and outside the pattern `AgreementId` declares: a constraint the
    // model states, so the boundary walk refuses it.
    let constraint = call(
        &router,
        "DescribeAgreement",
        json!({"agreementId": "not an agreement id!"}),
    )
    .await;
    // Well formed, naming an agreement this account DOES hold, and refused by a
    // rule of the service about the state that agreement is in ("the agreement
    // must be active"): a value this member does not admit.
    let cancelled =
        agreement_with_variable_term(&router, "ap-fieldtexts", "VariablePaymentTerm").await;
    let cancellation = cancel(&router, &cancelled).await;
    assert_eq!(
        cancellation.status, 200,
        "cancelling the agreement must succeed, got {} with {:?}",
        cancellation.status, cancellation.body
    );
    let unacceptable = call(
        &router,
        "SendAgreementPaymentRequest",
        payment_request(&cancelled),
    )
    .await;
    // Well formed, and naming no agreement of the account: nothing for the call
    // to act on, which is the unresolved-reference class.
    let unresolved = call(
        &router,
        "SendAgreementPaymentRequest",
        payment_request("agmt-nothing-holds-this"),
    )
    .await;
    // Absent from a request that has to carry it.
    let absent = call(&router, "DescribeAgreement", json!({})).await;

    let text_of = |answer: &Answer| {
        fields(answer)
            .first()
            .map(|(_, message)| message.clone())
            .unwrap_or_default()
    };
    for (case, answer, expected) in [
        ("a constraint the model declares", &constraint, FIELD_INVALID),
        ("a rule of the service", &unacceptable, FIELD_INVALID),
        (
            "an id naming no agreement",
            &unresolved,
            FIELD_INVALID_REFERENCE,
        ),
        ("a member left out", &absent, FIELD_MISSING),
    ] {
        assert_eq!(
            fields(answer),
            vec![(member.to_string(), expected.to_string())],
            "case {case:?}: expected one entry at {member:?} reading {expected:?}, got {:?}",
            answer.body
        );
    }

    let texts = [
        text_of(&constraint),
        text_of(&unacceptable),
        text_of(&unresolved),
        text_of(&absent),
    ];
    assert_eq!(
        texts[0], texts[1],
        "case {member:?}: a value a rule of the service refuses states the same sentence as a \
         value that misses a modelled constraint, got {texts:?}"
    );
    assert_ne!(
        texts[1], texts[2],
        "case {member:?}: a value this member does not admit and an identifier that resolves to \
         nothing are different classes and keep different sentences, got {texts:?}"
    );
    assert_ne!(
        texts[2], texts[3],
        "case {member:?}: an identifier that resolves to nothing and an absent member are \
         different classes and keep different sentences, got {texts:?}"
    );
}

// ---------------------------------------------------------------------------
// The unresolved-reference class, over the operations that RESOLVE an identifier
// ---------------------------------------------------------------------------

/// The term id the agreements below carry their `VariablePaymentTerm` under.
const TERM_ID: &str = "VariablePaymentTerm";

/// A `paymentRequestId` within `^pr-[a-zA-Z0-9]+$` that this emulator's minter
/// never produces: the rows below are about the agreement, the term or the
/// charge a call cannot resolve, so the child id is never the thing under test.
const UNKNOWN_PAYMENT_REQUEST: &str = "pr-ffffffffffffffff";

/// The operations carrying `agreementId` as an input member they RESOLVE: the
/// five calls of the payment-request family and the three listings scoped to one
/// agreement, each with the other members the model makes `@required` set to a
/// value it accepts — so the only thing a row varies is the agreement named
/// (`smithy_query(query="operation", operation="GetAgreementPaymentRequest,
/// AcceptAgreementPaymentRequest,RejectAgreementPaymentRequest,
/// CancelAgreementPaymentRequest,SendAgreementPaymentRequest,
/// ListAgreementPaymentRequests,ListAgreementCancellationRequests,
/// ListBillingAdjustmentRequests")`).
fn agreement_id_calls() -> Vec<(&'static str, Value)> {
    vec![
        (
            "GetAgreementPaymentRequest",
            json!({"paymentRequestId": UNKNOWN_PAYMENT_REQUEST}),
        ),
        (
            "AcceptAgreementPaymentRequest",
            json!({"paymentRequestId": UNKNOWN_PAYMENT_REQUEST}),
        ),
        (
            "RejectAgreementPaymentRequest",
            json!({
                "paymentRequestId": UNKNOWN_PAYMENT_REQUEST,
                "rejectionReason": "The invoice has already been settled",
            }),
        ),
        (
            "CancelAgreementPaymentRequest",
            json!({"paymentRequestId": UNKNOWN_PAYMENT_REQUEST}),
        ),
        (
            "SendAgreementPaymentRequest",
            json!({
                "termId": TERM_ID,
                "name": "Milestone one delivery",
                "chargeAmount": "12.5",
            }),
        ),
        (
            "ListAgreementPaymentRequests",
            json!({"partyType": "Acceptor"}),
        ),
        (
            "ListAgreementCancellationRequests",
            json!({"partyType": "Proposer"}),
        ),
        ("ListBillingAdjustmentRequests", json!({})),
    ]
}

/// An identifier naming no resource of the account is reported in the
/// unresolved-reference class, on every operation that resolves one — over the
/// operation, the kind of resource the identifier fails to name (an agreement,
/// and a term of one that does resolve) and the form of the identifier.
///
/// The envelope is asserted through the shared readers `error_bodies` owns, so
/// these rows state the same body every other suite over an unresolvable
/// identifier states.
#[tokio::test]
async fn an_identifier_that_resolves_to_nothing_states_its_own_sentence() {
    let router = emulator();
    // A stored agreement, so no row depends on the store being empty and the
    // term rows have an agreement that DOES resolve.
    let stored = agreement_with_variable_term(&router, "ap-unresolved", TERM_ID).await;

    for (operation, others) in agreement_id_calls() {
        for (id_case, id) in unresolvable_agreement_ids() {
            assert_ne!(
                id, stored,
                "case {id_case:?}: the id under test must not be the stored agreement"
            );
            let mut request = others.clone();
            request["agreementId"] = json!(id);
            let answer = call(&router, operation, request).await;
            assert_unresolvable_reference_fault(
                &format!("{operation} over {id_case}"),
                "INVALID_AGREEMENT_ID",
                "agreementId",
                &answer,
            );
        }
    }

    // The same class on another member and another kind of resource: a term of
    // an agreement that DOES resolve. `TermId` is `^[A-Za-z0-9+=;,.@\-_]+$`
    // (`smithy_query(query="shape", shape="TermId")`), so each of these is a
    // well-formed id naming no `VariablePaymentTerm` of that agreement.
    for (term_case, term_id) in [
        ("a term id no term of it carries", "vpt-not-this-one"),
        ("the shortest TermId, one character", "T"),
        ("a term id spelling another kind of term", "LegalTerm"),
    ] {
        let answer = call(
            &router,
            "SendAgreementPaymentRequest",
            json!({
                "agreementId": stored.clone(),
                "termId": term_id,
                "name": "Milestone one delivery",
                "chargeAmount": "12.5",
            }),
        )
        .await;
        assert_unresolvable_reference_fault(
            &format!("SendAgreementPaymentRequest over {term_case}"),
            "INVALID_TERM_ID",
            "termId",
            &answer,
        );
    }
}

/// The same class one level DOWN: a `chargeId` inside an element of a
/// `purchaseOrders` list, which an accept resolves against the charges of the
/// quote it names. The entry reads the same sentence at the element's own path,
/// so the text does not report the depth the member sits at — while a rule about
/// the LIST rather than about a charge that names nothing keeps the
/// unacceptable-value sentence at that same path.
#[tokio::test]
async fn an_unresolvable_charge_inside_a_list_element_states_the_same_sentence() {
    // (what the row varies, how many charges the quote mints, whether the first
    // minted charge is named, the second order, the element the entry names, the
    // reason, the class)
    for (case, charges, orders, element, reason, class) in [
        (
            "a charge id no quote ever minted",
            1usize,
            vec![order("ch-ffffffffffffffff", "PO-77")],
            1usize,
            "INVALID_CHARGE_ID",
            Class::UnresolvedReference,
        ),
        (
            "a charge id of a quote that implies no charge at all",
            0,
            vec![order("ch-0000000000000001", "PO-1")],
            1,
            "INVALID_CHARGE_ID",
            Class::UnresolvedReference,
        ),
        (
            "one resolvable charge and one that is not, the bad one second",
            2,
            vec![order("@minted", "PO-1"), order("ch-notmine", "PO-2")],
            2,
            "INVALID_CHARGE_ID",
            Class::UnresolvedReference,
        ),
        (
            // The same member at the same depth, refused by a rule about the
            // list: one charge carries ONE reference, so two elements naming it
            // ask for two.
            "the same charge named twice in one list",
            1,
            vec![order("@minted", "PO-1"), order("@minted", "PO-2")],
            2,
            "DUPLICATE_CHARGES",
            Class::Unacceptable,
        ),
    ] {
        let router = emulator();
        let (request_id, minted) = quote(&router, "ap-unresolvedcharge", charges).await;
        // `@minted` stands for the charge the quote actually minted, read back
        // from that quote rather than written by hand.
        let orders: Vec<Value> = orders
            .into_iter()
            .map(|mut element| {
                if element["chargeId"] == json!("@minted") {
                    element["chargeId"] = json!(minted[0]);
                }
                element
            })
            .collect();
        let answer = call(
            &router,
            "AcceptAgreementRequest",
            json!({"agreementRequestId": request_id, "purchaseOrders": orders}),
        )
        .await;
        let field = format!("purchaseOrders.{element}.member.chargeId");
        match class {
            Class::UnresolvedReference => {
                assert_unresolvable_reference_fault(case, reason, &field, &answer)
            }
            _ => assert_input_fault(case, reason, &field, &answer),
        }
    }
}

