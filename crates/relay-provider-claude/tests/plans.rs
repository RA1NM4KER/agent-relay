use std::{ffi::OsString, path::PathBuf};

use relay_core::ProfileName;
use relay_provider_claude::{
    AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeCommandPlanner, ClaudeLaunchRequest,
    CreateClaudeProfileRequest, InspectClaudeProfileRequest, SESSION_TRANSFER_SUPPORT,
    SessionTransferSupport,
};

#[test]
fn command_plans_are_non_executing_and_profile_scoped() {
    let config_dir = PathBuf::from("/safe/profiles/megan/claude");
    let planner = ClaudeCommandPlanner::new(PathBuf::from("/usr/local/bin/claude"));
    let create = planner.plan_auth_login(&CreateClaudeProfileRequest {
        name: ProfileName::new("megan").expect("valid name"),
        config_dir: config_dir.clone(),
    });
    let status = planner.plan_auth_status(&InspectClaudeProfileRequest {
        config_dir: config_dir.clone(),
    });
    let launch = planner.plan_launch(&ClaudeLaunchRequest {
        config_dir: config_dir.clone(),
        project_dir: PathBuf::from("/project with spaces"),
        arguments: vec![OsString::from("--resume"), OsString::from("session-id")],
    });

    assert!(create.references_config_dir(&config_dir));
    assert!(status.references_config_dir(&config_dir));
    assert!(launch.references_config_dir(&config_dir));
    assert_eq!(launch.current_dir, PathBuf::from("/project with spaces"));
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        assert!(
            launch
                .remove_environment
                .contains(&OsString::from(variable))
        );
    }
}

#[test]
fn transfer_support_remains_best_effort_and_version_gated() {
    assert_eq!(
        SESSION_TRANSFER_SUPPORT,
        SessionTransferSupport::BestEffortVersionGated
    );
}
