// SPDX-License-Identifier: CC0-1.0

//! What a `ValidationExceptionField` entry SAYS about the member it names.
//!
//! The rule under test: `ValidationExceptionField$message` is the fixed text of
//! the class the member failed in, reproduced exactly — one text for a member
//! that carried a value this service does not accept, one for a member that was
//! absent and is required of every request, and one for a member that was absent
//! and became required only through the `intent` the request stated. The variable
//! part of the entry is `name`, the rejected member's dotted request path taken
//! from the request. So the text depends on WHAT was wrong with the member and
//! not on which member, which operation, which depth it sits at, or whether the
//! modelled constraint walk or a rule inside a handler is what refused it.
//!
//! Each row differs from the others in what the rule generalises over: the class
//! of the failure, the constraint missed or the rule broken, the member, the
//! depth (top level, inside a structure, a member of a list element), the
//! operation, the reason, and whether the boundary walk or a rule inside a
//! handler refused the request — including the SAME member refused both ways, so
//! the sentence is shown not to report where the refusal was raised. The
//! recorded cases — an unacceptable party type on `ListAgreementPaymentRequests`
//! and on `ListAgreementCancellationRequests`, a `SearchAgreements` filter
//! refusal, a present-but-unacceptable member on
//! `SendAgreementPaymentRequest`, the absent per-element agreement identifier on
//! `UpdatePurchaseOrders`, and the intent-conditional proposal identifier on
//! `CreateAgreementRequest` — are rows among them.
//!
//! The harness (`emulator`, `call`, `term`, `agreement_type_filter`) is the one
//! `agreement_lifecycle` already owns.

use serde_json::{json, Value};

use crate::agreement_lifecycle::{agreement_type_filter, call, emulator, term, Answer};

/// The fixed text for a member that carried a value this service does not
/// accept, whichever part of the service refused it: a constraint the model
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
    /// The value is well formed, and a rule of the service does not accept it.
    Unacceptable,
    /// The member was absent, and every request has to carry it.
    Absent,
    /// The member was absent, and the request's `intent` is what required it.
    AbsentForIntent,
}

impl Class {
    fn text(self) -> &'static str {
        match self {
            // ONE text for both: a member that carried an unacceptable value is
            // one class of failure however the service came to refuse it.
            Self::InvalidConstraint | Self::Unacceptable => FIELD_INVALID,
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
            class: Class::Unacceptable,
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

/// The texts are each other's contrast: ONE member, refused in three ways, reads
/// in the text of the CLASS it failed in — the same invalid-member sentence
/// whether a constraint the model declares or a rule of the service refused the
/// value, and the absent-member sentence when it carried no value at all.
#[tokio::test]
async fn one_member_refused_in_three_ways_reads_the_text_of_its_class() {
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
    // Well formed, and naming no agreement this account can charge under: a rule
    // of the service, so a handler refuses it.
    let unacceptable = call(
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
        text_of(&absent),
    ];
    assert_eq!(
        texts[0], texts[1],
        "case {member:?}: a value a rule of the service refuses states the same sentence as a \
         value that misses a modelled constraint, got {texts:?}"
    );
    assert_ne!(
        texts[1], texts[2],
        "case {member:?}: an unacceptable value and an absent member are different classes and \
         keep different sentences, got {texts:?}"
    );
}

