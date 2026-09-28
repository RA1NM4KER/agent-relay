//! Deterministic GitHub Issue queue discovery.
//!
//! This first queue slice deliberately has no completion transition: GitHub labels are the
//! source of truth and only an explicitly `relay:ready` Issue can be selected.  A later command
//! may claim a selected issue, but it must use an identity-bearing GitHub marker and re-read the
//! Issue before it lets a worker begin.

use std::process::{Command, Stdio};

use relay_core::Error;
use serde::{Deserialize, Serialize};

use crate::{
    cli::TaskCommand,
    output::{CommandOutput, header, success},
};

const REPOSITORY: &str = "RA1NM4KER/agent-relay";
const READY: &str = "relay:ready";
const CLAIMED: &str = "relay:claimed";
const BLOCKED: &str = "relay:blocked";

#[derive(Debug, Clone, Deserialize)]
struct GitHubIssue {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    labels: Vec<GitHubLabel>,
    // GitHub's issues endpoint includes this field for pull requests.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum GitHubLabel {
    Named { name: String },
    // A foreign/malformed label is deliberately not queue state.
    Foreign { _value: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SelectedIssue {
    number: u64,
    title: String,
    url: String,
}

pub(crate) fn run(command: &TaskCommand) -> Result<CommandOutput, Error> {
    match command {
        TaskCommand::Next => next(),
    }
}

fn next() -> Result<CommandOutput, Error> {
    let output = Command::new("gh")
        .args([
            "api",
            "-X",
            "GET",
            "repos/RA1NM4KER/agent-relay/issues?state=open&per_page=100",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|_| Error::ProviderExecutableMissing)?;
    if !output.status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    let issues = decode_issues(&output.stdout)?;
    let selected = select_next(issues);
    let human = match &selected {
        Some(issue) => format!(
            "{}#{} {}\n{}",
            header("Next Relay task"),
            issue.number,
            issue.title,
            issue.url
        ),
        None => format!(
            "{}No open GitHub Issue is explicitly labelled `{READY}`.",
            header("Next Relay task")
        ),
    };
    success(
        "task.next",
        human,
        serde_json::json!({"repository": REPOSITORY, "issue": selected}),
    )
}

fn decode_issues(bytes: &[u8]) -> Result<Vec<GitHubIssue>, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::MalformedProviderOutput)
}

fn select_next(mut issues: Vec<GitHubIssue>) -> Option<SelectedIssue> {
    // The API's ordering is not a queue contract; issue number is stable across calls.
    issues.sort_by_key(|issue| issue.number);
    issues.into_iter().find_map(|issue| {
        if issue.pull_request.is_some() {
            return None;
        }
        let labels = issue
            .labels
            .iter()
            .filter_map(|label| match label {
                GitHubLabel::Named { name } => Some(name.as_str()),
                GitHubLabel::Foreign { .. } => None,
            })
            .collect::<Vec<_>>();
        (labels.contains(&READY) && !labels.contains(&CLAIMED) && !labels.contains(&BLOCKED))
            .then_some(SelectedIssue {
                number: issue.number,
                title: issue.title,
                url: issue.html_url,
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, labels: &[&str]) -> GitHubIssue {
        GitHubIssue {
            number,
            title: format!("issue {number}"),
            html_url: format!("https://example.test/{number}"),
            labels: labels
                .iter()
                .map(|name| GitHubLabel::Named {
                    name: (*name).into(),
                })
                .collect(),
            pull_request: None,
        }
    }

    #[test]
    fn selects_lowest_numbered_explicitly_ready_issue() {
        assert_eq!(
            select_next(vec![issue(9, &[READY]), issue(2, &[READY])])
                .unwrap()
                .number,
            2
        );
    }

    #[test]
    fn skips_claimed_blocked_pull_requests_and_foreign_labels() {
        let mut pull = issue(1, &[READY]);
        pull.pull_request = Some(serde_json::json!({}));
        let mut foreign = issue(2, &[READY]);
        foreign.labels.push(GitHubLabel::Foreign {
            _value: serde_json::json!({"unexpected": true}),
        });
        assert_eq!(
            select_next(vec![
                pull,
                issue(3, &[READY, CLAIMED]),
                issue(4, &[READY, BLOCKED]),
                foreign,
                issue(5, &[READY])
            ])
            .unwrap()
            .number,
            2
        );
    }

    #[test]
    fn returns_none_without_an_unclaimed_ready_issue() {
        assert!(
            select_next(vec![
                issue(1, &[]),
                issue(2, &[CLAIMED]),
                issue(3, &[BLOCKED])
            ])
            .is_none()
        );
    }

    #[test]
    fn rejects_malformed_github_api_response() {
        assert!(matches!(
            decode_issues(b"not json"),
            Err(Error::MalformedProviderOutput)
        ));
    }
}
