// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListAccessProfiles`.
//!
//! Every call goes through a real operation handler holding the SAME shared engine the
//! router hands every service, so a profile a create persisted is what the listing
//! reports. Failures are the operation's own modelled errors, so each assertion checks
//! the wire facts that are requirements: the HTTP status and the error code.
//!
//! The suite asserts the RULE rather than one recorded exchange: a collection listing
//! reports the records the store holds for the parent the request names, in creation
//! order, projected onto the modelled summary shape, cut into pages of at most
//! `maxResults` with a `nextToken` exactly while records remain. So:
//!
//! * the page carries the space's own profiles — the one the service provisioned with
//!   the space and then the ones a caller created — in creation order, and nothing of
//!   another space;
//! * each `AccessProfileSummary` member agrees with the profile `GetAccessProfile`
//!   answers with — the summary is a projection, never a separate record;
//! * every page size from one up to past the collection walks the same order and yields
//!   every profile exactly once;
//! * a space holding no profile of its own lists exactly the profile it was provisioned
//!   with, with no token;
//! * listing changes nothing: the same request twice answers the same page, and a
//!   following `GetAccessProfile` still answers;
//! * a space the store does not hold is the modelled 404, and a `nextToken` this
//!   service did not mint for this collection is the modelled 400.
//!
//! Each assertion names its case and the actual value, so a failure identifies the row
//! of the table that broke.

use std::collections::HashMap;
use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_access_profile::{CreateAccessProfile, CreateAccessProfileError};
use omni::ops::create_domain::CreateDomain;
use omni::ops::create_space::CreateSpace;
use omni::ops::delete_access_profile::{DeleteAccessProfile, DeleteAccessProfileError};
use omni::ops::get_access_profile::{GetAccessProfile, GetAccessProfileError};
use omni::ops::list_access_profiles::{ListAccessProfiles, ListAccessProfilesError};
use omni::ops::types::{
    AccessProfile, AccessProfileSummary, CreateAccessProfileInput, CreateAccessProfileOutput,
    CreateDomainInput, CreateSpaceInput, DeleteAccessProfileInput, DeleteAccessProfileOutput,
    GetAccessProfileInput, GetAccessProfileOutput, ListAccessProfilesInput,
    ListAccessProfilesOutput, LIST_ACCESS_PROFILES_INPUT_SCHEMA,
};
use omni::state::{ACCESS_PROFILE_TYPE_CUSTOMER_MANAGED, ACCESS_PROFILE_TYPE_SERVICE_MANAGED};

/// The region the suite signs for; the handlers record the signed region on each
/// resource, so every ARN below is expected to carry it.
const REGION: &str = "us-west-2";

/// A role ARN satisfying `Arn`'s `^arn:.+$`, which `CreateSpace` requires.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// A well-formed `SpaceId` (its modelled pattern) that names nothing.
const MISSING_SPACE_ID: &str = "00000000-0000-4000-8000-000000000000";

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name (the router's routing key) and the region the handlers record.
fn signed_request_parts() -> http::request::Parts {
    signed_request_parts_in(REGION)
}

/// The same, signed for `region` rather than for the suite's own.
///
/// An account holds at most ONE space per Region, so a case needing a second space signs
/// that create for another Region ([`SPACE_REGIONS`]).
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

/// The Regions this suite signs its space creates for, from the suite that owns the
/// space rule: an account holds at most one space per Region, so each space a case needs
/// is created in a Region of its own.
use super::domain_space_lifecycle::SPACE_REGIONS;

/// One emulator: one shared engine, the service mounted on a router, and the request
/// metadata a handler reads.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
    /// How many spaces this emulator has created, which picks each create's Region out
    /// of [`SPACE_REGIONS`].
    spaces_created: std::sync::atomic::AtomicUsize,
    /// The account's one domain, remembered after the first space is created in it.
    domain: std::sync::Mutex<Option<String>>,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        // The router's service and the operations constructed below all hold the SAME
        // engine, so both front doors read and write one store.
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts(),
            spaces_created: std::sync::atomic::AtomicUsize::new(0),
            domain: std::sync::Mutex::new(None),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's domain, created on first use.
    ///
    /// An account holds at most ONE domain, so every space this suite creates hangs off
    /// the same one; the create carries the `clientToken` it is admitted only with.
    async fn domain(&self, case: &str) -> String {
        if let Some(held) = self
            .domain
            .lock()
            .expect("the domain cell is not poisoned")
            .clone()
        {
            return held;
        }
        let created = CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: "omni-profile-listing".to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some("token-profile-listing".to_string()),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
            .domain_id;
        *self.domain.lock().expect("the domain cell is not poisoned") = Some(created.clone());
        created
    }

    /// A space to create profiles in, through the handlers that own those resources.
    ///
    /// An account holds at most ONE space per Region, so each space is created in a
    /// Region of its own ([`SPACE_REGIONS`]); the profile calls that follow address the
    /// space by its id, which no Region scopes.
    async fn space(&self, case: &str, name: &str) -> String {
        let domain_id = self.domain(case).await;
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
                    domain_id,
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

    async fn create_access_profile(
        &self,
        input: &CreateAccessProfileInput,
    ) -> Result<CreateAccessProfileOutput, CreateAccessProfileError> {
        CreateAccessProfile::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    async fn get_access_profile(
        &self,
        space_id: &str,
        profile_id: &str,
    ) -> Result<GetAccessProfileOutput, GetAccessProfileError> {
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

    async fn delete_access_profile(
        &self,
        space_id: &str,
        profile_id: &str,
    ) -> Result<DeleteAccessProfileOutput, DeleteAccessProfileError> {
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

    async fn list_access_profiles(
        &self,
        input: &ListAccessProfilesInput,
    ) -> Result<ListAccessProfilesOutput, ListAccessProfilesError> {
        ListAccessProfiles::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    /// Dispatch one rpcv2Cbor request over HTTP and return the status and body.
    async fn over_the_wire<I: SerializableStruct>(
        &self,
        operation: &str,
        input_schema: &Schema,
        input: &I,
    ) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(input_schema, input)
            .expect("the input shape serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!(
                "/service/CloudWatchOmniFrontend/operation/{operation}"
            ))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor");
        for (name, value) in &self.parts.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_modelled_error<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
    expected_status: u16,
    expected_code: &str,
) {
    match result {
        Ok(output) => panic!(
            "case {case:?}: expected {expected_code} ({expected_status}), got Ok({output:?})"
        ),
        Err(error) => {
            assert_eq!(
                error.shape_name(),
                expected_code,
                "case {case:?}: expected error code {expected_code}, got {error:?}"
            );
            assert_eq!(
                error.http_status(),
                expected_status,
                "case {case:?}: expected HTTP {expected_status} for {expected_code}, got {}",
                error.http_status()
            );
        }
    }
}

/// One row of the profiles a space is seeded with: the request members that vary.
struct ProfileRow {
    name: String,
    description: Option<String>,
    tags: Option<HashMap<String, String>>,
}

/// The rows span what the listing generalises over: `AccessProfileName`'s whole
/// `@length(1..256)` and its `^[a-zA-Z0-9 _.:/=+@-]+$` character class, a `description`
/// absent, empty and at `@length(max: 1024)`, and `tags` absent, one and many — none of
/// which the projection or the ordering may depend on.
fn profile_rows() -> Vec<ProfileRow> {
    vec![
        ProfileRow {
            name: "a".to_string(),
            description: None,
            tags: None,
        },
        ProfileRow {
            name: "Read Only _.:/=+@- profile".to_string(),
            description: Some("an optional description".to_string()),
            tags: Some(HashMap::from([("team".to_string(), "sre".to_string())])),
        },
        ProfileRow {
            name: "n".repeat(256),
            description: Some(String::new()),
            tags: Some(HashMap::new()),
        },
        ProfileRow {
            name: "many-tags".to_string(),
            description: Some("d".repeat(1024)),
            tags: Some(
                (0..50)
                    .map(|index| (format!("key{index}"), format!("value{index}")))
                    .collect(),
            ),
        },
        ProfileRow {
            name: "zeta".to_string(),
            description: Some("the last one created".to_string()),
            tags: None,
        },
    ]
}

/// The members `AccessProfileSummary` declares, as a comparable tuple.
fn summary_fields(
    summary: &AccessProfileSummary,
) -> (String, String, String, Option<String>, Option<String>) {
    (
        summary.profile_id.clone(),
        summary.arn.clone(),
        summary.name.clone(),
        summary.description.clone(),
        summary.profile_type.clone(),
    )
}

/// The same members read off the full profile, which is what the summary projects.
fn profile_fields(
    profile: &AccessProfile,
) -> (String, String, String, Option<String>, Option<String>) {
    (
        profile.profile_id.clone(),
        profile.arn.clone(),
        profile.name.clone(),
        profile.description.clone(),
        profile.profile_type.clone(),
    )
}

/// The id of the access profile `space_id` was PROVISIONED with: creating a space writes
/// the one SERVICE_MANAGED profile the service owns for it, so it is the first record of
/// every listing of that space.
async fn provisioned_profile(omni: &Omni, space_id: &str) -> String {
    let page = omni
        .list_access_profiles(&ListAccessProfilesInput {
            space_id: space_id.to_string(),
            ..Default::default()
        })
        .await
        .expect("the new space lists");
    let provisioned: Vec<&AccessProfileSummary> = page
        .items
        .iter()
        .filter(|item| item.profile_type.as_deref() == Some(ACCESS_PROFILE_TYPE_SERVICE_MANAGED))
        .collect();
    assert_eq!(
        provisioned.len(),
        1,
        "expected the new space to hold exactly one provisioned profile, got {:?}",
        page.items
    );
    provisioned[0].profile_id.clone()
}

/// Seed `space_id` with [`profile_rows`] and answer the ids of every profile the space
/// then holds, in creation order — the provisioned one first, because the service wrote
/// it when the space was created.
async fn seed(omni: &Omni, space_id: &str) -> Vec<String> {
    let mut created = vec![provisioned_profile(omni, space_id).await];
    for row in profile_rows() {
        created.push(
            omni.create_access_profile(&CreateAccessProfileInput {
                space_id: space_id.to_string(),
                name: row.name.clone(),
                description: row.description.clone(),
                tags: row.tags.clone(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|error| panic!("case {:?}: the create failed: {error:?}", row.name))
            .access_profile
            .profile_id,
        );
    }
    created
}

#[tokio::test]
async fn the_listing_reports_one_space_in_creation_order_as_summaries() {
    let omni = Omni::new();
    let space_id = omni.space("list", "profile-list").await;
    let other_space = omni.space("list-other", "profile-list-other").await;

    let created = seed(&omni, &space_id).await;
    let names: Vec<String> = profile_rows().into_iter().map(|row| row.name).collect();
    let other_provisioned = provisioned_profile(&omni, &other_space).await;
    // A profile of ANOTHER space, created under a name this space also holds: no page
    // of this space may carry it.
    let elsewhere = omni
        .create_access_profile(&CreateAccessProfileInput {
            space_id: other_space.clone(),
            name: names[0].clone(),
            ..Default::default()
        })
        .await
        .expect("the other space's profile is created")
        .access_profile
        .profile_id;

    // One page holding everything: the listing is the store's creation order.
    let page = omni
        .list_access_profiles(&ListAccessProfilesInput {
            space_id: space_id.clone(),
            ..Default::default()
        })
        .await
        .expect("the space lists");
    assert_eq!(
        (
            page.items
                .iter()
                .map(|item| item.profile_id.clone())
                .collect::<Vec<String>>(),
            page.next_token.clone(),
        ),
        (created.clone(), None),
        "expected the space's profiles in creation order, got {:?}",
        page.items
    );

    // The summary is a projection of the record `GetAccessProfile` answers with, so
    // every member it declares agrees with that profile — and `profileType` names who
    // manages it: the service for the profile the space was provisioned with, the
    // customer for the ones a caller created.
    for (index, item) in page.items.iter().enumerate() {
        let full = omni
            .get_access_profile(&space_id, &created[index])
            .await
            .unwrap_or_else(|error| panic!("case {index}: the profile does not read back: {error:?}"))
            .access_profile;
        assert_eq!(
            summary_fields(item),
            profile_fields(&full),
            "case {:?}: the summary disagrees with the profile {full:?}",
            item.name
        );
        let expected_type = if index == 0 {
            ACCESS_PROFILE_TYPE_SERVICE_MANAGED
        } else {
            ACCESS_PROFILE_TYPE_CUSTOMER_MANAGED
        };
        assert_eq!(
            item.profile_type.as_deref(),
            Some(expected_type),
            "case {:?}: expected profileType {expected_type}, got {:?}",
            item.name,
            item.profile_type
        );
    }

    // A listing is scoped to the space it names: the other space lists the profile it
    // was provisioned with and the one created in it, and nothing of this space's,
    // though both hold that name.
    let other = omni
        .list_access_profiles(&ListAccessProfilesInput {
            space_id: other_space.clone(),
            ..Default::default()
        })
        .await
        .expect("the other space lists");
    assert_eq!(
        other
            .items
            .iter()
            .map(|item| item.profile_id.clone())
            .collect::<Vec<String>>(),
        vec![other_provisioned.clone(), elsewhere.clone()],
        "the other space listed {:?}",
        other.items
    );
}

#[tokio::test]
async fn every_page_size_walks_the_same_order_exactly_once() {
    let omni = Omni::new();
    let space_id = omni.space("pages", "profile-pages").await;
    let created = seed(&omni, &space_id).await;

    // Rows: a page smaller than the collection, one page short of it, exactly it, one
    // past it, and the modelled maximum `maxResults` admits.
    for page_size in [1, 2, 4, 5, 6, 7, 100] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = omni
                .list_access_profiles(&ListAccessProfilesInput {
                    space_id: space_id.clone(),
                    max_results: Some(page_size),
                    next_token: token.clone(),
                })
                .await
                .unwrap_or_else(|error| panic!("case {page_size}: the page failed: {error:?}"));
            assert!(
                page.items.len() <= page_size as usize,
                "case {page_size}: a page carried {} items",
                page.items.len()
            );
            walked.extend(page.items.iter().map(|item| item.profile_id.clone()));
            token = page.next_token;
            if token.is_none() {
                break;
            }
            assert!(
                walked.len() < created.len() + 1,
                "case {page_size}: paging ran past the collection with {walked:?}"
            );
        }
        assert_eq!(
            walked, created,
            "case {page_size}: walking the pages gave {walked:?}"
        );
    }

    // A space nobody has created a profile in holds exactly the one it was provisioned
    // with, in one page with no token — not a refusal and not an empty page.
    let empty_space = omni.space("empty", "profile-pages-empty").await;
    let only_provisioned = provisioned_profile(&omni, &empty_space).await;
    let empty = omni
        .list_access_profiles(&ListAccessProfilesInput {
            space_id: empty_space.clone(),
            ..Default::default()
        })
        .await
        .expect("a space with no created profile lists");
    assert_eq!(
        (
            empty
                .items
                .iter()
                .map(|item| item.profile_id.clone())
                .collect::<Vec<String>>(),
            empty.next_token.clone(),
        ),
        (vec![only_provisioned], None),
        "a space with no created profile listed {:?}",
        empty.items
    );
}

#[tokio::test]
async fn the_listing_changes_nothing_and_refuses_what_it_cannot_resume() {
    let omni = Omni::new();
    let space_id = omni.space("reads", "profile-reads").await;
    let created = seed(&omni, &space_id).await;

    // A read: the same request twice answers the same page, and the records are all
    // still readable afterwards.
    let request = ListAccessProfilesInput {
        space_id: space_id.clone(),
        max_results: Some(2),
        ..Default::default()
    };
    let first = omni
        .list_access_profiles(&request)
        .await
        .expect("the first listing succeeds");
    let again = omni
        .list_access_profiles(&request)
        .await
        .expect("the second listing succeeds");
    assert_eq!(
        (
            first
                .items
                .iter()
                .map(summary_fields)
                .collect::<Vec<(String, String, String, Option<String>, Option<String>)>>(),
            first.next_token.clone(),
        ),
        (
            again
                .items
                .iter()
                .map(summary_fields)
                .collect::<Vec<(String, String, String, Option<String>, Option<String>)>>(),
            again.next_token.clone(),
        ),
        "listing twice answered different pages: {:?} then {:?}",
        first.items,
        again.items
    );
    for profile_id in &created {
        assert!(
            omni.get_access_profile(&space_id, profile_id).await.is_ok(),
            "case {profile_id:?}: the listing consumed the profile"
        );
    }

    // A space the store does not hold is a miss rather than an empty page.
    expect_modelled_error(
        "an unknown space",
        omni.list_access_profiles(&ListAccessProfilesInput {
            space_id: MISSING_SPACE_ID.to_string(),
            ..Default::default()
        })
        .await,
        404,
        "ResourceNotFoundException",
    );

    // A `nextToken` this service did not mint for THIS collection cannot name a resume
    // point: rows span text that is not a token at all, hex that decodes to nothing
    // this service writes, and a token minted for another collection's listing.
    let foreign_collection = hex::encode("spaces:1");
    for token in [
        "not-a-token",
        "6e6f7065",
        foreign_collection.as_str(),
        &"f".repeat(2048),
    ] {
        expect_modelled_error(
            token,
            omni.list_access_profiles(&ListAccessProfilesInput {
                space_id: space_id.clone(),
                next_token: Some(token.to_string()),
                ..Default::default()
            })
            .await,
            400,
            "ValidationException",
        );
    }
}

#[tokio::test]
async fn a_deleted_profile_is_gone_from_the_listing() {
    let omni = Omni::new();
    let space_id = omni.space("delete", "profile-list-delete").await;
    let created = seed(&omni, &space_id).await;

    // The listing answers from the same store the delete writes, so a removed record is
    // gone from it and the ones left behind keep their creation order.
    let removed = created[1].clone();
    omni.delete_access_profile(&space_id, &removed)
        .await
        .expect("the delete succeeds");
    let expected: Vec<String> = created
        .iter()
        .filter(|profile_id| **profile_id != removed)
        .cloned()
        .collect();
    let page = omni
        .list_access_profiles(&ListAccessProfilesInput {
            space_id: space_id.clone(),
            ..Default::default()
        })
        .await
        .expect("the space lists");
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.profile_id.clone())
            .collect::<Vec<String>>(),
        expected,
        "the listing after the delete is {:?}",
        page.items
    );

    // Paging after the delete walks the shortened collection, so no page resumes from
    // the record that is gone.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = omni
            .list_access_profiles(&ListAccessProfilesInput {
                space_id: space_id.clone(),
                max_results: Some(1),
                next_token: token.clone(),
            })
            .await
            .expect("the page after the delete succeeds");
        walked.extend(page.items.iter().map(|item| item.profile_id.clone()));
        token = page.next_token;
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        walked, expected,
        "paging after the delete gave {walked:?}"
    );
}

#[tokio::test]
async fn the_listing_answers_over_the_rpcv2cbor_wire() {
    let omni = Omni::new();
    // The parent space and its profiles are created through the handlers holding the
    // same engine the router's service holds, which is what makes the wire call below
    // see them.
    let space_id = omni.space("wire", "profile-list-wire").await;
    let created = seed(&omni, &space_id).await;

    let codec = CborCodec::default();
    let (status, body) = omni
        .over_the_wire(
            "ListAccessProfiles",
            LIST_ACCESS_PROFILES_INPUT_SCHEMA,
            &ListAccessProfilesInput {
                space_id: space_id.clone(),
                max_results: Some(2),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        status,
        200,
        "ListAccessProfiles over the wire answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let page = {
        let mut deserializer = codec.create_deserializer(&body);
        ListAccessProfilesOutput::deserialize(&mut deserializer).expect("the output deserializes")
    };
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.profile_id.clone())
            .collect::<Vec<String>>(),
        created[..2].to_vec(),
        "the wire page is {:?}",
        page.items
    );

    // The token the wire page carried resumes the SAME order over the wire.
    let token = page
        .next_token
        .clone()
        .expect("profiles remain beyond the first wire page");
    let (status, body) = omni
        .over_the_wire(
            "ListAccessProfiles",
            LIST_ACCESS_PROFILES_INPUT_SCHEMA,
            &ListAccessProfilesInput {
                space_id: space_id.clone(),
                max_results: Some(100),
                next_token: Some(token),
            },
        )
        .await;
    assert_eq!(
        status,
        200,
        "the resumed wire page answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let rest = {
        let mut deserializer = codec.create_deserializer(&body);
        ListAccessProfilesOutput::deserialize(&mut deserializer).expect("the output deserializes")
    };
    assert_eq!(
        (
            rest.items
                .iter()
                .map(|item| item.profile_id.clone())
                .collect::<Vec<String>>(),
            rest.next_token.clone(),
        ),
        (created[2..].to_vec(), None),
        "the resumed wire page is {:?}",
        rest.items
    );

    // A space the store does not hold is the modelled 404 on the wire too.
    let (status, body) = omni
        .over_the_wire(
            "ListAccessProfiles",
            LIST_ACCESS_PROFILES_INPUT_SCHEMA,
            &ListAccessProfilesInput {
                space_id: MISSING_SPACE_ID.to_string(),
                ..Default::default()
            },
        )
        .await;
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 404,
        "an unknown space over the wire answered {status}: {rendered:?}"
    );
    assert!(
        rendered.contains("ResourceNotFoundException"),
        "the wire error body does not name ResourceNotFoundException: {rendered:?}"
    );
}
