use super::*;

#[test]
fn ssh_config_args_keep_quoted_paths_whole() {
    assert_eq!(
        ssh_config_args(r#""/Users/me/Library/Application Support/x/ssh_config" ~/.other/config"#),
        vec!["/Users/me/Library/Application Support/x/ssh_config".to_string(), "~/.other/config".to_string()]
    );
}

#[test]
fn ssh_hosts_follow_includes_and_skip_patterns() {
    let home = std::env::temp_dir().join(format!("swarm-ssh-{}", std::process::id()));
    let spaced = home.join("App Support");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(&spaced).unwrap();
    std::fs::write(
        home.join(".ssh/config"),
        format!("Include \"{}/extra\"\nHost devbox *.internal\n  HostName 10.0.0.1\nHost=build\n", spaced.display()),
    )
    .unwrap();
    std::fs::write(spaced.join("extra"), "Host spark spark-l\n").unwrap();

    let mut hosts = Vec::new();
    read_ssh_config(&home.join(".ssh/config"), &home, &mut hosts, &mut Default::default());
    assert_eq!(hosts, vec!["spark", "spark-l", "devbox", "build"]);
    std::fs::remove_dir_all(&home).unwrap();
}
