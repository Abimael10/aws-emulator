// SPDX-License-Identifier: CC0-1.0

//! The bodies of the AUTOMATION runbooks Systems Manager publishes, beside the handful
//! that already lived in [`super::documents`].
//!
//! One file for one concern, read only by the catalogue table
//! ([`super::PUBLISHED_DOCUMENTS`]). Each body declares the parameters the "Systems
//! Manager Automation runbook reference" of the Amazon Web Services Systems Manager User
//! Guide documents for that runbook, so a read derives its description, schema version and
//! parameters from the body rather than restating them; an Automation action runs in the
//! service rather than on a node, so a runbook restricts itself to no platform. The body
//! is this emulator's stand-in for the published body, the way the bodies already in
//! [`super::documents`] are.

/// Starts a stopped EC2 instance and waits for it to be running.
pub const START_EC2_INSTANCE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Start one Amazon EC2 instance and wait until it is running.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "StringList",
      "description": "(Required) The id of the EC2 instance to start."
    }
  },
  "mainSteps": [
    {
      "name": "startInstances",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": "{{ InstanceId }}",
        "DesiredState": "running"
      }
    }
  ]
}"#;

/// Stops a running EC2 instance and waits for it to be stopped.
pub const STOP_EC2_INSTANCE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Stop one Amazon EC2 instance and wait until it is stopped.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "StringList",
      "description": "(Required) The id of the EC2 instance to stop."
    }
  },
  "mainSteps": [
    {
      "name": "stopInstances",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": "{{ InstanceId }}",
        "DesiredState": "stopped"
      }
    }
  ]
}"#;

/// Stops an EC2 instance and starts it again.
pub const RESTART_EC2_INSTANCE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Restart one Amazon EC2 instance and wait until it is running again.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "StringList",
      "description": "(Required) The id of the EC2 instance to restart."
    }
  },
  "mainSteps": [
    {
      "name": "stopInstances",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": "{{ InstanceId }}",
        "DesiredState": "stopped"
      }
    },
    {
      "name": "startInstances",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": "{{ InstanceId }}",
        "DesiredState": "running"
      }
    }
  ]
}"#;

/// Terminates an EC2 instance.
pub const TERMINATE_EC2_INSTANCE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Terminate one Amazon EC2 instance and wait until it is terminated.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "StringList",
      "description": "(Required) The id of the EC2 instance to terminate."
    }
  },
  "mainSteps": [
    {
      "name": "terminateInstances",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": "{{ InstanceId }}",
        "DesiredState": "terminated"
      }
    }
  ]
}"#;

/// Creates an AMI from a running or stopped EC2 instance.
pub const CREATE_IMAGE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Create an Amazon Machine Image from one Amazon EC2 instance.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "String",
      "description": "(Required) The id of the EC2 instance the image is created from."
    },
    "ImageName": {
      "type": "String",
      "description": "(Optional) The name of the image to create.",
      "default": "AMI created by Automation"
    },
    "ImageDescription": {
      "type": "String",
      "description": "(Optional) The description of the image to create.",
      "default": "AMI created by AWS Systems Manager Automation"
    },
    "NoReboot": {
      "type": "Boolean",
      "description": "(Optional) Whether to leave the instance running while the image is created.",
      "default": true
    }
  },
  "mainSteps": [
    {
      "name": "createImage",
      "action": "aws:createImage",
      "inputs": {
        "InstanceId": "{{ InstanceId }}",
        "ImageName": "{{ ImageName }}",
        "ImageDescription": "{{ ImageDescription }}",
        "NoReboot": "{{ NoReboot }}"
      }
    }
  ]
}"#;

/// Creates a snapshot of an EBS volume.
pub const CREATE_SNAPSHOT: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Create a snapshot of one Amazon EBS volume.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "VolumeId": {
      "type": "String",
      "description": "(Required) The id of the EBS volume the snapshot is taken of."
    },
    "Description": {
      "type": "String",
      "description": "(Optional) The description of the snapshot to create.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "createSnapshot",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "ec2",
        "Api": "CreateSnapshot",
        "VolumeId": "{{ VolumeId }}",
        "Description": "{{ Description }}"
      }
    }
  ]
}"#;

/// Deletes a snapshot of an EBS volume.
pub const DELETE_SNAPSHOT: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Delete one snapshot of an Amazon EBS volume.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "SnapshotId": {
      "type": "String",
      "description": "(Required) The id of the snapshot to delete."
    }
  },
  "mainSteps": [
    {
      "name": "deleteSnapshot",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "ec2",
        "Api": "DeleteSnapshot",
        "SnapshotId": "{{ SnapshotId }}"
      }
    }
  ]
}"#;

/// Patches a Linux AMI and registers the updated image.
pub const UPDATE_LINUX_AMI: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Update a Linux Amazon Machine Image and register the updated image.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "SourceAmiId": {
      "type": "String",
      "description": "(Required) The id of the AMI the update starts from."
    },
    "IamInstanceProfileName": {
      "type": "String",
      "description": "(Required) The name of the instance profile the temporary instance is launched with."
    },
    "TargetAmiName": {
      "type": "String",
      "description": "(Optional) The name of the updated AMI.",
      "default": "UpdateLinuxAmi_from_{{ SourceAmiId }}"
    },
    "InstanceType": {
      "type": "String",
      "description": "(Optional) The instance type of the temporary instance.",
      "default": "t2.micro"
    },
    "PreUpdateScript": {
      "type": "String",
      "description": "(Optional) The script to run before the packages are updated.",
      "default": "None"
    },
    "PostUpdateScript": {
      "type": "String",
      "description": "(Optional) The script to run after the packages are updated.",
      "default": "None"
    },
    "IncludePackages": {
      "type": "String",
      "description": "(Optional) The packages to update, space separated. 'all' updates every package.",
      "default": "all"
    },
    "ExcludePackages": {
      "type": "String",
      "description": "(Optional) The packages to leave at their installed version, space separated.",
      "default": "none"
    },
    "SubnetId": {
      "type": "String",
      "description": "(Optional) The subnet the temporary instance is launched in.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "launchInstance",
      "action": "aws:runInstances",
      "inputs": {
        "ImageId": "{{ SourceAmiId }}",
        "InstanceType": "{{ InstanceType }}",
        "IamInstanceProfileName": "{{ IamInstanceProfileName }}",
        "SubnetId": "{{ SubnetId }}"
      }
    },
    {
      "name": "updatePackages",
      "action": "aws:runCommand",
      "inputs": {
        "DocumentName": "AWS-RunShellScript",
        "Parameters": {
          "commands": ["update-packages {{ IncludePackages }}"]
        }
      }
    },
    {
      "name": "createUpdatedImage",
      "action": "aws:createImage",
      "inputs": {
        "ImageName": "{{ TargetAmiName }}"
      }
    }
  ]
}"#;

/// Patches a node, taking a snapshot first so the patching can be rolled back.
pub const PATCH_INSTANCE_WITH_ROLLBACK: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Patch one managed node, snapshotting its root volume first so the patching can be rolled back.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "String",
      "description": "(Required) The id of the managed node to patch."
    },
    "SnapshotDescription": {
      "type": "String",
      "description": "(Optional) The description of the snapshot taken before the patching.",
      "default": "Snapshot taken before patching"
    },
    "RebootOption": {
      "type": "String",
      "description": "(Optional) Whether the node may be rebooted to finish the patching.",
      "allowedValues": ["RebootIfNeeded", "NoReboot"],
      "default": "RebootIfNeeded"
    }
  },
  "mainSteps": [
    {
      "name": "createRollbackSnapshot",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "ec2",
        "Api": "CreateSnapshot",
        "Description": "{{ SnapshotDescription }}"
      }
    },
    {
      "name": "patchInstance",
      "action": "aws:runCommand",
      "inputs": {
        "DocumentName": "AWS-RunPatchBaseline",
        "InstanceIds": ["{{ InstanceId }}"],
        "Parameters": {
          "Operation": "Install",
          "RebootOption": "{{ RebootOption }}"
        }
      }
    }
  ]
}"#;

/// Changes the instance type of a stopped EC2 instance.
pub const RESIZE_INSTANCE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Change the instance type of one Amazon EC2 instance.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "InstanceId": {
      "type": "String",
      "description": "(Required) The id of the EC2 instance to resize."
    },
    "InstanceType": {
      "type": "String",
      "description": "(Required) The instance type the instance is changed to."
    }
  },
  "mainSteps": [
    {
      "name": "stopInstance",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": ["{{ InstanceId }}"],
        "DesiredState": "stopped"
      }
    },
    {
      "name": "modifyInstanceAttribute",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "ec2",
        "Api": "ModifyInstanceAttribute",
        "InstanceId": "{{ InstanceId }}",
        "InstanceType": { "Value": "{{ InstanceType }}" }
      }
    },
    {
      "name": "startInstance",
      "action": "aws:changeInstanceState",
      "inputs": {
        "InstanceIds": ["{{ InstanceId }}"],
        "DesiredState": "running"
      }
    }
  ]
}"#;
