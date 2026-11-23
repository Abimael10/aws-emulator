// SPDX-License-Identifier: CC0-1.0

//! WHICH of a launch's own readings happen BEFORE `RunInstances` answers a `DryRun` probe,
//! driven end to end over the wire the service speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness).
//!
//! The rule under test: `DryRun` "checks whether you have the required permissions for the
//! action, WITHOUT ACTUALLY MAKING THE REQUEST, and provides an error response. If you have
//! the required permissions, the error response is DryRunOperation"
//! (`RunInstancesRequest$DryRun`). So the launch's OWN parameters — a combination of members
//! that cannot be given together, a value that is no id of the kind the parameter names — and
//! the resources the request NAMES are read first, and a launch that cannot be served for one
//! of them is refused for THAT member whether or not the flag is set. Only the refusals that
//! come out of reading two of the named resources AGAINST EACH OTHER belong to the real call,
//! which the sibling suite `tests/all/dry_run_probe_against_request_validation.rs` pins from
//! the other side.
//!
//! Every row below is one input along what the rule generalises over — which member is at
//! fault, whether the resource it names is held, and how a security group is named (by name
//! beside a subnet, by name alone, as a value that is no group id, on the launch itself and on
//! one of its network interfaces) — so no row is the specification. Each row is sent twice:
//! once with the flag and once without, because the rule is that both answer the same.

use service_com_amazonaws_ec2_amazonec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE,
    DRY_RUN_OPERATION_STATUS, INVALID_GROUP_NOT_FOUND, INVALID_KEY_PAIR_NOT_FOUND,
    INVALID_PARAMETER_COMBINATION,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for: the account's default VPC and its default subnets
/// are seeded for it, so a launch that names no network of its own is placed.
const REGION: &str = "us-east-1";

/// The AMI every launch here names. Which image an instance comes from has nothing to do with
/// where the probe is answered.
const AMI: &str = "ami-0123456789abcdef0";

/// The status every refusal of a launch's own members carries.
const REFUSAL_STATUS: u16 = 400;

/// The key pair the account holds for the rows that name a held one.
const HELD_KEY_PAIR: &str = "deploy-key";

/// What one row expects of both calls it makes.
enum Expected {
    /// The launch's own members are read before the flag, so both calls are refused this code
    /// and this sentence and neither launches anything.
    Refused {
        code: &'static str,
        message: String,
    },
    /// Nothing of the launch's own members is at fault: the probe is the whole answer, and the
    /// same request without the flag is served.
    Probe,
    /// The other side of the line: the probe is still the whole answer, because the refusal
    /// comes out of reading two of the resources the launch named AGAINST EACH OTHER — which
    /// the real call does and the probe does not reach. The flagless call is refused `code`.
    ProbeThenRefused { code: &'static str },
}

/// One row: the case, the members the launch carries beside the defaults, and what it answers.
struct Row {
    case: &'static str,
    params: Vec<Param>,
    expected: Expected,
}

/// The members every launch below carries beside the ones its row is about.
fn launch(extra: Vec<Param>) -> Vec<Param> {
    let mut params = vec![
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    params.extend(extra);
    params
}

/// `NetworkInterface.<index>` form fields for one entry of a launch's `NetworkInterfaces`.
fn launch_interface(index: usize, fields: &[(&str, &str)]) -> Vec<Param> {
    fields
        .iter()
        .map(|(name, value)| param(&format!("NetworkInterface.{index}.{name}"), value))
        .collect()
}

/// One `action` call carrying `params`.
async fn ec2_call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that must be served, as its body.
async fn served(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with body {body}"
    );
    body
}

/// One subnet of this Region's default network, and the id and name of a security group of the
/// same VPC — the resources a launch of this suite names when its row is about them.
async fn default_network(router: &MultiServiceRouter) -> (String, String, String) {
    let body = served("the default network", router, "DescribeSubnets", &[]).await;
    let subnet_id = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("the default network must have a subnet: {body}"));
    let body = served("the default network", router, "DescribeSecurityGroups", &[]).await;
    let group_id = element(&body, "groupId")
        .unwrap_or_else(|| panic!("the default network must have a security group: {body}"));
    let group_name = element(&body, "groupName")
        .unwrap_or_else(|| panic!("a security group must report its name: {body}"));
    (subnet_id, group_id, group_name)
}

/// RULE: a launch's own members and the resources it names are read BEFORE the probe is
/// answered, so each row answers the same thing with the flag and without it.
#[tokio::test]
async fn a_launchs_own_members_are_read_before_the_probe_is_answered() {
    let (router, engine) = super::ec2_query::emulator();
    let (subnet_id, held_group_id, held_group_name) = default_network(&router).await;
    // The key pair the "held" row names, created through the operation that owns it.
    served(
        "the held key pair",
        &router,
        "CreateKeyPair",
        &[param("KeyName", HELD_KEY_PAIR)],
    )
    .await;

    let rows: Vec<Row> = vec![
        // Nothing wrong with the launch's own members: the probe is the whole answer.
        Row {
            case: "a launch that would have succeeded",
            params: launch(Vec::new()),
            expected: Expected::Probe,
        },
        Row {
            case: "a launch naming a key pair the account holds",
            params: launch(vec![param("KeyName", HELD_KEY_PAIR)]),
            expected: Expected::Probe,
        },
        Row {
            case: "a launch naming a group of its subnet's VPC by id",
            params: launch(
                vec![param("SubnetId", &subnet_id)]
                    .into_iter()
                    .chain(indexed("SecurityGroupId", &[held_group_id.as_str()]))
                    .collect(),
            ),
            expected: Expected::Probe,
        },
        // The resources the launch NAMES: a key pair the account holds none of names no
        // credential its instances could carry. The two rows differ along the name's
        // character set, which is what a key pair name generalises over.
        Row {
            case: "a key pair name the account holds none of",
            params: launch(vec![param("KeyName", "never-created")]),
            expected: Expected::Refused {
                code: INVALID_KEY_PAIR_NOT_FOUND,
                message: "The key pair 'never-created' does not exist".to_string(),
            },
        },
        Row {
            case: "a key pair name of punctuation and digits the account holds none of",
            params: launch(vec![param("KeyName", "ops_key.1")]),
            expected: Expected::Refused {
                code: INVALID_KEY_PAIR_NOT_FOUND,
                message: "The key pair 'ops_key.1' does not exist".to_string(),
            },
        },
        // The launch's own members read against each other: a group NAME is "[Default VPC]
        // The names of the security groups" (`$SecurityGroups`), so it cannot be given beside
        // the subnet the launch is placed in — not even when the account holds that group.
        Row {
            case: "a security group named by name beside a subnet",
            params: launch(
                vec![param("SubnetId", &subnet_id)]
                    .into_iter()
                    .chain([param("SecurityGroup.1", &held_group_name)])
                    .collect(),
            ),
            expected: Expected::Refused {
                code: INVALID_PARAMETER_COMBINATION,
                message: "The parameter groupName cannot be used with the parameter subnet"
                    .to_string(),
            },
        },
        Row {
            case: "a group name the account holds none of, beside a subnet",
            params: launch(
                vec![param("SubnetId", &subnet_id)]
                    .into_iter()
                    .chain([param("SecurityGroup.1", "never-created")])
                    .collect(),
            ),
            expected: Expected::Refused {
                code: INVALID_PARAMETER_COMBINATION,
                message: "The parameter groupName cannot be used with the parameter subnet"
                    .to_string(),
            },
        },
        // The same name with NO subnet of the launch's own is the default VPC's own spelling
        // of a group, which is served.
        Row {
            case: "a security group of the default network, named by name",
            params: launch(vec![param("SecurityGroup.1", &held_group_name)]),
            expected: Expected::Probe,
        },
        // The same key-pair reading on a launch of a different SHAPE: one that names its
        // network interfaces rather than its subnet. The member at fault is the launch's own
        // either way.
        Row {
            case: "a key pair nothing holds, on a launch naming its network interfaces",
            params: launch(launch_interface(
                1,
                &[("DeviceIndex", "0"), ("SubnetId", subnet_id.as_str())],
            ))
            .into_iter()
            .chain([param("KeyName", "never-created")])
            .collect(),
            expected: Expected::Refused {
                code: INVALID_KEY_PAIR_NOT_FOUND,
                message: "The key pair 'never-created' does not exist".to_string(),
            },
        },
        // A WELL-FORMED group id the account holds none of is the other side of the line: it
        // is resolved against the subnet the launch would sit in, which the probe comes
        // before, so the probe wins over that refusal.
        Row {
            case: "a well-formed group id the account holds none of",
            params: launch(
                vec![param("SubnetId", &subnet_id)]
                    .into_iter()
                    .chain(indexed("SecurityGroupId", &["sg-0000000000000ffff"]))
                    .collect(),
            ),
            expected: Expected::ProbeThenRefused {
                code: INVALID_GROUP_NOT_FOUND,
            },
        },
    ];

    let instances = || engine.state::<Ec2State>().lock().instances.len();
    let mut launched = 0usize;

    for Row {
        case,
        params,
        expected,
    } in rows
    {
        let mut probe = params.clone();
        probe.push(param("DryRun", "true"));
        let (status, body) = ec2_call(&router, "RunInstances", &probe).await;
        match &expected {
            Expected::Probe | Expected::ProbeThenRefused { .. } => {
                assert_eq!(
                    status, DRY_RUN_OPERATION_STATUS,
                    "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS} from the probe, \
                     got {status} with body {body}"
                );
                let answered = error_code(&body);
                assert_eq!(
                    answered, DRY_RUN_OPERATION,
                    "case {case:?}: expected {DRY_RUN_OPERATION} from the probe, got \
                     {answered:?} in {body}"
                );
                let message = error_message(&body);
                assert_eq!(
                    message, DRY_RUN_OPERATION_MESSAGE,
                    "case {case:?}: expected EC2's own probe sentence, got {message:?}"
                );
            }
            Expected::Refused { code, message } => {
                assert_eq!(
                    status, REFUSAL_STATUS,
                    "case {case:?}: expected HTTP {REFUSAL_STATUS} from the probe, got \
                     {status} with body {body}"
                );
                let answered = error_code(&body);
                assert_eq!(
                    &answered, code,
                    "case {case:?}: expected {code} from the probe, got {answered:?} in {body}"
                );
                let answered = error_message(&body);
                assert_eq!(
                    &answered, message,
                    "case {case:?}: expected the probe's sentence {message:?}, got {answered:?}"
                );
            }
        }

        // The SAME request without the flag: a refused row is refused the same way, and a
        // probe row is served.
        let (status, body) = ec2_call(&router, "RunInstances", &params).await;
        match &expected {
            Expected::Probe => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: without the flag the launch must be served, got {status} \
                     with body {body}"
                );
                launched += 1;
            }
            Expected::ProbeThenRefused { code } => {
                let answered = error_code(&body);
                assert_eq!(
                    (status, answered.as_str()),
                    (REFUSAL_STATUS, *code),
                    "case {case:?}: without the flag the launch must be refused {code}, got \
                     {status} with body {body}"
                );
            }
            Expected::Refused { code, message } => {
                let answered = error_code(&body);
                assert_eq!(
                    (status, answered.as_str()),
                    (REFUSAL_STATUS, *code),
                    "case {case:?}: without the flag the launch must be refused {code}, got \
                     {status} with body {body}"
                );
                let answered = error_message(&body);
                assert_eq!(
                    &answered, message,
                    "case {case:?}: without the flag the sentence must be {message:?}, got \
                     {answered:?}"
                );
            }
        }
    }

    // Every probe performed nothing and every refusal launched nothing: the store holds
    // exactly the instances the served rows minted.
    assert_eq!(
        instances(),
        launched,
        "the store must hold exactly the {launched} instances the served launches minted"
    );
}
