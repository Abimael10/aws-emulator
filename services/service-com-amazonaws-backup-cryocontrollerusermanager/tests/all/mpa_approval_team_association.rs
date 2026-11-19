// SPDX-License-Identifier: CC0-1.0

//! `AssociateBackupVaultMpaApprovalTeam`: the approval team a vault is put under has to be
//! a team the service that MINTS approval teams holds.
//!
//! The rule under test: a member that names a resource of another service is not accepted
//! on its FORM — the owning service is asked whether it holds the resource, and a value it
//! answers it holds none under is refused as that member's value, with nothing recorded.
//! So a vault reported as subject to multi-party approval has an approval team, and a
//! caller that names a team that does not exist gets the modelled refusal to branch on
//! instead of a success it cannot detect.
//!
//! Multi-party approval is therefore MOUNTED beside AWS Backup on one engine, with AWS
//! Organizations beside it (creating an approval team is restricted to the management
//! account of the organization Organizations records), so every team a case associates is
//! one created through that service's own create over its own wire and resolved through its
//! own read — never a value this suite made up. Every Backup request goes over real HTTP
//! through the router, so what is asserted is what a `backup` SDK client sees.
//!
//! The tables span what the rule generalises over: the team-name forms
//! `ApprovalTeamName`'s pattern admits (`^[a-zA-Z0-9._-]+$`, `length(max: 64)`), and the
//! ways a well-formed team ARN can name no team — one never created, one of another
//! Region, one of another account, and one the approval service no longer holds.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logically_air_gapped_vault::create_air_gapped;
use crate::vault_and_backup_job::{call, call_reported, error_code, string_at, ACCOUNT, REGION};

/// The error shape, and the `Code`, every refusal in this suite carries.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The `Code` AWS Backup fills in for "a member of the request carries a value this service
/// cannot accept", with the member named in `Context`.
const INVALID_PARAMETER_VALUE: &str = "INVALID_PARAMETER_VALUE";

/// The member every refusal here blames.
const MEMBER: &str = "MpaApprovalTeamArn";

/// The retention window every air-gapped vault in this suite is created with: the
/// documented floor of `MinRetentionDays` and a year.
const WINDOW: &str = r#"{"MinRetentionDays": 7, "MaxRetentionDays": 365}"#;

/// The routing header AWS Organizations is reached by (that crate's `OPERATIONS.md`:
/// `POST /` with `X-Amz-Target: AWSOrganizationsV20161128.<Op>`).
const ORGANIZATIONS_TARGET: &str = "AWSOrganizationsV20161128.CreateOrganization";

/// AWS Backup, multi-party approval and AWS Organizations on ONE engine, plus that engine's
/// router: the team a vault is put under is the approval service's own resource, so that
/// service has to be mounted for a team to resolve at all, and Organizations has to be
/// mounted for a team to be creatable in the first place.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    /// The three services as the emulator mounts them, with the caller's own account made
    /// the management account of an organization through Organizations' OWN API — the
    /// state in which a client may create an approval team.
    async fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_mpa_awsfluffycoreservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        let emulator = Self { router, engine };
        emulator.join_an_organization().await;
        emulator
    }

    /// "The account whose user is calling the CreateOrganization operation automatically
    /// becomes the management account of the new organization" — so one request over
    /// Organizations' own `awsJson1_1` wire is what makes the caller able to create teams.
    async fn join_an_organization(&self) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("organizations.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", ORGANIZATIONS_TARGET)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTONLY/20260101/{REGION}/organizations/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=00"
                ),
            )
            .body(bytes::Bytes::from_static(b"{}"))
            .expect("a well-formed Organizations request");
        let response = self.router.dispatch(request).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "setup: CreateOrganization must succeed so a team can be created"
        );
    }

    /// One `restJson1` request to the approval service.
    async fn mpa(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the body"))
        });
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("mpa.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/mpa/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("a well-formed approval-service request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create an approval team named `name` through the approval service's OWN create, and
    /// answer the ARN it minted — read off its response, so the value a Backup request
    /// names is one that service really holds.
    ///
    /// The attached policy comes from that service's own published catalog, listed over the
    /// wire: an attachment it cannot resolve is refused, so the reference is taken from the
    /// catalog rather than spelled here.
    async fn create_approval_team(&self, name: &str) -> String {
        let (status, listed) = self.mpa("POST", "/policies/?List", None).await;
        assert_eq!(
            status, 200,
            "setup: ListPolicies must answer the published catalog, got {status} {listed:?}"
        );
        let policy = listed
            .get("Policies")
            .and_then(Value::as_array)
            .and_then(|policies| policies.first())
            .map(|policy| string_at(policy, "Arn"))
            .unwrap_or_default();
        assert!(
            !policy.is_empty(),
            "setup: the catalog reported no policy to attach: {listed:?}"
        );
        let body = json!({
            "Name": name,
            "Description": "the team a vault's multi-party approval is subject to",
            "ApprovalStrategy": { "MofN": { "MinApprovalsRequired": 1 } },
            "Approvers": [{
                "PrimaryIdentityId": "identity-0001",
                "PrimaryIdentitySourceArn":
                    format!("arn:aws:mpa:{REGION}:{ACCOUNT}:identity-source/ssoins-0001"),
            }],
            "Policies": [{ "PolicyArn": format!("{policy}/$DEFAULT") }],
        });
        let (status, created) = self.mpa("POST", "/approval-teams", Some(body)).await;
        assert_eq!(
            status, 200,
            "setup: CreateApprovalTeam {name} must succeed, got {status} {created:?}"
        );
        let arn = string_at(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "setup: CreateApprovalTeam answered no Arn: {created:?}"
        );
        arn
    }

    /// Drop the stored team `arn` from the approval service's records, so the ARN keeps its
    /// form while naming nothing.
    ///
    /// Written through that service's own store seam: the modelled deletion of an ACTIVE
    /// team is a multi-step approval handshake of its own, and what this row needs is only
    /// the state it ends in — a team the approval service no longer holds.
    fn forget_approval_team(&self, arn: &str) {
        let store = service_com_amazonaws_mpa_awsfluffycoreservice::state::service_state(
            &*self.engine,
        )
        .expect("the approval-team store");
        let mut guard = store.lock();
        guard.remove_team(arn);
    }

    /// `PUT /backup-vaults/{BackupVaultName}/mpaApprovalTeam`, as the model binds the
    /// association.
    async fn associate(&self, vault: &str, team_arn: &str) -> (u16, String, Value) {
        call_reported(
            &self.router,
            "PUT",
            &format!("/backup-vaults/{vault}/mpaApprovalTeam"),
            Some(&json!({ "MpaApprovalTeamArn": team_arn }).to_string()),
        )
        .await
    }

    /// `POST /backup-vaults/{BackupVaultName}/mpaApprovalTeam?delete`, as the model binds
    /// the disassociation.
    async fn disassociate(&self, vault: &str) -> (u16, Value) {
        call(
            &self.router,
            "POST",
            &format!("/backup-vaults/{vault}/mpaApprovalTeam?delete"),
            None,
        )
        .await
    }

    /// Create a logically air-gapped vault — the one type multi-party approval is a
    /// facility of — under `name`.
    async fn create_vault(&self, name: &str) {
        let (status, created) = create_air_gapped(&self.router, name, WINDOW).await;
        assert_eq!(
            status, 200,
            "setup: CreateLogicallyAirGappedBackupVault {name} must succeed, got {status} \
             {created:?}"
        );
    }

    /// `MpaApprovalTeamArn` as `DescribeBackupVault` reports it, or `"<unset>"` when the
    /// vault carries none.
    async fn reported_team(&self, vault: &str) -> String {
        let (status, described) =
            call(&self.router, "GET", &format!("/backup-vaults/{vault}"), None).await;
        assert_eq!(
            status, 200,
            "DescribeBackupVault {vault} must answer 200, got {status} {described:?}"
        );
        described
            .get("MpaApprovalTeamArn")
            .and_then(Value::as_str)
            .unwrap_or("<unset>")
            .to_string()
    }
}

/// Assert one association was refused as the member that names the team, reporting the
/// class's token, the member in `Context` and the condition's fixed sentence.
fn assert_refused(case: &str, answered: (u16, String, Value)) {
    let (status, errortype, body) = answered;
    let reported = (
        status,
        errortype.clone(),
        error_code(&body),
        string_at(&body, "Code"),
        string_at(&body, "Context"),
        string_at(&body, "Message"),
    );
    assert_eq!(
        reported,
        (
            400,
            INVALID_PARAMETER.to_string(),
            INVALID_PARAMETER.to_string(),
            INVALID_PARAMETER_VALUE.to_string(),
            MEMBER.to_string(),
            format!("Parameter {MEMBER} is invalid."),
        ),
        "case {case:?}: expected 400 {INVALID_PARAMETER} blaming {MEMBER}, got {status} \
         {errortype} {body:?}"
    );
}

/// The team-name forms `ApprovalTeamName` admits (`^[a-zA-Z0-9._-]+$`,
/// `length(max: 64)`), each created through the approval service and then put in charge of
/// a vault.
fn team_names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain team name", "restore-access-team".to_string()),
        (
            "the dot and underscore the pattern admits",
            "Team.Alpha_1".to_string(),
        ),
        ("a one-character team name", "a".to_string()),
        (
            // `ApprovalTeamName` is `length(max: 64)` — the published maximum.
            "a team name at the published maximum length",
            "n".repeat(64),
        ),
    ]
}

/// A team the approval service HOLDS is recorded on the vault and read back; the same
/// vault is then refused a well-formed ARN of a team it does not hold, and keeps the team
/// it already had.
#[tokio::test]
async fn only_a_team_the_approval_service_holds_is_recorded_on_a_vault() {
    for (case, name) in team_names() {
        let emulator = Emulator::new().await;
        emulator.create_vault("team-vault").await;
        let team_arn = emulator.create_approval_team(&name).await;

        let (status, errortype, associated) = emulator.associate("team-vault", &team_arn).await;
        assert_eq!(
            (status, errortype.as_str()),
            (204, "<unset>"),
            "case {case:?} ({team_arn}): expected the modelled 204, got {status} \
             {associated:?}"
        );
        let reported = emulator.reported_team("team-vault").await;
        assert_eq!(
            reported, team_arn,
            "case {case:?}: the describe must report the team the association stored, got \
             {reported}"
        );

        // The same form, naming a team this account does not hold: refused, and the team
        // the vault already has survives the refusal.
        let unheld = format!("{team_arn}-no-such-team");
        assert_refused(
            &format!("{case}, a team that was never created"),
            emulator.associate("team-vault", &unheld).await,
        );
        let reported = emulator.reported_team("team-vault").await;
        assert_eq!(
            reported, team_arn,
            "case {case:?}: a refused association must leave the stored team alone, got \
             {reported}"
        );
    }
}

/// The ways a value of the form a team's ARN takes can still name no team of this account.
///
/// `arn` is built from the team the approval service holds, so each row differs from a
/// resolvable ARN in exactly the part it names.
fn unresolvable_rows(held: &str, held_name: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "a team that was never created",
            format!("arn:aws:mpa:{REGION}:{ACCOUNT}:approval-team/8f14e45f-ceea-467a-9ba3-6b1f3c9a0d21"),
        ),
        (
            "the held team's name in another Region",
            format!("arn:aws:mpa:eu-central-1:{ACCOUNT}:approval-team/{held_name}"),
        ),
        (
            "the held team's name in another account",
            format!("arn:aws:mpa:{REGION}:210987654321:approval-team/{held_name}"),
        ),
        (
            "the held team's name in another partition",
            held.replace("arn:aws:", "arn:aws-us-gov:"),
        ),
    ]
}

/// A well-formed team ARN that names no team of this account is refused, nothing is
/// recorded, and the disassociation of a vault that never carried a team is the modelled
/// no-op success — the whole sequence a client wiring multi-party approval onto a vault
/// drives.
#[tokio::test]
async fn a_team_the_approval_service_does_not_hold_is_refused_and_records_nothing() {
    let emulator = Emulator::new().await;
    emulator.create_vault("bare-vault").await;
    let held_name = "held-team";
    let held = emulator.create_approval_team(held_name).await;

    for (case, team_arn) in unresolvable_rows(&held, held_name) {
        assert_refused(case, emulator.associate("bare-vault", &team_arn).await);
        let reported = emulator.reported_team("bare-vault").await;
        assert_eq!(
            reported, "<unset>",
            "case {case:?}: a refused association must record no team, got {reported}"
        );
    }

    // A team the approval service no longer holds stops resolving, so the SAME ARN that
    // was accepted is refused afterwards.
    let (status, _errortype, associated) = emulator.associate("bare-vault", &held).await;
    assert_eq!(
        status, 204,
        "the held team must be associable, got {status} {associated:?}"
    );
    emulator.forget_approval_team(&held);
    emulator.create_vault("second-vault").await;
    assert_refused(
        "a team the approval service no longer holds",
        emulator.associate("second-vault", &held).await,
    );
    let reported = emulator.reported_team("second-vault").await;
    assert_eq!(
        reported, "<unset>",
        "a team that no longer exists must record nothing, got {reported}"
    );

    // And the disassociation of a vault that never carried a team is an idempotent no-op,
    // whichever way the vault came to carry none.
    for attempt in 1..=2 {
        let (status, answered) = emulator.disassociate("second-vault").await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (204, "<none>"),
            "attempt {attempt}: expected the modelled 204 no-op, got {status} {answered:?}"
        );
        let reported = emulator.reported_team("second-vault").await;
        assert_eq!(
            reported, "<unset>",
            "attempt {attempt}: the vault must still report no team, got {reported}"
        );
    }
}

/// A vault's team can be REPLACED, and only by another team the approval service holds: the
/// replacement is read back, and a refused replacement leaves the team in place.
#[tokio::test]
async fn a_team_is_replaced_only_by_another_team_the_approval_service_holds() {
    let emulator = Emulator::new().await;
    emulator.create_vault("replacing-vault").await;
    let first = emulator.create_approval_team("first-team").await;
    let (status, _errortype, associated) = emulator.associate("replacing-vault", &first).await;
    assert_eq!(
        status, 204,
        "setup: the first team must associate, got {status} {associated:?}"
    );

    let unheld = format!("arn:aws:mpa:{REGION}:{ACCOUNT}:approval-team/second-team");
    assert_refused(
        "a replacement team that was never created",
        emulator.associate("replacing-vault", &unheld).await,
    );
    let reported = emulator.reported_team("replacing-vault").await;
    assert_eq!(
        reported, first,
        "a refused replacement must leave the first team in place, got {reported}"
    );

    let second = emulator.create_approval_team("second-team").await;
    assert_eq!(
        second, unheld,
        "the created team should carry the ARN the refused request named, got {second}"
    );
    let (status, _errortype, replaced) = emulator.associate("replacing-vault", &second).await;
    assert_eq!(
        status, 204,
        "the created replacement must associate, got {status} {replaced:?}"
    );
    let reported = emulator.reported_team("replacing-vault").await;
    assert_eq!(
        reported, second,
        "the describe must report the replacement, got {reported}"
    );
}
