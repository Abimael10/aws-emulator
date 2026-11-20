// SPDX-License-Identifier: CC0-1.0

//! What this API holds a caller-supplied ARN to, read by every handler BEFORE it consults
//! `crate::state`.
//!
//! The modelled `Arn` shape bounds only the characters and the length of the value a member
//! carries — `@length 1..255`, `@pattern ^[A-Za-z0-9:.\/_-]*$`
//! (`smithy_query(query="shape", shape="Arn", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`)
//! — so a value that names no resource of this API at all satisfies it and reaches the
//! handler. Route 53 ARC decides whether the value IS an ARN of the resource the member
//! documents before it decides whether that resource exists: a value that is not one is a
//! permanent CALLER error, refused on the input with `ValidationException` (HTTP 400) and
//! the one fixed sentence the boundary refusal already states
//! ([`super::refusal::INVALID_RESOURCE_ARN_MESSAGE`]).
//!
//! It is never the not-found class: `ResourceNotFoundException` says a resource is missing,
//! which a client answers by re-resolving its control panel or failing over to another
//! cluster endpoint — a retry loop a caller bug must not enter. So the two refusals are kept
//! apart here, in front of every lookup, and only a value that names a resource of the right
//! KIND is looked up at all.
//!
//! Which values are read: every ARN-bearing member of this model, the same set the boundary
//! rewording covers ([`super::refusal`]) — `RoutingControlArn` on the get and both updates
//! and on a batch entry ([`ROUTING_CONTROL`]), the listing's `ControlPanelArn` filter
//! ([`CONTROL_PANEL`]), and each element of `SafetyRulesToOverride` ([`SAFETY_RULE`]). The
//! check runs where the boundary's would, in front of the quota and the authorization
//! decision, so a value that is not an ARN of the documented resource is refused in the same
//! place whether the modelled pattern catches it or not.
//!
//! Well-formedness itself is the tree's ONE ARN parse (`service_common::arn`), and the
//! vendor and the resource form are the control plane's: these are the ARNs IT mints, so
//! this service cannot disagree with it about what a routing control's ARN is.

use crate::ops::types::ValidationException;

/// The control plane that mints every ARN this API addresses.
use service_com_amazonaws_route53recoverycontrolconfig_route53recoverycontrolconfig as recovery_control_config;

/// The ARN vendor (third field) every recovery-control resource carries, as the control
/// plane that creates them mints it.
const VENDOR: &str = recovery_control_config::state::ARN_SERVICE;

/// The `<type>` segments of a resource field, outermost first: what the resource a member
/// documents is written as.
///
/// The control plane nests a resource under its parent by appending to the parent's resource
/// field, so the segments alternate `<type>/<id>`: a control panel is `controlpanel/<id>`,
/// and a routing control and a safety rule are that panel's field with their own
/// `<type>/<id>` appended (`src/ops/create_control_panel.rs`,
/// `create_routing_control.rs`, `create_safety_rule.rs` of
/// `service-com-amazonaws-route53recoverycontrolconfig-route53recoverycontrolconfig`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    /// The type segments, outermost first.
    types: &'static [&'static str],
}

/// A routing control: `arn:<partition>:route53-recovery-control:<region>:<account>:controlpanel/<panel id>/routingcontrol/<control id>`.
pub const ROUTING_CONTROL: Kind = Kind {
    types: &["controlpanel", "routingcontrol"],
};

/// A control panel: the same ARN without the nested routing control.
pub const CONTROL_PANEL: Kind = Kind {
    types: &["controlpanel"],
};

/// A safety rule: a control panel's ARN with the rule nested under it.
pub const SAFETY_RULE: Kind = Kind {
    types: &["controlpanel", "safetyrule"],
};

impl Kind {
    /// Whether `value` is a well-formed ARN of a resource of this kind: the six
    /// colon-separated fields of an ARN, the recovery-control vendor, and a resource field
    /// that is exactly this kind's `<type>/<id>` segments with a non-empty id for each.
    ///
    /// The Region and the account fields are not read: AWS writes a partition-global
    /// resource with an empty Region (which is how these ARNs are minted), and WHOSE account
    /// a well-formed ARN names is an authorization question, not a well-formedness one.
    #[must_use]
    pub fn names(self, value: &str) -> bool {
        let Some(arn) = ::service_common::arn::parse_of(value, VENDOR) else {
            return false;
        };
        let mut segments = arn.resource.split('/');
        for expected in self.types {
            if segments.next() != Some(*expected) {
                return false;
            }
            if !segments.next().is_some_and(|id| !id.is_empty()) {
                return false;
            }
        }
        // Nothing may follow the kind's own id: a routing control's ARN is not a control
        // panel's, and a deeper path names a resource of some other kind again.
        segments.next().is_none()
    }
}

/// `Ok` when every value `arns` yields is a well-formed ARN of `kind`, and this API's
/// refusal of an invalid resource ARN as soon as one is not.
///
/// Every value the request carries is read, not just the first, so a batch is refused for
/// any entry whose ARN names no routing control. The sentence names nothing of the request,
/// so WHICH value failed does not change the answer.
pub fn check<'a>(
    kind: Kind,
    arns: impl IntoIterator<Item = &'a str>,
) -> Result<(), ValidationException> {
    if arns.into_iter().all(|value| kind.names(value)) {
        return Ok(());
    }
    Err(invalid_resource_arn())
}

/// The modelled `ValidationException` (HTTP 400) a value that is not an ARN of the resource
/// the member documents is refused with.
///
/// The one fixed sentence and nothing else: no `Reason`, no `Fields` and no value of the
/// request, so a handler's refusal is the same answer on the wire as the boundary's refusal
/// of a value outside the modelled pattern.
#[must_use]
pub fn invalid_resource_arn() -> ValidationException {
    ValidationException {
        message: super::refusal::INVALID_RESOURCE_ARN_MESSAGE.to_string(),
        reason: None,
        fields: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real routing control ARN, as the control plane mints one.
    const CONTROL: &str =
        "arn:aws:route53-recovery-control::000000000000:controlpanel/p0/routingcontrol/r0";
    /// Its control panel's.
    const PANEL: &str = "arn:aws:route53-recovery-control::000000000000:controlpanel/p0";
    /// A safety rule of that panel.
    const RULE: &str =
        "arn:aws:route53-recovery-control::000000000000:controlpanel/p0/safetyrule/s0";

    /// RULE: a member documented as the ARN of one kind of resource names one only when the
    /// value is a well-formed ARN of the recovery-control vendor whose resource field is that
    /// kind's own `<type>/<id>` path — so an ARN of another kind, of another vendor, or no
    /// ARN at all names nothing.
    #[test]
    fn each_kind_names_only_its_own_resource_form() {
        // (case, the value, which kinds it names)
        let rows: Vec<(&str, &str, Vec<Kind>)> = vec![
            ("a routing control ARN", CONTROL, vec![ROUTING_CONTROL]),
            ("a control panel ARN", PANEL, vec![CONTROL_PANEL]),
            ("a safety rule ARN", RULE, vec![SAFETY_RULE]),
            (
                "a cluster ARN",
                "arn:aws:route53-recovery-control::000000000000:cluster/c0",
                vec![],
            ),
            (
                "an ARN of another partition",
                "arn:aws-us-gov:route53-recovery-control::000000000000:controlpanel/p0/routingcontrol/r0",
                vec![ROUTING_CONTROL],
            ),
            (
                "an ARN carrying a Region",
                "arn:aws:route53-recovery-control:us-east-1:000000000000:controlpanel/p0",
                vec![CONTROL_PANEL],
            ),
            (
                "an ARN with no account",
                "arn:aws:route53-recovery-control:::controlpanel/p0",
                vec![CONTROL_PANEL],
            ),
            (
                "an ARN of another vendor",
                "arn:aws:route53:::controlpanel/p0/routingcontrol/r0",
                vec![],
            ),
            (
                "an ARN whose vendor differs in case",
                "arn:aws:Route53-Recovery-Control::000000000000:controlpanel/p0",
                vec![],
            ),
            (
                "a routing control ARN with a further path segment",
                "arn:aws:route53-recovery-control::000000000000:controlpanel/p0/routingcontrol/r0/x",
                vec![],
            ),
            (
                "a resource field whose id is empty",
                "arn:aws:route53-recovery-control::000000000000:controlpanel/",
                vec![],
            ),
            (
                "a resource field naming only a type",
                "arn:aws:route53-recovery-control::000000000000:controlpanel",
                vec![],
            ),
            ("a bare word", "not-an-arn", vec![]),
            ("one character", "a", vec![]),
            (
                "the characters the pattern admits, in no ARN form",
                "aZ0:./_-",
                vec![],
            ),
            ("an empty value", "", vec![]),
            (
                "a value whose first field is not arn",
                "urn:aws:route53-recovery-control::000000000000:controlpanel/p0",
                vec![],
            ),
            (
                "an ARN stopping before its resource field",
                "arn:aws:route53-recovery-control::000000000000",
                vec![],
            ),
        ];
        // (the kind, how a case names it)
        let kinds = [
            (ROUTING_CONTROL, "a routing control"),
            (CONTROL_PANEL, "a control panel"),
            (SAFETY_RULE, "a safety rule"),
        ];
        for (case, value, named) in rows {
            for (kind, label) in kinds {
                let expected = named.contains(&kind);
                let actual = kind.names(value);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected it to name {label}={expected}, got {actual}"
                );
            }
        }
    }

    /// RULE: a set of ARNs is accepted only when EVERY value in it names the kind asked for,
    /// and the refusal carries the fixed sentence alone.
    #[test]
    fn one_value_that_names_nothing_refuses_the_whole_set() {
        // (case, the values, whether the set is accepted as routing controls)
        let rows: Vec<(&str, Vec<&str>, bool)> = vec![
            ("no ARN at all", vec![], true),
            ("one routing control", vec![CONTROL], true),
            ("two routing controls", vec![CONTROL, CONTROL], true),
            ("one value that is no ARN", vec!["not-an-arn"], false),
            ("a routing control then a non-ARN", vec![CONTROL, "not-an-arn"], false),
            ("a non-ARN then a routing control", vec!["not-an-arn", CONTROL], false),
            ("a routing control then a panel", vec![CONTROL, PANEL], false),
        ];
        for (case, values, accepted) in rows {
            let outcome = check(ROUTING_CONTROL, values.iter().copied());
            match (accepted, &outcome) {
                (true, Ok(())) => {}
                (false, Err(refusal)) => {
                    assert_eq!(
                        (refusal.message.as_str(), refusal.reason.as_deref()),
                        (super::super::refusal::INVALID_RESOURCE_ARN_MESSAGE, None),
                        "case {case:?}: expected the fixed ARN refusal alone, got {refusal:?}"
                    );
                    assert!(
                        refusal.fields.is_none(),
                        "case {case:?}: the refusal names no member, got {:?}",
                        refusal.fields
                    );
                }
                _ => panic!("case {case:?}: expected accepted={accepted}, got {outcome:?}"),
            }
        }
    }
}
