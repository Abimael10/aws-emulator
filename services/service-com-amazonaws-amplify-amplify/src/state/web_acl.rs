// SPDX-License-Identifier: CC0-1.0

//! The WAF firewall in front of a hosted app: the web ACL that is associated with it,
//! and how far that association has got.
//!
//! `App$wafConfiguration` "describes the Firewall configuration for a hosted Amplify
//! application … a direct integration with WAF"
//! (`smithy_query(query="doc",shape="WafConfiguration")`), and the association itself is
//! WAFv2's own record: `AssociateWebACL` puts a web ACL in front of the app's ARN and
//! `DisassociateWebACL` takes it away. So this service keeps NO second copy of it — the
//! configuration an app reports is READ from that record on every report
//! ([`waf_configuration`]), exactly the way the API Gateway stage's `webAclArn` is. An app
//! then cannot report a firewall WAF no longer holds, an association made before or after
//! the app was read needs nothing written here, and WAF's own reader
//! (`GetWebACLForResource`) and this member cannot disagree.
//!
//! `wafStatus` is "the status of the process to associate or disassociate a web ACL to an
//! Amplify app" (`WafConfiguration$wafStatus`), so it reports where that PROCESS has got
//! to: `ASSOCIATING` while it is still being applied and `ASSOCIATION_SUCCESS` once it
//! is — the two statuses an association moves through, out of the `WafStatus` value set
//! (`smithy_query(query="shape",shape="WafStatus")`). An app no web ACL is in front of
//! reports no configuration at all.

use crate::ops::types::WafConfiguration;

/// How long after `AssociateWebACL` an app reports its firewall as still ASSOCIATING.
///
/// The association is a process rather than an instant — "the changes take a small
/// amount of time to propagate to all areas where the resources are stored"
/// (WAFv2 `AssociateWebACL`'s own doc) — so a caller that reads the app straight after
/// associating sees `ASSOCIATING`, and one that reads it again later sees
/// `ASSOCIATION_SUCCESS`. Nothing waits for the span: the instant the association was
/// made is in WAF's record, and the status is computed from it on each report.
pub const ASSOCIATION_SETTLES_AFTER_MILLIS: i64 = 3_000;

/// `WafConfiguration$wafStatus` while the association is still being applied.
pub const WAF_STATUS_ASSOCIATING: &str = "ASSOCIATING";

/// `WafConfiguration$wafStatus` once it has been applied.
pub const WAF_STATUS_ASSOCIATION_SUCCESS: &str = "ASSOCIATION_SUCCESS";

/// The firewall configuration of the app whose ARN is `app_arn`, or `None` when no web
/// ACL is in front of it.
///
/// `None` as well when this emulator does not mount WAFv2 at all: with no one holding
/// associations, no app can carry one, which is the same answer as an app nothing was
/// put in front of.
///
/// Call it with NO lock of this service's store held: it locks the WAFv2 store, and that
/// service holds no record of this one, so the two are never held at the same time.
pub fn waf_configuration<Eng: Send + Sync + 'static>(
    engine: &Eng,
    app_arn: &str,
) -> Option<WafConfiguration> {
    let waf = ::service_com_amazonaws_wafv2_awswaf_20190729::state::service_state(engine)?;
    let association = {
        let store = waf.lock();
        store.web_acl_association(app_arn).cloned()?
    };
    Some(WafConfiguration {
        web_acl_arn: Some(association.web_acl_arn),
        waf_status: Some(
            waf_status(association.associated_at_millis, super::now_millis()).to_string(),
        ),
        // "The reason for the current status of the Firewall configuration": an
        // association that is being applied, or has been, carries no reason to report —
        // the status says all there is to say about it.
        status_reason: None,
    })
}

/// The app record `app` as a reader reports it: carrying the firewall configuration of
/// the web ACL WAF currently has in front of it.
///
/// Fills the member on the OUTGOING copy only, out of WAF's own record, so a read
/// changes nothing a later caller sees and every operation that answers with an app
/// reports the same firewall.
pub fn reported_waf_configuration<Eng: Send + Sync + 'static>(
    engine: &Eng,
    mut app: crate::ops::types::App,
) -> crate::ops::types::App {
    app.waf_configuration = waf_configuration(engine, &app.app_arn);
    app
}

/// The `wafStatus` of an association made at `associated_at_millis`, read at `now`.
fn waf_status(associated_at_millis: i64, now: i64) -> &'static str {
    if now.saturating_sub(associated_at_millis) < ASSOCIATION_SETTLES_AFTER_MILLIS {
        WAF_STATUS_ASSOCIATING
    } else {
        WAF_STATUS_ASSOCIATION_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The association reports the process as running until it has settled, and as
    /// successful from then on — the boundary included, since the span is how long it
    /// takes and not how long it is still running after that.
    #[test]
    fn the_status_settles_once_the_span_has_passed() {
        let associated_at = 1_700_000_000_000;
        for (case, elapsed, expected) in [
            ("read in the same millisecond", 0, WAF_STATUS_ASSOCIATING),
            ("read a millisecond later", 1, WAF_STATUS_ASSOCIATING),
            (
                "read a millisecond before the span is up",
                ASSOCIATION_SETTLES_AFTER_MILLIS - 1,
                WAF_STATUS_ASSOCIATING,
            ),
            (
                "read exactly when the span is up",
                ASSOCIATION_SETTLES_AFTER_MILLIS,
                WAF_STATUS_ASSOCIATION_SUCCESS,
            ),
            (
                "read long after the span is up",
                ASSOCIATION_SETTLES_AFTER_MILLIS * 1_000,
                WAF_STATUS_ASSOCIATION_SUCCESS,
            ),
        ] {
            let status = waf_status(associated_at, associated_at + elapsed);
            assert_eq!(
                status, expected,
                "case {case:?}: expected {expected}, got {status}"
            );
        }
    }
}
