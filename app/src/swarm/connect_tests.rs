use super::*;

#[test]
fn password_servers_get_key_setup() {
    let (explanation, command) =
        ssh_fix("docker@192.168.1.180", "docker@192.168.1.180: Permission denied (publickey,password).").unwrap();
    assert!(explanation.contains("password"));
    let command = command.unwrap();
    assert!(command.contains("ssh-keygen"));
    assert!(command.contains("ssh-copy-id docker@192.168.1.180 && ssh-add"));
}

#[test]
fn key_only_servers_are_told_so() {
    let (explanation, command) = ssh_fix("box", "box: Permission denied (publickey).").unwrap();
    assert!(explanation.contains("only accepts SSH keys"));
    assert!(explanation.contains("administrator"));
    assert_eq!(command.as_deref(), Some("ssh-add"));
}

#[test]
fn hosts_are_quoted_in_commands() {
    let command = key_setup_command("box; rm -rf x");
    assert!(command.contains("ssh-copy-id 'box; rm -rf x' && ssh-add"));
    let (_, command) = ssh_fix("a b", "Host key verification failed.").unwrap();
    assert_eq!(command.as_deref(), Some("ssh 'a b'"));
}

#[test]
fn failure_text_names_the_host_once() {
    assert_eq!(failure_text("h", "h: Permission denied"), "h: Permission denied");
    assert_eq!(failure_text("h", "Connection closed"), "h: Connection closed");
}
