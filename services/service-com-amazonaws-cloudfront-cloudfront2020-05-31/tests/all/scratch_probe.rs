// SPDX-License-Identifier: CC0-1.0

//! A PROBE of the account's Anycast static IP list entitlement where NOTHING publishes a
//! quota, end to end over HTTP.
//!
//! The companion of [`crate::anycast_ip_list_entitlement`], which drives the same rule on an
//! emulator whose Service Quotas service IS mounted. How many Anycast static IP lists an
//! account may hold is the value its Service Quotas quota for them is in force at, and an
//! engine carrying no Service Quotas store publishes no quota for the account to be read at —
//! so the entitlement is the same AWS default a fresh account's quota carries, which is ZERO:
//! every `CreateAnycastIpList` is refused with the modelled `EntityLimitExceeded` (HTTP 400)
//! under the sentence that tells the caller to have the quota raised, and nothing is
//! allocated.
//!
//! The rows span the creates the lifecycle suite's table holds, so the refusal is asserted for
//! every shape of create rather than for one.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::anycast_ip_list_lifecycle::{create_table, post_anycast_ip_list};

/// The sentence the refusal carries, spelled out here rather than taken from the crate, so a
/// reworded refusal fails this suite.
const ENTITLEMENT_EXCEEDED_MESSAGE: &str =
    "To request a limit increase for your Anycast static IP list, contact AWS Support: \
     https://us-east-1.console.aws.amazon.com/servicequotas/home/services/cloudfront/quotas/\
     L-6A19EDFD";

/// An emulator carrying ONLY this service: no Service Quotas store is mounted on its engine,
/// so the account's quotas cannot be read at all.
fn emulator_without_service_quotas() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

#[tokio::test]
async fn an_engine_that_publishes_no_quota_entitles_the_account_to_no_list() {
    let router = emulator_without_service_quotas();

    for create in create_table() {
        let answer = post_anycast_ip_list(&router, &create.xml()).await;
        assert_eq!(
            answer.status, 400,
            "case {}: expected HTTP 400, got {} with {}",
            create.case, answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityLimitExceeded",
            "case {}: expected Code EntityLimitExceeded, got {:?}",
            create.case,
            answer.error_code()
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            message, ENTITLEMENT_EXCEEDED_MESSAGE,
            "case {}: expected the limit-increase sentence, got {message:?}",
            create.case
        );
    }
}
