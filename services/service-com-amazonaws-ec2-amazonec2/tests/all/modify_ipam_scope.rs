// SPDX-License-Identifier: CC0-1.0

//! `ModifyIpamScope`, driven over the wire the service actually speaks (`ec2Query`, through
//! the crate's shared [`super::ec2_query`] harness).
//!
//! The rule under test: a modification that answers 200 is COMMITTED, so every later read of
//! the resource reports the value the modification set — never the value it replaced. Here
//! that is the scope's description and the external authority system it is linked to: each
//! member the request carries replaces the stored one, the member asked to be REMOVED goes
//! back to the default (absent), and a member the request does not carry keeps the value the
//! scope already held. `DescribeIpamScopes` is what reads it back, so the assertion is made
//! against the reader a client would use rather than against the modify's own echo alone.
//!
//! The rows of [`a_modification_is_what_every_later_read_of_the_scope_reports`] differ along
//! what the rule generalises over — which member the request carries, whether the scope had a
//! value for it already, what kind of text the value is, and whether the request sets a
//! member or removes it — so no single row is the specification. The refusal tables pin the
//! other half of the rule: a modification that is refused changes nothing at all.

use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS, INCORRECT_STATE,
    INVALID_IPAM_SCOPE_ID_MALFORMED, INVALID_IPAM_SCOPE_ID_NOT_FOUND,
    INVALID_PARAMETER_COMBINATION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for, which is therefore each IPAM's home Region.
const REGION: &str = "us-west-2";

/// The one external authority type the model offers
/// (`smithy_query(query="shape",shape="IpamScopeExternalAuthorityType")` is `infoblox`).
const EXTERNAL_AUTHORITY_TYPE: &str = "infoblox";

/// What a scope reports about itself for the two members a modification can change: its
/// description, and the external authority it is linked by as `(type, identifier)`.
type ReportedScope = (Option<String>, Option<(String, String)>);

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Pin that `action` refused `params` with HTTP `status`, `code` and `message`.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    status: u16,
    code: &str,
    message: Option<&str>,
) {
    let (answered, body) = invoke(router, action, params).await;
    assert_eq!(
        answered, status,
        "case {case:?}: expected HTTP {status} from {action}, got {answered} with body {body}"
    );
    let answered_code = error_code(&body);
    assert_eq!(
        answered_code, code,
        "case {case:?}: expected the code {code} from {action}, got {answered_code:?} in {body}"
    );
    if let Some(message) = message {
        let answered_message = error_message(&body);
        assert_eq!(
            answered_message, message,
            "case {case:?}: expected the message {message:?} from {action}, got \
             {answered_message:?}"
        );
    }
}

/// The scope a response carries, read for the two members a modification can change.
fn reported_scope(case: &str, body: &str) -> ReportedScope {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamScopeId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no IPAM scope (no <ipamScopeId>) in {body}"));
    let authority = child(node, "externalAuthorityConfiguration").map(|configuration| {
        (
            child_text(configuration, "type").unwrap_or_default(),
            child_text(configuration, "externalResourceIdentifier").unwrap_or_default(),
        )
    });
    (child_text(node, "description"), authority)
}

/// The id of the scope a response carries.
fn reported_scope_id(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamScopeId").is_some())
        .and_then(|node| child_text(node, "ipamScopeId"))
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamScopeId> in {body}"))
}

/// The form fields of an `ExternalAuthorityConfiguration` naming `identifier`.
fn authority_params(prefix: &str, identifier: &str) -> Vec<Param> {
    vec![
        param(&format!("{prefix}.Type"), EXTERNAL_AUTHORITY_TYPE),
        param(
            &format!("{prefix}.ExternalResourceIdentifier"),
            identifier,
        ),
    ]
}

/// Create one IPAM and one scope in it, as the request's own members describe the scope, and
/// answer the scope's id.
async fn created_scope(
    case: &str,
    router: &MultiServiceRouter,
    description: Option<&str>,
    authority: Option<&str>,
) -> String {
    let ipam = ok(case, router, "CreateIpam", &[]).await;
    let ipam_id = document(case, &ipam)
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamId").is_some())
        .and_then(|node| child_text(node, "ipamId"))
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamId> in {ipam}"));

    let mut params = vec![param("IpamId", &ipam_id)];
    if let Some(description) = description {
        params.push(param("Description", description));
    }
    if let Some(identifier) = authority {
        params.extend(authority_params("ExternalAuthorityConfiguration", identifier));
    }
    let body = ok(case, router, "CreateIpamScope", &params).await;
    reported_scope_id(case, &body)
}

/// `DescribeIpamScopes` narrowed to one id, as the one scope it reports.
async fn described(case: &str, router: &MultiServiceRouter, scope_id: &str) -> ReportedScope {
    let body = ok(
        case,
        router,
        "DescribeIpamScopes",
        &[param("IpamScopeId.1", scope_id)],
    )
    .await;
    reported_scope(case, &body)
}

/// One row of the modification table: what the scope was created with, the members the modify
/// request carries, and what every later read of the scope must then report.
struct ModifyCase {
    case: &'static str,
    created_description: Option<&'static str>,
    created_authority: Option<&'static str>,
    /// The `Description` the request carries, if any.
    description: Option<&'static str>,
    /// The external resource identifier the request links the scope by, if any.
    authority: Option<&'static str>,
    /// Whether the request asks for the external authority link to be removed.
    remove_authority: bool,
    expected: (Option<&'static str>, Option<&'static str>),
}

/// RULE: a `ModifyIpamScope` that answers 200 has COMMITTED its change, so the scope the
/// modify answers and the scope `DescribeIpamScopes` reports afterwards both carry the value
/// the request set — a member it did not carry keeps the stored value, and a member it removed
/// is back to the default.
#[tokio::test]
async fn a_modification_is_what_every_later_read_of_the_scope_reports() {
    let cases = [
        ModifyCase {
            case: "a description on a scope created without one",
            created_description: None,
            created_authority: None,
            description: Some("probe scope"),
            authority: None,
            remove_authority: false,
            expected: (Some("probe scope"), None),
        },
        ModifyCase {
            case: "a description replacing the one the create stored",
            created_description: Some("probe scope"),
            created_authority: None,
            description: Some("probe scope, widened"),
            authority: None,
            remove_authority: false,
            expected: (Some("probe scope, widened"), None),
        },
        ModifyCase {
            case: "a single-character description",
            created_description: Some("probe scope"),
            created_authority: None,
            description: Some("x"),
            authority: None,
            remove_authority: false,
            expected: (Some("x"), None),
        },
        ModifyCase {
            case: "a description of multibyte characters",
            created_description: Some("probe scope"),
            created_authority: None,
            description: Some("réseau privé — 内部"),
            authority: None,
            remove_authority: false,
            expected: (Some("réseau privé — 内部"), None),
        },
        ModifyCase {
            case: "an external authority on a scope linked to none",
            created_description: Some("probe scope"),
            created_authority: None,
            description: None,
            authority: Some("1.identity.account.realm.42"),
            remove_authority: false,
            expected: (Some("probe scope"), Some("1.identity.account.realm.42")),
        },
        ModifyCase {
            case: "an external authority replacing the one the create stored",
            created_description: None,
            created_authority: Some("1.identity.account.realm.42"),
            description: None,
            authority: Some("2.identity.account.realm.43"),
            remove_authority: false,
            expected: (None, Some("2.identity.account.realm.43")),
        },
        ModifyCase {
            case: "the external authority removed",
            created_description: Some("probe scope"),
            created_authority: Some("1.identity.account.realm.42"),
            description: None,
            authority: None,
            remove_authority: true,
            expected: (Some("probe scope"), None),
        },
        ModifyCase {
            case: "a description and an external authority in one request",
            created_description: Some("probe scope"),
            created_authority: None,
            description: Some("probe scope, linked"),
            authority: Some("3.identity.account.realm.44"),
            remove_authority: false,
            expected: (
                Some("probe scope, linked"),
                Some("3.identity.account.realm.44"),
            ),
        },
        ModifyCase {
            case: "a description set while the external authority is removed",
            created_description: Some("probe scope"),
            created_authority: Some("1.identity.account.realm.42"),
            description: Some("probe scope, unlinked"),
            authority: None,
            remove_authority: true,
            expected: (Some("probe scope, unlinked"), None),
        },
    ];

    for row in cases {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let scope_id = created_scope(
            case,
            &router,
            row.created_description,
            row.created_authority,
        )
        .await;

        let mut params = vec![param("IpamScopeId", &scope_id)];
        if let Some(description) = row.description {
            params.push(param("Description", description));
        }
        if let Some(identifier) = row.authority {
            params.extend(authority_params("ExternalAuthorityConfiguration", identifier));
        }
        if row.remove_authority {
            params.push(param("RemoveExternalAuthorityConfiguration", "true"));
        }

        let body = ok(case, &router, "ModifyIpamScope", &params).await;
        let expected: ReportedScope = (
            row.expected.0.map(str::to_string),
            row.expected
                .1
                .map(|identifier| (EXTERNAL_AUTHORITY_TYPE.to_string(), identifier.to_string())),
        );
        let answered = reported_scope(case, &body);
        assert_eq!(
            answered, expected,
            "case {case:?}: the modify must answer the scope it stored, expected \
             {expected:?}, got {answered:?}"
        );

        let read_back = described(case, &router, &scope_id).await;
        assert_eq!(
            read_back, expected,
            "case {case:?}: the read after the modify must report the modified scope, \
             expected {expected:?}, got {read_back:?}"
        );
    }
}

/// RULE, the other half: a `ModifyIpamScope` that is REFUSED commits nothing, so the scope
/// reads back exactly as it did before the refused call.
///
/// The rows differ along what the request is refused FOR: an id of no scope of the account, a
/// string that is no IPAM scope id at all, a request that both sets and removes the one
/// external authority link, and a probe that asks whether the call would have worked.
#[tokio::test]
async fn a_refused_modification_leaves_the_scope_as_it_was() {
    /// `(case, extra request fields, status, code, message)`.
    type Row = (&'static str, Vec<Param>, u16, &'static str, Option<String>);

    let unknown = "ipam-scope-00000000000000000";
    let rows: Vec<Row> = vec![
        (
            "an id this account holds no scope for",
            vec![param("IpamScopeId", unknown)],
            400,
            INVALID_IPAM_SCOPE_ID_NOT_FOUND,
            Some(format!("The IPAM scope ({unknown}) does not exist.")),
        ),
        (
            "a string that is no IPAM scope id at all",
            vec![param("IpamScopeId", "the-private-scope")],
            400,
            INVALID_IPAM_SCOPE_ID_MALFORMED,
            None,
        ),
        (
            "an external authority both set and removed",
            vec![
                param("RemoveExternalAuthorityConfiguration", "true"),
                param(
                    "ExternalAuthorityConfiguration.Type",
                    EXTERNAL_AUTHORITY_TYPE,
                ),
                param(
                    "ExternalAuthorityConfiguration.ExternalResourceIdentifier",
                    "9.identity.account.realm.9",
                ),
            ],
            400,
            INVALID_PARAMETER_COMBINATION,
            None,
        ),
        (
            "a probe of a modification that would have worked",
            vec![
                param("Description", "never stored"),
                param("DryRun", "true"),
            ],
            DRY_RUN_OPERATION_STATUS,
            DRY_RUN_OPERATION,
            Some(DRY_RUN_OPERATION_MESSAGE.to_string()),
        ),
    ];

    for (case, extra, status, code, message) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let scope_id = created_scope(
            case,
            &router,
            Some("probe scope"),
            Some("1.identity.account.realm.42"),
        )
        .await;
        let before = described(case, &router, &scope_id).await;

        // The row either names its own scope id (the unresolvable ones) or acts on the scope
        // the account holds, so every row is one call of the same operation.
        let names_scope = extra
            .iter()
            .any(|(name, _)| name.as_str() == "IpamScopeId");
        let mut params = if names_scope {
            Vec::new()
        } else {
            vec![param("IpamScopeId", &scope_id)]
        };
        params.extend(extra);
        refused(
            case,
            &router,
            "ModifyIpamScope",
            &params,
            status,
            code,
            message.as_deref(),
        )
        .await;

        let after = described(case, &router, &scope_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused modification must leave the scope as it was, expected \
             {before:?}, got {after:?}"
        );
    }
}

/// RULE: a scope whose deletion has been asked for has nothing left to configure, so a
/// modification of it is refused rather than written.
///
/// The delete leaves the scope `delete-in-progress` and the clock owes its REMOVAL a span
/// later, so the refusal is `IncorrectState` while the scope is still reported and this
/// resource's `NotFound` once the removal has fallen due. Both are a refusal of the same rule,
/// and which one answers depends only on where the clock stands, so the assertion admits
/// either and nothing is waited for.
#[tokio::test]
async fn a_scope_whose_deletion_is_under_way_cannot_be_modified() {
    let case = "a scope a delete has acted on";
    let (router, _engine) = super::ec2_query::emulator();
    let scope_id = created_scope(case, &router, Some("probe scope"), None).await;

    ok(
        case,
        &router,
        "DeleteIpamScope",
        &[param("IpamScopeId", &scope_id)],
    )
    .await;

    let (status, body) = invoke(
        &router,
        "ModifyIpamScope",
        &[
            param("IpamScopeId", &scope_id),
            param("Description", "never stored"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from ModifyIpamScope, got {status} with body {body}"
    );
    let code = error_code(&body);
    assert!(
        code == INCORRECT_STATE || code == INVALID_IPAM_SCOPE_ID_NOT_FOUND,
        "case {case:?}: expected {INCORRECT_STATE} or {INVALID_IPAM_SCOPE_ID_NOT_FOUND} from \
         ModifyIpamScope, got {code:?} in {body}"
    );
}
