//! swarm-term: commands on the focused session that work through the
//! bundled `swarmctl` (run again after a reboot, export, record) and
//! layouts (`swarmctl up`, Save Window as Layout…).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use warp_terminal::local_tty::daemon::{self, DaemonSession};
use warpui::platform::SaveFilePickerConfiguration;
use warpui::{SingletonEntity, ViewContext};

use super::Workspace;
use crate::swarm::sessions::SwarmSessions;

/// Recordings in progress, by daemon session id: the `swarmctl record`
/// process writing each one.
static RECORDINGS: Mutex<BTreeMap<String, std::process::Child>> = Mutex::new(BTreeMap::new());

/// The bundled swarmctl.
fn swarmctl() -> Option<PathBuf> {
    daemon::bundled_swarmd_dir(&daemon::local_platform()).map(|d| d.join("swarmctl"))
}

fn swarmctl_command(host: Option<&str>) -> Option<std::process::Command> {
    let mut command = std::process::Command::new(swarmctl()?);
    if let Some(host) = host {
        command.arg("--host").arg(host);
    }
    Some(command)
}

/// A name usable as a file name: no folders, no control characters.
fn file_name(name: &str) -> String {
    let cleaned: String = daemon::clean_text(name)
        .chars()
        .map(|c| if c == '/' || c == ':' || c == '\\' { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "session".to_string() } else { cleaned }
}

fn downloads() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads"))
}

impl Workspace {
    /// The focused pane's daemon session and its host.
    pub(super) fn swarm_focused_session(&self, ctx: &warpui::AppContext) -> Option<(Option<String>, DaemonSession)> {
        let view = self.active_tab_pane_group().as_ref(ctx).focused_session_view(ctx)?;
        let id = view.as_ref(ctx).active_block_session_id()?.as_u64();
        let (host, session) = SwarmSessions::as_ref(ctx).session_for(id)?;
        Some((host, session.clone()))
    }

    /// Run Again After Reboot: the focused session's last command, or off.
    pub(super) fn swarm_toggle_restart(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((host, session)) = self.swarm_focused_session(ctx) else {
            self.swarm_alert("No session", "Focus a session's pane first.", ctx);
            return;
        };
        let name = daemon::clean_text(&session.name.clone().unwrap_or_else(|| session.title.clone()));
        if session.ui.contains_key(daemon::META_RESTART) {
            let ui = BTreeMap::from([(daemon::META_RESTART.to_string(), String::new())]);
            daemon::set_session_meta(host, session.id.clone(), ui);
            self.swarm_alert("Comes back as a shell", format!("After a reboot, {name} comes back as a shell."), ctx);
            return;
        }
        let command = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_session_view(ctx)
            .and_then(|view| view.as_ref(ctx).swarm_recent_command());
        let Some(command) = command else {
            self.swarm_alert(
                "Nothing to run again",
                "Run the command in this session first, such as your dev server; Swarm runs its last command again after a reboot.",
                ctx,
            );
            return;
        };
        if let Err(why) = swarm_client_check(&command) {
            self.swarm_alert("Can't run that again", why, ctx);
            return;
        }
        let ui = BTreeMap::from([(daemon::META_RESTART.to_string(), command.clone())]);
        daemon::set_session_meta(host.clone(), session.id.clone(), ui);
        // Running it again needs the daemon to recreate sessions at all.
        if let Some(mut c) = swarmctl_command(host.as_deref()) {
            std::thread::spawn(move || {
                let _ = c.args(["daemon", "restore", "on"]).output();
            });
        }
        let place = host.unwrap_or_else(|| "this Mac".to_string());
        self.swarm_alert(
            "Runs again after a reboot",
            format!("When {place} restarts, Swarm brings {name} back in its folder and runs:\n\n{command}"),
            ctx,
        );
    }

    /// Export Output…: the focused session's kept output as a web page with
    /// its colors (`.html`) or plain text (any other name).
    pub(super) fn swarm_export_output(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((host, session)) = self.swarm_focused_session(ctx) else {
            self.swarm_alert("No session", "Focus a session's pane first.", ctx);
            return;
        };
        let name = file_name(&session.name.clone().unwrap_or_else(|| session.title.clone()));
        let mut config = SaveFilePickerConfiguration::new().with_default_filename(format!("{name}.html"));
        if let Some(dir) = downloads() {
            config = config.with_default_directory(dir);
        }
        ctx.open_save_file_picker(
            move |path, me, ctx| {
                let Some(path) = path else { return };
                let Some(mut command) = swarmctl_command(host.as_deref()) else { return };
                let html = path.ends_with(".html") || path.ends_with(".htm");
                command.arg("dump");
                if html {
                    command.arg("--html");
                }
                command.arg(&session.id);
                let Some(daemons) = daemon::daemons() else { return };
                let rx = daemons.run(async move {
                    let output = tokio::process::Command::from(command).output().await?;
                    if !output.status.success() {
                        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
                    }
                    std::fs::write(&path, &output.stdout)?;
                    anyhow::Ok(path)
                });
                let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| match result {
                    Ok(Ok(path)) => ctx.open_file_path_in_explorer(std::path::Path::new(&path)),
                    Ok(Err(e)) => me.swarm_alert("Couldn't export the output", format!("{e:#}"), ctx),
                    Err(_) => {}
                });
                let _ = me;
            },
            config,
        );
    }

    /// Start or Stop Recording…: records the focused session as an
    /// asciinema recording (`swarmctl record`), until chosen again.
    pub(super) fn swarm_toggle_recording(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((host, session)) = self.swarm_focused_session(ctx) else {
            self.swarm_alert("No session", "Focus a session's pane first.", ctx);
            return;
        };
        let running = RECORDINGS.lock().unwrap_or_else(|e| e.into_inner()).remove(&session.id);
        if let Some(child) = running {
            // swarmctl finishes the file on Ctrl-C.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGINT);
            }
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
            self.swarm_alert("Recording stopped", "Play it with asciinema play, or share it on asciinema.org.", ctx);
            return;
        }
        let name = file_name(&session.name.clone().unwrap_or_else(|| session.title.clone()));
        let mut config = SaveFilePickerConfiguration::new().with_default_filename(format!("{name}.cast"));
        if let Some(dir) = downloads() {
            config = config.with_default_directory(dir);
        }
        ctx.open_save_file_picker(
            move |path, me, ctx| {
                let Some(path) = path else { return };
                let Some(mut command) = swarmctl_command(host.as_deref()) else { return };
                command
                    .args(["record", &session.id, "-o", &path])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                match command.spawn() {
                    Ok(child) => {
                        RECORDINGS.lock().unwrap_or_else(|e| e.into_inner()).insert(session.id.clone(), child);
                    }
                    Err(e) => me.swarm_alert("Couldn't start recording", e.to_string(), ctx),
                }
            },
            config,
        );
    }

    /// Opens the palette on the layouts to start.
    pub(super) fn swarm_choose_layout(&mut self, ctx: &mut ViewContext<Self>) {
        self.open_palette(
            crate::palette::PaletteMode::Navigation,
            crate::server::telemetry::PaletteSource::Keybinding,
            ctx,
        );
        self.palette.update(ctx, |palette, ctx| palette.insert_query_text("Start layout ", ctx));
    }

    /// Starts layout `name` (`swarmctl up`); this app opens its sessions
    /// when they appear.
    pub(super) fn swarm_start_layout(&mut self, name: String, ctx: &mut ViewContext<Self>) {
        let Some(mut command) = swarmctl_command(None) else { return };
        command.args(["up", &name]);
        let Some(daemons) = daemon::daemons() else { return };
        let rx = daemons.run(async move {
            let output = tokio::process::Command::from(command).output().await?;
            if !output.status.success() {
                anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim().trim_start_matches("swarmctl: "));
            }
            anyhow::Ok(())
        });
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, result, ctx| {
            if let Ok(Err(e)) = result {
                me.swarm_alert(format!("Couldn't start {name}"), format!("{e:#}"), ctx);
            }
        });
    }

    /// Save Window as Layout…: this window's tabs, their panes, each
    /// session's host, folder and command to run again, as a layout file.
    pub(super) fn swarm_save_layout(&mut self, ctx: &mut ViewContext<Self>) {
        let sessions = SwarmSessions::as_ref(ctx);
        let mut tabs = Vec::new();
        for tab in &self.tabs {
            let group = tab.pane_group.as_ref(ctx);
            let mut panes = Vec::new();
            for pane_id in group.visible_pane_ids() {
                let Some(view) = group.terminal_view_from_pane_id(pane_id, ctx) else { continue };
                let Some(id) = view.as_ref(ctx).active_block_session_id().map(|id| id.as_u64()) else { continue };
                let Some((host, s)) = sessions.session_for(id) else { continue };
                panes.push(SavedPane {
                    host,
                    name: s.name.clone(),
                    cwd: s.cwd.clone(),
                    run: s.ui.get(daemon::META_RESTART).cloned(),
                });
            }
            if !panes.is_empty() {
                tabs.push(panes);
            }
        }
        if tabs.is_empty() {
            self.swarm_alert("Nothing to save", "This window has no Swarm sessions.", ctx);
            return;
        }
        let text = layout_toml(&tabs);
        let dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/swarm/layouts"));
        let mut config = SaveFilePickerConfiguration::new().with_default_filename("layout.toml".to_string());
        if let Some(dir) = dir {
            let _ = std::fs::create_dir_all(&dir);
            config = config.with_default_directory(dir);
        }
        ctx.open_save_file_picker(
            move |path, me, ctx| {
                let Some(path) = path else { return };
                match std::fs::write(&path, &text) {
                    Ok(()) => {
                        let name = std::path::Path::new(&path)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        me.swarm_alert(
                            "Layout saved",
                            format!("Start it with Swarm → Start Layout…, or swarmctl up {name}. Edit {path} to add commands to run."),
                            ctx,
                        );
                    }
                    Err(e) => me.swarm_alert("Couldn't save the layout", e.to_string(), ctx),
                }
            },
            config,
        );
    }
}

fn swarm_client_check(command: &str) -> Result<(), String> {
    warp_terminal::local_tty::daemon::check_restart_command(command)
}

struct SavedPane {
    host: Option<String>,
    name: Option<String>,
    cwd: Option<String>,
    run: Option<String>,
}

fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A layout file for `tabs`, in `swarmctl up`'s format. A tab's panes are
/// on its first pane's host, side by side.
fn layout_toml(tabs: &[Vec<SavedPane>]) -> String {
    let mut out = String::from("# A Swarm layout: start it with swarmctl up <name> or Swarm → Start Layout….\n");
    for panes in tabs {
        for (i, pane) in panes.iter().enumerate() {
            out.push_str(if i == 0 { "\n[[tab]]\n" } else { "\n[[tab.pane]]\n" });
            if let Some(name) = &pane.name {
                out.push_str(&format!("name = {}\n", toml_string(name)));
            }
            if i == 0 {
                if let Some(host) = &pane.host {
                    out.push_str(&format!("host = {}\n", toml_string(host)));
                }
            }
            if let Some(cwd) = &pane.cwd {
                let shown = match pane.host {
                    None => crate::swarm::home_relative(cwd),
                    Some(_) => crate::swarm::remote_home_relative(cwd),
                };
                out.push_str(&format!("cwd = {}\n", toml_string(&shown)));
            }
            if let Some(run) = &pane.run {
                out.push_str(&format!("run = {}\n", toml_string(run)));
            }
            if i > 0 {
                out.push_str("split = \"right\"\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_becomes_a_layout_file() {
        let tabs = vec![
            vec![
                SavedPane { host: None, name: Some("web \"app\"".into()), cwd: Some("/tmp".into()), run: Some("npm run dev".into()) },
                SavedPane { host: None, name: None, cwd: Some("/tmp".into()), run: None },
            ],
            vec![SavedPane { host: Some("devbox".into()), name: None, cwd: None, run: None }],
        ];
        let text = layout_toml(&tabs);
        assert!(text.contains("[[tab]]\nname = \"web \\\"app\\\"\"\ncwd = \"/tmp\"\nrun = \"npm run dev\"\n"), "{text}");
        assert!(text.contains("[[tab.pane]]\ncwd = \"/tmp\"\nsplit = \"right\"\n"), "{text}");
        assert!(text.contains("[[tab]]\nhost = \"devbox\"\n"), "{text}");
        assert_eq!(file_name("../a/b: c"), "-a-b- c");
        assert_eq!(file_name(""), "session");
    }
}
