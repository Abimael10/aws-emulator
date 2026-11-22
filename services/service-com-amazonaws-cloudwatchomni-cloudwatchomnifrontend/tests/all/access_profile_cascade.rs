// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a `DeleteAccessProfile` takes with it.
//!
//! An access grant whose principal IS an access profile is an authorization OF that
//! profile — `AccessGrantPrincipalType`'s ACCESS_PROFILE value ("An access profile.")
//! makes the profile the principal the grant authorizes — so it cannot outlive the
//! principal it names. The suite asserts that over a TABLE rather than one recorded
//! exchange:
//!
//! * `DeleteAccessProfile` succeeds while grants the profile HOLDS exist, whatever those
//!   grants carry (their name, their permission, how many of them there are);
//! * the profile is then not found, and every grant it held is not found either;
//! * a grant held by ANOTHER profile of the same space is untouched, and so is that
//!   profile: the cascade reaches what the deleted principal held and nothing else;
//! * the grants the SERVICE provisioned with the space are untouched too, because they
//!   are held by other principals;
//! * the cascade is complete rather than reported: the grant listing stops reporting the
//!   removed grants.
//!
//! Every call goes through a real operation handler holding the SAME shared engine the
//! router hands every service, and each assertion names its case and the actual value.

use std::sync::Arc;

use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_access_grant::CreateAccessGrant;
use omni::ops::create_access_profile::CreateAccessProfile;
use omni::ops::create_domain::CreateDomain;
use omni::ops::create_space::CreateSpace;
use omni::ops::delete_access_profile::{DeleteAccessProfile, DeleteAccessProfileError};
use omni::ops::get_access_grant::{GetAccessGrant, GetAccessGrantError};
use omni::ops::get_access_profile::{GetAccessProfile, GetAccessProfileError};
use omni::ops::list_access_grants::ListAccessGrants;
use omni::ops::types::{
    AccessGrantPrincipal, CreateAccessGrantInput, CreateAccessProfileInput, CreateDomainInput,
    CreateSpaceInput, DeleteAccessProfileInput, GetAccessGrantInput, GetAccessProfileInput,
    ListAccessGrantsInput,
};
use omni::state::{ACCESS_GRANT_PRINCIPAL_TYPE_ACCESS_PROFILE, ACCESS_GRANT_TYPE_SERVICE_MANAGED};

/// The Regions a space create is signed for, from the suite that owns the space rule: an
/// account holds at most one space per Region.
use super::domain_space_lifecycle::SPACE_REGIONS;

/// A role ARN satisfying `Arn`'s `^arn:.+$`, which `CreateSpace` requires.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// `AccessGrantPermission`'s read value, which a grant that scopes nothing carries
/// (`smithy_query(query="shape",shape="AccessGrantPermission")`: CUSTOM, READ,
/// READ_WRITE_DELETE, SPACE_ADMIN).
const READ_PERMISSION: &str = "READ";

/// The request metadata a call signed for `region` carries.
fn signed_request_parts_in(region: &str) -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{region}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine and the service mounted on a router.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
    /// How many spaces this emulator has created, which picks each create's Region out of
    /// [`SPACE_REGIONS`].
    spaces_created: std::sync::atomic::AtomicUsize,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts_in(SPACE_REGIONS[0]),
            spaces_created: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's one domain.
    async fn domain(&self, case: &str, name: &str) -> String {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
            .domain_id
    }

    /// A space of `domain_id`, created in a Region of its own.
    async fn space(&self, case: &str, domain_id: &str, name: &str) -> String {
        let index = self
            .spaces_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let region = SPACE_REGIONS
            .get(index)
            .copied()
            .expect("the suite signs each space it creates for a Region of its own");
        let parts = signed_request_parts_in(region);
        let ctx = OperationContext::new(
            &parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        );
        CreateSpace::new(Arc::clone(&self.engine))
            .handle(
                CreateSpaceInput {
                    name: name.to_string(),
                    domain_id: domain_id.to_string(),
                    data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
                    ..Default::default()
                },
                &ctx,
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateSpace failed: {error:?}"))
            .space
            .space_id
    }

    /// An access profile of `space_id`.
    async fn profile(&self, case: &str, space_id: &str, name: &str) -> String {
        CreateAccessProfile::new(Arc::clone(&self.engine))
            .handle(
                CreateAccessProfileInput {
                    space_id: space_id.to_string(),
                    name: name.to_string(),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateAccessProfile failed: {error:?}"))
            .access_profile
            .profile_id
    }

    /// A grant of `space_id` whose principal is the access profile `profile_id`, i.e. one
    /// the profile HOLDS.
    async fn grant_to_profile(
        &self,
        case: &str,
        domain_id: &str,
        space_id: &str,
        profile_id: &str,
        name: &str,
    ) -> String {
        CreateAccessGrant::new(Arc::clone(&self.engine))
            .handle(
                CreateAccessGrantInput {
                    domain_id: domain_id.to_string(),
                    space_id: space_id.to_string(),
                    name: name.to_string(),
                    permission: READ_PERMISSION.to_string(),
                    principal: AccessGrantPrincipal {
                        principal_type: ACCESS_GRANT_PRINCIPAL_TYPE_ACCESS_PROFILE.to_string(),
                        principal_id: Some(profile_id.to_string()),
                        principal_attributes: None,
                    },
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateAccessGrant failed: {error:?}"))
            .access_grant
            .grant_id
    }

    async fn delete_profile(
        &self,
        space_id: &str,
        profile_id: &str,
    ) -> Result<omni::ops::types::DeleteAccessProfileOutput, DeleteAccessProfileError> {
        DeleteAccessProfile::new(Arc::clone(&self.engine))
            .handle(
                DeleteAccessProfileInput {
                    space_id: space_id.to_string(),
                    profile_id: profile_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    async fn read_profile(
        &self,
        space_id: &str,
        profile_id: &str,
    ) -> Result<omni::ops::types::GetAccessProfileOutput, GetAccessProfileError> {
        GetAccessProfile::new(Arc::clone(&self.engine))
            .handle(
                GetAccessProfileInput {
                    space_id: space_id.to_string(),
                    profile_id: profile_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    async fn read_grant(
        &self,
        grant_id: &str,
    ) -> Result<omni::ops::types::GetAccessGrantOutput, GetAccessGrantError> {
        GetAccessGrant::new(Arc::clone(&self.engine))
            .handle(
                GetAccessGrantInput {
                    grant_id: grant_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// The ids of the grants the listing reports for `space_id`, service-provisioned ones
    /// included.
    async fn grant_ids(&self, case: &str, space_id: &str) -> Vec<String> {
        ListAccessGrants::new(Arc::clone(&self.engine))
            .handle(
                ListAccessGrantsInput {
                    space_id: Some(space_id.to_string()),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListAccessGrants failed: {error:?}"))
            .items
            .into_iter()
            .map(|item| item.grant_id)
            .collect()
    }

    /// The ids of the grants the SERVICE provisioned with `space_id`.
    async fn service_managed_grant_ids(&self, case: &str, space_id: &str) -> Vec<String> {
        ListAccessGrants::new(Arc::clone(&self.engine))
            .handle(
                ListAccessGrantsInput {
                    space_id: Some(space_id.to_string()),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListAccessGrants failed: {error:?}"))
            .items
            .into_iter()
            .filter(|item| item.grant_type == ACCESS_GRANT_TYPE_SERVICE_MANAGED)
            .map(|item| item.grant_id)
            .collect()
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_not_found<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
) {
    match result {
        Ok(output) => panic!("case {case:?}: expected ResourceNotFoundException, got {output:?}"),
        Err(error) => assert_eq!(
            (error.shape_name(), error.http_status()),
            ("ResourceNotFoundException", 404),
            "case {case:?}: expected ResourceNotFoundException (404), got {error:?}"
        ),
    }
}

#[tokio::test]
async fn deleting_an_access_profile_removes_the_grants_it_holds_and_nothing_else() {
    let omni = Omni::new();
    let domain_id = omni.domain("cascade", "omni-profile-cascade").await;

    // Rows differ along HOW MANY grants the deleted profile holds and what they are
    // named, because the cascade follows the principal each grant names rather than
    // anything else the grant carries.
    for (index, (case, grant_names)) in [
        ("one grant", vec!["only-grant"]),
        ("two grants", vec!["first-grant", "second-grant"]),
        (
            "three grants of differing names",
            vec!["a", "grant_with_underscores", &"g".repeat(64)],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        // Each row works in a space of its own, which an account holds one of per Region.
        let space_id = omni
            .space(case, &domain_id, &format!("cascade-space-{index}"))
            .await;
        let provisioned = omni.service_managed_grant_ids(case, &space_id).await;
        let subject = omni.profile(case, &space_id, "subject").await;
        let bystander = omni.profile(case, &space_id, "bystander").await;

        let mut held = Vec::new();
        for name in &grant_names {
            held.push(
                omni.grant_to_profile(case, &domain_id, &space_id, &subject, name)
                    .await,
            );
        }
        // One grant of the OTHER profile, which the delete may not touch.
        let kept = omni
            .grant_to_profile(case, &domain_id, &space_id, &bystander, "bystander-grant")
            .await;

        // The delete succeeds: a grant the profile holds is not a reference to it.
        omni.delete_profile(&space_id, &subject)
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: DeleteAccessProfile failed: {error:?}")
            });

        // The profile and every grant it held are gone from the readers that resolve them.
        expect_not_found(case, omni.read_profile(&space_id, &subject).await);
        for grant_id in &held {
            expect_not_found(
                &format!("{case}: the held grant {grant_id}"),
                omni.read_grant(grant_id).await,
            );
        }

        // The other profile's grant survives, and so does the other profile.
        omni.read_profile(&space_id, &bystander)
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the other profile must be left alone: {error:?}")
            });
        omni.read_grant(&kept).await.unwrap_or_else(|error| {
            panic!("case {case:?}: the other profile's grant must survive: {error:?}")
        });

        // The listing agrees with the readers: the removed grants are gone from it, and
        // the grants the service provisioned with the space are still there.
        let listed = omni.grant_ids(case, &space_id).await;
        for grant_id in &held {
            assert!(
                !listed.contains(grant_id),
                "case {case:?}: the listing still reports the removed grant {grant_id:?}: \
                 {listed:?}"
            );
        }
        assert!(
            listed.contains(&kept),
            "case {case:?}: the listing dropped the surviving grant {kept:?}: {listed:?}"
        );
        for grant_id in &provisioned {
            assert!(
                listed.contains(grant_id),
                "case {case:?}: the listing dropped the service-provisioned grant \
                 {grant_id:?}: {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_delete_is_idempotent_once_the_profile_and_its_grants_are_gone() {
    let omni = Omni::new();
    let domain_id = omni.domain("repeat", "omni-profile-repeat").await;
    let space_id = omni.space("repeat", &domain_id, "cascade-repeat").await;
    let profile_id = omni.profile("repeat", &space_id, "subject").await;
    let grant_id = omni
        .grant_to_profile("repeat", &domain_id, &space_id, &profile_id, "held")
        .await;

    // @idempotent, and the model declares no `ResourceNotFoundException`: the same delete
    // repeated answers success and removes nothing more.
    for attempt in ["the first delete", "the same delete again"] {
        omni.delete_profile(&space_id, &profile_id)
            .await
            .unwrap_or_else(|error| panic!("case {attempt:?}: DeleteAccessProfile failed: {error:?}"));
        expect_not_found(attempt, omni.read_profile(&space_id, &profile_id).await);
        expect_not_found(attempt, omni.read_grant(&grant_id).await);
    }
}
