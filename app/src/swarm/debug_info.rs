//! Help → Copy Debug Info: the app's, its daemons' and its updater's state as
//! text to paste into a bug report.

use std::time::SystemTime;

use warp_terminal::local_tty::daemon::DaemonStatus;

/// Everything the report says, gathered by the caller.
pub struct Report {
    pub app_version: String,
    pub build: String,
    pub arch: &'static str,
    pub bundle: String,
    /// `Ok` if Swarm can replace itself with an update, else why not.
    pub self_update: Result<(), String>,
    pub download_in_background: bool,
    pub install_when_idle: bool,
    pub last_check: Option<(SystemTime, String)>,
    /// The version downloaded in the background and not installed yet.
    pub pending_update: Option<String>,
    pub env: Vec<(String, String)>,
    /// Each daemon: `None` for this Mac, else the host, with its status or why it couldn't be read.
    pub daemons: Vec<(Option<String>, Result<DaemonStatus, String>)>,
    pub home: Option<String>,
}

impl Report {
    pub fn to_text(&self, now: SystemTime) -> String {
        let mut out = Vec::new();
        out.push(format!("Swarm {} ({}, {})", self.app_version, self.build, self.arch));
        out.push(format!("App: {}", self.bundle));
        out.push(match &self.self_update {
            Ok(()) => "Installs its own updates: yes".to_string(),
            Err(why) => format!("Installs its own updates: no ({why})"),
        });
        out.push(format!(
            "Update settings: download in background {}, install when not in use {}",
            on_off(self.download_in_background),
            on_off(self.install_when_idle)
        ));
        out.push(match &self.last_check {
            Some((at, outcome)) => format!("Last update check: {} ago, {outcome}", ago(*at, now)),
            None => "Last update check: none yet in this run".to_string(),
        });
        if let Some(version) = &self.pending_update {
            out.push(format!("Downloaded update waiting to install: {version}"));
        }
        for (name, value) in &self.env {
            out.push(format!("{name}={value}"));
        }
        for (host, status) in &self.daemons {
            let place = host.as_deref().unwrap_or("this Mac");
            out.push(match status {
                Ok(s) => {
                    let mut line = format!(
                        "Daemon on {place}: swarmd {} on {} ({}), {} sessions ({} running, {} detached), start {}",
                        s.version,
                        s.hostname,
                        s.os,
                        s.sessions,
                        s.running,
                        s.detached,
                        if s.at_login { "at login" } else { "on demand" }
                    );
                    if let Some(linger) = s.linger {
                        line.push_str(&format!(", linger {}", on_off(linger)));
                    }
                    line
                }
                Err(e) => format!("Daemon on {place}: not reachable ({e})"),
            });
        }
        let text = out.join("\n") + "\n";
        match &self.home {
            Some(home) if !home.is_empty() => text.replace(home.as_str(), "~"),
            _ => text,
        }
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn ago(at: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(at).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
#[path = "debug_info_tests.rs"]
mod tests;
