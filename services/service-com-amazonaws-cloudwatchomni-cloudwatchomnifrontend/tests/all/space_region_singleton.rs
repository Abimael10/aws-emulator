// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the account's per-Region SPACE rule.
//!
//! A CloudWatchOmni space is the account's single resource of its kind in a Region, so
//! the suite asserts that rule over a TABLE of Regions rather than one recorded exchange:
//!
//! * the FIRST `CreateSpace` of a Region succeeds, whichever Region it is;
//! * every later `CreateSpace` signed for that Region is refused with the modelled
//!   `ConflictException` (409) whose message states the caller's account and the
//!   request's Region and nothing else — not the name asked for, not the space already
//!   held — whatever the request differs in (its name, its tags, its AgentCore role);
//! * the refusals write nothing: `ListSpaces` keeps reporting one space per Region, and
//!   the space the Region holds is unchanged;
//! * the Region is the slot: a space in another Region is accepted while the first is
//!   held, so an account holds as many spaces as it has Regions with one each;
//! * deleting the space frees its Region, and a create then succeeds there again as a
//!   NEW space rather than resurrecting the deleted one;
//! * the rule is about the account's Region rather than the parent: the create names the
//!   account's one domain either way;
//! * an `@idempotencyToken` repeat of the create that MINTED the Region's space is
//!   answered with that space, not with the conflict — the token's own doc ("Repeated
//!   requests with the same token return the original result instead of creating a
//!   duplicate") — so a timed-out caller can still retry.
//!
//! Every call goes through a real operation handler holding the SAME shared engine the
//! router hands every service, and each assertion names its case and the actual value.

use std::collections::HashMap;
use std::sync::Arc;

use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::create_space::{CreateSpace, CreateSpaceError, CreateSpaceErrorModeled};
use omni::ops::delete_space::DeleteSpace;
use omni::ops::get_space::GetSpace;
use omni::ops::list_spaces::ListSpaces;
use omni::ops::types::{
    CreateDomainInput, CreateSpaceInput, DeleteSpaceInput, GetSpaceInput, ListSpacesInput, Space,
    SpaceSummary,
};
use omni::scaffolded::Scaffolded;

/// The Regions a space create is signed for, from the suite that owns the space rule.
use super::domain_space_lifecycle::SPACE_REGIONS;

/// A role ARN satisfying `Arn`'s `^arn:.+$`, which `CreateSpace` requires.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// An AgentCore evaluation role ARN satisfying `Arn`'s `^arn:.+$`, so a row can differ in
/// more than its name.
const AGENT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniAgentCore";

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

/// The message the account's Region conflict carries: the fixed words around the caller's
/// account and the Region the request was signed for.
fn space_already_exists(region: &str) -> String {
    format!(
        "A space already exists for account {} in region {region}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// One emulator: one shared engine and the service mounted on a router.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
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
        }
    }

    /// The request metadata a handler reads, signed for `region`.
    ///
    /// The parts are held by the caller, because an `OperationContext` borrows them.
    fn ctx<'a>(&'a self, parts: &'a http::request::Parts) -> OperationContext<'a> {
        OperationContext::new(
            parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's one domain, created in the first Region of the table.
    async fn domain(&self, case: &str, name: &str) -> String {
        let parts = signed_request_parts_in(SPACE_REGIONS[0]);
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(&parts),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
            .domain_id
    }

    /// Ask for a space in `region`, and answer what the handler answered.
    async fn create_space(
        &self,
        input: &CreateSpaceInput,
        region: &str,
    ) -> Result<Space, CreateSpaceError> {
        let parts = signed_request_parts_in(region);
        CreateSpace::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx(&parts))
            .await
            .map(|output| output.space)
    }

    /// Every space the account holds, in creation order, through the listing a caller
    /// enumerates them with.
    async fn spaces(&self, case: &str, domain_id: &str) -> Vec<SpaceSummary> {
        let parts = signed_request_parts_in(SPACE_REGIONS[0]);
        ListSpaces::new(Arc::clone(&self.engine))
            .handle(
                ListSpacesInput {
                    domain_id: Some(domain_id.to_string()),
                    ..Default::default()
                },
                &self.ctx(&parts),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListSpaces failed: {error:?}"))
            .items
    }

    /// The space `space_id` names, through the reader a caller resolves it with.
    async fn read_space(&self, case: &str, space_id: &str) -> Space {
        let parts = signed_request_parts_in(SPACE_REGIONS[0]);
        GetSpace::new(Arc::clone(&self.engine))
            .handle(
                GetSpaceInput {
                    space_id: space_id.to_string(),
                },
                &self.ctx(&parts),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetSpace failed: {error:?}"))
            .space
    }

    /// Remove the space `space_id` names.
    async fn delete_space(&self, case: &str, space_id: &str) {
        let parts = signed_request_parts_in(SPACE_REGIONS[0]);
        DeleteSpace::new(Arc::clone(&self.engine))
            .handle(
                DeleteSpaceInput {
                    space_id: space_id.to_string(),
                },
                &self.ctx(&parts),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: DeleteSpace failed: {error:?}"));
    }
}

fn create_input(name: &str, domain_id: &str) -> CreateSpaceInput {
    CreateSpaceInput {
        name: name.to_string(),
        domain_id: domain_id.to_string(),
        data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
        ..Default::default()
    }
}

/// The `message` of the conflict a create earns, with the modelled code and status
/// checked first.
fn conflict_message(case: &str, result: Result<Space, CreateSpaceError>) -> String {
    match result {
        Ok(space) => panic!("case {case:?}: expected ConflictException (409), got Ok({space:?})"),
        Err(error) => {
            assert_eq!(
                (error.shape_name(), error.http_status()),
                ("ConflictException", 409),
                "case {case:?}: expected ConflictException (409), got {error:?}"
            );
            match error {
                Scaffolded::Modeled(CreateSpaceErrorModeled::ConflictException(conflict)) => {
                    conflict.message
                }
                other => {
                    panic!("case {case:?}: expected the modelled ConflictException, got {other:?}")
                }
            }
        }
    }
}

#[tokio::test]
async fn an_account_holds_one_space_per_region_and_a_second_is_the_modelled_conflict() {
    let omni = Omni::new();
    let domain_id = omni.domain("regions", "omni-space-regions").await;

    // Rows span the Region class the credential scope carries: Regions of four naming
    // forms, from the shared table of real Region codes. The rule is about the Region,
    // so the first create of each succeeds and every later one is refused.
    let regions = [
        SPACE_REGIONS[0],
        SPACE_REGIONS[1],
        SPACE_REGIONS[5],
        SPACE_REGIONS[13],
    ];
    let mut held: Vec<(String, &str)> = Vec::new();

    for (index, region) in regions.into_iter().enumerate() {
        // A Region nothing of the account occupies accepts the create, even while every
        // earlier Region holds one.
        let first = omni
            .create_space(&create_input(&format!("space-{index}"), &domain_id), region)
            .await
            .unwrap_or_else(|error| {
                panic!("case {region:?}: the first space of the Region failed: {error:?}")
            });
        assert_eq!(
            first.region, region,
            "case {region:?}: the space records region {:?}",
            first.region
        );
        held.push((first.space_id.clone(), region));

        // Rows differ along what a second request can carry: another name, the SAME name,
        // a name with tags, and one with the optional AgentCore role. None of it changes
        // the answer, and none of it reaches the message.
        let second_requests: Vec<(&str, CreateSpaceInput)> = vec![
            (
                "another name",
                create_input(&format!("space-{index}-again"), &domain_id),
            ),
            (
                "the name the Region's space already holds",
                create_input(&format!("space-{index}"), &domain_id),
            ),
            (
                "a request carrying tags",
                CreateSpaceInput {
                    tags: Some(HashMap::from([("team".to_string(), "sre".to_string())])),
                    ..create_input(&format!("space-{index}-tagged"), &domain_id)
                },
            ),
            (
                "a request carrying an AgentCore role",
                CreateSpaceInput {
                    agent_core_evaluation_role_arn: Some(AGENT_ROLE_ARN.to_string()),
                    ..create_input(&format!("space-{index}-agentcore"), &domain_id)
                },
            ),
        ];
        for (what, request) in second_requests {
            let case = format!("{region}: {what}");
            let message = conflict_message(&case, omni.create_space(&request, region).await);
            let expected = space_already_exists(region);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains(&request.name) && !message.contains(&first.space_id),
                "case {case:?}: the refusal names the request or the space it conflicts \
                 with: {message:?}"
            );
        }

        // The refusals wrote nothing: the Region's space is the first one, unchanged.
        let reread = omni.read_space(region, &first.space_id).await;
        assert_eq!(
            format!("{reread:?}"),
            format!("{first:?}"),
            "case {region:?}: a refused create changed the Region's space: {reread:?}"
        );
    }

    // One space per Region and no more, all under the account's one domain.
    let listed = omni.spaces("after the refusals", &domain_id).await;
    let mut reported: Vec<(String, String)> = listed
        .iter()
        .map(|item| (item.region.clone(), item.space_id.clone()))
        .collect();
    reported.sort();
    let mut expected: Vec<(String, String)> = held
        .iter()
        .map(|(space_id, region)| ((*region).to_string(), space_id.clone()))
        .collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "the account must hold exactly one space per Region, got {listed:?}"
    );
}

#[tokio::test]
async fn deleting_the_region_s_space_frees_the_region_for_a_new_one() {
    let omni = Omni::new();
    let domain_id = omni.domain("freed", "omni-space-freed").await;

    // Rows span the Regions a space can be held in: each is filled, freed and filled
    // again, and the space the Region holds afterwards is a NEW record.
    for region in [SPACE_REGIONS[0], SPACE_REGIONS[7]] {
        let first = omni
            .create_space(&create_input("freed-space", &domain_id), region)
            .await
            .unwrap_or_else(|error| panic!("case {region:?}: the first create failed: {error:?}"));
        let refused = conflict_message(
            region,
            omni.create_space(&create_input("freed-space-again", &domain_id), region)
                .await,
        );
        assert_eq!(
            refused,
            space_already_exists(region),
            "case {region:?}: unexpected refusal {refused:?}"
        );

        omni.delete_space(region, &first.space_id).await;
        let second = omni
            .create_space(&create_input("freed-space-again", &domain_id), region)
            .await
            .unwrap_or_else(|error| {
                panic!("case {region:?}: the freed Region refused a create: {error:?}")
            });
        assert_ne!(
            second.space_id, first.space_id,
            "case {region:?}: the create after the delete must mint a new space, got {:?} twice",
            second.space_id
        );
        assert_eq!(
            (second.region.as_str(), second.name.as_str()),
            (region, "freed-space-again"),
            "case {region:?}: the new space reports {second:?}"
        );
    }

    let listed = omni.spaces("after the deletes", &domain_id).await;
    assert_eq!(
        listed.len(),
        2,
        "each Region must hold exactly one space, got {listed:?}"
    );
}

#[tokio::test]
async fn a_token_repeat_of_the_create_that_minted_the_space_answers_with_that_space() {
    let omni = Omni::new();
    let domain_id = omni.domain("replay", "omni-space-replay").await;

    // Rows span `ClientToken`'s `@length(1..64)` and its `^[!-~]+$` class, each in a
    // Region of its own: the token is what lets a caller whose first attempt timed out
    // retry a create the account's Region rule would otherwise refuse.
    for (index, token) in ["!", "a-token", "~{}[]|\\^`", &"t".repeat(64)]
        .into_iter()
        .enumerate()
    {
        let region = SPACE_REGIONS[index];
        let request = CreateSpaceInput {
            client_token: Some(token.to_string()),
            ..create_input(&format!("replayed-{index}"), &domain_id)
        };
        let first = omni
            .create_space(&request, region)
            .await
            .unwrap_or_else(|error| panic!("case {token:?}: the first create failed: {error:?}"));
        let replayed = omni
            .create_space(&request, region)
            .await
            .unwrap_or_else(|error| panic!("case {token:?}: the replay failed: {error:?}"));
        assert_eq!(
            format!("{replayed:?}"),
            format!("{first:?}"),
            "case {token:?}: a token replay must answer with the Region's space, got {replayed:?}"
        );

        // Another token in that Region is the conflict: it is the Region that is taken,
        // not the token.
        let message = conflict_message(
            token,
            omni.create_space(
                &CreateSpaceInput {
                    client_token: Some(format!("{token}x")),
                    ..request.clone()
                },
                region,
            )
            .await,
        );
        assert_eq!(
            message,
            space_already_exists(region),
            "case {token:?}: unexpected refusal {message:?}"
        );
    }

    let listed = omni.spaces("after the replays", &domain_id).await;
    assert_eq!(
        listed.len(),
        4,
        "a replay must mint no second space, got {listed:?}"
    );
}
