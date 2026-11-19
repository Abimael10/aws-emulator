// SPDX-License-Identifier: CC0-1.0

//! `CreateVolume` as a PERMISSION PROBE: what a `DryRun` is answered when the policies in
//! force do not allow the action, and what the blob such a refusal hands the caller decodes
//! back to — driven end to end over the wire EC2 speaks (`ec2Query`), with the role, its
//! policy and the session held by the IAM and STS services mounted on the SAME engine, so the
//! decision under test is the one EC2's gate took about the credential `sts:AssumeRole`
//! minted.
//!
//! THE RULE UNDER TEST, in three parts:
//!
//! * `DryRun` is an AUTHORIZATION question answered BEFORE the dry-run short-circuit —
//!   "checks whether you have the required permissions for the action, without actually
//!   making the request, and provides an error response. If you have the required
//!   permissions, the error response is `DryRunOperation`. Otherwise, it is
//!   `UnauthorizedOperation`" (`CreateVolumeRequest$DryRun`) — so a principal the documents
//!   in force do not permit is refused HTTP 403 `UnauthorizedOperation` whether or not the
//!   flag is set, and `DryRunOperation` is answered only to a principal that IS permitted.
//!   The refusal names the principal, the action, the resource the decision was taken on and
//!   the clause saying WHICH document withheld the grant, and nothing is created in either
//!   outcome.
//! * A volume create is judged on the resource it would CREATE: the `volume` resource type
//!   the IAM Service Authorization Reference lists for `CreateVolume`
//!   (`arn:${Partition}:ec2:${Region}:${Account}:volume/${VolumeId}`), which the request
//!   carries no id for — there is no volume until the call succeeds — so the decision is
//!   taken on that type with every id, and that is the resource the refusal names.
//! * The refusal closes with an encoded authorization failure message — "only certain Amazon
//!   Web Services operations return an encoded message… the documentation for an individual
//!   operation indicates whether that operation returns an encoded message"
//!   (`sts:DecodeAuthorizationMessage`) — whichever document in force reached the decision,
//!   and that blob decodes, through STS, to the decision EC2 took: not allowed, explicitly
//!   denied exactly when a `Deny` said so, the principal's ARN and unique id, the BARE
//!   operation name as the requested action, and the resource it was judged on.
//!
//! The rows span what the rule generalises over: WHICH document withheld the grant (the
//! absence of any allow in the role's own identity policy, an explicit `Deny` in the session
//! policy the mint passed), whether the request sets the flag at all, which Availability Zone
//! it names, how large a volume it asks for and whether it names a volume type. No row is the
//! specification: each is one input along the rule.
//!
//! The pieces of the drive that are about no one operation — the role with its own document,
//! the session minted of it, the split of a refusal into its sentence and its blob, and the
//! decode of that blob — come from [`super::denied_launch_probe_encoded_message`], which
//! pins the same rule on the other operation whose refusals carry the trailer, rather than
//! from a second copy of them here.

use super::account_wide_read_authorization::{
    call_as, ec2_as, element, emulator, statement, ACCOUNT_KEY_ID, REGION, STS_API_VERSION,
};
use super::denied_launch_probe_encoded_message::{
    assumed, decoded, role_with, sentence_and_blob,
};
use super::ec2_query::{elements, error_code, error_message};

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// EC2's error code for a `DryRun` whose caller DOES hold the action, the status it carries
/// and the sentence it states.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// What a refusal that hands the caller an encoded authorization failure message closes with,
/// before the blob itself — written out here as AWS words it.
const ENCODED_MESSAGE_MARKER: &str = " Encoded authorization failure message: ";

/// The IAM action a volume create is authorized under, and the bare operation name the
/// decoded decision reports as the requested action.
const CREATE_VOLUME_ACTION: &str = "ec2:CreateVolume";
const CREATE_VOLUME_OPERATION: &str = "CreateVolume";

/// The one action the role of the no-allow rows is allowed, and nothing else: the grant that
/// proves its sessions are live without saying anything about EC2.
const ALLOWED_ACTION: &str = "sts:GetCallerIdentity";

/// The `volume` resource a create in [`REGION`] is judged on: the type with every id, the
/// request naming no volume that exists yet.
fn judged_volume_resource() -> String {
    ::engine::emulator_arn("ec2", REGION, "volume/*")
}

/// The clause a refusal closes with for a request no readable identity-based document allows,
/// read from the shared wording every service that reaches that decision states, so this
/// suite asserts the clause rather than a second spelling of it.
fn no_identity_policy_allows(action: &str) -> String {
    service_common::iam_authorization::no_identity_policy_allows(action)
}

/// The clause a refusal reached by a session policy's explicit `Deny` closes with, from that
/// same shared wording.
fn session_explicit_deny() -> String {
    service_common::iam_authorization::SESSION_POLICY_EXPLICIT_DENY.to_string()
}

/// Which document withheld the grant on a row, which decides both the clause the refusal
/// closes with and whether the decoded decision reports an explicit deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecidedBy {
    /// Nothing in the role's own identity policy allows the action.
    NoAllow,
    /// The session policy the mint passed states a `Deny` about it.
    SessionDeny,
}

/// The Availability Zones of [`REGION`] whose state is `available`, which is what a volume
/// create may name: read back over the wire rather than written here.
async fn available_zones(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (status, body) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "the Region's zones must be readable, got {status} {body}"
    );
    let states = elements(&body, "zoneState");
    assert!(
        !states.is_empty() && states.iter().all(|state| state == "available"),
        "expected every reported zone to be available, got {states:?}: {body}"
    );
    let names = elements(&body, "zoneName");
    assert!(
        names.len() >= 2,
        "expected the Region to publish at least two zones, got {names:?}"
    );
    names
}

/// The ids of the volumes the account holds, as the account's own credential reads them.
async fn volume_ids(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) -> Vec<String> {
    let (status, body) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeVolumes", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the volumes must be readable, got {status} {body}"
    );
    elements(&body, "volumeId")
}

/// A session policy allowing the read a row proves its credential live with and denying the
/// volume create, with `denied` as the `Action` of the `Deny` statement.
fn allow_then_deny(denied: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":[\"ec2:DescribeVolumes\",\"ec2:CreateVolume\"],\
         \"Resource\":\"*\"}},\
         {{\"Effect\":\"Deny\",\"Action\":\"{denied}\",\"Resource\":\"*\"}}]}}"
    )
}

/// One row: the probe one session makes, and which document withheld the grant from it.
struct Row {
    case: &'static str,
    /// The role the session is of, and the session's own name — both carried by the principal
    /// a refusal names.
    role: &'static str,
    session: &'static str,
    /// Which document reached the decision.
    decided_by: DecidedBy,
    /// Which of the Region's zones the create names.
    zone: usize,
    /// The size the create asks for, in gibibytes.
    size: &'static str,
    /// The volume type the create names, `None` for a create naming none.
    volume_type: Option<&'static str>,
    /// Whether the request sets the `DryRun` flag.
    dry_run: bool,
}

/// RULE: a `CreateVolume` the policies in force do not allow is refused `UnauthorizedOperation`
/// on the `volume` resource it would create — with the flag set exactly as without it — and the
/// refusal hands the caller an encoded authorization failure message that decodes to that very
/// decision. Nothing is created.
#[tokio::test]
async fn a_denied_volume_probe_hands_the_caller_the_decision_to_decode() {
    let router = emulator();
    let zones = available_zones(&router).await;
    let rows = vec![
        Row {
            case: "no allow at all: one gibibyte in the first zone",
            role: "volume-probe-no-allow",
            session: "denied-one-gib",
            decided_by: DecidedBy::NoAllow,
            zone: 0,
            size: "1",
            volume_type: None,
            dry_run: true,
        },
        Row {
            case: "no allow at all: a larger volume of a named type in another zone",
            role: "volume-probe-no-allow-larger",
            session: "denied-hundred-gib",
            decided_by: DecidedBy::NoAllow,
            zone: 1,
            size: "100",
            volume_type: Some("gp3"),
            dry_run: true,
        },
        Row {
            case: "no allow at all: the same create with the flag unset",
            role: "volume-probe-no-allow-unflagged",
            session: "denied-unflagged",
            decided_by: DecidedBy::NoAllow,
            zone: 0,
            size: "8",
            volume_type: Some("st1"),
            dry_run: false,
        },
        Row {
            case: "the session policy's explicit deny over a role allowed every action",
            role: "volume-probe-session-deny",
            session: "denied-by-scope",
            decided_by: DecidedBy::SessionDeny,
            zone: 1,
            size: "1",
            volume_type: None,
            dry_run: true,
        },
    ];

    for row in rows {
        let Row {
            case,
            role,
            session,
            decided_by,
            zone,
            size,
            volume_type,
            dry_run,
        } = row;

        // The role, its own document and the session minted of it: a role allowed ONLY an STS
        // read for the rows the absence of a grant decides, and a role allowed every action of
        // this service for the row its session policy decides.
        let (identity, scope) = match decided_by {
            DecidedBy::NoAllow => (statement("Allow", ALLOWED_ACTION, "*"), None),
            DecidedBy::SessionDeny => (
                statement("Allow", "ec2:*", "*"),
                Some(allow_then_deny(CREATE_VOLUME_ACTION)),
            ),
        };
        role_with(&router, case, role, &identity).await;
        let (akid, principal, assumed_role_id) =
            assumed(&router, case, role, session, scope.as_deref()).await;

        // The credential is live and the only variable is the authorization of the EC2 call:
        // the one thing each row's documents DO allow is proven on that same credential first.
        match decided_by {
            DecidedBy::NoAllow => {
                let (status, who) = call_as(
                    &router,
                    "sts",
                    STS_API_VERSION,
                    &akid,
                    "GetCallerIdentity",
                    &[],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the allowed STS read must be served on the same credential, \
                     got {status} {who}"
                );
                assert_eq!(
                    element(case, &who, "Arn"),
                    principal,
                    "case {case:?}: the credential must speak for the session, got {who}"
                );
            }
            DecidedBy::SessionDeny => {
                let (status, read) = ec2_as(&router, &akid, "DescribeVolumes", &[]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the allowed read must be served on the same credential, got \
                     {status} {read}"
                );
            }
        }

        let held_before = volume_ids(&router, case).await;

        let mut params: Vec<(&str, &str)> = vec![
            ("AvailabilityZone", zones[zone].as_str()),
            ("Size", size),
        ];
        if let Some(volume_type) = volume_type {
            params.push(("VolumeType", volume_type));
        }
        if dry_run {
            params.push(("DryRun", "true"));
        }
        let (status, refused) = ec2_as(&router, &akid, CREATE_VOLUME_OPERATION, &params).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (403, UNAUTHORIZED_OPERATION),
            "case {case:?}: a create the documents in force do not allow must be refused as \
             unauthorized, got {status} {refused}"
        );

        let (sentence, blob) = sentence_and_blob(case, &error_message(&refused));
        let judged = judged_volume_resource();
        let clause = match decided_by {
            DecidedBy::NoAllow => no_identity_policy_allows(CREATE_VOLUME_ACTION),
            DecidedBy::SessionDeny => session_explicit_deny(),
        };
        assert_eq!(
            sentence,
            format!(
                "You are not authorized to perform this operation. User: {principal} is not \
                 authorized to perform: {CREATE_VOLUME_ACTION} on resource: {judged} {clause}."
            ),
            "case {case:?}: expected the refusal to name the volume resource and the document \
             that withheld the grant, got {refused}"
        );

        // The blob decodes, through the service that issued it, to the decision EC2 took.
        let document = decoded(&router, case, &blob).await;
        assert_eq!(
            (
                document["allowed"].as_bool(),
                document["explicitDeny"].as_bool(),
                document["context"]["principal"]["arn"].as_str(),
                document["context"]["principal"]["id"].as_str(),
                document["context"]["action"].as_str(),
                document["context"]["resource"].as_str(),
            ),
            (
                Some(false),
                Some(decided_by == DecidedBy::SessionDeny),
                Some(principal.as_str()),
                Some(assumed_role_id.as_str()),
                Some(CREATE_VOLUME_OPERATION),
                Some(judged.as_str()),
            ),
            "case {case:?}: expected the decision the gate took, got {document}"
        );

        // Nothing was created: a refused request performs no work at all, flag or no flag.
        let held_after = volume_ids(&router, case).await;
        assert_eq!(
            held_after, held_before,
            "case {case:?}: a refused create must create nothing, expected the volumes \
             {held_before:?}, got {held_after:?}"
        );
    }
}

/// RULE (the control that separates authorization from the work): the same create by a
/// principal the policies in force DO allow is answered `DryRunOperation` — so the probe
/// reports the permission rather than the refusal — carries no encoded authorization failure
/// message, there being no decision to diagnose, and still creates nothing.
#[tokio::test]
async fn an_authorized_volume_probe_answers_the_dry_run_verdict_and_creates_nothing() {
    let router = emulator();
    let zones = available_zones(&router).await;
    let case = "a role whose own policy allows the volume create";
    let role = "volume-probe-allowed";
    role_with(
        &router,
        case,
        role,
        &statement("Allow", CREATE_VOLUME_ACTION, "*"),
    )
    .await;
    let (akid, _, _) = assumed(&router, case, role, "allowed-probe", None).await;
    let held_before = volume_ids(&router, case).await;

    let (status, answered) = ec2_as(
        &router,
        &akid,
        CREATE_VOLUME_OPERATION,
        &[
            ("AvailabilityZone", zones[0].as_str()),
            ("Size", "1"),
            ("DryRun", "true"),
        ],
    )
    .await;
    let message = error_message(&answered);
    assert_eq!(
        (status, error_code(&answered).as_str(), message.as_str()),
        (DRY_RUN_STATUS, DRY_RUN_OPERATION, DRY_RUN_MESSAGE),
        "case {case:?}: expected the dry-run verdict, got {status} {answered}"
    );
    assert!(
        !message.contains(ENCODED_MESSAGE_MARKER),
        "case {case:?}: an authorized probe diagnoses nothing, got {message:?}"
    );

    let held_after = volume_ids(&router, case).await;
    assert_eq!(
        held_after, held_before,
        "case {case:?}: a dry run must create nothing, expected the volumes {held_before:?}, \
         got {held_after:?}"
    );
}
