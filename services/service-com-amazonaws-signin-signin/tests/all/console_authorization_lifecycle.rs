// SPDX-License-Identifier: CC0-1.0

//! The console-authorization configuration's whole lifecycle, end to end over HTTP, and
//! the two refusals that guard it.
//!
//! `PutConsoleAuthorizationConfiguration` enables it for a target,
//! `GetConsoleAuthorizationConfiguration` reports what the put persisted, and
//! `DeleteConsoleAuthorizationConfiguration` releases it — reporting that target with
//! `consoleAuthorizationEnabled` false, after which the target is missing again.
//!
//! Three rules are under test here:
//!
//! * the configuration of a target is exactly what the writes left there, so a delete
//!   reports the target it released as no longer enabled and every later read of that
//!   target — another get, another delete — answers `ResourceNotFoundException` with the
//!   fixed sentence AWS states for a configuration that is not there;
//! * the put is a CREATE: a target whose console authorization is already enabled is a
//!   `ConflictException`, and the conflict tracks that STATE, so the identical put is
//!   served again once a delete has cleared it;
//! * a target naming an AWS Organizations entity is served only to a caller authorized
//!   for it — the management account of the organization that entity belongs to — and is
//!   `AccessDeniedException` to anyone else, on the write and on the reads alike, which
//!   includes the caller whose account is in no organization yet: that authority comes
//!   from `CreateOrganization` having been called and from nothing else.
//!
//! The rows differ along what the rules generalise over: which alternative of
//! `TargetId`'s pattern the target matches (account id, organization id, organizational
//! root id, at both published length bounds), whether the request names a target at all,
//! and which of the three operations is called.

use serde_json::{json, Value};

use crate::signin_harness::{
    configured_targets, emulator_with_organization, post, Answer, WithOrganization,
    EMULATOR_ACCOUNT,
};

/// The fixed sentence AWS states for a target that has no console-authorization
/// configuration. Spelled out here rather than imported, so the crate's own constant is
/// what this compares against.
const NOT_FOUND_MESSAGE: &str = "Console authorization configuration not found";

/// The fixed sentence AWS states when the target is already configured.
const ALREADY_EXISTS_MESSAGE: &str = "Configuration already exists";

/// The fixed sentence AWS states for a call scoped to an organization the caller has no
/// authority over.
const NOT_AUTHORIZED_MESSAGE: &str = "Caller is not authorized for this organization";

/// `OAuth2ErrorCode`'s member for a refusal of a resource that is not there.
const NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// `OAuth2ErrorCode`'s member for a conflict with the current state.
const CONFLICT_CODE: &str = "CONFLICT";

/// `OAuth2ErrorCode`'s member for a caller that may not perform the operation.
const INSUFFICIENT_PERMISSIONS_CODE: &str = "INSUFFICIENT_PERMISSIONS";

/// One target the caller IS authorized for, as `(case, request, targetId, scope)`.
///
/// Written from `TargetId`'s own pattern `^(\d{12}|o-[a-z0-9]{10}|r-[0-9a-z]{4,32})$` —
/// an account id, an AWS Organizations organization id and an organizational root id.
/// The organization row is the id the Organizations control plane minted for the
/// emulated account's own organization, read back from `with`, because an `o-` id of
/// another organization is one this caller has no authority over.
///
/// A root id is absent here on purpose: `TargetId` also carries `@length(min: 12, max:
/// 32)`, and an Organizations root id is `r-` plus the 8 characters `RootId`'s own
/// `^r-[0-9a-z]{4,32}$` admits, so the root of the caller's organization is 10 characters
/// and this member cannot carry it at all. The ROOT form is exercised where it IS
/// expressible — as a foreign target, below.
fn authorized_targets(with: &WithOrganization) -> Vec<(&'static str, Value, String, &'static str)> {
    vec![
        (
            "a 12-digit account id",
            json!({"targetId": "123456789012"}),
            "123456789012".to_string(),
            "ACCOUNT",
        ),
        (
            "no targetId at all, which addresses the caller's own account",
            json!({}),
            EMULATOR_ACCOUNT.to_string(),
            "ACCOUNT",
        ),
        (
            "the caller's own organization id",
            json!({"targetId": with.organization_id.clone()}),
            with.organization_id.clone(),
            "ORGANIZATION",
        ),
    ]
}

/// Every `TargetId` of an Organizations form that names no entity of the caller's own
/// organization, as `(case, targetId)`.
///
/// The rows span both Organizations alternatives of the pattern and, for a root id, each
/// published bound of `TargetId`'s `@length(min: 12, max: 32)` — the narrower of the two
/// constraints on a root id — so no row can be passing because of one id's shape.
fn foreign_targets() -> Vec<(&'static str, String)> {
    vec![
        ("an organization id", "o-00000000zz".to_string()),
        ("another organization id", "o-abc1234567".to_string()),
        (
            "a root id at the 12-character length floor",
            "r-0123456789".to_string(),
        ),
        (
            "a root id at the 32-character length ceiling",
            format!("r-{}", "a".repeat(30)),
        ),
    ]
}

async fn put_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    request: Value,
) -> Answer {
    post(router, "/put-console-authorization-configuration", request).await
}

async fn get_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    request: Value,
) -> Answer {
    post(router, "/get-console-authorization-configuration", request).await
}

async fn delete_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    request: Value,
) -> Answer {
    post(router, "/delete-console-authorization-configuration", request).await
}

/// The three members every one of the three operations reports, as a comparable triple.
fn reported(answer: &Answer) -> (String, String, Option<bool>) {
    (
        answer.body["targetId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        answer.body["scope"].as_str().unwrap_or_default().to_string(),
        answer.body["consoleAuthorizationEnabled"].as_bool(),
    )
}

/// Assert that `answer` is the 404 `ResourceNotFoundException` a missing configuration
/// earns, worded as AWS words it.
fn assert_not_found(case: &str, reader: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_code(),
            answer.error_member(),
            answer.message()
        ),
        (404, "ResourceNotFoundException", NOT_FOUND_CODE, NOT_FOUND_MESSAGE),
        "case {case:?}: the {reader} of an unconfigured target must answer 404 \
         ResourceNotFoundException / {NOT_FOUND_MESSAGE:?}, got {} with {:?}",
        answer.status,
        answer.body
    );
}

#[tokio::test]
async fn the_configuration_of_a_target_is_what_the_writes_left_there() {
    let with = emulator_with_organization().await;
    let router = &with.router;

    for (case, request, target_id, scope) in authorized_targets(&with) {
        // Nothing is configured for this target yet, so BOTH readers report it missing —
        // the delete included, which is what makes a cleanup path able to tell "there was
        // nothing to release" from "released".
        for (reader, answer) in [
            ("get", get_configuration(router, request.clone()).await),
            ("delete", delete_configuration(router, request.clone()).await),
        ] {
            assert_not_found(case, reader, &answer);
        }

        let written = put_configuration(router, request.clone()).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: a put must be accepted, got {} with {:?}",
            written.status, written.body
        );
        assert_eq!(
            reported(&written),
            (target_id.clone(), scope.to_string(), Some(true)),
            "case {case:?}: the put must report the target it enabled, got {:?}",
            written.body
        );

        // A request of its own observes exactly what the put persisted.
        let read = get_configuration(router, request.clone()).await;
        assert_eq!(
            (read.status, reported(&read)),
            (200, (target_id.clone(), scope.to_string(), Some(true))),
            "case {case:?}: the get must report what the put persisted, got {} with {:?}",
            read.status,
            read.body
        );

        // The put is a create: the target is already enabled, so a second one conflicts
        // with the state it is in and leaves that state alone.
        let conflicted = put_configuration(router, request.clone()).await;
        assert_eq!(
            (
                conflicted.status,
                conflicted.error_code(),
                conflicted.error_member(),
                conflicted.message()
            ),
            (409, "ConflictException", CONFLICT_CODE, ALREADY_EXISTS_MESSAGE),
            "case {case:?}: a second put must answer 409 ConflictException / \
             {ALREADY_EXISTS_MESSAGE:?}, got {} with {:?}",
            conflicted.status,
            conflicted.body
        );
        let unchanged = get_configuration(router, request.clone()).await;
        assert_eq!(
            (unchanged.status, reported(&unchanged)),
            (200, (target_id.clone(), scope.to_string(), Some(true))),
            "case {case:?}: the refused second put must leave the configuration enabled, \
             got {} with {:?}",
            unchanged.status,
            unchanged.body
        );

        // The delete reports the target it released, with console authorization no
        // longer enabled — the state it just left the target in.
        let released = delete_configuration(router, request.clone()).await;
        assert_eq!(
            (released.status, reported(&released)),
            (200, (target_id.clone(), scope.to_string(), Some(false))),
            "case {case:?}: the delete must report the released target as no longer \
             enabled, got {} with {:?}",
            released.status,
            released.body
        );

        // And the release really happened: the target is missing again to every reader,
        // so a repeated delete is a 404 rather than a second success.
        for (reader, answer) in [
            ("get", get_configuration(router, request.clone()).await),
            ("delete", delete_configuration(router, request.clone()).await),
        ] {
            assert_not_found(case, reader, &answer);
        }

        // The conflict tracked the enabled state, not the target: with that state
        // cleared, the identical put is served again.
        let again = put_configuration(router, request.clone()).await;
        assert_eq!(
            (again.status, reported(&again)),
            (200, (target_id.clone(), scope.to_string(), Some(true))),
            "case {case:?}: the identical put must be served once the delete has cleared \
             the configuration, got {} with {:?}",
            again.status,
            again.body
        );
    }
}

/// A delete releases ONE target: every other configured target is untouched, so the
/// delete cannot be answering from anything but the per-target records.
#[tokio::test]
async fn a_delete_releases_only_the_target_it_names() {
    let with = emulator_with_organization().await;
    let router = &with.router;
    let cases = authorized_targets(&with);

    for (case, request, ..) in &cases {
        let written = put_configuration(router, request.clone()).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: a put must be accepted, got {} with {:?}",
            written.status, written.body
        );
    }

    for (index, (case, request, target_id, scope)) in cases.iter().enumerate() {
        let released = delete_configuration(router, request.clone()).await;
        assert_eq!(
            (released.status, reported(&released)),
            (200, (target_id.clone(), (*scope).to_string(), Some(false))),
            "case {case:?}: the delete must release this target, got {} with {:?}",
            released.status,
            released.body
        );

        // Every target not yet deleted is still enabled; every one already deleted is
        // still gone.
        for (other, other_request, other_target, other_scope) in &cases[index + 1..] {
            let read = get_configuration(router, other_request.clone()).await;
            assert_eq!(
                (read.status, reported(&read)),
                (
                    200,
                    (other_target.clone(), (*other_scope).to_string(), Some(true))
                ),
                "case {case:?}: deleting it must leave {other:?} enabled, got {} with {:?}",
                read.status,
                read.body
            );
        }
        for (earlier, earlier_request, ..) in &cases[..=index] {
            let read = get_configuration(router, earlier_request.clone()).await;
            assert_not_found(earlier, "get", &read);
        }
    }
}

/// An `o-`/`r-` target that names no entity of the caller's own organization is a target
/// the caller has no authority over: every one of the three operations refuses it with
/// `AccessDeniedException`, and the refused put records nothing for it.
#[tokio::test]
async fn an_organizations_target_the_caller_does_not_own_is_denied() {
    let with = emulator_with_organization().await;
    let router = &with.router;

    for (case, target_id) in foreign_targets() {
        let request = json!({"targetId": target_id});
        for (operation, answer) in [
            ("put", put_configuration(router, request.clone()).await),
            ("get", get_configuration(router, request.clone()).await),
            ("delete", delete_configuration(router, request.clone()).await),
        ] {
            assert_eq!(
                (
                    answer.status,
                    answer.error_code(),
                    answer.error_member(),
                    answer.message()
                ),
                (
                    400,
                    "AccessDeniedException",
                    INSUFFICIENT_PERMISSIONS_CODE,
                    NOT_AUTHORIZED_MESSAGE
                ),
                "case {case:?}: the {operation} of {target_id:?} must be denied with \
                 {NOT_AUTHORIZED_MESSAGE:?}, got {} with {:?}",
                answer.status,
                answer.body
            );
        }
    }

    assert_eq!(
        configured_targets(&with.engine),
        Vec::<String>::new(),
        "a denied put must record no configuration at all"
    );

    // The caller's OWN organization is served, so the refusal above is about authority
    // over the entity and not about the form of the id.
    let written = put_configuration(router, json!({"targetId": with.organization_id.clone()})).await;
    assert_eq!(
        written.status,
        200,
        "the caller's own organization {:?} must be served, got {} with {:?}",
        with.organization_id,
        written.status,
        written.body
    );
}

/// Authority over an `o-`/`r-` target follows the account's MEMBERSHIP of an
/// organization, which is state a call establishes: with the Organizations control plane
/// mounted but no organization created from this account, every Organizations-form
/// target — the very id that control plane later mints included — is denied, and the
/// same id is served once `CreateOrganization` has made this account the management
/// account of it.
///
/// The rows span both Organizations alternatives of `TargetId`'s pattern and each
/// published bound of its `@length(min: 12, max: 32)`, plus an organization id that
/// exists in one emulator and not in the other, so no row can be passing because of an
/// id's shape.
#[tokio::test]
async fn an_organizations_target_is_denied_until_the_organization_is_created() {
    // Both control planes mounted and nothing else: mounting Organizations establishes
    // no membership, so this account belongs to no organization yet.
    let (pending_router, pending_engine) =
        crate::signin_harness::emulator_with_organizations_mounted();

    // A second emulator where the organization HAS been created, so both halves of the
    // case can name the one id this control plane mints for the emulated account.
    let with = emulator_with_organization().await;

    let mut rows: Vec<(String, String)> = foreign_targets()
        .into_iter()
        .map(|(case, target_id)| (case.to_string(), target_id))
        .collect();
    rows.push((
        "the id of an organization this account has not created".to_string(),
        with.organization_id.clone(),
    ));

    for (case, target_id) in &rows {
        let request = json!({"targetId": target_id});
        for (operation, answer) in [
            (
                "put",
                put_configuration(&pending_router, request.clone()).await,
            ),
            (
                "get",
                get_configuration(&pending_router, request.clone()).await,
            ),
            (
                "delete",
                delete_configuration(&pending_router, request.clone()).await,
            ),
        ] {
            assert_eq!(
                (
                    answer.status,
                    answer.error_code(),
                    answer.error_member(),
                    answer.message()
                ),
                (
                    400,
                    "AccessDeniedException",
                    INSUFFICIENT_PERMISSIONS_CODE,
                    NOT_AUTHORIZED_MESSAGE
                ),
                "case {case:?}: with no organization created, the {operation} of \
                 {target_id:?} must be denied with {NOT_AUTHORIZED_MESSAGE:?}, got {} \
                 with {:?}",
                answer.status,
                answer.body
            );
        }
    }

    assert_eq!(
        configured_targets(&pending_engine),
        Vec::<String>::new(),
        "a denied put must record no configuration at all"
    );

    // The account's own account-form target names no Organizations entity, so it is
    // served whether or not this account is in an organization.
    let own_account = put_configuration(&pending_router, json!({})).await;
    assert_eq!(
        own_account.status, 200,
        "case \"the caller's own account\": an account-form target names no organization \
         and must be served with none created, got {} with {:?}",
        own_account.status, own_account.body
    );

    // The same organization id, in the emulator whose account created it: authority over
    // it came from that call, and from nothing else.
    let served =
        put_configuration(&with.router, json!({"targetId": with.organization_id.clone()})).await;
    assert_eq!(
        served.status,
        200,
        "case {:?}: the organization this account created must be served, got {} with {:?}",
        with.organization_id,
        served.status,
        served.body
    );
}

/// With no Organizations control plane to resolve an entity through, no `o-`/`r-` target
/// is the caller's: the authorization check fails CLOSED rather than admitting the
/// request. An account-form target is unaffected — it names no Organizations entity.
#[tokio::test]
async fn an_organizations_target_is_denied_when_no_organization_is_recorded() {
    let router = crate::signin_harness::emulator();

    for (case, target_id) in foreign_targets() {
        let answer = put_configuration(&router, json!({"targetId": target_id})).await;
        assert_eq!(
            (answer.status, answer.error_code(), answer.message()),
            (400, "AccessDeniedException", NOT_AUTHORIZED_MESSAGE),
            "case {case:?}: with no organization recorded, {target_id:?} must be denied, \
             got {} with {:?}",
            answer.status,
            answer.body
        );
    }

    let served = put_configuration(&router, json!({"targetId": EMULATOR_ACCOUNT})).await;
    assert_eq!(
        served.status, 200,
        "an account-form target names no organization and must still be served, got {} \
         with {:?}",
        served.status,
        served.body
    );
}

/// The not-found refusal is worded as AWS words it: the fixed sentence in full, with no
/// part of the request interpolated into it — whichever reader answers it, and whether
/// the target was never configured or was configured and released.
#[tokio::test]
async fn a_missing_configuration_is_refused_in_aws_s_own_words() {
    let with = emulator_with_organization().await;
    let router = &with.router;

    for (case, request, target_id, _) in authorized_targets(&with) {
        // Never configured.
        for (reader, answer) in [
            ("get", get_configuration(router, request.clone()).await),
            ("delete", delete_configuration(router, request.clone()).await),
        ] {
            assert_not_found(case, reader, &answer);
            assert!(
                !answer.message().contains(&target_id),
                "case {case:?}: the {reader}'s refusal must interpolate no part of the \
                 request, got {:?}",
                answer.message()
            );
        }

        // Configured and then released: the same sentence, from the same two readers.
        assert_eq!(
            put_configuration(router, request.clone()).await.status,
            200,
            "case {case:?}: a put must be accepted"
        );
        assert_eq!(
            delete_configuration(router, request.clone()).await.status,
            200,
            "case {case:?}: the delete must be accepted"
        );
        for (reader, answer) in [
            ("get", get_configuration(router, request.clone()).await),
            ("delete", delete_configuration(router, request.clone()).await),
        ] {
            assert_not_found(case, reader, &answer);
            assert!(
                !answer.message().contains(&target_id),
                "case {case:?}: the {reader}'s refusal after the delete must interpolate \
                 no part of the request, got {:?}",
                answer.message()
            );
        }
    }
}
