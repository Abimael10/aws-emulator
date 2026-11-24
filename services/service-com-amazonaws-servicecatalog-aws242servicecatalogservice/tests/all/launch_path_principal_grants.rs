// SPDX-License-Identifier: CC0-1.0

//! The PRINCIPAL half of a launch path, over the wire, with the account's identity records
//! served by the IAM service — and sessions of a role by STS — mounted beside Service
//! Catalog on the ONE shared engine.
//!
//! The rule under test: "a path is dependent on a specific product, porfolio, and
//! principal" (`ListLaunchPaths`), so a portfolio is a launch path for a request only when
//! it carries the product AND grants the CALLING principal access to the portfolio —
//! "when provisioning a product that's been added to a portfolio, you must grant your
//! user, group, or role access to the portfolio". Until both halves are stored, the
//! product is not launchable: the path resolves to nothing, which `ListLaunchPaths`
//! refuses with the modelled 400 `ResourceNotFoundException` rather than with an empty
//! page. The gate is the state the store holds NOW, so revoking the grant takes the path
//! away again, and it is per PRINCIPAL, so one caller's path is not another's.
//!
//! Each case is a ROW of a table spanning what the rule generalises over: which
//! portfolios carry the product and which of them grant the caller, the `PrincipalType`
//! and ARN FORM an association names the caller by (exact, wildcarded, account-less, the
//! wrong name, the wrong entity type), the kind of principal the grant names (a user, a
//! group that user belongs to, the role behind a session), and the order the two halves
//! are stored and removed in.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_signed_by, create_portfolio, create_product, emulator, error_code,
    grant_principal_on_portfolio, harness_principal_arn, seed_launch_path, ACCOUNT_ID,
    DEFAULT_REGION, HARNESS_ACCESS_KEY_ID,
};

/// The awsQuery API version an IAM request carries
/// (`com.amazonaws.iam#AWSIdentityManagementV20100508`, version 2010-05-08).
const IAM_API_VERSION: &str = "2010-05-08";

/// The awsQuery API version an STS request carries
/// (`com.amazonaws.sts#AWSSecurityTokenServiceV20110615`, version 2011-06-15).
const STS_API_VERSION: &str = "2011-06-15";

/// The modelled refusal of a product that resolves no launch path for the caller
/// (`ListLaunchPaths` errors: `InvalidParametersException` | `ResourceNotFoundException`,
/// both HTTP 400).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A trust document that lets this account's own principals assume the role it is attached
/// to, which is what a client that hands a session to its catalog clients writes.
fn account_trust_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT_ID}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// One emulator carrying Service Catalog, the service that holds the account's principals
/// and the service that vends sessions of a role — all on the ONE shared engine, exactly
/// as `core/router/src/lib.rs` mounts them.
///
/// WHO a request speaks for is a question about the account's identities, which IAM holds
/// and STS reports, so a suite that drives two principals must have both there.
fn emulator_with_sessions() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The harness signs nothing, so authentication is off for this router only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_servicecatalog_aws242servicecatalogservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// Drive one awsQuery request (IAM, STS) through the same router, signed by
/// `access_key_id`, as a client's own SDK sends it: a form body answered with XML.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut form = ::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", version);
    for (name, value) in params {
        form.append_pair(name, value);
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        // An unsigned request is routed by the endpoint host's first label, which is that
        // service's signing name.
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{DEFAULT_REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(::bytes::Bytes::from(form.finish()))
        .expect("a well-formed query-protocol request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, read)
}

/// The text of the first `<tag>` of an XML body.
fn xml_text(case: &str, body: &str, tag: &str) -> String {
    body.split(&format!("<{tag}>"))
        .nth(1)
        .and_then(|rest| rest.split(&format!("</{tag}>")).next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: no <{tag}> in {body}"))
}

/// The path ids `ListLaunchPaths` reports for `product_id` to the caller signing with
/// `access_key_id`, or `None` when the product resolves no path for that caller — which is
/// the modelled 400 `ResourceNotFoundException` and never an empty page.
///
/// A set, because the ORDER a page walks is its own rule (the suite `launch_paths.rs`
/// pins it); what this suite is about is WHICH paths a caller gets.
async fn listed_paths(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    product_id: &str,
) -> Option<PathSet> {
    let (status, body) = call_signed_by(
        router,
        access_key_id,
        DEFAULT_REGION,
        "ListLaunchPaths",
        &json!({"ProductId": product_id}),
    )
    .await;
    if status == 400 {
        assert_eq!(
            error_code(&body),
            NOT_FOUND,
            "case {case:?}: a product with no launch path for the caller is the modelled \
             not-found, got {body}"
        );
        return None;
    }
    assert_eq!(
        status, 200,
        "case {case:?}: expected the paths to be listed, got {status} with {body}"
    );
    let summaries = body["LaunchPathSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no LaunchPathSummaries in {body}"))
        .iter()
        .map(|summary| {
            (
                summary["Id"].as_str().unwrap_or_default().to_string(),
                summary["Name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        !summaries.is_empty(),
        "case {case:?}: a served page carries at least one path, got {body}"
    );
    Some(summaries)
}

/// The launch paths of one response, as `(path id, path name)` pairs.
type PathSet = std::collections::BTreeSet<(String, String)>;

/// What `DescribeProduct` — the other reader of the same launch paths — reports for
/// `product_id`: its `LaunchPaths` and its summary's `HasDefaultPath`, or `None` when the
/// end-user view reaches no such product.
async fn described_paths(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    product_id: &str,
) -> Option<(PathSet, bool)> {
    let (status, body) = call_signed_by(
        router,
        access_key_id,
        DEFAULT_REGION,
        "DescribeProduct",
        &json!({"Id": product_id}),
    )
    .await;
    if status == 400 {
        assert_eq!(
            error_code(&body),
            NOT_FOUND,
            "case {case:?}: the end-user view refuses a product it cannot reach with the \
             modelled not-found, got {body}"
        );
        return None;
    }
    assert_eq!(
        status, 200,
        "case {case:?}: expected the product to be described, got {status} with {body}"
    );
    let paths = body["LaunchPaths"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no LaunchPaths in {body}"))
        .iter()
        .map(|path| {
            (
                path["Id"].as_str().unwrap_or_default().to_string(),
                path["Name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let has_default_path = body
        .pointer("/ProductViewSummary/HasDefaultPath")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("case {case:?}: no HasDefaultPath in {body}"));
    Some((paths, has_default_path))
}

/// Remove the grant of `principal_arn` on `portfolio_id`.
async fn revoke_principal_on_portfolio(
    router: &MultiServiceRouter,
    portfolio_id: &str,
    principal_arn: &str,
) {
    let (status, body) = call(
        router,
        "DisassociatePrincipalFromPortfolio",
        &json!({"PortfolioId": portfolio_id, "PrincipalARN": principal_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DisassociatePrincipalFromPortfolio({portfolio_id}, {principal_arn}): expected 200, got \
         {status} with {body}"
    );
}

/// Neither half of the triple is a path on its own, and the gate is LIVE state: a product
/// in a portfolio that grants the caller nothing is not launchable, granting the caller
/// makes exactly that portfolio its path, and revoking the grant takes it away again.
#[tokio::test]
async fn a_path_needs_the_product_and_the_principal_association_and_follows_both() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "granting-portfolio", "acme", &[], "portfolio")
        .await;
    let product_id = create_product(&router, "gated-product", "product").await;
    let caller = harness_principal_arn();

    // The product association ALONE, which is the half every administrator makes first.
    let (status, body) = call(
        &router,
        "AssociateProductWithPortfolio",
        &json!({"ProductId": product_id, "PortfolioId": portfolio_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the product association must be served, got {status} with {body}"
    );

    // Rows: the steps of the gate, each stating whether the grant is held AFTER the step's
    // own call and what the product's paths must then be. The grant is made, removed and
    // made again, so nothing latches: the answer follows the stored associations.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        /// Nothing is done; the state is whatever the previous row left.
        Nothing,
        /// Grant the caller access to the portfolio.
        Grant,
        /// Remove that grant.
        Revoke,
    }
    let steps: [(&str, Step, bool); 5] = [
        ("the product association alone", Step::Nothing, false),
        ("the caller granted access", Step::Grant, true),
        ("the grant still held", Step::Nothing, true),
        ("the grant revoked", Step::Revoke, false),
        ("the grant made again", Step::Grant, true),
    ];

    for (index, (case, step, launchable)) in steps.into_iter().enumerate() {
        match step {
            Step::Nothing => {}
            Step::Grant => {
                grant_principal_on_portfolio(&router, &portfolio_id, &caller, "IAM").await
            }
            Step::Revoke => revoke_principal_on_portfolio(&router, &portfolio_id, &caller).await,
        }

        let listed = listed_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let expected = launchable.then(|| {
            PathSet::from([(portfolio_id.clone(), "granting-portfolio".to_string())])
        });
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the paths {expected:?}, got {listed:?}"
        );

        // The end-user read of the product resolves the SAME paths, so the two responses
        // cannot contradict each other: a product with no path for this caller is outside
        // the catalog it sees, and one with a single path has that path as its default.
        let described = described_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let wanted = expected
            .clone()
            .map(|paths| (paths, true));
        assert_eq!(
            described, wanted,
            "case {case:?}: expected DescribeProduct to report {wanted:?}, got {described:?}"
        );

        // A launch goes through a path, so a product with none cannot be provisioned
        // through one either.
        let (status, provisioned) = call(
            &router,
            "ProvisionProduct",
            &json!({
                "ProductId": product_id,
                "ProvisioningArtifactId": "pa-nonexistent",
                "ProvisionedProductName": "gated-launch",
                "ProvisionToken": format!("gated-launch-{index}"),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: this launch names no stored version, so it is refused either \
             way, got {status} with {provisioned}"
        );
        if !launchable {
            assert_eq!(
                error_code(&provisioned),
                NOT_FOUND,
                "case {case:?}: a product with no launch path for the caller cannot be \
                 provisioned, got {provisioned}"
            );
        }
    }
}

/// Only the portfolios that grant the caller are the product's paths: a product carried by
/// several portfolios reports exactly the granting ones, and `HasDefaultPath` follows that
/// set rather than the portfolios the product happens to sit in.
#[tokio::test]
async fn only_the_granting_portfolios_of_a_product_are_its_paths() {
    // The display names of the portfolios every row seeds; a row picks which of them carry
    // the product and which of those grant the caller.
    const PORTFOLIOS: [&str; 3] = ["alpha-portfolio", "beta-portfolio", "gamma-portfolio"];

    // Rows differ along the COUNT on each side of the intersection: no grant, a grant on a
    // portfolio that does not carry the product, one of several, all of them, and a grant
    // that leaves exactly one path (the default-path boundary).
    struct Row {
        case: &'static str,
        /// The portfolios that carry the product, by index into `PORTFOLIOS`.
        carrying: &'static [usize],
        /// The portfolios that grant the caller access, by the same index.
        granting: &'static [usize],
        /// The portfolios that must therefore be the product's paths.
        expected: &'static [usize],
    }
    let rows: [Row; 6] = [
        Row {
            case: "carried by one, granted by none",
            carrying: &[0],
            granting: &[],
            expected: &[],
        },
        Row {
            case: "granted on a portfolio that does not carry it",
            carrying: &[0],
            granting: &[1],
            expected: &[],
        },
        Row {
            case: "carried and granted by the same one",
            carrying: &[0],
            granting: &[0],
            expected: &[0],
        },
        Row {
            case: "carried by three, granted by one",
            carrying: &[0, 1, 2],
            granting: &[1],
            expected: &[1],
        },
        Row {
            case: "carried by three, granted by two",
            carrying: &[0, 1, 2],
            granting: &[0, 2],
            expected: &[0, 2],
        },
        Row {
            case: "carried by three, granted by all",
            carrying: &[0, 1, 2],
            granting: &[0, 1, 2],
            expected: &[0, 1, 2],
        },
    ];

    for Row {
        case,
        carrying,
        granting,
        expected,
    } in rows
    {
        let router = emulator();
        let caller = harness_principal_arn();
        let mut portfolio_ids = Vec::new();
        for (index, display_name) in PORTFOLIOS.iter().enumerate() {
            portfolio_ids
                .push(create_portfolio(&router, display_name, "acme", &[], &format!("p{index}"))
                    .await);
        }
        let product_id = create_product(&router, "shared-product", "product").await;
        for index in carrying {
            let (status, body) = call(
                &router,
                "AssociateProductWithPortfolio",
                &json!({"ProductId": product_id, "PortfolioId": portfolio_ids[*index]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the product association must be served, got {status} with {body}"
            );
        }
        for index in granting {
            grant_principal_on_portfolio(&router, &portfolio_ids[*index], &caller, "IAM").await;
        }

        let listed = listed_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let wanted: Option<PathSet> = (!expected.is_empty()).then(|| {
            expected
                .iter()
                .map(|index| (portfolio_ids[*index].clone(), PORTFOLIOS[*index].to_string()))
                .collect()
        });
        assert_eq!(
            listed, wanted,
            "case {case:?}: expected the paths {wanted:?}, got {listed:?}"
        );

        let described = described_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let wanted_describe = wanted
            .clone()
            .map(|paths| (paths, expected.len() == 1));
        assert_eq!(
            described, wanted_describe,
            "case {case:?}: expected DescribeProduct to report {wanted_describe:?}, got \
             {described:?}"
        );
    }
}

/// A grant names the caller by a fully defined ARN or by a PATTERN: an `IAM` association
/// names the one principal its ARN is, while an `IAM_PATTERN` — which may omit the account
/// and carry `*`/`?` — names every principal it matches.
#[tokio::test]
async fn a_grant_names_the_caller_by_its_arn_or_by_a_pattern() {
    // The name of the principal the harness's own credential speaks for, which the rows
    // below write their patterns over rather than spelling a literal.
    let user = ::service_common::caller_principal::AMBIENT_USER_NAME;
    let caller = harness_principal_arn();

    // Rows differ along the TYPE, the wildcard form, the account field and the entity type
    // the association names: each states whether that association names this caller.
    struct Grant {
        case: &'static str,
        /// The `PrincipalARN` the association is made with.
        principal_arn: String,
        /// The `PrincipalType` it is made under.
        principal_type: &'static str,
        /// Whether that association names this caller, and so makes the product
        /// launchable for it.
        grants: bool,
    }
    let rows: Vec<Grant> = vec![
        Grant {
            case: "the caller's own ARN",
            principal_arn: caller.clone(),
            principal_type: "IAM",
            grants: true,
        },
        Grant {
            case: "another user of the account",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:user/somebody-else"),
            principal_type: "IAM",
            grants: false,
        },
        Grant {
            case: "a role, which the caller is not",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:role/{user}"),
            principal_type: "IAM",
            grants: false,
        },
        Grant {
            case: "the caller's own ARN as a pattern",
            principal_arn: caller.clone(),
            principal_type: "IAM_PATTERN",
            grants: true,
        },
        Grant {
            case: "every user of the account",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:user/*"),
            principal_type: "IAM_PATTERN",
            grants: true,
        },
        Grant {
            case: "a prefix of the caller's name",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:user/{}*", &user[..3]),
            principal_type: "IAM_PATTERN",
            grants: true,
        },
        Grant {
            case: "one character of the caller's name",
            principal_arn: format!(
                "arn:aws:iam::{ACCOUNT_ID}:user/{}?",
                &user[..user.len() - 1]
            ),
            principal_type: "IAM_PATTERN",
            grants: true,
        },
        Grant {
            case: "an account-less pattern, which reads as this account",
            principal_arn: format!("arn:aws:iam:::user/{user}"),
            principal_type: "IAM_PATTERN",
            grants: true,
        },
        Grant {
            case: "a pattern over another name",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:user/somebody-*"),
            principal_type: "IAM_PATTERN",
            grants: false,
        },
        Grant {
            case: "a pattern over every role of the account",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:role/*"),
            principal_type: "IAM_PATTERN",
            grants: false,
        },
        Grant {
            case: "a pattern one character short of the caller's name",
            principal_arn: format!("arn:aws:iam::{ACCOUNT_ID}:user/{}?", &user[..3]),
            principal_type: "IAM_PATTERN",
            grants: false,
        },
    ];

    for Grant {
        case,
        principal_arn,
        principal_type,
        grants,
    } in rows
    {
        let router = emulator();
        let portfolio_id = create_portfolio(&router, "pattern-portfolio", "acme", &[], "p").await;
        let product_id = create_product(&router, "pattern-product", "product").await;
        let (status, body) = call(
            &router,
            "AssociateProductWithPortfolio",
            &json!({"ProductId": product_id, "PortfolioId": portfolio_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the product association must be served, got {status} with {body}"
        );
        grant_principal_on_portfolio(&router, &portfolio_id, &principal_arn, principal_type).await;

        let listed = listed_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let wanted = grants
            .then(|| PathSet::from([(portfolio_id.clone(), "pattern-portfolio".to_string())]));
        assert_eq!(
            listed, wanted,
            "case {case:?}: {principal_arn} as {principal_type} must make the product \
             launchable = {grants}, got {listed:?}"
        );
    }
}

/// A grant may name a GROUP — "the principal (user, role, or group)" — and a group's grant
/// is in force on the users the account's identity records hold in it: the caller gets the
/// path while it is a member, and loses it when the membership goes.
#[tokio::test]
async fn a_grant_to_a_group_reaches_the_users_it_holds() {
    let router = emulator_with_sessions();
    let group = "catalog-launchers";
    let user = ::service_common::caller_principal::AMBIENT_USER_NAME;
    let (status, created) = query(
        &router,
        HARNESS_ACCESS_KEY_ID,
        "iam",
        IAM_API_VERSION,
        "CreateGroup",
        &[("GroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "seed iam:CreateGroup({group}): expected 200, got {status} with {created}"
    );
    let group_arn = xml_text("the group", &created, "Arn");

    let portfolio_id = create_portfolio(&router, "group-portfolio", "acme", &[], "p").await;
    let product_id = create_product(&router, "group-product", "product").await;
    let (status, body) = call(
        &router,
        "AssociateProductWithPortfolio",
        &json!({"ProductId": product_id, "PortfolioId": portfolio_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the product association must be served, got {status} with {body}"
    );
    grant_principal_on_portfolio(&router, &portfolio_id, &group_arn, "IAM").await;

    // Rows: the membership before the read, and whether the product is then launchable.
    // The group's grant never changes, so what moves is the MEMBERSHIP alone.
    struct Membership {
        case: &'static str,
        /// The IAM action that changes the membership before this row reads, if any.
        action: Option<&'static str>,
        /// Whether the product is then launchable for the caller.
        launchable: bool,
    }
    let rows: [Membership; 3] = [
        Membership {
            case: "the group holds nobody",
            action: None,
            launchable: false,
        },
        Membership {
            case: "the caller added to the group",
            action: Some("AddUserToGroup"),
            launchable: true,
        },
        Membership {
            case: "the caller removed from the group",
            action: Some("RemoveUserFromGroup"),
            launchable: false,
        },
    ];
    for Membership {
        case,
        action,
        launchable,
    } in rows
    {
        if let Some(action) = action {
            let (status, body) = query(
                &router,
                HARNESS_ACCESS_KEY_ID,
                "iam",
                IAM_API_VERSION,
                action,
                &[("GroupName", group), ("UserName", user)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: iam:{action} must be served, got {status} with {body}"
            );
        }
        let listed = listed_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        let wanted = launchable
            .then(|| PathSet::from([(portfolio_id.clone(), "group-portfolio".to_string())]));
        assert_eq!(
            listed, wanted,
            "case {case:?}: expected the paths {wanted:?}, got {listed:?}"
        );
    }
}

/// A request signed with a temporary credential speaks for a SESSION of a role, and the
/// permissions in force on it are the role's: a grant naming the role is a path for every
/// session of that role, for nobody else, and only while the grant is held.
#[tokio::test]
async fn a_grant_to_a_role_is_a_path_for_the_sessions_of_that_role() {
    let router = emulator_with_sessions();
    let role = "catalog-launcher";
    let (status, created) = query(
        &router,
        HARNESS_ACCESS_KEY_ID,
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[
            ("RoleName", role),
            ("AssumeRolePolicyDocument", &account_trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "seed iam:CreateRole({role}): expected 200, got {status} with {created}"
    );
    let role_arn = xml_text("the role", &created, "Arn");

    let (status, assumed) = query(
        &router,
        HARNESS_ACCESS_KEY_ID,
        "sts",
        STS_API_VERSION,
        "AssumeRole",
        &[
            ("RoleArn", &role_arn),
            ("RoleSessionName", "launch-session"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "seed sts:AssumeRole({role_arn}): expected 200, got {status} with {assumed}"
    );
    let session_key = xml_text("the session", &assumed, "AccessKeyId");

    let portfolio_id = create_portfolio(&router, "session-portfolio", "acme", &[], "p").await;
    let product_id = create_product(&router, "session-product", "product").await;
    // The product association, plus a grant for the HARNESS's own principal: the session
    // is a different principal, so that grant must not make the product launchable for it.
    seed_launch_path(&router, &product_id, &portfolio_id).await;

    // Rows: what is granted on the portfolio before the session reads, and whether the
    // session can then launch. The role's grant is made and removed, so the session's
    // answer follows the grant on its ROLE and not on any other principal.
    struct Held {
        case: &'static str,
        /// The change made to the ROLE's grant before this row reads, if any.
        change: Option<Change>,
        /// Whether the session can then launch the product.
        launchable: bool,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Change {
        /// Grant the role access to the portfolio.
        Grant,
        /// Remove that grant.
        Revoke,
    }
    let rows: [Held; 4] = [
        Held {
            case: "only the harness principal is granted",
            change: None,
            launchable: false,
        },
        Held {
            case: "the role behind the session granted",
            change: Some(Change::Grant),
            launchable: true,
        },
        Held {
            case: "the grant still held",
            change: None,
            launchable: true,
        },
        Held {
            case: "the role's grant revoked",
            change: Some(Change::Revoke),
            launchable: false,
        },
    ];
    for Held {
        case,
        change,
        launchable,
    } in rows
    {
        match change {
            Some(Change::Grant) => {
                grant_principal_on_portfolio(&router, &portfolio_id, &role_arn, "IAM").await
            }
            Some(Change::Revoke) => {
                revoke_principal_on_portfolio(&router, &portfolio_id, &role_arn).await
            }
            None => {}
        }

        let listed = listed_paths(&router, case, &session_key, &product_id).await;
        let wanted = launchable
            .then(|| PathSet::from([(portfolio_id.clone(), "session-portfolio".to_string())]));
        assert_eq!(
            listed, wanted,
            "case {case:?}: expected the session's paths {wanted:?}, got {listed:?}"
        );

        // The harness's own principal keeps its own path throughout: the gate is per
        // principal, so one caller's grant is never read as another's.
        let harness_sees = listed_paths(&router, case, HARNESS_ACCESS_KEY_ID, &product_id).await;
        assert_eq!(
            harness_sees,
            Some(PathSet::from([(
                portfolio_id.clone(),
                "session-portfolio".to_string()
            )])),
            "case {case:?}: the granted harness principal must keep its path, got \
             {harness_sees:?}"
        );
    }
}
