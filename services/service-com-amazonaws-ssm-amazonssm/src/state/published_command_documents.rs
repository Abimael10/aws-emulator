// SPDX-License-Identifier: CC0-1.0

//! The bodies of the COMMAND and SESSION documents Systems Manager publishes, beside the
//! handful that already lived in [`super::documents`].
//!
//! One file for one concern: a body here is read by nothing but the catalogue table
//! ([`super::PUBLISHED_DOCUMENTS`]), which is what a read of a published document is
//! projected from. Each body is written in [`super::DEFAULT_DOCUMENT_FORMAT`] and declares
//! the parameters the "Systems Manager command document reference" and the "Session
//! document schema" of the Amazon Web Services Systems Manager User Guide document for
//! that document, plus the step whose plugin decides the platforms it is compatible with
//! ([`super::document_platforms`]) — so everything a read reports beyond the name, the type
//! and the publication date is DERIVED from the body here, exactly as it is for a document
//! a caller created. The body is this emulator's stand-in for the published body, the way
//! the bodies already in [`super::documents`] are.

/// Distributor's install/uninstall document: it runs a package the account or Amazon
/// published on the node.
pub const CONFIGURE_AWS_PACKAGE: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Install or uninstall an AWS Systems Manager Distributor package on a managed node.",
  "parameters": {
    "action": {
      "type": "String",
      "description": "(Required) Whether to install or uninstall the package.",
      "allowedValues": ["Install", "Uninstall"],
      "default": "Install"
    },
    "installationType": {
      "type": "String",
      "description": "(Optional) How to install the package when it is already installed.",
      "allowedValues": ["Uninstall and reinstall", "In-place update"],
      "default": "Uninstall and reinstall"
    },
    "name": {
      "type": "String",
      "description": "(Required) The name of the package to install or uninstall."
    },
    "version": {
      "type": "String",
      "description": "(Optional) The package version. An empty value installs the default version.",
      "default": ""
    },
    "additionalArguments": {
      "type": "StringMap",
      "description": "(Optional) The additional parameters to provide to the install or uninstall script.",
      "default": {}
    }
  },
  "mainSteps": [
    {
      "name": "configurePackage",
      "action": "aws:configurePackage",
      "inputs": {
        "action": "{{ action }}",
        "installationType": "{{ installationType }}",
        "name": "{{ name }}",
        "version": "{{ version }}",
        "additionalArguments": "{{ additionalArguments }}"
      }
    }
  ]
}"#;

/// Installs, repairs or uninstalls an MSI application, which is a Windows-only plugin.
pub const INSTALL_APPLICATION: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Install, repair or uninstall an application on a Windows managed node.",
  "parameters": {
    "action": {
      "type": "String",
      "description": "(Required) The action to take on the application.",
      "allowedValues": ["Install", "Repair", "Uninstall"],
      "default": "Install"
    },
    "parameters": {
      "type": "String",
      "description": "(Optional) The parameters to pass to the installer.",
      "default": ""
    },
    "source": {
      "type": "String",
      "description": "(Required) The URL of the .msi file to install."
    },
    "sourceHash": {
      "type": "String",
      "description": "(Optional) The SHA256 hash of the .msi file.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "installApplication",
      "action": "aws:applications",
      "inputs": {
        "action": "{{ action }}",
        "parameters": "{{ parameters }}",
        "source": "{{ source }}",
        "sourceHash": "{{ sourceHash }}"
      }
    }
  ]
}"#;

/// Installs a PowerShell module and runs commands against it: the PowerShell module
/// plugin, which runs on Windows.
pub const INSTALL_POWER_SHELL_MODULE: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Install a PowerShell module on a Windows managed node and run commands against it.",
  "parameters": {
    "source": {
      "type": "String",
      "description": "(Required) The URL of the PowerShell module to install."
    },
    "sourceHash": {
      "type": "String",
      "description": "(Optional) The SHA256 hash of the module archive.",
      "default": ""
    },
    "commands": {
      "type": "StringList",
      "description": "(Optional) The commands to run after the module is installed.",
      "default": []
    },
    "workingDirectory": {
      "type": "String",
      "description": "(Optional) The path to the working directory on the managed node.",
      "default": ""
    },
    "executionTimeout": {
      "type": "String",
      "description": "(Optional) The time in seconds for a command to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "installPowerShellModule",
      "action": "aws:psModule",
      "inputs": {
        "source": "{{ source }}",
        "sourceHash": "{{ sourceHash }}",
        "runCommand": "{{ commands }}",
        "workingDirectory": "{{ workingDirectory }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    }
  ]
}"#;

/// Installs the Windows updates a node is missing.
pub const INSTALL_WINDOWS_UPDATES: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Install Windows updates on a managed node, selected by category and severity.",
  "parameters": {
    "Action": {
      "type": "String",
      "description": "(Required) Whether to install the updates or only scan for them.",
      "allowedValues": ["Install", "Scan"],
      "default": "Install"
    },
    "AllowReboot": {
      "type": "String",
      "description": "(Optional) Whether the node may be rebooted to finish an update.",
      "allowedValues": ["True", "False"],
      "default": "True"
    },
    "IncludeKbs": {
      "type": "String",
      "description": "(Optional) The KB article ids to install, comma separated.",
      "default": ""
    },
    "ExcludeKbs": {
      "type": "String",
      "description": "(Optional) The KB article ids to leave out, comma separated.",
      "default": ""
    },
    "Categories": {
      "type": "String",
      "description": "(Optional) The update categories to install, comma separated.",
      "default": ""
    },
    "SeverityLevels": {
      "type": "String",
      "description": "(Optional) The update severity levels to install, comma separated.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "installWindowsUpdates",
      "action": "aws:runPowerShellScript",
      "inputs": {
        "commands": ["Install-WindowsUpdates"],
        "timeoutSeconds": "7200"
      }
    }
  ]
}"#;

/// Reports the Windows updates a node is missing, without installing any of them.
pub const FIND_WINDOWS_UPDATES: &str = r#"{
  "schemaVersion": "2.2",
  "description": "List the Windows updates a managed node is missing.",
  "parameters": {
    "UpdateLevel": {
      "type": "String",
      "description": "(Optional) The severity of the updates to report on.",
      "allowedValues": ["All", "Critical", "Important", "Low", "Moderate", "Unspecified"],
      "default": "All"
    },
    "KbArticleIds": {
      "type": "String",
      "description": "(Optional) The KB article ids to report on, comma separated.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "findWindowsUpdates",
      "action": "aws:runPowerShellScript",
      "inputs": {
        "commands": ["Get-MissingWindowsUpdates"],
        "timeoutSeconds": "3600"
      }
    }
  ]
}"#;

/// Downloads a script from a remote location (an S3 object or a Git repository) and runs
/// it: the download plugin runs on every platform, so the document is compatible with
/// all of them.
pub const RUN_REMOTE_SCRIPT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Download a script from a remote location and run it on a managed node.",
  "parameters": {
    "sourceType": {
      "type": "String",
      "description": "(Required) Where the script is downloaded from.",
      "allowedValues": ["GitHub", "S3", "HTTP"],
      "default": "S3"
    },
    "sourceInfo": {
      "type": "StringMap",
      "description": "(Required) The information required to retrieve the script from its source.",
      "default": {}
    },
    "commandLine": {
      "type": "String",
      "description": "(Required) The command line to run once the script is downloaded."
    },
    "workingDirectory": {
      "type": "String",
      "description": "(Optional) The path to the working directory on the managed node.",
      "default": ""
    },
    "executionTimeout": {
      "type": "String",
      "description": "(Optional) The time in seconds for a command to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "downloadContent",
      "action": "aws:downloadContent",
      "inputs": {
        "sourceType": "{{ sourceType }}",
        "sourceInfo": "{{ sourceInfo }}",
        "destinationPath": "{{ workingDirectory }}"
      }
    },
    {
      "name": "runRemoteScript",
      "action": "aws:runDocument",
      "inputs": {
        "documentType": "LocalPath",
        "documentPath": "{{ commandLine }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    }
  ]
}"#;

/// Runs another SSM document, local to the node or one of the account's.
pub const RUN_DOCUMENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Run an SSM document stored on the managed node or in Systems Manager.",
  "parameters": {
    "documentType": {
      "type": "String",
      "description": "(Required) Whether the document to run is a local path or an SSM document.",
      "allowedValues": ["LocalPath", "SSMDocument"],
      "default": "SSMDocument"
    },
    "documentPath": {
      "type": "String",
      "description": "(Required) The path or the name of the document to run."
    },
    "documentParameters": {
      "type": "StringMap",
      "description": "(Optional) The parameters to pass to the document being run.",
      "default": {}
    },
    "workingDirectory": {
      "type": "String",
      "description": "(Optional) The path to the working directory on the managed node.",
      "default": ""
    },
    "executionTimeout": {
      "type": "String",
      "description": "(Optional) The time in seconds for a command to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "runDocument",
      "action": "aws:runDocument",
      "inputs": {
        "documentType": "{{ documentType }}",
        "documentPath": "{{ documentPath }}",
        "documentParameters": "{{ documentParameters }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    }
  ]
}"#;

/// Makes the agent apply its associations again, immediately.
pub const REFRESH_ASSOCIATION: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Apply the State Manager associations of a managed node immediately.",
  "parameters": {
    "associationIds": {
      "type": "StringList",
      "description": "(Optional) The association ids to apply. An empty list applies every association of the node.",
      "default": []
    }
  },
  "mainSteps": [
    {
      "name": "refreshAssociation",
      "action": "aws:refreshAssociation",
      "inputs": {
        "associationIds": "{{ associationIds }}"
      }
    }
  ]
}"#;

/// Joins a node to an AWS Directory Service domain.
pub const JOIN_DIRECTORY_SERVICE_DOMAIN: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Join a managed node to an AWS Directory Service domain.",
  "parameters": {
    "directoryId": {
      "type": "String",
      "description": "(Required) The id of the directory to join."
    },
    "directoryName": {
      "type": "String",
      "description": "(Required) The DNS name of the directory to join."
    },
    "directoryOU": {
      "type": "String",
      "description": "(Optional) The organizational unit the node is placed in.",
      "default": ""
    },
    "dnsIpAddresses": {
      "type": "StringList",
      "description": "(Optional) The IP addresses of the DNS servers of the directory.",
      "default": []
    }
  },
  "mainSteps": [
    {
      "name": "joinDomain",
      "action": "aws:domainJoin",
      "inputs": {
        "directoryId": "{{ directoryId }}",
        "directoryName": "{{ directoryName }}",
        "directoryOU": "{{ directoryOU }}",
        "dnsIpAddresses": "{{ dnsIpAddresses }}"
      }
    }
  ]
}"#;

/// Runs Ansible playbooks on a node: its step precondition restricts it to Linux.
pub const APPLY_ANSIBLE_PLAYBOOKS: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Run Ansible playbooks on a Linux managed node.",
  "parameters": {
    "SourceType": {
      "type": "String",
      "description": "(Required) Where the playbooks are downloaded from.",
      "allowedValues": ["GitHub", "S3"],
      "default": "S3"
    },
    "SourceInfo": {
      "type": "StringMap",
      "description": "(Required) The information required to retrieve the playbooks from their source.",
      "default": {}
    },
    "InstallDependencies": {
      "type": "String",
      "description": "(Optional) Whether to install Ansible and its dependencies first.",
      "allowedValues": ["True", "False"],
      "default": "True"
    },
    "PlaybookFile": {
      "type": "String",
      "description": "(Optional) The playbook to run.",
      "default": "hello-world-playbook.yml"
    },
    "ExtraVariables": {
      "type": "String",
      "description": "(Optional) The extra variables to pass to Ansible.",
      "default": "SSM=True"
    },
    "Check": {
      "type": "String",
      "description": "(Optional) Whether to run the playbook in check mode.",
      "allowedValues": ["True", "False"],
      "default": "False"
    },
    "Verbose": {
      "type": "String",
      "description": "(Optional) The verbosity Ansible is run with.",
      "allowedValues": ["-v", "-vv", "-vvv", "-vvvv"],
      "default": "-v"
    },
    "TimeoutSeconds": {
      "type": "String",
      "description": "(Optional) The time in seconds for the playbook run to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "runAnsiblePlaybook",
      "action": "aws:runShellScript",
      "precondition": { "StringEquals": ["platformType", "Linux"] },
      "inputs": {
        "commands": ["ansible-playbook {{ PlaybookFile }}"],
        "timeoutSeconds": "{{ TimeoutSeconds }}"
      }
    }
  ]
}"#;

/// Configures the CloudWatch agent on a node. Published at schema 1.2, whose steps are
/// the `runtimeConfig` plugin entries rather than `mainSteps`.
pub const CONFIGURE_CLOUD_WATCH: &str = r#"{
  "schemaVersion": "1.2",
  "description": "Configure the Amazon CloudWatch agent on a Windows managed node.",
  "parameters": {
    "status": {
      "type": "String",
      "description": "(Optional) Whether to enable or disable the CloudWatch agent.",
      "allowedValues": ["Enabled", "Disabled"],
      "default": "Enabled"
    },
    "properties": {
      "type": "String",
      "description": "(Optional) The configuration the CloudWatch agent is run with.",
      "default": ""
    }
  },
  "runtimeConfig": {
    "aws:cloudWatch": {
      "settings": {
        "startType": "{{ status }}"
      },
      "properties": "{{ properties }}"
    }
  }
}"#;

/// Runs Chef InSpec profiles on a node.
pub const RUN_INSPEC_CHECKS: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Run Chef InSpec profiles on a managed node and report the compliance result.",
  "parameters": {
    "sourceType": {
      "type": "String",
      "description": "(Required) Where the InSpec profile is downloaded from.",
      "allowedValues": ["GitHub", "S3"],
      "default": "GitHub"
    },
    "sourceInfo": {
      "type": "StringMap",
      "description": "(Required) The information required to retrieve the InSpec profile from its source.",
      "default": {}
    },
    "dryRun": {
      "type": "String",
      "description": "(Optional) Whether to report the checks without recording compliance.",
      "allowedValues": ["True", "False"],
      "default": "False"
    }
  },
  "mainSteps": [
    {
      "name": "runInspecChecks",
      "action": "aws:runShellScript",
      "inputs": {
        "commands": ["inspec exec ."],
        "timeoutSeconds": "3600"
      }
    }
  ]
}"#;

/// The session document Session Manager opens an SSH session with. A session document
/// declares no step, so it restricts the document to no platform.
pub const START_SSH_SESSION: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Open an SSH session to a managed node through Session Manager.",
  "sessionType": "Port",
  "parameters": {
    "portNumber": {
      "type": "String",
      "description": "(Optional) The SSH port on the managed node.",
      "default": "22",
      "allowedPattern": "^([0-9]|[1-9][0-9]{1,3}|[1-5][0-9]{4}|6[0-4][0-9]{3}|65[0-4][0-9]{2}|655[0-2][0-9]|6553[0-5])$"
    }
  },
  "properties": {
    "portNumber": "{{ portNumber }}"
  }
}"#;

/// The session document Session Manager forwards a port on the node with.
pub const START_PORT_FORWARDING_SESSION: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Forward a port on a managed node to the local machine through Session Manager.",
  "sessionType": "Port",
  "parameters": {
    "portNumber": {
      "type": "String",
      "description": "(Optional) The port on the managed node that traffic is forwarded to.",
      "default": "80"
    },
    "localPortNumber": {
      "type": "String",
      "description": "(Optional) The local port traffic is forwarded from. 0 picks an unused port.",
      "default": "0"
    }
  },
  "properties": {
    "portNumber": "{{ portNumber }}",
    "localPortNumber": "{{ localPortNumber }}"
  }
}"#;

/// The session document Session Manager forwards a port on a host REACHED FROM the node
/// with.
pub const START_PORT_FORWARDING_SESSION_TO_REMOTE_HOST: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Forward a port on a remote host reachable from a managed node through Session Manager.",
  "sessionType": "Port",
  "parameters": {
    "host": {
      "type": "String",
      "description": "(Required) The host name or IP address of the remote host traffic is forwarded to."
    },
    "portNumber": {
      "type": "String",
      "description": "(Optional) The port on the remote host that traffic is forwarded to.",
      "default": "80"
    },
    "localPortNumber": {
      "type": "String",
      "description": "(Optional) The local port traffic is forwarded from. 0 picks an unused port.",
      "default": "0"
    }
  },
  "properties": {
    "host": "{{ host }}",
    "portNumber": "{{ portNumber }}",
    "localPortNumber": "{{ localPortNumber }}"
  }
}"#;

/// The session document Session Manager runs ONE interactive command with.
pub const START_INTERACTIVE_COMMAND: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Run one interactive command on a managed node through Session Manager.",
  "sessionType": "InteractiveCommands",
  "parameters": {
    "command": {
      "type": "String",
      "description": "(Required) The command the session runs interactively."
    }
  },
  "properties": {
    "commands": "{{ command }}"
  }
}"#;

/// The session document Session Manager runs ONE non-interactive command with.
pub const START_NON_INTERACTIVE_COMMAND: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Run one non-interactive command on a managed node through Session Manager.",
  "sessionType": "NonInteractiveCommands",
  "parameters": {
    "command": {
      "type": "String",
      "description": "(Required) The command the session runs without a terminal."
    }
  },
  "properties": {
    "commands": "{{ command }}"
  }
}"#;
