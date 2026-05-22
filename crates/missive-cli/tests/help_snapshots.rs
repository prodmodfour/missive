use clap::{Parser, error::ErrorKind};
use missive_cli::Cli;

fn rendered_help(args: &[&str]) -> String {
    let error = Cli::try_parse_from(args)
        .expect_err("--help should ask clap to render help instead of parsing a command");

    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    error.to_string()
}

fn assert_help_snapshot(args: &[&str], expected: &str) {
    assert_eq!(
        rendered_help(args),
        expected,
        "help snapshot changed for {args:?}"
    );
}

#[test]
fn top_level_help_snapshot_is_stable() {
    assert_help_snapshot(
        &["missive", "--help"],
        include_str!("snapshots/help-top.txt"),
    );
}

#[test]
fn key_subcommand_help_snapshots_are_stable() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["missive", "agent", "--help"],
            include_str!("snapshots/help-agent.txt"),
        ),
        (
            &["missive", "send", "--help"],
            include_str!("snapshots/help-send.txt"),
        ),
        (
            &["missive", "task", "--help"],
            include_str!("snapshots/help-task.txt"),
        ),
        (
            &["missive", "task", "artifact", "--help"],
            include_str!("snapshots/help-task-artifact.txt"),
        ),
        (
            &["missive", "context", "--help"],
            include_str!("snapshots/help-context.txt"),
        ),
        (
            &["missive", "group", "--help"],
            include_str!("snapshots/help-group.txt"),
        ),
        (
            &["missive", "bcast", "--help"],
            include_str!("snapshots/help-bcast.txt"),
        ),
        (
            &["missive", "gateway", "--help"],
            include_str!("snapshots/help-gateway.txt"),
        ),
        (
            &["missive", "gateway", "run", "--help"],
            include_str!("snapshots/help-gateway-run.txt"),
        ),
        (
            &["missive", "gateway", "install", "--help"],
            include_str!("snapshots/help-gateway-install.txt"),
        ),
        (
            &["missive", "gateway", "start", "--help"],
            include_str!("snapshots/help-gateway-start.txt"),
        ),
        (
            &["missive", "gateway", "stop", "--help"],
            include_str!("snapshots/help-gateway-stop.txt"),
        ),
        (
            &["missive", "gateway", "status", "--help"],
            include_str!("snapshots/help-gateway-status.txt"),
        ),
        (
            &["missive", "gateway", "uninstall", "--help"],
            include_str!("snapshots/help-gateway-uninstall.txt"),
        ),
        (
            &["missive", "webhook", "--help"],
            include_str!("snapshots/help-webhook.txt"),
        ),
        (
            &["missive", "webhook", "run", "--help"],
            include_str!("snapshots/help-webhook-run.txt"),
        ),
        (
            &["missive", "push", "--help"],
            include_str!("snapshots/help-push.txt"),
        ),
        (
            &["missive", "events", "--help"],
            include_str!("snapshots/help-events.txt"),
        ),
        (
            &["missive", "completion", "--help"],
            include_str!("snapshots/help-completion.txt"),
        ),
    ];

    for (args, expected) in cases {
        assert_help_snapshot(args, expected);
    }
}
