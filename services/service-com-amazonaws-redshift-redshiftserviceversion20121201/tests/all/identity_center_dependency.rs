// SPDX-License-Identifier: CC0-1.0

//! The IAM Identity Center dependency an application registration has.
//!
//! RULE under test: the managed application a Redshift (or Query Editor) IAM Identity
//! Center application is backed by is a resource OF the Identity Center instance the
//! create names, so the create performs `sso:CreateApplication` in that instance on the
//! caller's behalf — an instance the account's Identity Center does not hold has no
//! application namespace to create one in, the dependent call is denied, and the create
//! is refused with the modelled `DependentServiceAccessDeniedFault` (HTTP 403) carrying
//! the standard IAM authorization-failure sentence: the CALLING PRINCIPAL, the action,
//! the Identity Center application resource of the named instance, and that no
//! resource-based policy allows that action. Nothing is registered, and a create naming
//! an instance the account DOES hold is served.
//!
//! RULE under test (the refusal's FORM): that sentence was answered by the DEPENDENT
//! service, not by Redshift, so it is reported relayed — the downstream sentence, then
//! the envelope the SDK layer states around a failed relayed call, naming the service
//! that answered, the status it answered with, that call's own request id and how many
//! attempts it took. A refusal Redshift answers ITSELF carries no such envelope.
//!
//! The rows differ along what the rule generalises over: which operation registers the
//! application (both creates that take an `IdcInstanceArn`), the form of the instance id
//! and the partition the instance ARN carries, whether the instance is one the account
//! holds, and what else the create asks for (optional members, tags, token issuers) —
//! which the dependency cannot depend on.
//!
//! Every request goes over the real awsQuery wire through the shared harness
//! ([`crate::cluster_lifecycle`]), so what is asserted is what an SDK sees.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator, IDENTITY_CENTER_INSTANCES, REGION};

/// The IAM Identity Center action a registration performs on the caller's behalf, as IAM
/// names it.
const ACTION: &str = "sso:CreateApplication";

/// The error code and HTTP status the dependent denial is relayed on
/// (`smithy_query(query="operation", operation="CreateRedshiftIdcApplication,CreateQev2IdcApplication")`
/// — `DependentServiceAccessDeniedFault`, httpError 403, awsQueryError code
/// `DependentServiceAccessDenied`).
const DENIED_CODE: &str = "DependentServiceAccessDenied";
const DENIED_STATUS: u16 = 403;

/// The IAM role an application assumes to invoke the IAM Identity Center API.
const IAM_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/RedshiftIdcRole";

/// The dependent service the denied nested create was made to, named the way the SDK
/// layer that relayed the call reports it — neither that endpoint's signing name (`sso`)
/// nor the casing of its `sdkId`.
const DEPENDENT_SERVICE: &str = "SsoAdmin";

/// The HTTP status the dependent service answered the nested create with.
///
/// The instance the request named resolves to no instance of the account, which that
/// service reports on the not-found channel its own `CreateApplication` declares; it is
/// an awsJson1_1 endpoint whose wire answers carry HTTP 400 for a refusal of the request
/// whatever 4xx the shape models (the reading that service's own crate takes of its
/// answers, `services/service-com-amazonaws-ssoadmin-swbexternalservice/src/scaffolded.rs`).
const DEPENDENT_STATUS: &str = "400";

/// How many attempts the relayed call took: a refusal of the caller's own input is not
/// retried, so the call that came back refused was the first and only one.
const DEPENDENT_ATTEMPTS: &str = "1";

/// Instance ARNs that name no instance this account holds, in the forms the owning
/// service's `InstanceArn` pattern admits (`^arn:[\w-]+:sso:::instance/(sso)?ins-[a-zA-Z0-9-.]{16}$`):
/// both spellings of the instance id, one carrying the punctuation the pattern allows,
/// and one in another partition — whose id is held in `aws`, so the partition is part of
/// what identifies the instance.
const NOT_HELD: [&str; 4] = [
    "arn:aws:sso:::instance/ssoins-0000000000000000",
    "arn:aws:sso:::instance/ins-ffffffffffffffff",
    "arn:aws:sso:::instance/ssoins-z.y-x.w-v.u-t1s2",
    "arn:aws-us-gov:sso:::instance/ssoins-1111111111111111",
];

/// The instance id inside an instance ARN.
fn instance_id(instance_arn: &str) -> &str {
    instance_arn
        .rsplit('/')
        .next()
        .expect("an instance ARN names an instance")
}

/// The partition an ARN carries.
fn partition(instance_arn: &str) -> &str {
    instance_arn
        .split(':')
        .nth(1)
        .expect("an ARN carries a partition")
}

/// The sentence IAM states for a denied cross-service action, written here from the rule
/// rather than from the implementation: the calling principal, the action, the Identity
/// Center APPLICATION resource of the named instance (the account and the application id
/// are the wildcards the decision is evaluated over, since the application has no id
/// until the call that would create it succeeds), and that no resource-based policy
/// allows it.
fn expected_message(principal: &str, instance_arn: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {ACTION} on resource: \
         arn:{}:sso::*:application/{}/* because no resource-based policy allows the \
         {ACTION} action",
        partition(instance_arn),
        instance_id(instance_arn)
    )
}

/// A relayed refusal, as a client parses it back out of the message: the reason the
/// dependent service itself gave, then the four facts about the call it came from.
#[derive(Debug)]
struct Relayed {
    reason: String,
    service: String,
    status: String,
    request_id: String,
    attempts: String,
}

/// `message` split on the envelope literals the rule states — written here from the rule
/// rather than from the implementation — or `None` when it carries no envelope at all.
fn parse_relayed(message: &str) -> Option<Relayed> {
    let (reason, rest) = message.split_once(" (Service: ")?;
    let (service, rest) = rest.split_once(", Status Code: ")?;
    let (status, rest) = rest.split_once(", Request ID: ")?;
    let (request_id, rest) = rest.split_once(") (SDK Attempt Count: ")?;
    let attempts = rest.strip_suffix(')')?;
    Some(Relayed {
        reason: reason.to_string(),
        service: service.to_string(),
        status: status.to_string(),
        request_id: request_id.to_string(),
        attempts: attempts.to_string(),
    })
}

/// Assert that `answer` relays `expected_reason` from the dependent service, and answer
/// the request id the envelope named that call by.
fn assert_relayed_denial(case: &str, answer: &Answer, expected_reason: &str) -> String {
    let message = answer
        .text("Message")
        .unwrap_or_else(|| panic!("case {case:?}: expected a refusal message, got {}", answer.body));
    let relayed = parse_relayed(&message).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the reason followed by \"(Service: <service>, Status Code: \
             <status>, Request ID: <id>) (SDK Attempt Count: <n>)\", got {message:?}"
        )
    });
    assert_eq!(
        relayed.reason, expected_reason,
        "case {case:?}: expected the relayed reason {expected_reason:?}, got {:?}",
        relayed.reason
    );
    assert_eq!(
        (
            relayed.service.as_str(),
            relayed.status.as_str(),
            relayed.attempts.as_str()
        ),
        (DEPENDENT_SERVICE, DEPENDENT_STATUS, DEPENDENT_ATTEMPTS),
        "case {case:?}: expected the call to {DEPENDENT_SERVICE} answered \
         {DEPENDENT_STATUS} after {DEPENDENT_ATTEMPTS} attempt(s), got {relayed:?}"
    );
    assert!(
        ::service_common::query_error_envelope::is_request_id(&relayed.request_id),
        "case {case:?}: expected the envelope to name the relayed call by a request id, got \
         {:?}",
        relayed.request_id
    );
    relayed.request_id
}

/// The principal every request of this suite is refused as: the harness signs with the
/// emulator's own long-term credential, and the account contract states which principal
/// that credential speaks for — the same one `sts:GetCallerIdentity` reports.
fn caller_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// `CreateRedshiftIdcApplication` naming `instance_arn`, with the extra parameters a row
/// carries.
async fn create_redshift_application(
    emulator: &Emulator,
    name: &str,
    instance_arn: &str,
    extra: &[(&str, &str)],
) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![
        ("IdcInstanceArn", instance_arn),
        ("RedshiftIdcApplicationName", name),
        ("IdcDisplayName", "RedshiftIdc"),
        ("IamRoleArn", IAM_ROLE_ARN),
    ];
    params.extend_from_slice(extra);
    emulator.call("CreateRedshiftIdcApplication", &params).await
}

/// `CreateQev2IdcApplication` naming `instance_arn`, with the extra parameters a row
/// carries.
async fn create_qev2_application(
    emulator: &Emulator,
    name: &str,
    instance_arn: &str,
    extra: &[(&str, &str)],
) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![
        ("IdcInstanceArn", instance_arn),
        ("Qev2IdcApplicationName", name),
        ("IdcDisplayName", "Qev2Idc"),
    ];
    params.extend_from_slice(extra);
    emulator.call("CreateQev2IdcApplication", &params).await
}

/// Which registration a row drives, and the read that reports what it registered.
#[derive(Debug, Clone, Copy)]
enum Registration {
    Redshift,
    Qev2,
}

impl Registration {
    /// The `Describe*` action that lists what this registration stores.
    fn describe(self) -> &'static str {
        match self {
            Self::Redshift => "DescribeRedshiftIdcApplications",
            Self::Qev2 => "DescribeQev2IdcApplications",
        }
    }

    /// The element each listed application carries its NAME in.
    fn name_member(self) -> &'static str {
        match self {
            Self::Redshift => "RedshiftIdcApplicationName",
            Self::Qev2 => "Qev2IdcApplicationName",
        }
    }

    /// The awsQueryError code THIS service answers a name it already holds an
    /// application under with
    /// (`smithy_query(query="shape", shape="RedshiftIdcApplicationAlreadyExistsFault,Qev2IdcApplicationAlreadyExistsFault")`
    /// — httpError 400, codes `RedshiftIdcApplicationAlreadyExists` and
    /// `Qev2IdcApplicationAlreadyExists`): a refusal of this service's own, reached
    /// without calling anybody.
    fn already_exists_code(self) -> &'static str {
        match self {
            Self::Redshift => "RedshiftIdcApplicationAlreadyExists",
            Self::Qev2 => "Qev2IdcApplicationAlreadyExists",
        }
    }

    /// The `IdcOnboardStatus` a registration of this kind is reported with.
    fn onboard_status(self) -> &'static str {
        match self {
            Self::Redshift => state::REDSHIFT_IDC_ONBOARD_STATUS,
            Self::Qev2 => state::QEV2_IDC_ONBOARD_STATUS,
        }
    }

    /// Drive this registration.
    async fn create(
        self,
        emulator: &Emulator,
        name: &str,
        instance_arn: &str,
        extra: &[(&str, &str)],
    ) -> Answer {
        match self {
            Self::Redshift => {
                create_redshift_application(emulator, name, instance_arn, extra).await
            }
            Self::Qev2 => create_qev2_application(emulator, name, instance_arn, extra).await,
        }
    }

    /// The element a created application carries its own ARN in, which every later
    /// call reaches it by.
    fn arn_member(self) -> &'static str {
        match self {
            Self::Redshift => "RedshiftIdcApplicationArn",
            Self::Qev2 => "Qev2IdcApplicationArn",
        }
    }

    /// Delete the application a create registered, so a case that registers one per
    /// row leaves the account as it found it — which is what a row needs when the
    /// account holds at most ONE application of that kind (the QEV2 one).
    async fn release(self, emulator: &Emulator, case: &str, created: &Answer) {
        let arn = created
            .text(self.arn_member())
            .unwrap_or_else(|| panic!("case {case:?}: a created application carries an ARN"));
        let action = match self {
            Self::Redshift => "DeleteRedshiftIdcApplication",
            Self::Qev2 => "DeleteQev2IdcApplication",
        };
        let deleted = emulator.call(action, &[(self.arn_member(), arn.as_str())]).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected {action} served with 200, got {} with body {}",
            deleted.status, deleted.body
        );
    }
}

/// The names of the applications this registration's own read lists.
async fn registered_names(
    emulator: &Emulator,
    registration: Registration,
    case: &str,
) -> Vec<String> {
    let listed = emulator.call(registration.describe(), &[]).await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: expected {} served, got {} with body {}",
        registration.describe(),
        listed.status,
        listed.body
    );
    crate::cluster_lifecycle::all_text(&listed.body, registration.name_member())
}

/// Put one more instance into the account's IAM Identity Center store, where that
/// service's own `CreateInstance` writes it.
fn hold_instance(emulator: &Emulator, instance_arn: &str) {
    use service_com_amazonaws_ssoadmin_swbexternalservice::state as identity_center;

    let state = emulator
        .engine
        .state::<identity_center::ComAmazonawsSsoadminSwbexternalserviceState>();
    let instance = identity_center::Instance::active(
        instance_id(instance_arn),
        identity_center::SEED_IDENTITY_STORE_ID,
        identity_center::SEED_INSTANCE_TYPE,
        Some(identity_center::SEED_INSTANCE_NAME.to_string()),
        ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
        REGION,
    );
    state
        .lock()
        .instances
        .insert(instance.instance_arn.clone(), instance);
}

/// One row of the request-shape table: which registration drives it and what else the
/// create asks for beside the members every row carries.
struct Shape {
    registration: Registration,
    case: &'static str,
    extra: Vec<(&'static str, &'static str)>,
}

/// RULE: a registration naming an Identity Center instance the account does not hold is
/// refused with the relayed `sso:CreateApplication` denial and registers nothing.
#[tokio::test]
async fn a_registration_naming_an_instance_the_account_does_not_hold_is_denied() {
    let emulator = Emulator::new();
    let principal = caller_principal();

    // The other members a row carries, so the dependency can be seen not to depend on
    // them: nothing more, every optional member of the Redshift create, and a tagged
    // create.
    let shapes: Vec<Shape> = vec![
        Shape {
            registration: Registration::Redshift,
            case: "the required members alone",
            extra: vec![],
        },
        Shape {
            registration: Registration::Redshift,
            case: "a namespaced Lakehouse create with a token issuer",
            extra: vec![
                ("IdentityNamespace", "corp.directory_1"),
                ("ApplicationType", "Lakehouse"),
                (
                    "AuthorizedTokenIssuerList.member.1.TrustedTokenIssuerArn",
                    "arn:aws:sso::000000000000:trustedTokenIssuer/ssoins-1111111111111111/tti-1",
                ),
                ("SsoTagKeys.TagKey.1", "owner"),
            ],
        },
        Shape {
            registration: Registration::Redshift,
            case: "a tagged create",
            extra: vec![("Tags.Tag.1.Key", "owner"), ("Tags.Tag.1.Value", "alice")],
        },
        Shape {
            registration: Registration::Qev2,
            case: "the required members alone",
            extra: vec![],
        },
        Shape {
            registration: Registration::Qev2,
            case: "a tagged create",
            extra: vec![("Tags.Tag.1.Key", "env"), ("Tags.Tag.1.Value", "prod")],
        },
    ];

    for (index, instance_arn) in NOT_HELD.into_iter().enumerate() {
        for (position, shape) in shapes.iter().enumerate() {
            let registration = shape.registration;
            let case = format!("{registration:?} {} naming {instance_arn}", shape.case);
            let name = format!("denied-app-{index}-{position}");
            let before = registered_names(&emulator, registration, &case).await;

            let answer = registration
                .create(&emulator, &name, instance_arn, &shape.extra)
                .await;

            assert_eq!(
                (answer.status, answer.error_code().as_deref()),
                (DENIED_STATUS, Some(DENIED_CODE)),
                "case {case:?}: expected {DENIED_STATUS} {DENIED_CODE}, got {} with body {}",
                answer.status,
                answer.body
            );
            let expected = expected_message(&principal, instance_arn);
            assert_relayed_denial(&case, &answer, &expected);
            // The refusal registered nothing: no application was minted, so this
            // registration's own read lists exactly what it listed before.
            let after = registered_names(&emulator, registration, &case).await;
            assert_eq!(
                after, before,
                "case {case:?}: expected the refusal to register nothing, so {before:?}, got \
                 {after:?}"
            );
            assert!(
                !after.contains(&name),
                "case {case:?}: expected {name:?} to be registered by nothing, got {after:?}"
            );
        }
    }
}

/// RULE (the other half): a registration naming an instance the account DOES hold is
/// served, and the managed application it reports is named inside that instance — read
/// back through the operation's own `Describe`.
#[tokio::test]
async fn a_registration_naming_an_instance_the_account_holds_is_served() {
    let emulator = Emulator::new();

    for (index, instance_arn) in IDENTITY_CENTER_INSTANCES.into_iter().enumerate() {
        for registration in [Registration::Redshift, Registration::Qev2] {
            let case = format!("{registration:?} naming the held instance {instance_arn}");
            let name = format!("held-app-{index}");
            let created = registration
                .create(&emulator, &name, instance_arn, &[])
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected 200, got {} with body {}",
                created.status, created.body
            );

            // The managed application is a resource of the instance the request named,
            // so the instance's own id is part of its ARN.
            let managed = created
                .text("IdcManagedApplicationArn")
                .unwrap_or_default();
            let within = format!("application/{}/", instance_id(instance_arn));
            assert!(
                managed.contains(&within),
                "case {case:?}: expected the managed application to be named inside the \
                 instance ({within:?}), got {managed:?}"
            );
            assert_eq!(
                created.text("IdcOnboardStatus").as_deref(),
                Some(registration.onboard_status()),
                "case {case:?}: expected the registered onboard status, got {:?}",
                created.text("IdcOnboardStatus")
            );

            // Round trip: the application the create registered is what a later read
            // reports.
            let listed = registered_names(&emulator, registration, &case).await;
            assert!(
                listed.contains(&name),
                "case {case:?}: expected the read to list {name:?}, got {listed:?}"
            );

            // The row releases what it registered: an account holds at most one QEV2
            // application, so the next instance's row starts from an account that
            // holds none.
            registration.release(&emulator, &case, &created).await;
        }
    }
}

/// RULE (what the decision is grounded in): the held set is the OWNING service's state,
/// so the very ARN a registration was refused for is served once the account's Identity
/// Center holds that instance — nothing about the request changed.
#[tokio::test]
async fn an_instance_the_account_comes_to_hold_is_served_afterwards() {
    let emulator = Emulator::new();
    let instance_arn = NOT_HELD[0];

    for registration in [Registration::Redshift, Registration::Qev2] {
        let case = format!("{registration:?} before the account holds {instance_arn}");
        let name = "late-instance-app";
        let refused = registration.create(&emulator, name, instance_arn, &[]).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (DENIED_STATUS, Some(DENIED_CODE)),
            "case {case:?}: expected {DENIED_STATUS} {DENIED_CODE}, got {} with body {}",
            refused.status,
            refused.body
        );
    }

    hold_instance(&emulator, instance_arn);

    for registration in [Registration::Redshift, Registration::Qev2] {
        let case = format!("{registration:?} after the account holds {instance_arn}");
        let name = "late-instance-app";
        let served = registration.create(&emulator, name, instance_arn, &[]).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected 200, got {} with body {}",
            served.status, served.body
        );
        let listed = registered_names(&emulator, registration, &case).await;
        assert!(
            listed.contains(&name.to_string()),
            "case {case:?}: expected the read to list {name:?}, got {listed:?}"
        );
    }
}

/// One row of the relayed-refusal table: which registration made the nested call, the
/// instance ARN it named, and what else the create asked for.
struct RelayRow {
    registration: Registration,
    instance_arn: &'static str,
    extra: Vec<(&'static str, &'static str)>,
}

/// RULE (the refusal's FORM): the denial was answered by the DEPENDENT service, so every
/// relayed refusal states that service's own sentence and then the envelope naming the
/// call it came from — the service that answered, the status it answered with, that
/// call's request id and how many attempts it took. The id belongs to the CALL, so two
/// refusals never report one id while everything else about them is identical.
///
/// The rows differ along what the rule generalises over: which operation made the nested
/// call, the form and partition of the instance ARN it named, and whether the create
/// asked for anything beside its required members.
#[tokio::test]
async fn every_relayed_denial_states_the_dependent_call_it_came_from() {
    let emulator = Emulator::new();
    let principal = caller_principal();

    let rows: Vec<RelayRow> = vec![
        RelayRow {
            registration: Registration::Redshift,
            instance_arn: NOT_HELD[0],
            extra: vec![],
        },
        RelayRow {
            registration: Registration::Redshift,
            instance_arn: NOT_HELD[1],
            extra: vec![("IdentityNamespace", "corp.directory_1")],
        },
        RelayRow {
            registration: Registration::Redshift,
            instance_arn: NOT_HELD[3],
            extra: vec![],
        },
        RelayRow {
            registration: Registration::Qev2,
            instance_arn: NOT_HELD[2],
            extra: vec![],
        },
        RelayRow {
            registration: Registration::Qev2,
            instance_arn: NOT_HELD[3],
            extra: vec![("Tags.Tag.1.Key", "env"), ("Tags.Tag.1.Value", "prod")],
        },
    ];

    let mut request_ids: Vec<String> = Vec::new();
    for (position, row) in rows.iter().enumerate() {
        let RelayRow {
            registration,
            instance_arn,
            extra,
        } = row;
        let case = format!("{registration:?} naming {instance_arn}");
        let name = format!("relayed-app-{position}");
        let answer = registration
            .create(&emulator, &name, instance_arn, extra)
            .await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (DENIED_STATUS, Some(DENIED_CODE)),
            "case {case:?}: expected {DENIED_STATUS} {DENIED_CODE}, got {} with body {}",
            answer.status,
            answer.body
        );
        let expected = expected_message(&principal, instance_arn);
        request_ids.push(assert_relayed_denial(&case, &answer, &expected));

        // The same request again: one call, one request id, so the repeat is relayed
        // under an id of its own.
        let again = registration
            .create(&emulator, &name, instance_arn, extra)
            .await;
        request_ids.push(assert_relayed_denial(&case, &again, &expected));
    }

    let mut unique = request_ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        request_ids.len(),
        "case \"every relayed denial\": expected each relayed call its own request id, got \
         {request_ids:?}"
    );
}

/// RULE (its boundary): the envelope names a call made to ANOTHER service, so a refusal
/// this service reaches on its own terms — a name it already holds an application under —
/// carries no envelope at all.
#[tokio::test]
async fn a_refusal_this_service_answers_itself_carries_no_relayed_envelope() {
    let emulator = Emulator::new();
    let instance_arn = IDENTITY_CENTER_INSTANCES[0];

    for registration in [Registration::Redshift, Registration::Qev2] {
        let case = format!("{registration:?} re-registering a held name");
        let name = "own-refusal-app";
        let first = registration.create(&emulator, name, instance_arn, &[]).await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: expected the first create served, got {} with body {}",
            first.status, first.body
        );

        let again = registration.create(&emulator, name, instance_arn, &[]).await;
        assert_eq!(
            (again.status, again.error_code().as_deref()),
            (400, Some(registration.already_exists_code())),
            "case {case:?}: expected 400 {}, got {} with body {}",
            registration.already_exists_code(),
            again.status,
            again.body
        );
        let message = again.text("Message").unwrap_or_default();
        assert!(
            parse_relayed(&message).is_none() && !message.contains("(Service: "),
            "case {case:?}: expected a refusal of this service's own to name no relayed \
             call, got {message:?}"
        );
    }
}
