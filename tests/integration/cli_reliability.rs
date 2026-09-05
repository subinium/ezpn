use crate::common::*;

#[test]
fn invalid_arguments_fail_without_panic_or_daemon() {
    let env = TestEnv::new();
    let huge = usize::MAX.to_string();
    for args in [
        vec!["--unknown"],
        vec!["-S"],
        vec!["0", "1"],
        vec!["101", "1"],
        vec![&huge, "2"],
        vec!["--shell"],
        vec!["--session", "../escape"],
    ] {
        let output = run_bounded(env.command().args(&args));
        assert!(!output.status.success(), "accepted {args:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "{output:?}"
        );
        assert_eq!(
            std::fs::read_dir(env.socket_dir()).unwrap().count(),
            0,
            "spawned daemon for {args:?}"
        );
    }
}

#[test]
fn non_tty_interactive_start_fails_before_spawning() {
    let env = TestEnv::new();
    let output = run_bounded(env.command().args(["-S", "non-tty", "1", "1"]));
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("ssh -t"),
        "missing actionable TTY diagnostic: {error}"
    );
    assert_eq!(std::fs::read_dir(env.socket_dir()).unwrap().count(), 0);
}

#[test]
fn help_and_version_work_without_a_tty() {
    let env = TestEnv::new();
    for argument in ["--help", "--version"] {
        let output = run_bounded(env.command().arg(argument));
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("ezpn"));
        if argument == "--help" {
            let help = String::from_utf8_lossy(&output.stdout);
            for stale in ["Ctrl+D", "Ctrl+E", "Ctrl+N", "Ctrl+G", "Ctrl+W"] {
                assert!(!help.contains(stale), "stale binding {stale}: {help}");
            }
            assert!(help.contains("--trust-project"));
            assert!(help.contains("ezpn doctor"));
        }
    }
}
