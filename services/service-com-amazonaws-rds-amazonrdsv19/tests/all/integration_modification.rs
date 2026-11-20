// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ModifyIntegration`: the operation that modifies a zero-ETL
//! integration this service holds.
//!
//! The rules under test:
//!
//! * a modification replaces, on the integration the identifier NAMES — the name a create
//!   gave it or its ARN — exactly the members the request carries (`IntegrationName`,
//!   `DataFilter`, `Description`), leaves every member it does not carry as stored, and
//!   answers with the whole integration in the `modifying` status, which the clock takes
//!   back to `active`; the next read of the integration reports what was written, so the
//!   modification and the report cannot disagree;
//! * an identifier that names no integration the account holds is the modelled 404,
//!   whichever form it took;
//! * an integration that is not up — still being created, or already applying another
//!   modification — takes no modification: that is the modelled state fault, and nothing is
//!   written for it;
//! * a request that names no member to modify at all is refused as a parameter combination
//!   this service cannot serve;
//! * a new name another integration of the same account and region already holds is the
//!   modelled conflict fault, and neither integration's name changes.
//!
//! The rules are asserted over TABLES spanning what they generalise over: which of the
//! three modifiable members the request carries (each alone and all three together), the
//! two forms "the unique identifier of the integration" takes, and one row per way an
//! identifier can name nothing.
//!
//! Every call goes over the wire: real awsQuery form POSTs through a `MultiServiceRouter`
//! carrying the SigV4 credential scope the router keys on, and every assertion reads the
//! XML the emulator answered with. Nothing waits for a fixed span: each stage is reached by
//! polling a REPORTING call through the one poll loop this test target shares
//! (`lifecycle_poll_loops::poll_until`), so only a transition this service's clock owes can
//! end a wait.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, text, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An Amazon Redshift namespace ARN, the kind of target `CreateIntegration$TargetArn` takes.
const REDSHIFT_TARGET_ARN: &str =
    "arn:aws:redshift-serverless:us-west-2:000000000000:namespace/zero-etl";

/// The `DataFilter` every integration below is created with, so a row that does not ask for
/// a new one can assert the stored filter was left alone.
const CREATED_FILTER: &str = "include: *.*";

/// The `Description` every integration below is created with, for the same reason.
const CREATED_DESCRIPTION: &str = "the integration as created";

/// One row of the modification table: how the modify names the integration, and which of
/// the three modifiable members it carries.
struct ModifyCase {
    case: &'static str,
    /// The name the create gives the integration.
    name: &'static str,
    /// Whether the modify names the integration by its ARN rather than by its name.
    by_arn: bool,
    /// `IntegrationName`: "a new name for the integration".
    new_name: Option<&'static str>,
    /// `DataFilter`: "a new data filter for the integration".
    new_filter: Option<&'static str>,
    /// `Description`: "a new description for the integration".
    new_description: Option<&'static str>,
}

impl ModifyCase {
    /// The parameters the modification carries beyond the identifier.
    fn changes(&self) -> Vec<(&'static str, &'static str)> {
        let mut params: Vec<(&'static str, &'static str)> = Vec::new();
        if let Some(name) = self.new_name {
            params.push(("IntegrationName", name));
        }
        if let Some(filter) = self.new_filter {
            params.push(("DataFilter", filter));
        }
        if let Some(description) = self.new_description {
            params.push(("Description", description));
        }
        params
    }

    /// The name the integration carries after the modification.
    fn name_after(&self) -> &'static str {
        self.new_name.unwrap_or(self.name)
    }

    /// The data filter the integration carries after the modification.
    fn filter_after(&self) -> &'static str {
        self.new_filter.unwrap_or(CREATED_FILTER)
    }

    /// The description the integration carries after the modification.
    fn description_after(&self) -> &'static str {
        self.new_description.unwrap_or(CREATED_DESCRIPTION)
    }
}

/// The class the modification rule generalises over: each modifiable member alone and all
/// three at once, under both forms "the unique identifier of the integration" takes.
const MODIFY_CASES: &[ModifyCase] = &[
    ModifyCase {
        case: "a new data filter alone, named by name",
        name: "mod-integration-filter",
        by_arn: false,
        new_name: None,
        new_filter: Some("include: sales.public.orders"),
        new_description: None,
    },
    ModifyCase {
        case: "a new description alone, named by ARN",
        name: "mod-integration-description",
        by_arn: true,
        new_name: None,
        new_filter: None,
        new_description: Some("the integration after the modification"),
    },
    ModifyCase {
        case: "a new name alone, named by name",
        name: "mod-integration-named",
        by_arn: false,
        new_name: Some("mod-integration-renamed"),
        new_filter: None,
        new_description: None,
    },
    ModifyCase {
        case: "a new name, filter and description together, named by ARN",
        name: "mod-integration-everything",
        by_arn: true,
        new_name: Some("mod-integration-all-renamed"),
        new_filter: Some("exclude: staging.*.*"),
        new_description: Some("renamed, refiltered and redescribed"),
    },
];

/// `DescribeIntegrations` naming one integration, as the HTTP status and the body.
async fn describe(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeIntegrations",
        &[("IntegrationIdentifier", identifier)],
    )
    .await
}

/// `ModifyIntegration` naming one integration and carrying `changes`.
async fn modify(
    router: &MultiServiceRouter,
    identifier: &str,
    changes: &[(&str, &str)],
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![("IntegrationIdentifier", identifier)];
    params.extend_from_slice(changes);
    call(router, "ModifyIntegration", &params).await
}

/// Create a DB cluster to be an integration's source and poll it until it is `available`,
/// answering the ARN that names it — the form `CreateIntegration$SourceArn` takes.
async fn available_source_cluster(router: &MultiServiceRouter, identifier: &str) -> String {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "aurora-mysql"),
            ],
        )
        .await,
    );
    let described = [("DBClusterIdentifier", identifier)];
    poll_until(
        identifier,
        "the source DB cluster reporting available",
        || call(router, "DescribeDBClusters", &described),
        |http, body| http == 200 && optional_text(body, "Status").as_deref() == Some("available"),
    )
    .await;
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{identifier}")
}

/// `CreateIntegration` for `name` over `source_arn`, answering the ARN the create minted.
async fn create_integration(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source_arn: &str,
) -> String {
    let created = expect_ok(
        case,
        call(
            router,
            "CreateIntegration",
            &[
                ("IntegrationName", name),
                ("SourceArn", source_arn),
                ("TargetArn", REDSHIFT_TARGET_ARN),
                ("DataFilter", CREATED_FILTER),
                ("Description", CREATED_DESCRIPTION),
            ],
        )
        .await,
    );
    text(&created, "IntegrationArn")
}

/// Poll `DescribeIntegrations` until the integration reports `status`, answering the body it
/// settled on.
async fn poll_integration_until(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) -> String {
    let (body, _polls) = poll_until(
        case,
        &format!("integration {identifier} reporting {status}"),
        || describe(router, identifier),
        |http, body| http == 200 && optional_text(body, "Status").as_deref() == Some(status),
    )
    .await;
    body
}

/// The members a modification must not have touched, read off whichever response carries the
/// integration: `(IntegrationArn, SourceArn, TargetArn, CreateTime)`.
fn untouched(body: &str) -> (String, String, String, String) {
    (
        text(body, "IntegrationArn"),
        text(body, "SourceArn"),
        text(body, "TargetArn"),
        text(body, "CreateTime"),
    )
}

/// The three modifiable members, read off whichever response carries the integration.
fn modifiable(body: &str) -> (String, String, String) {
    (
        text(body, "IntegrationName"),
        text(body, "DataFilter"),
        text(body, "Description"),
    )
}

#[tokio::test]
async fn a_modification_replaces_the_members_it_names_and_leaves_the_rest() {
    let router = emulator();
    let source_arn = available_source_cluster(&router, "mod-integration-src").await;

    // Every row's integration is created before any of them is polled, so the spans their
    // creates owe run together rather than one after another.
    let mut created: Vec<(String, String)> = Vec::new();
    for row in MODIFY_CASES {
        let arn = create_integration(&router, row.case, row.name, &source_arn).await;
        created.push((arn, String::new()));
    }
    for (row, entry) in MODIFY_CASES.iter().zip(created.iter_mut()) {
        // An integration is modifiable once it is up, which is looked for rather than
        // waited out.
        let up = poll_integration_until(&router, row.case, row.name, "active").await;
        entry.1 = text(&up, "CreateTime");
    }

    for (row, (arn, create_time)) in MODIFY_CASES.iter().zip(created.iter()) {
        let case = row.case;
        let identifier = if row.by_arn { arn.as_str() } else { row.name };
        let changes = row.changes();
        let modified = expect_ok(case, modify(&router, identifier, &changes).await);

        // The answer carries the integration with the members the request named replaced and
        // the ones it did not left as stored.
        assert_eq!(
            modifiable(&modified),
            (
                row.name_after().to_string(),
                row.filter_after().to_string(),
                row.description_after().to_string(),
            ),
            "case {case:?}: got {modified}"
        );
        // Nothing the request cannot address moved: the ARN the create minted, the source and
        // the target it pairs, and the instant it was created at.
        assert_eq!(
            untouched(&modified),
            (
                arn.clone(),
                source_arn.clone(),
                REDSHIFT_TARGET_ARN.to_string(),
                create_time.clone(),
            ),
            "case {case:?}: got {modified}"
        );
        // An accepted modification is being applied, which is the status the integration
        // reports from now on.
        assert_eq!(
            text(&modified, "Status"),
            "modifying",
            "case {case:?}: got {modified}"
        );

        // The next read reports exactly what the modification wrote, and the clock takes the
        // integration back to `active` on its own. The ARN is what the read names, because a
        // rename does not change it.
        let reported = poll_integration_until(&router, case, arn, "active").await;
        assert_eq!(
            modifiable(&reported),
            (
                row.name_after().to_string(),
                row.filter_after().to_string(),
                row.description_after().to_string(),
            ),
            "case {case:?}: got {reported}"
        );
        assert_eq!(
            untouched(&reported),
            (
                arn.clone(),
                source_arn.clone(),
                REDSHIFT_TARGET_ARN.to_string(),
                create_time.clone(),
            ),
            "case {case:?}: got {reported}"
        );

        // A renamed integration answers to its new name, and to its old one no longer: the
        // name is what the store keys it by.
        if let Some(new_name) = row.new_name {
            let by_new_name = expect_ok(case, describe(&router, new_name).await);
            assert_eq!(
                text(&by_new_name, "IntegrationArn"),
                *arn,
                "case {case:?}: got {by_new_name}"
            );
            expect_error(case, describe(&router, row.name).await, 404, "IntegrationNotFoundFault");
        }
    }
}

#[tokio::test]
async fn an_identifier_that_names_no_integration_takes_no_modification() {
    let router = emulator();
    let source_arn = available_source_cluster(&router, "mod-miss-src").await;
    let held_arn =
        create_integration(&router, "the held integration", "held-integration", &source_arn)
            .await;
    poll_integration_until(&router, "the held integration", "held-integration", "active").await;

    // (case, the region the call is signed for, the identifier it names) — the ways an
    // identifier can name no integration the account holds: a name nothing created, a near
    // miss of the held name, an ARN nothing created, and the held integration addressed in
    // another region, since RDS is regional.
    let rows: [(&str, &str, &str); 5] = [
        ("a name nothing created", REGION, "no-such-integration"),
        ("a near miss of the held name", REGION, "held-integratio"),
        (
            "an ARN nothing created",
            REGION,
            "arn:aws:rds:us-west-2:000000000000:integration:11111111-2222-3333-4444-555555555555",
        ),
        ("the held name in another region", "eu-west-1", "held-integration"),
        ("the held ARN in another region", "eu-west-1", held_arn.as_str()),
    ];

    for (case, region, identifier) in rows {
        // The refusal names the integration it could not find by the ARN that identifies one
        // integration of one account in one region: the value as the request spelled it where
        // that was already an ARN, and otherwise the ARN the name would have in the region
        // the call was signed for. The two sentences after it are Amazon RDS's own fixed
        // text, which tells the caller the identifier may be stale rather than malformed.
        let named = if identifier.starts_with("arn:") {
            identifier.to_string()
        } else {
            format!("arn:aws:rds:{region}:{ACCOUNT}:integration:{identifier}")
        };
        let expected = format!(
            "Integration with identifier {named} could not be found. The integration might \
             have been deleted, or you provided an incorrect identifier. Check the \
             integration identifier and try again."
        );
        let body = expect_error(
            case,
            call_in(
                &router,
                region,
                "ModifyIntegration",
                &[
                    ("IntegrationIdentifier", identifier),
                    ("Description", "a description nothing takes"),
                ],
            )
            .await,
            404,
            "IntegrationNotFoundFault",
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the whole text {expected:?}, got {message:?}"
        );
    }

    // Every refusal above left the integration that IS held exactly as it was created.
    let reported = expect_ok(
        "the held integration after every refusal",
        describe(&router, "held-integration").await,
    );
    assert_eq!(
        modifiable(&reported),
        (
            "held-integration".to_string(),
            CREATED_FILTER.to_string(),
            CREATED_DESCRIPTION.to_string(),
        ),
        "case \"the held integration after every refusal\": got {reported}"
    );
}

#[tokio::test]
async fn an_integration_that_is_not_up_takes_no_modification() {
    let router = emulator();
    let source_arn = available_source_cluster(&router, "mod-state-src").await;
    let arn =
        create_integration(&router, "a new integration", "mod-state-target", &source_arn).await;

    // An integration this service is still bringing up is not one a modification can be
    // applied to, and the fault names the status the caller has to wait out.
    let refused = expect_error(
        "an integration that is still being created",
        modify(
            &router,
            "mod-state-target",
            &[("Description", "a description a creating integration takes")],
        )
        .await,
        400,
        "InvalidIntegrationStateFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        "Integration mod-state-target is in creating state but must be active to be modified",
        "case \"an integration that is still being created\": got {refused}"
    );

    poll_integration_until(&router, "the created integration", &arn, "active").await;
    // One modification is accepted, and while it is being applied the integration is not up:
    // a second one is refused the same way, which is what makes two modifications of one
    // integration settle as one accepted and one refused.
    expect_ok(
        "the accepted modification",
        modify(&router, "mod-state-target", &[("Description", "the accepted description")]).await,
    );
    let second = expect_error(
        "an integration already applying a modification",
        modify(
            &router,
            "mod-state-target",
            &[("Description", "a description that is refused")],
        )
        .await,
        400,
        "InvalidIntegrationStateFault",
    );
    assert_eq!(
        text(&second, "Message"),
        "Integration mod-state-target is in modifying state but must be active to be modified",
        "case \"an integration already applying a modification\": got {second}"
    );

    // The refused modification wrote nothing: the integration settles carrying what the
    // ACCEPTED one wrote.
    let settled = poll_integration_until(&router, "the modified integration", &arn, "active").await;
    assert_eq!(
        text(&settled, "Description"),
        "the accepted description",
        "case \"the modified integration\": got {settled}"
    );
}

#[tokio::test]
async fn a_request_that_names_no_modification_is_refused() {
    let router = emulator();
    let source_arn = available_source_cluster(&router, "mod-empty-src").await;
    let arn = create_integration(&router, "the integration", "mod-empty-target", &source_arn).await;
    poll_integration_until(&router, "the integration", &arn, "active").await;

    // Both forms of the identifier, because what cannot be served is the request rather than
    // the integration it addresses.
    for (case, identifier) in [("by name", "mod-empty-target"), ("by ARN", arn.as_str())] {
        let refused = expect_error(
            case,
            modify(&router, identifier, &[]).await,
            400,
            "InvalidParameterCombination",
        );
        assert_eq!(
            text(&refused, "Message"),
            "No modifications were requested",
            "case {case:?}: got {refused}"
        );
    }

    // Neither refusal moved the integration's status or any member of it.
    let reported = expect_ok("the unmodified integration", describe(&router, &arn).await);
    assert_eq!(
        (modifiable(&reported), text(&reported, "Status")),
        (
            (
                "mod-empty-target".to_string(),
                CREATED_FILTER.to_string(),
                CREATED_DESCRIPTION.to_string(),
            ),
            "active".to_string(),
        ),
        "case \"the unmodified integration\": got {reported}"
    );
}

#[tokio::test]
async fn a_new_name_another_integration_holds_is_refused() {
    let router = emulator();
    let source_arn = available_source_cluster(&router, "mod-clash-src").await;
    let mine =
        create_integration(&router, "the renamed integration", "mod-clash-mine", &source_arn)
            .await;
    let theirs = create_integration(
        &router,
        "the integration holding the name",
        "mod-clash-theirs",
        &source_arn,
    )
    .await;
    poll_integration_until(&router, "the renamed integration", &mine, "active").await;
    poll_integration_until(&router, "the integration holding the name", &theirs, "active").await;

    let refused = expect_error(
        "a new name another integration holds",
        modify(
            &router,
            "mod-clash-mine",
            &[("IntegrationName", "mod-clash-theirs")],
        )
        .await,
        400,
        "IntegrationConflictOperationFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        "A conflicting conditional operation is currently in progress against this resource.",
        "case \"a new name another integration holds\": got {refused}"
    );
    // Neither integration moved: the refused rename took nothing from either of them.
    for (case, arn, name) in [
        ("the integration that asked to be renamed", &mine, "mod-clash-mine"),
        ("the integration holding the name", &theirs, "mod-clash-theirs"),
    ] {
        let reported = expect_ok(case, describe(&router, arn).await);
        assert_eq!(
            (text(&reported, "IntegrationName"), text(&reported, "Status")),
            (name.to_string(), "active".to_string()),
            "case {case:?}: got {reported}"
        );
    }

    // A new name equal to the one the integration already has collides with nothing: it is
    // that same integration, so the modification is accepted.
    let renamed = expect_ok(
        "a new name equal to the stored one",
        modify(
            &router,
            "mod-clash-mine",
            &[("IntegrationName", "mod-clash-mine")],
        )
        .await,
    );
    assert_eq!(
        (
            text(&renamed, "IntegrationName"),
            text(&renamed, "IntegrationArn"),
            text(&renamed, "Status"),
        ),
        (
            "mod-clash-mine".to_string(),
            mine.clone(),
            "modifying".to_string(),
        ),
        "case \"a new name equal to the stored one\": got {renamed}"
    );
}
