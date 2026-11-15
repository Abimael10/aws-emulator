// SPDX-License-Identifier: CC0-1.0

//! A theme's resource permissions: granting, revoking and reading them back.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the route, the status, and a body the handler computed from the shared store.
//!
//! The rule under test: a resource's permission list holds ONE entry per principal, so
//! an update is applied per principal — a grant adds its actions to that principal's
//! entry (creating it when the principal holds nothing yet), a revoke takes its actions
//! away and drops the entry once the principal holds none — and a read reports the
//! stored list of the theme the request identifies by `(AwsAccountId, ThemeId)`. So the
//! rows below vary what that rule generalises over: how many principals and actions an
//! update names, whether the principal is new or already granted, whether a revoke takes
//! part or all of what a principal holds, whether one call both grants and revokes, the
//! size of the resulting list, and the account.

use serde_json::{json, Value};

use super::data_source_data_set_lifecycle::Harness;

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// The region the test client signs for; it is the region in every ARN below.
const REGION: &str = "us-west-2";
/// The theme most rows grant on.
const THEME: &str = "perm-theme";

fn theme_arn(account: &str, theme_id: &str) -> String {
    format!("arn:aws:quicksight:{REGION}:{account}:theme/{theme_id}")
}

/// A QuickSight principal: `ResourcePermission$Principal` documents the ARN of a
/// QuickSight user or group.
fn user_arn(account: &str, user: &str) -> String {
    format!("arn:aws:quicksight:{REGION}:{account}:user/default/{user}")
}

/// The actions a theme's owner holds. `UpdateThemePermissions`'s own documentation
/// lists the owner grouping; `ResourcePermission$Actions` targets `ActionList`, a list
/// of plain IAM action name strings the model constrains no catalog of, so the service
/// stores whatever the caller grants.
/// Source: smithy_query(query="operation", operation="UpdateThemePermissions").
const OWNER_ACTIONS: [&str; 11] = [
    "quicksight:DescribeTheme",
    "quicksight:DescribeThemeAlias",
    "quicksight:ListThemeAliases",
    "quicksight:ListThemeVersions",
    "quicksight:DeleteTheme",
    "quicksight:UpdateTheme",
    "quicksight:CreateThemeAlias",
    "quicksight:DeleteThemeAlias",
    "quicksight:UpdateThemeAlias",
    "quicksight:UpdateThemePermissions",
    "quicksight:DescribeThemePermissions",
];

/// The actions a theme's plain user holds — the user grouping the same documentation
/// lists.
const USER_ACTIONS: [&str; 4] = [
    "quicksight:DescribeTheme",
    "quicksight:DescribeThemeAlias",
    "quicksight:ListThemeAliases",
    "quicksight:ListThemeVersions",
];

/// One `ResourcePermission` as the wire carries it.
fn permission(principal: &str, actions: &[&str]) -> Value {
    json!({ "Principal": principal, "Actions": actions })
}

/// Create one theme, optionally with a permission list, and answer its ARN.
async fn create_theme(
    harness: &Harness,
    account: &str,
    theme_id: &str,
    permissions: Option<Value>,
) -> String {
    let mut body = json!({
        "Name": "permissions fixture",
        "BaseThemeId": "MIDNIGHT",
        "Configuration": { "DataColorPalette": { "Colors": ["#37BFF5"] } },
    });
    if let Some(permissions) = permissions {
        body["Permissions"] = permissions;
    }
    let (status, response) = harness
        .call(
            "POST",
            &format!("/accounts/{account}/themes/{theme_id}"),
            Some(body),
        )
        .await;
    assert_eq!(
        status, 200,
        "fixture {theme_id:?}: create expected 200, got {status} with {response}"
    );
    response["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {theme_id:?}: create returned no Arn: {response}"))
        .to_string()
}

/// One `DescribeThemePermissions` call.
async fn describe_permissions(harness: &Harness, account: &str, theme_id: &str) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!("/accounts/{account}/themes/{theme_id}/permissions"),
            None,
        )
        .await
}

/// One `UpdateThemePermissions` call carrying the grants and revokes given.
async fn update_permissions(
    harness: &Harness,
    account: &str,
    theme_id: &str,
    grant: Option<Value>,
    revoke: Option<Value>,
) -> (u16, Value) {
    let mut body = json!({});
    if let Some(grant) = grant {
        body["GrantPermissions"] = grant;
    }
    if let Some(revoke) = revoke {
        body["RevokePermissions"] = revoke;
    }
    harness
        .call(
            "PUT",
            &format!("/accounts/{account}/themes/{theme_id}/permissions"),
            Some(body),
        )
        .await
}

/// The permission list a response reports, as `(principal, actions)` pairs in the order
/// it reported them.
fn permissions_of(body: &Value) -> Vec<(String, Vec<String>)> {
    body["Permissions"]
        .as_array()
        .unwrap_or_else(|| panic!("the response carries no Permissions list: {body}"))
        .iter()
        .map(|entry| {
            let principal = entry["Principal"]
                .as_str()
                .unwrap_or_else(|| panic!("an entry carries no Principal: {body}"))
                .to_string();
            let actions = entry["Actions"]
                .as_array()
                .unwrap_or_else(|| panic!("an entry carries no Actions: {body}"))
                .iter()
                .map(|action| {
                    action
                        .as_str()
                        .unwrap_or_else(|| panic!("an action is not a string: {body}"))
                        .to_string()
                })
                .collect();
            (principal, actions)
        })
        .collect()
}

/// One expected permission list as borrowed test data: `(principal, actions)` pairs.
type WantList<'a> = Vec<(&'a str, &'a [&'a str])>;

/// One row of the create-and-read table: the case, the theme id, the `Permissions` the
/// create carries, and the list the read must report.
type CreateCase<'a> = (&'static str, &'static str, Option<Value>, WantList<'a>);

/// One row of a round-trip table: the case, what the step grants, what it revokes, and
/// the list the read must report after it.
type StepCase<'a> = (&'static str, Option<Value>, Option<Value>, WantList<'a>);

/// One row of the keying table: the case, the account and theme id read, and the list
/// that read must report.
type KeyingCase<'a> = (&'static str, &'static str, &'static str, WantList<'a>);

/// The expected shape of [`permissions_of`], built from borrowed test data.
fn expected(entries: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    entries
        .iter()
        .map(|(principal, actions)| {
            (
                (*principal).to_string(),
                actions.iter().map(|action| (*action).to_string()).collect(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_read_reports_the_permission_list_the_create_persisted() {
    let harness = Harness::new();
    let analyst = user_arn(ACCOUNT, "analyst");
    let auditor = user_arn(ACCOUNT, "auditor");

    // Rows: a theme created with no list, with one entry, and with two entries holding
    // different numbers of actions.
    let cases: [CreateCase; 3] = [
        ("no permissions", "theme-none", None, vec![]),
        (
            "one owner",
            "theme-one",
            Some(json!([permission(&analyst, &OWNER_ACTIONS)])),
            vec![(analyst.as_str(), OWNER_ACTIONS.as_slice())],
        ),
        (
            "an owner and a user",
            "theme-two",
            Some(json!([
                permission(&analyst, &OWNER_ACTIONS),
                permission(&auditor, &USER_ACTIONS),
            ])),
            vec![
                (analyst.as_str(), OWNER_ACTIONS.as_slice()),
                (auditor.as_str(), USER_ACTIONS.as_slice()),
            ],
        ),
    ];

    for (case, theme_id, permissions, want) in cases {
        let arn = create_theme(&harness, ACCOUNT, theme_id, permissions).await;
        let (status, body) = describe_permissions(&harness, ACCOUNT, theme_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        let actual = permissions_of(&body);
        let want = expected(&want);
        assert_eq!(
            actual, want,
            "case {case:?}: expected {want:?}, got {actual:?} in {body}"
        );
        assert_eq!(
            body["ThemeId"],
            json!(theme_id),
            "case {case:?}: expected the theme's id, got {body}"
        );
        assert_eq!(
            body["ThemeArn"],
            json!(arn),
            "case {case:?}: expected the theme's ARN {arn}, got {body}"
        );
        assert!(
            body["RequestId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected a RequestId, got {body}"
        );
    }
}

#[tokio::test]
async fn an_update_grants_and_revokes_per_principal() {
    let analyst = user_arn(ACCOUNT, "analyst");
    let auditor = user_arn(ACCOUNT, "auditor");
    // One action outside the user grouping, so a row can grant to a principal that
    // already holds some.
    const EXTRA: &str = "quicksight:UpdateTheme";

    // One row: the case name, what the call grants, what it revokes, and the list the
    // theme is left holding. Each row starts from the same fixture — the analyst an
    // owner, the auditor a plain user — so the rows are independent of each other.
    type Case = (
        &'static str,
        Option<Value>,
        Option<Value>,
        Vec<(String, Vec<String>)>,
    );
    let owner = (analyst.clone(), OWNER_ACTIONS.map(str::to_string).to_vec());
    let user = (auditor.clone(), USER_ACTIONS.map(str::to_string).to_vec());
    let stranger = user_arn(ACCOUNT, "stranger");

    let cases: Vec<Case> = vec![
        (
            "granting a principal that holds nothing adds an entry",
            Some(json!([permission(&stranger, &USER_ACTIONS)])),
            None,
            vec![
                owner.clone(),
                user.clone(),
                (stranger.clone(), USER_ACTIONS.map(str::to_string).to_vec()),
            ],
        ),
        (
            "granting a principal that already holds some adds only the new actions",
            Some(json!([permission(&auditor, &[USER_ACTIONS[0], EXTRA])])),
            None,
            vec![
                owner.clone(),
                (
                    auditor.clone(),
                    USER_ACTIONS
                        .iter()
                        .chain([EXTRA].iter())
                        .map(|action| (*action).to_string())
                        .collect(),
                ),
            ],
        ),
        (
            "granting only actions a principal already holds changes nothing",
            Some(json!([permission(&analyst, &OWNER_ACTIONS)])),
            None,
            vec![owner.clone(), user.clone()],
        ),
        (
            "revoking part of what a principal holds leaves the rest",
            None,
            Some(json!([permission(&analyst, &[OWNER_ACTIONS[10]])])),
            vec![
                (
                    analyst.clone(),
                    OWNER_ACTIONS[..10]
                        .iter()
                        .map(|action| (*action).to_string())
                        .collect(),
                ),
                user.clone(),
            ],
        ),
        (
            "revoking everything a principal holds drops its entry",
            None,
            Some(json!([permission(&auditor, &USER_ACTIONS)])),
            vec![owner.clone()],
        ),
        (
            "revoking an action a principal does not hold changes nothing",
            None,
            Some(json!([permission(&auditor, &[OWNER_ACTIONS[4]])])),
            vec![owner.clone(), user.clone()],
        ),
        (
            "revoking from a principal that holds nothing changes nothing",
            None,
            Some(json!([permission(&stranger, &USER_ACTIONS)])),
            vec![owner.clone(), user.clone()],
        ),
        (
            "one call grants one principal and revokes another",
            Some(json!([permission(&stranger, &USER_ACTIONS)])),
            Some(json!([permission(&auditor, &USER_ACTIONS)])),
            vec![
                owner.clone(),
                (stranger.clone(), USER_ACTIONS.map(str::to_string).to_vec()),
            ],
        ),
        (
            "one call revoking and granting the same principal leaves what it granted",
            Some(json!([permission(&auditor, &[EXTRA])])),
            Some(json!([permission(&auditor, &USER_ACTIONS)])),
            vec![owner.clone(), (auditor.clone(), vec![EXTRA.to_string()])],
        ),
        (
            "two principals granted in one call",
            Some(json!([
                permission(&stranger, &USER_ACTIONS),
                permission(&analyst, &[EXTRA]),
            ])),
            None,
            vec![
                owner.clone(),
                user.clone(),
                (stranger.clone(), USER_ACTIONS.map(str::to_string).to_vec()),
            ],
        ),
        (
            "a call that neither grants nor revokes changes nothing",
            None,
            None,
            vec![owner.clone(), user.clone()],
        ),
    ];

    for (case, grant, revoke, want) in cases {
        let harness = Harness::new();
        let arn = create_theme(
            &harness,
            ACCOUNT,
            THEME,
            Some(json!([
                permission(&analyst, &OWNER_ACTIONS),
                permission(&auditor, &USER_ACTIONS),
            ])),
        )
        .await;

        let (status, body) = update_permissions(&harness, ACCOUNT, THEME, grant, revoke).await;
        assert_eq!(
            status, 200,
            "case {case:?}: update expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["ThemeArn"],
            json!(arn),
            "case {case:?}: update expected the theme's ARN {arn}, got {body}"
        );
        assert_eq!(
            body["ThemeId"],
            json!(THEME),
            "case {case:?}: update expected the theme's id, got {body}"
        );
        // The update reports the list it leaves the theme holding, which is the list
        // the read then answers with.
        let reported = permissions_of(&body);
        assert_eq!(
            reported, want,
            "case {case:?}: the update expected {want:?}, got {reported:?} in {body}"
        );

        let (read_status, read) = describe_permissions(&harness, ACCOUNT, THEME).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: read expected 200, got {read_status} with {read}"
        );
        let actual = permissions_of(&read);
        assert_eq!(
            actual, want,
            "case {case:?}: expected {want:?}, got {actual:?} in {read}"
        );
    }
}

#[tokio::test]
async fn an_update_is_visible_to_every_later_read_and_a_read_changes_nothing() {
    let harness = Harness::new();
    let analyst = user_arn(ACCOUNT, "analyst");
    create_theme(&harness, ACCOUNT, THEME, None).await;

    // Rows: the list after each step of a grant/revoke round trip, read twice per step
    // so a read that consumed or moved anything would show up here.
    let steps: [StepCase; 4] = [
        ("before any grant", None, None, vec![]),
        (
            "after granting the owner",
            Some(json!([permission(&analyst, &OWNER_ACTIONS)])),
            None,
            vec![(analyst.as_str(), OWNER_ACTIONS.as_slice())],
        ),
        (
            "after revoking one action",
            None,
            Some(json!([permission(&analyst, &[OWNER_ACTIONS[10]])])),
            vec![(analyst.as_str(), &OWNER_ACTIONS[..10])],
        ),
        (
            "after revoking the rest",
            None,
            Some(json!([permission(&analyst, &OWNER_ACTIONS[..10])])),
            vec![],
        ),
    ];

    for (case, grant, revoke, want) in steps {
        if grant.is_some() || revoke.is_some() {
            let (status, body) = update_permissions(&harness, ACCOUNT, THEME, grant, revoke).await;
            assert_eq!(
                status, 200,
                "case {case:?}: update expected 200, got {status} with {body}"
            );
        }
        let want = expected(&want);
        for round in 0..2 {
            let (status, body) = describe_permissions(&harness, ACCOUNT, THEME).await;
            assert_eq!(
                status, 200,
                "case {case:?} read {round}: expected 200, got {status} with {body}"
            );
            let actual = permissions_of(&body);
            assert_eq!(
                actual, want,
                "case {case:?} read {round}: expected {want:?}, got {actual:?} in {body}"
            );
        }
    }
}

#[tokio::test]
async fn permissions_are_held_per_account_and_per_theme() {
    let harness = Harness::new();
    let analyst = user_arn(ACCOUNT, "analyst");
    let other = user_arn(OTHER_ACCOUNT, "analyst");
    create_theme(&harness, ACCOUNT, THEME, None).await;
    create_theme(&harness, ACCOUNT, "perm-theme-sibling", None).await;
    create_theme(&harness, OTHER_ACCOUNT, THEME, None).await;

    let (status, body) = update_permissions(
        &harness,
        ACCOUNT,
        THEME,
        Some(json!([permission(&analyst, &OWNER_ACTIONS)])),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the grant expected 200, got {status} with {body}"
    );
    let (status, body) = update_permissions(
        &harness,
        OTHER_ACCOUNT,
        THEME,
        Some(json!([permission(&other, &USER_ACTIONS)])),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the other account's grant expected 200, got {status} with {body}"
    );

    // Rows: the granted theme, its sibling in the same account, and the theme of the
    // same id in another account.
    let cases: [KeyingCase; 3] = [
        (
            "the granted theme",
            ACCOUNT,
            THEME,
            vec![(analyst.as_str(), OWNER_ACTIONS.as_slice())],
        ),
        (
            "a sibling theme of the same account",
            ACCOUNT,
            "perm-theme-sibling",
            vec![],
        ),
        (
            "the same theme id in another account",
            OTHER_ACCOUNT,
            THEME,
            vec![(other.as_str(), USER_ACTIONS.as_slice())],
        ),
    ];

    for (case, account, theme_id, want) in cases {
        let (status, body) = describe_permissions(&harness, account, theme_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        let actual = permissions_of(&body);
        let want = expected(&want);
        assert_eq!(
            actual, want,
            "case {case:?}: expected {want:?}, got {actual:?} in {body}"
        );
    }
}

#[tokio::test]
async fn a_theme_holds_at_most_the_modelled_number_of_grants() {
    let harness = Harness::new();
    create_theme(&harness, ACCOUNT, THEME, None).await;

    // The published bound: the list shape a theme's `Permissions` member targets,
    // `UpdateResourcePermissionList`, is `length 0..100`, so 100 principals fit and the
    // 101st does not.
    // Source: smithy_query(query="shape", shape="UpdateResourcePermissionList").
    const MAX: usize = 100;
    let principals: Vec<String> = (0..=MAX)
        .map(|index| user_arn(ACCOUNT, &format!("user-{index:03}")))
        .collect();

    let grant: Vec<Value> = principals[..MAX]
        .iter()
        .map(|principal| permission(principal, &USER_ACTIONS))
        .collect();
    let (status, body) =
        update_permissions(&harness, ACCOUNT, THEME, Some(json!(grant)), None).await;
    assert_eq!(
        status, 200,
        "case at the bound: expected 200, got {status} with {body}"
    );
    let (status, read) = describe_permissions(&harness, ACCOUNT, THEME).await;
    assert_eq!(
        status, 200,
        "case at the bound: read expected 200, got {status} with {read}"
    );
    let actual = permissions_of(&read);
    assert_eq!(
        actual.len(),
        MAX,
        "case at the bound: expected {MAX} entries, got {:?} in {read}",
        actual.len()
    );

    // One past the bound.
    let (status, body) = update_permissions(
        &harness,
        ACCOUNT,
        THEME,
        Some(json!([permission(&principals[MAX], &USER_ACTIONS)])),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "case past the bound: expected 400, got {status} with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("InvalidParameterValueException"),
        "case past the bound: expected InvalidParameterValueException, got {body}"
    );
    let (status, read) = describe_permissions(&harness, ACCOUNT, THEME).await;
    assert_eq!(
        status, 200,
        "case past the bound: read expected 200, got {status} with {read}"
    );
    let actual = permissions_of(&read);
    assert_eq!(
        actual.len(),
        MAX,
        "case past the bound: the refused grant was stored anyway: {read}"
    );
}

#[tokio::test]
async fn a_theme_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    let analyst = user_arn(ACCOUNT, "analyst");
    create_theme(&harness, ACCOUNT, THEME, None).await;

    // Rows: a read and an update of an id nothing created, of a deleted theme, and of
    // an account that holds nothing.
    let (status, body) = harness
        .call("DELETE", &format!("/accounts/{ACCOUNT}/themes/{THEME}"), None)
        .await;
    assert_eq!(
        status, 200,
        "the fixture delete expected 200, got {status} with {body}"
    );

    let cases: [(&str, &str, &str); 3] = [
        ("an id nothing created", ACCOUNT, "perm-theme-absent"),
        ("a deleted theme", ACCOUNT, THEME),
        ("an account holding nothing", "123456789012", THEME),
    ];

    for (case, account, theme_id) in cases {
        let want_message = format!("Theme {} is not found", theme_arn(account, theme_id));
        let (status, body) = describe_permissions(&harness, account, theme_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: read expected 404, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: read expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["Message"],
            json!(want_message),
            "case {case:?}: read expected the message {want_message:?}, got {body}"
        );

        let (status, body) = update_permissions(
            &harness,
            account,
            theme_id,
            Some(json!([permission(&analyst, &USER_ACTIONS)])),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: update expected 404, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: update expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["Message"],
            json!(want_message),
            "case {case:?}: update expected the message {want_message:?}, got {body}"
        );
    }
}
