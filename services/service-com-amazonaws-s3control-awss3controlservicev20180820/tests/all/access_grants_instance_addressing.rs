// SPDX-License-Identifier: CC0-1.0

//! The S3 Access Grants instance bootstrap, driven end to end over HTTP through every
//! endpoint authority a client can send it to.
//!
//! The rule under test: an S3 Control request is addressed in an account — resolved from the
//! `{AccountId}.` host prefix of whatever endpoint authority it was sent to and from the
//! `x-amz-account-id` header beside it (`AccountId` is `@required`, `@hostLabel`,
//! `httpHeader: x-amz-account-id`, and the operation's `endpoint.hostPrefix` is
//! `{AccountId}.` — `smithy_query(query="operation",operation="ListAccessGrants")`) — and in
//! the Region it was signed for, so what the Access Grants surface answers depends on that
//! account, that Region and the recorded state and NEVER on how the endpoint is spelled:
//! while the account holds no instance there the instance-scoped reads are refused with
//! `AccessGrantsInstanceNotExistsError` and the dissociation with `InvalidRequest` rather
//! than answered with an empty collection, once `CreateAccessGrantsInstance` has recorded the
//! singleton all four answer over it, and a second create is the
//! `AccessGrantsInstanceAlreadyExists` conflict that leaves it untouched.
//!
//! What this pins that the sibling suites do not: they all address the PUBLIC endpoint
//! (`call_in_region` builds `{AccountId}.s3-control.<region>.amazonaws.com`), while a client
//! pointed at this emulator prepends the same prefix to the loopback or named authority it
//! was configured with, and may write the URI in absolute form. The refusal wording, the
//! paging and the filters of this surface are
//! [`crate::access_grants_listings`]'s; who is authorized for an account is
//! [`crate::account_authorization`]'s, whose authority table and request builder the rows
//! below reuse rather than copy.
//!
//! The table differs along what the rule generalises over: the authority the host prefix sits
//! ahead of (the public endpoint, a loopback address with and without a port, a named host),
//! whether the authority carries the prefix at all, whether the URI is written in origin or
//! absolute form, and which call of the surface is asked.

use crate::access_point_lifecycle::{call_at_authority, Emulator, Reply, ACCOUNT, REGION, XMLNS};
use crate::account_authorization::prefixed_authorities;

/// The code and status every instance-scoped READ of this surface refuses an account with no
/// instance with, and the fixed sentence beside it — recorded behaviour, since the model
/// declares no errors for these operations. Spelled out here rather than imported from the
/// service: the recorded wording is what this suite is about, so a change to it has to fail.
const NO_INSTANCE_STATUS: u16 = 404;
/// See [`NO_INSTANCE_STATUS`].
const NO_INSTANCE_CODE: &str = "AccessGrantsInstanceNotExistsError";
/// See [`NO_INSTANCE_STATUS`]. No request value is interpolated into it.
const NO_INSTANCE_MESSAGE: &str = "Access Grants Instance does not exist";

/// What `DissociateAccessGrantsIdentityCenter` answers that same precondition with.
const DISSOCIATE_WITHOUT_INSTANCE_STATUS: u16 = 400;
/// See [`DISSOCIATE_WITHOUT_INSTANCE_STATUS`].
const DISSOCIATE_WITHOUT_INSTANCE_CODE: &str = "InvalidRequest";
/// See [`DISSOCIATE_WITHOUT_INSTANCE_STATUS`].
const DISSOCIATE_WITHOUT_INSTANCE_MESSAGE: &str =
    "The specified Access Grants Instance does not exist";

/// What a second `CreateAccessGrantsInstance` for an account that already has one answers.
const ALREADY_EXISTS_STATUS: u16 = 409;
/// See [`ALREADY_EXISTS_STATUS`].
const ALREADY_EXISTS_CODE: &str = "AccessGrantsInstanceAlreadyExists";
/// See [`ALREADY_EXISTS_STATUS`].
const ALREADY_EXISTS_MESSAGE: &str = "Access Grants Instance already exists";

/// The id every S3 Access Grants instance carries, as
/// `smithy_query(query="doc",shape="AccessGrantsInstanceId")` states it: "The ID of the S3
/// Access Grants instance. The ID is default."
const INSTANCE_ID: &str = "default";

/// One call of the surface this suite drives: how it is addressed, and the refusal it answers
/// while the account holds no instance.
struct Call {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    /// The status, `<Code>` and fixed message this call answers an account with no instance
    /// with.
    no_instance: (u16, &'static str, &'static str),
    /// The element a successful answer carries one entry of its collection in, for the calls
    /// that report a collection; `None` for the calls that report no collection.
    entry_element: Option<&'static str>,
}

/// Every call a client bootstrapping Access Grants makes: the singleton read, the three
/// instance-scoped listings, and the dissociation.
fn surface() -> [Call; 5] {
    [
        Call {
            case: "GetAccessGrantsInstance",
            method: "GET",
            path: "/v20180820/accessgrantsinstance",
            no_instance: (NO_INSTANCE_STATUS, NO_INSTANCE_CODE, NO_INSTANCE_MESSAGE),
            entry_element: None,
        },
        Call {
            case: "ListAccessGrants",
            method: "GET",
            path: "/v20180820/accessgrantsinstance/grants",
            no_instance: (NO_INSTANCE_STATUS, NO_INSTANCE_CODE, NO_INSTANCE_MESSAGE),
            entry_element: Some("AccessGrantId"),
        },
        Call {
            case: "ListAccessGrantsLocations",
            method: "GET",
            path: "/v20180820/accessgrantsinstance/locations",
            no_instance: (NO_INSTANCE_STATUS, NO_INSTANCE_CODE, NO_INSTANCE_MESSAGE),
            entry_element: Some("AccessGrantsLocationId"),
        },
        Call {
            case: "ListCallerAccessGrants",
            method: "GET",
            path: "/v20180820/accessgrantsinstance/caller/grants",
            no_instance: (NO_INSTANCE_STATUS, NO_INSTANCE_CODE, NO_INSTANCE_MESSAGE),
            entry_element: Some("GrantScope"),
        },
        Call {
            case: "DissociateAccessGrantsIdentityCenter",
            method: "DELETE",
            path: "/v20180820/accessgrantsinstance/identitycenter",
            no_instance: (
                DISSOCIATE_WITHOUT_INSTANCE_STATUS,
                DISSOCIATE_WITHOUT_INSTANCE_CODE,
                DISSOCIATE_WITHOUT_INSTANCE_MESSAGE,
            ),
            entry_element: None,
        },
    ]
}

/// One way a client can spell the endpoint it sends a request to: the case, the authority the
/// request states, and whether the URI is written in absolute form (scheme, authority and
/// path) rather than the origin form (`/v20180820/…`) a client usually sends.
struct Addressing {
    case: String,
    authority: String,
    absolute: bool,
}

/// Every endpoint spelling the bootstrap sequence is run over, for a request naming
/// `account`.
///
/// The prefixed forms are [`prefixed_authorities`] — every authority a client reaches this
/// service at, reused rather than re-tabled — plus the bare forms, where the authority carries
/// no prefix and the header alone names the account, and the absolute-form URI a client may
/// write instead of the origin form.
fn addressing_forms(account: &str) -> Vec<Addressing> {
    let mut forms: Vec<Addressing> = prefixed_authorities(account)
        .into_iter()
        .map(|(case, authority)| Addressing {
            case: format!("the host prefix ahead of {case}"),
            authority,
            absolute: false,
        })
        .collect();
    forms.push(Addressing {
        case: "a bare loopback authority, the header alone naming the account".to_string(),
        authority: "127.0.0.1:9297".to_string(),
        absolute: false,
    });
    forms.push(Addressing {
        case: "the bare public endpoint, the header alone naming the account".to_string(),
        authority: format!("s3-control.{REGION}.amazonaws.com"),
        absolute: false,
    });
    forms.push(Addressing {
        case: "an absolute-form URI carrying the host prefix ahead of a loopback authority"
            .to_string(),
        authority: format!("{account}.127.0.0.1:9297"),
        absolute: true,
    });
    forms
}

/// Send one call of the surface, addressed as `form` spells the endpoint and naming `account`
/// in the header every operation binds its `AccountId` to.
async fn send(
    emulator: &Emulator,
    form: &Addressing,
    account: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Reply {
    call_at_authority(
        &emulator.router,
        REGION,
        &form.authority,
        form.absolute,
        method,
        path,
        Some(account),
        body,
    )
    .await
}

/// `CreateAccessGrantsInstance` for `account`, addressed as `form` spells the endpoint.
async fn create_instance(emulator: &Emulator, form: &Addressing, account: &str) -> Reply {
    send(
        emulator,
        form,
        account,
        "POST",
        "/v20180820/accessgrantsinstance",
        &format!("<CreateAccessGrantsInstanceRequest xmlns=\"{XMLNS}\"/>"),
    )
    .await
}

/// `GetAccessGrantsInstance` for `account`, addressed as `form` spells the endpoint.
async fn get_instance(emulator: &Emulator, form: &Addressing, account: &str) -> Reply {
    send(
        emulator,
        form,
        account,
        "GET",
        "/v20180820/accessgrantsinstance",
        "",
    )
    .await
}

#[tokio::test]
async fn the_bootstrap_sequence_answers_the_same_through_every_endpoint_spelling() {
    for form in addressing_forms(ACCOUNT) {
        // A fresh emulator per row: the instance is a per-account, per-Region singleton, so
        // each row has to start from an account that holds none.
        let emulator = Emulator::start();

        // Probe first, exactly as a client bootstrapping Access Grants does: every call of
        // the surface states that the account has no instance, in the fixed words of its own
        // refusal class, so a client can tell "no instance yet" from "no such API".
        for call in surface() {
            let refused = send(&emulator, &form, ACCOUNT, call.method, call.path, "").await;
            assert_eq!(
                (
                    refused.status,
                    refused.code(),
                    refused.text("Message").unwrap_or_default()
                ),
                (
                    call.no_instance.0,
                    call.no_instance.1.to_string(),
                    call.no_instance.2.to_string()
                ),
                "case {} over {}: expected the no-instance refusal, got {} with body {}",
                call.case,
                form.case,
                refused.status,
                refused.body
            );
        }

        // Create the instance the probe reported missing.
        let created = create_instance(&emulator, &form, ACCOUNT).await;
        assert_eq!(
            (created.status, created.text("AccessGrantsInstanceId")),
            (200, Some(INSTANCE_ID.to_string())),
            "case {}: expected the create to record the singleton, got {} with body {}",
            form.case,
            created.status,
            created.body
        );
        let created_arn = created
            .text("AccessGrantsInstanceArn")
            .unwrap_or_else(|| panic!("case {}: no ARN in {}", form.case, created.body));

        // Now every call of the surface answers over that instance: each listing reports an
        // empty page with no continuation token, the instance holding no locations and no
        // grants, and the read and the dissociation succeed.
        for call in surface() {
            let answered = send(&emulator, &form, ACCOUNT, call.method, call.path, "").await;
            assert!(
                (200..300).contains(&answered.status),
                "case {} over {}: expected a success once the instance exists, got {} with \
                 body {}",
                call.case,
                form.case,
                answered.status,
                answered.body
            );
            if let Some(element) = call.entry_element {
                assert!(
                    answered.texts(element).is_empty() && answered.text("NextToken").is_none(),
                    "case {} over {}: expected an empty page with no token for an instance \
                     holding nothing, got body {}",
                    call.case,
                    form.case,
                    answered.body
                );
            }
        }
        let read = get_instance(&emulator, &form, ACCOUNT).await;
        assert_eq!(
            (
                read.status,
                read.text("AccessGrantsInstanceId"),
                read.text("AccessGrantsInstanceArn")
            ),
            (
                200,
                Some(INSTANCE_ID.to_string()),
                Some(created_arn.clone())
            ),
            "case {}: expected the read to report the instance the create recorded, got body \
             {}",
            form.case,
            read.body
        );

        // A second create is the conflict, and it leaves the instance the first one recorded
        // exactly as it was.
        let repeated = create_instance(&emulator, &form, ACCOUNT).await;
        assert_eq!(
            (
                repeated.status,
                repeated.code(),
                repeated.text("Message").unwrap_or_default()
            ),
            (
                ALREADY_EXISTS_STATUS,
                ALREADY_EXISTS_CODE.to_string(),
                ALREADY_EXISTS_MESSAGE.to_string()
            ),
            "case {}: expected a repeat create to be refused as a conflict, got {} with body \
             {}",
            form.case,
            repeated.status,
            repeated.body
        );
        let after = get_instance(&emulator, &form, ACCOUNT).await;
        assert_eq!(
            (
                after.status,
                after.text("AccessGrantsInstanceId"),
                after.text("AccessGrantsInstanceArn")
            ),
            (200, Some(INSTANCE_ID.to_string()), Some(created_arn)),
            "case {}: expected the refused create to leave the instance untouched, got body \
             {}",
            form.case,
            after.body
        );
    }
}
