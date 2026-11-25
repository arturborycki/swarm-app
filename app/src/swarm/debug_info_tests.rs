use std::time::{Duration, SystemTime};

use super::*;

fn status(version: &str) -> DaemonStatus {
    DaemonStatus {
        version: version.into(),
        hostname: "box".into(),
        os: "Linux 6.8".into(),
        at_login: false,
        restore_after_reboot: None,
        spill_to_disk: None,
        linger: Some(true),
        sessions: 3,
        running: 2,
        detached: 1,
    }
}

#[test]
fn report_lists_app_updates_and_daemons_with_home_shortened() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let report = Report {
        app_version: "0.1.11".into(),
        build: "0.1.11+abc".into(),
        arch: "aarch64",
        bundle: "/Users/me/Applications/Swarm.app".into(),
        self_update: Err("you can't change /Applications with your account.".into()),
        download_in_background: true,
        install_when_idle: false,
        last_check: Some((now - Duration::from_secs(125), "up to date".into())),
        pending_update: Some("0.1.12".into()),
        env: vec![("SWARM_STATE_DIR".into(), "/Users/me/swarm".into())],
        daemons: vec![(None, Ok(status("0.1.11+abc"))), (Some("devbox".into()), Err("timed out".into()))],
        home: Some("/Users/me".into()),
    };
    let text = report.to_text(now);
    assert_eq!(
        text,
        "Swarm 0.1.11 (0.1.11+abc, aarch64)\n\
         App: ~/Applications/Swarm.app\n\
         Installs its own updates: no (you can't change /Applications with your account.)\n\
         Update settings: download in background on, install when not in use off\n\
         Last update check: 2m ago, up to date\n\
         Downloaded update waiting to install: 0.1.12\n\
         SWARM_STATE_DIR=~/swarm\n\
         Daemon on this Mac: swarmd 0.1.11+abc on box (Linux 6.8), 3 sessions (2 running, 1 detached), start on demand, linger on\n\
         Daemon on devbox: not reachable (timed out)\n"
    );
}

#[test]
fn report_without_a_check_says_so() {
    let report = Report {
        app_version: "1".into(),
        build: "1+x".into(),
        arch: "x86_64",
        bundle: "/Applications/Swarm.app".into(),
        self_update: Ok(()),
        download_in_background: false,
        install_when_idle: false,
        last_check: None,
        pending_update: None,
        env: vec![],
        daemons: vec![],
        home: None,
    };
    let text = report.to_text(SystemTime::now());
    assert!(text.contains("Installs its own updates: yes\n"));
    assert!(text.contains("Last update check: none yet in this run\n"));
    assert!(!text.contains("waiting to install"));
}
