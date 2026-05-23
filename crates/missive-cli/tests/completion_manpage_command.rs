use std::collections::BTreeMap;

use missive_cli::run_from_with_environment;
use missive_core::MissiveExitCode;
use serde_json::Value;

fn run(args: &[&str], environment: &BTreeMap<String, String>) -> (i32, String, String) {
    let current_dir = tempfile::tempdir().expect("tempdir");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment(
        args.iter().copied(),
        environment,
        current_dir.path(),
        &mut stdout,
        &mut stderr,
    );

    (
        code,
        String::from_utf8(stdout).expect("stdout should be UTF-8"),
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
    )
}

fn first_lines(text: &str, count: usize) -> String {
    let mut lines = text.lines().take(count).collect::<Vec<_>>().join("\n");
    lines.push('\n');
    lines
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn completion_prefix_snapshots_are_stable_for_supported_shells() {
    let environment = BTreeMap::from([(
        "MISSIVE_CONFIG".to_owned(),
        "/definitely/missing/missive.toml".to_owned(),
    )]);
    let cases = [
        ("bash", include_str!("snapshots/completion-bash-prefix.txt")),
        ("zsh", include_str!("snapshots/completion-zsh-prefix.txt")),
        ("fish", include_str!("snapshots/completion-fish-prefix.txt")),
        (
            "powershell",
            include_str!("snapshots/completion-powershell-prefix.txt"),
        ),
    ];

    for (shell, expected_prefix) in cases {
        let (code, stdout, stderr) = run(&["missive", "completion", shell], &environment);

        assert_eq!(code, MissiveExitCode::Success.as_i32(), "{shell}");
        assert!(stderr.is_empty(), "{shell}: {stderr}");
        assert_eq!(
            first_lines(&stdout, 20),
            normalize_newlines(expected_prefix),
            "{shell}"
        );
        assert!(stdout.contains("missive"), "{shell}");
        assert!(stdout.contains("completion"), "{shell}");
        assert!(stdout.contains("manpage"), "{shell}");
        assert!(stdout.contains("protocol-version"), "{shell}");
    }
}

#[test]
fn completion_json_wraps_generated_script_for_automation() {
    let (code, stdout, stderr) = run(
        &["missive", "completion", "fish", "--json"],
        &BTreeMap::new(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty());
    let value: Value = serde_json::from_str(&stdout).expect("completion JSON should parse");
    assert_eq!(value["kind"], "completion");
    assert_eq!(value["data"]["shell"], "fish");
    assert_eq!(value["data"]["command"], "missive");
    assert_eq!(value["data"]["file_name"], "missive.fish");
    assert!(
        value["data"]["install_hint"]
            .as_str()
            .unwrap()
            .contains("fish")
    );
    assert!(
        value["data"]["script"]
            .as_str()
            .unwrap()
            .contains("complete -c missive")
    );
}

#[test]
fn manpage_generates_roff_without_loading_config() {
    let environment = BTreeMap::from([(
        "MISSIVE_CONFIG".to_owned(),
        "/definitely/missing/missive.toml".to_owned(),
    )]);
    let (code, stdout, stderr) = run(&["missive", "manpage"], &environment);

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty());
    assert!(stdout.contains(".TH missive 1"));
    assert!(stdout.contains(".SH NAME"));
    assert!(
        stdout.contains("missive \\- Manage A2A\\-native agent communication from the terminal.")
    );
    assert!(stdout.contains("completion"));
    assert!(stdout.contains("manpage"));
    assert!(stdout.contains("\\-\\-protocol\\-version"));
}

#[test]
fn manpage_json_wraps_generated_roff_for_automation() {
    let (code, stdout, stderr) = run(&["missive", "manpage", "--json"], &BTreeMap::new());

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty());
    let value: Value = serde_json::from_str(&stdout).expect("manpage JSON should parse");
    assert_eq!(value["kind"], "manpage");
    assert_eq!(value["data"]["page"], "missive");
    assert_eq!(value["data"]["section"], "1");
    assert_eq!(value["data"]["file_name"], "missive.1");
    assert!(
        value["data"]["install_hint"]
            .as_str()
            .unwrap()
            .contains("man1")
    );
    assert!(
        value["data"]["roff"]
            .as_str()
            .unwrap()
            .contains(".TH missive 1")
    );
}
