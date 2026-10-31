//! swarm-term: shells owned by the `swarmd` daemon instead of this process.
//!
//! The daemon keeps each session's PTY and process alive after the app quits.
//! To the rest of Warp a daemon session looks like any other PTY: the spawner
//! returns one end of a socket pair as the "leader fd", and a bridge task moves
//! bytes between the other end and the daemon attachment. Resizes go through
//! [`PtyHandle::resize`], the child's exit is reported through
//! [`PtyHandle::process_termination_status`] plus a `SIGCHLD` to ourselves,
//! and [`PtyHandle::kill`] only detaches: closing a tab leaves the session
//! running. Set `SWARM_DAEMON=0` to spawn shells directly as upstream Warp does.
//!
//! A session lives on a host: `None` is the local daemon, `Some(alias)` a
//! remote one reached through `ssh <alias> swarmd bridge`. [`Daemons`] keeps
//! one connection per host for the spawner and the UI.

use std::collections::{BTreeMap, HashMap};
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use swarm_client::swarm_proto::{AttachMode, CreateOpts, SessionInfo, Size};
use swarm_client::{strip_terminal_responses, Attachment, Client, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::shell::ShellStarter;
use super::spawner::PtyHandle;
use super::unix::{build_host_shell_command, PtySpawnResult};
use super::PtyOptions;
use crate::event::ObservedExitStatus;
use crate::SizeInfo;

/// After replayed output, terminal answers to replayed queries are dropped
/// for this long (they would otherwise be typed into the live shell).
const REPLAY_RESPONSE_GUARD: Duration = Duration::from_millis(250);

/// UI metadata keys this app stores on daemon sessions.
pub const META_CREATED_BY: &str = "created_by";
pub const META_SESSION_ID: &str = "warp.session_id";
pub const META_SHELL: &str = "warp.shell";
pub const META_AUTO_ATTACH: &str = "auto_attach";
pub const META_TAB_ORDER: &str = "tab_order";
/// The tab group a session's tab is in: its name, or `#<id>` for an unnamed one.
pub const META_GROUP: &str = "group";

/// Warp session ids of shells this app re-attached to. Their shell setup
/// already ran, so a replayed `InitShell` hook must not run it again.
static REATTACHED: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
    std::sync::LazyLock::new(Default::default);

pub fn mark_reattached(warp_session_id: u64) {
    REATTACHED.lock().unwrap().insert(warp_session_id);
}

/// Where each daemon-backed Warp session lives (true: a remote host). Warp
/// otherwise guesses from the shell's hostname, which changes as a Mac moves
/// between networks, so long-lived local sessions would turn "remote".
static SESSION_IS_REMOTE: LazyLock<Mutex<HashMap<u64, bool>>> = LazyLock::new(Default::default);

pub fn set_session_locality(warp_session_id: u64, is_remote: bool) {
    SESSION_IS_REMOTE.lock().unwrap().insert(warp_session_id, is_remote);
}

/// `Some(true)` if the session is on a remote host, `Some(false)` if on this
/// Mac's daemon, `None` if it isn't a daemon session this app knows.
pub fn session_is_remote(warp_session_id: u64) -> Option<bool> {
    SESSION_IS_REMOTE.lock().unwrap().get(&warp_session_id).copied()
}


pub fn is_reattached(warp_session_id: u64) -> bool {
    REATTACHED.lock().unwrap().contains(&warp_session_id)
}

/// Daemon connections shared by the spawner and the UI.
pub struct Daemons {
    rt: tokio::runtime::Runtime,
    clients: tokio::sync::Mutex<HashMap<Option<String>, Client>>,
}

static DAEMONS: LazyLock<Option<Daemons>> = LazyLock::new(|| {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("swarm-daemon")
        .enable_all()
        .build();
    match rt {
        Ok(rt) => Some(Daemons { rt, clients: Default::default() }),
        Err(e) => {
            log::warn!("swarm: can't start the daemon client runtime: {e}");
            None
        }
    }
});

/// The shared connections, if daemon sessions are on.
pub fn daemons() -> Option<&'static Daemons> {
    if enabled() {
        DAEMONS.as_ref()
    } else {
        None
    }
}

impl Daemons {
    /// The connection to `host`'s daemon (`None`: local), reconnecting, and
    /// starting the local daemon, as needed.
    pub async fn client(&self, host: Option<&str>) -> Result<Client> {
        let key = host.map(str::to_string);
        let mut clients = self.clients.lock().await;
        if let Some(c) = clients.get(&key) {
            if tokio::time::timeout(Duration::from_secs(5), c.ping()).await.is_ok_and(|r| r.is_ok()) {
                return Ok(c.clone());
            }
            clients.remove(&key);
        }
        let c = match host {
            None => Client::connect_local("warp").await.context("connect to swarmd")?,
            Some(h) => Client::connect_ssh(h, "warp").await.with_context(|| format!("connect to swarmd on {h}"))?,
        };
        clients.insert(key, c.clone());
        Ok(c)
    }

    /// Hosts with an open connection (always including the local daemon).
    async fn connected_hosts(&self) -> Vec<Option<String>> {
        let mut hosts: Vec<_> = self.clients.lock().await.keys().cloned().collect();
        if !hosts.contains(&None) {
            hosts.push(None);
        }
        hosts
    }

    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    /// Runs `f` on the daemon runtime and returns a receiver for its result,
    /// so UI code can await it without a tokio context.
    pub fn run<T: Send + 'static>(
        &'static self,
        f: impl std::future::Future<Output = T> + Send + 'static,
    ) -> async_channel::Receiver<T> {
        let (tx, rx) = async_channel::bounded(1);
        self.rt.spawn(async move {
            let _ = tx.send(f.await).await;
        });
        rx
    }
}

/// Set in a new tab's env vars to re-attach it to a running daemon session
/// instead of starting a shell; never reaches the shell's environment.
pub const REATTACH_SESSION_ENV: &str = "SWARM_REATTACH_SESSION_ID";
/// The re-attached shell's Warp session id, which the model must register.
pub const REATTACH_WARP_ID_ENV: &str = "SWARM_REATTACH_WARP_SESSION_ID";
/// Set on a session without Warp shell integration that is an idle shell of
/// this type (`zsh`, `bash`, `fish`): Warp's setup script is typed into it.
pub const REATTACH_WARPIFY_SHELL_ENV: &str = "SWARM_REATTACH_WARPIFY_SHELL";
/// The ssh host alias of a remote session; absent for the local daemon.
pub const REATTACH_HOST_ENV: &str = "SWARM_REATTACH_HOST";

/// What a tab re-attaching to a daemon session needs to do.
#[derive(Debug, Clone)]
pub struct ReattachRequest {
    /// The shell's existing Warp session id, if it already has Warp's integration.
    pub warp_session_id: Option<u64>,
    /// Set up Warp's integration in this idle shell.
    pub warpify_shell: Option<String>,
    /// The session's host; `None` for this Mac's daemon.
    pub host: Option<String>,
}

/// The re-attach request carried by a new tab's env vars, if any. A known Warp
/// session id is recorded so the replayed `InitShell` hook doesn't re-run setup.
pub fn reattach_request(env: &std::collections::HashMap<std::ffi::OsString, std::ffi::OsString>) -> Option<ReattachRequest> {
    let get = |key: &str| env.get(std::ffi::OsStr::new(key)).and_then(|v| v.to_str()).map(str::to_string);
    get(REATTACH_SESSION_ENV)?;
    let warp_session_id = get(REATTACH_WARP_ID_ENV).and_then(|v| v.parse().ok());
    if let Some(id) = warp_session_id {
        mark_reattached(id);
    }
    Some(ReattachRequest {
        warp_session_id,
        warpify_shell: get(REATTACH_WARPIFY_SHELL_ENV),
        host: get(REATTACH_HOST_ENV),
    })
}

/// `env` without the re-attach keys (for shell recovery, which must start fresh).
pub fn without_reattach_keys(
    env: &std::collections::HashMap<std::ffi::OsString, std::ffi::OsString>,
) -> std::collections::HashMap<std::ffi::OsString, std::ffi::OsString> {
    let mut env = env.clone();
    env.remove(std::ffi::OsStr::new(REATTACH_SESSION_ENV));
    env.remove(std::ffi::OsStr::new(REATTACH_WARP_ID_ENV));
    env.remove(std::ffi::OsStr::new(REATTACH_WARPIFY_SHELL_ENV));
    env.remove(std::ffi::OsStr::new(REATTACH_HOST_ENV));
    env
}

pub use swarm_client::swarm_proto::SessionInfo as DaemonSession;

/// Calls `on_change` with `host`'s full session list, now and after every
/// change, reconnecting as needed. Stops once `on_change` returns `false`.
pub fn watch_sessions(host: Option<String>, on_change: impl Fn(Vec<DaemonSession>) -> bool + Send + Sync + 'static) {
    let Some(daemons) = daemons() else { return };
    daemons.rt.spawn(async move {
        loop {
            match watch_once(daemons, host.as_deref(), &on_change).await {
                Ok(false) => return,
                Ok(true) => {}
                Err(e) => log::debug!("swarm: session watch ({host:?}): {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// One connection's worth of watching. `Ok(false)`: the consumer is gone.
async fn watch_once(
    daemons: &Daemons,
    host: Option<&str>,
    on_change: &impl Fn(Vec<DaemonSession>) -> bool,
) -> Result<bool> {
    let client = daemons.client(host).await?;
    let mut events = client.subscribe().await?;
    loop {
        if !on_change(client.list().await?) {
            return Ok(false);
        }
        // Wake on the next event, or every few seconds so a dead daemon is
        // noticed (the next `list` fails and we reconnect).
        if let Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) =
            tokio::time::timeout(Duration::from_secs(5), events.recv()).await
        {
            return Ok(true);
        }
        // Coalesce bursts (several sessions producing output) into one refresh.
        tokio::time::sleep(Duration::from_millis(150)).await;
        while events.try_recv().is_ok() {}
    }
}

/// Marks the daemon sessions behind these Warp session ids as not reopening
/// (`auto_attach=false`), in the background: the user closed their tabs.
pub fn forget_sessions(warp_session_ids: Vec<u64>) {
    let mut ui = BTreeMap::new();
    ui.insert(META_AUTO_ATTACH.to_string(), "false".to_string());
    update_sessions(warp_session_ids, swarm_client::swarm_proto::MetaUpdate { name: None, ui });
}

/// Names (or, with `None`, un-names) the daemon sessions behind these Warp
/// session ids, in the background: the user renamed their tab.
pub fn name_sessions(warp_session_ids: Vec<u64>, name: Option<String>) {
    let update = swarm_client::swarm_proto::MetaUpdate { name: Some(name), ui: BTreeMap::new() };
    update_sessions(warp_session_ids, update);
}

/// Puts the daemon sessions behind these Warp session ids in the tab group
/// `group` (empty: in none), in the background, so reattaching regroups them.
pub fn group_sessions(warp_session_ids: Vec<u64>, group: String) {
    let mut ui = BTreeMap::new();
    ui.insert(META_GROUP.to_string(), group);
    update_sessions(warp_session_ids, swarm_client::swarm_proto::MetaUpdate { name: None, ui });
}

fn update_sessions(warp_session_ids: Vec<u64>, update: swarm_client::swarm_proto::MetaUpdate) {
    let Some(daemons) = daemons() else { return };
    if warp_session_ids.is_empty() {
        return;
    }
    daemons.rt.spawn(async move {
        for host in daemons.connected_hosts().await {
            let result = async {
                let client = daemons.client(host.as_deref()).await?;
                for session in client.list().await? {
                    let id = session.ui.get(META_SESSION_ID).and_then(|v| v.parse::<u64>().ok());
                    if id.is_some_and(|id| warp_session_ids.contains(&id)) {
                        client.set_meta(&session.id, update.clone()).await?;
                    }
                }
                anyhow::Ok(())
            }
            .await;
            if let Err(e) = result {
                log::warn!("swarm: couldn't update sessions on {host:?}: {e:#}");
            }
        }
    });
}

/// Running sessions marked to reopen (`auto_attach=true`) that no client is
/// attached to, in tab order. Starts the daemon if needed; empty if
/// the daemon is unavailable.
pub fn reattachable_sessions() -> Vec<DaemonSession> {
    let Some(daemons) = daemons() else { return Vec::new() };
    let sessions = daemons.block_on(async { daemons.client(None).await?.list().await });
    let mut sessions = match sessions {
        Ok(s) => s,
        Err(e) => {
            log::warn!("swarm: can't list daemon sessions: {e:#}");
            return Vec::new();
        }
    };
    sessions.retain(|s| {
        s.is_running() && s.clients.is_empty() && s.ui.get(META_AUTO_ATTACH).map(String::as_str) == Some("true")
    });
    sessions.sort_by_key(|s| (s.ui.get(META_TAB_ORDER).and_then(|v| v.parse::<u64>().ok()).unwrap_or(u64::MAX), s.created_ms));
    sessions
}

pub fn enabled() -> bool {
    std::env::var("SWARM_DAEMON").map(|v| v != "0").unwrap_or(true)
}

pub struct DaemonSpawner {
    daemons: &'static Daemons,
}

enum Ctl {
    Resize(Size),
    Detach,
}

impl DaemonSpawner {
    pub fn new() -> Result<Self> {
        let daemons = daemons().context("daemon sessions are off")?;
        Ok(DaemonSpawner { daemons })
    }

    /// Creates a daemon session running the shell described by `options`.
    pub fn spawn(&self, options: PtyOptions) -> Result<(PtySpawnResult, Box<dyn PtyHandle>)> {
        let PtyOptions {
            size,
            window_id,
            shell_starter,
            start_dir,
            mut env_vars,
            enable_ssh_wrapper,
            reuse_ssh_control_master,
            shell_debug_mode,
            honor_ps1,
            node_version_chip_enabled,
            close_fds: _,
        } = options;
        let reattach = env_vars.remove(std::ffi::OsStr::new(REATTACH_SESSION_ENV));
        let warp_id = env_vars.remove(std::ffi::OsStr::new(REATTACH_WARP_ID_ENV));
        let warpify = env_vars.remove(std::ffi::OsStr::new(REATTACH_WARPIFY_SHELL_ENV));
        let host = env_vars.remove(std::ffi::OsStr::new(REATTACH_HOST_ENV)).map(|h| h.to_string_lossy().into_owned());
        if let Some(id) = reattach {
            let id = id.to_string_lossy().into_owned();
            let (client, info, att) = self.daemons.block_on(async {
                let client = self.daemons.client(host.as_deref()).await?;
                let att = client.attach(&id, AttachMode::Full, Some(to_size(&size)), false).await?;
                // Open as a tab again: reopen it at the next launch too.
                let mut ui = BTreeMap::new();
                ui.insert(META_AUTO_ATTACH.to_string(), "true".to_string());
                // A shell being set up for Warp now has a Warp session id; later
                // re-attaches take the normal path.
                if let (Some(shell), Some(warp_id)) = (&warpify, &warp_id) {
                    ui.insert(META_SESSION_ID.to_string(), warp_id.to_string_lossy().into_owned());
                    ui.insert(META_SHELL.to_string(), shell.to_string_lossy().into_owned());
                    ui.insert(META_CREATED_BY.to_string(), "warp".to_string());
                }
                let update = swarm_client::swarm_proto::MetaUpdate { name: None, ui };
                let info = client.set_meta(&id, update).await.unwrap_or_else(|_| att.info.clone());
                anyhow::Ok((client, info, att))
            })?;
            log::info!("swarm: re-attached to daemon session {id} on {host:?} (pid {})", info.pid);
            return self.bridge(host, client, info, att, true);
        }

        let session_id = shell_starter.session_id();
        set_session_locality(session_id.as_u64(), false);
        let shell_type = shell_starter.shell_type();
        let ShellStarter::Direct(starter) = shell_starter else {
            bail!("only direct shells run in the daemon");
        };

        // Build exactly the command upstream Warp would spawn, then hand its
        // program, args, env and cwd to the daemon.
        let command = build_host_shell_command(
            starter,
            window_id,
            env_vars,
            start_dir,
            enable_ssh_wrapper,
            reuse_ssh_control_master,
            shell_debug_mode,
            honor_ps1,
            node_version_chip_enabled,
        );
        let mut argv = vec![command.get_program().to_string_lossy().into_owned()];
        argv.extend(command.get_args().map(|a| a.to_string_lossy().into_owned()));
        let env = command
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned())))
            .collect();
        let cwd = command.get_current_dir().map(|d| d.display().to_string());

        let mut ui = BTreeMap::new();
        ui.insert(META_CREATED_BY.to_string(), "warp".to_string());
        ui.insert(META_SESSION_ID.to_string(), session_id.as_u64().to_string());
        ui.insert(META_SHELL.to_string(), format!("{shell_type:?}"));
        ui.insert(META_AUTO_ATTACH.to_string(), "true".to_string());
        let opts = CreateOpts { command: argv, cwd, env, size: to_size(&size), name: None, ui };

        let (client, info, att) = self.daemons.block_on(async {
            let client = self.daemons.client(None).await?;
            let info = client.create(opts).await.context("create session")?;
            let att = client.attach(&info.id, AttachMode::Full, Some(to_size(&size)), false).await?;
            anyhow::Ok((client, info, att))
        })?;
        log::info!("swarm: created daemon session {} (pid {})", info.id, info.pid);
        self.bridge(None, client, info, att, false)
    }

    fn bridge(
        &self,
        host: Option<String>,
        client: Client,
        info: SessionInfo,
        att: Attachment,
        guard_replay: bool,
    ) -> Result<(PtySpawnResult, Box<dyn PtyHandle>)> {
        let (ours, theirs) = StdUnixStream::pair()?;
        ours.set_nonblocking(true)?;
        let status = Arc::new(Mutex::new(None));
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        // A remote shell's pid means nothing here and could name an unrelated
        // local process; report launchd's, which nothing here can signal.
        let pid = if host.is_some() { 1 } else { info.pid };
        self.daemons.rt.spawn(run_bridge(self.daemons, host, client, att, ours, ctl_rx, status.clone(), guard_replay));
        let handle = DaemonPtyHandle { pid, status, ctl: ctl_tx };
        Ok((PtySpawnResult { pid, leader_fd: theirs.into_raw_fd() }, Box::new(handle)))
    }
}

fn to_size(size: &SizeInfo) -> Size {
    Size { cols: size.columns().clamp(1, u16::MAX as usize) as u16, rows: size.rows().clamp(1, u16::MAX as usize) as u16 }
}

#[allow(clippy::too_many_arguments)]
async fn run_bridge(
    daemons: &'static Daemons,
    host: Option<String>,
    mut client: Client,
    mut att: Attachment,
    sock: StdUnixStream,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    status: Arc<Mutex<Option<ObservedExitStatus>>>,
    guard_replay: bool,
) {
    let sock = match tokio::net::UnixStream::from_std(sock) {
        Ok(s) => s,
        Err(e) => {
            log::error!("swarm: bridge socket: {e}");
            return;
        }
    };
    let (mut rd, mut wr) = sock.into_split();
    let id = att.info.id.clone();
    let mut last_offset = att.replay_from;
    let mut guard_until: Option<Instant> = None;
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        tokio::select! {
            ev = att.recv() => match ev {
                Some(StreamEvent::Output { offset, data, replay }) => {
                    if wr.write_all(&data).await.is_err() {
                        break;
                    }
                    last_offset = offset + data.len() as u64;
                    if replay && guard_replay {
                        guard_until = Some(Instant::now() + REPLAY_RESPONSE_GUARD);
                    }
                }
                Some(StreamEvent::Exited { code, signal }) => {
                    let observed = match (code, signal) {
                        (Some(c), _) => ObservedExitStatus::Code(c),
                        (None, Some(s)) => ObservedExitStatus::Signal(s),
                        _ => ObservedExitStatus::Unavailable,
                    };
                    *status.lock().unwrap() = Some(observed);
                    // Warp notices child exits through SIGCHLD; this one isn't our child.
                    unsafe {
                        libc::kill(libc::getpid(), libc::SIGCHLD);
                    }
                }
                Some(StreamEvent::Lagged { resume_at }) => {
                    let from = resume_at.min(last_offset);
                    match client.attach(&id, AttachMode::ResumeFrom(from), None, false).await {
                        Ok(a) => att = a,
                        Err(e) => {
                            log::warn!("swarm: re-attach to {id} failed: {e:#}");
                            break;
                        }
                    }
                }
                Some(StreamEvent::Closed { reason }) => {
                    log::info!("swarm: session {id} stream closed: {reason}");
                    // A dropped connection (sleep, network change, ssh died)
                    // resumes exactly where it stopped; a removed session doesn't.
                    match reconnect(daemons, host.as_deref(), &id, last_offset, &mut ctl).await {
                        Some((c, a)) => {
                            client = c;
                            att = a;
                        }
                        None => break,
                    }
                }
                None => break,
            },
            n = rd.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut data = buf[..n].to_vec();
                    if guard_until.is_some_and(|t| Instant::now() < t) {
                        data = strip_terminal_responses(&data);
                    }
                    if !data.is_empty() && att.input(data).await.is_err() {
                        break;
                    }
                }
            },
            c = ctl.recv() => match c {
                Some(Ctl::Resize(size)) => {
                    let _ = att.resize(size).await;
                }
                Some(Ctl::Detach) => break,
                None => break,
            },
        }
    }
    // Dropping the attachment detaches; the session keeps running.
    drop(att);
}

/// Re-attaches to `id` on `host` from `offset`, retrying with backoff for a
/// while. `None` if the session is gone, the tab was closed meanwhile, or it
/// keeps failing.
async fn reconnect(
    daemons: &Daemons,
    host: Option<&str>,
    id: &str,
    offset: u64,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
) -> Option<(Client, Attachment)> {
    let mut delay = Duration::from_millis(500);
    for attempt in 1..=40 {
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            c = ctl.recv() => {
                if matches!(c, Some(Ctl::Detach) | None) {
                    return None;
                }
            }
        }
        match daemons.client(host).await {
            Ok(client) => match client.attach(id, AttachMode::ResumeFrom(offset), None, false).await {
                Ok(att) => {
                    log::info!("swarm: reconnected to {id} on {host:?} after {attempt} attempt(s)");
                    return Some((client, att));
                }
                Err(e) if e.to_string().contains("no session") => {
                    log::info!("swarm: session {id} is gone");
                    return None;
                }
                Err(e) => log::debug!("swarm: reconnect to {id}: {e:#}"),
            },
            Err(e) => log::debug!("swarm: reconnect to {host:?}: {e:#}"),
        }
        delay = (delay * 2).min(Duration::from_secs(15));
    }
    None
}

struct DaemonPtyHandle {
    pid: u32,
    status: Arc<Mutex<Option<ObservedExitStatus>>>,
    ctl: mpsc::UnboundedSender<Ctl>,
}

impl PtyHandle for DaemonPtyHandle {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn process_termination_status(&mut self) -> Result<Option<ObservedExitStatus>> {
        Ok(*self.status.lock().unwrap())
    }

    /// Detaches only: the daemon keeps the session (and whether it reopens).
    fn kill(&mut self) -> Result<()> {
        let _ = self.ctl.send(Ctl::Detach);
        Ok(())
    }

    fn resize(&self, size: &SizeInfo) -> Option<Result<()>> {
        Some(self.ctl.send(Ctl::Resize(to_size(size))).map_err(|_| anyhow::anyhow!("session bridge closed")))
    }
}

// ---------------------------------------------------------------------------
// Remote hosts

/// Host aliases from `~/.ssh/config` and the files it `Include`s, in order,
/// without wildcard patterns.
pub fn ssh_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return Vec::new() };
    let mut hosts = Vec::new();
    let mut seen_files = std::collections::HashSet::new();
    read_ssh_config(&home.join(".ssh/config"), &home, &mut hosts, &mut seen_files);
    hosts
}

fn read_ssh_config(
    path: &std::path::Path,
    home: &std::path::Path,
    hosts: &mut Vec<String>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        let mut parts = line.splitn(2, |c: char| c.is_whitespace() || c == '=');
        let (Some(key), Some(rest)) = (parts.next(), parts.next()) else { continue };
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                for alias in rest.split_whitespace() {
                    let alias = alias.trim_matches('"');
                    if !alias.contains(['*', '?', '!']) && !hosts.iter().any(|h| h == alias) {
                        hosts.push(alias.to_string());
                    }
                }
            }
            "include" => {
                for file in ssh_config_args(rest) {
                    let file = match file.strip_prefix("~/") {
                        Some(rest) => home.join(rest),
                        None if file.starts_with('/') => std::path::PathBuf::from(file),
                        None => home.join(".ssh").join(file),
                    };
                    read_ssh_config(&file, home, hosts, seen);
                }
            }
            _ => {}
        }
    }
}

/// Splits an ssh_config argument list, keeping `"quoted paths with spaces"` whole.
fn ssh_config_args(rest: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = rest.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            args.push(chars.by_ref().take_while(|&c| c != '"').collect());
        } else {
            let mut arg = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
            args.push(arg);
        }
    }
    args
}

/// What connecting to a host found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheck {
    /// `swarmd` is installed; sessions can be listed and created.
    Ready,
    /// ssh works but `swarmd` isn't installed. `installable`: the host runs this
    /// Mac's OS and CPU, so the app's own binaries can be copied there.
    NeedsInstall { os: String, installable: bool },
    /// ssh itself failed; ssh's own message.
    Unreachable(String),
}

const REMOTE_BIN_DIR: &str = ".local/share/swarm/bin";

async fn ssh(host: &str, command: &str) -> Result<std::process::Output> {
    let output = tokio::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, command])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("run ssh")?;
    Ok(output)
}

/// Checks ssh access to `host` and whether `swarmd` is installed there.
pub async fn check_host(host: &str) -> HostCheck {
    let probe = format!(r#"uname -sm; p="$HOME/{REMOTE_BIN_DIR}/swarmd"; [ -x "$p" ] || p=$(command -v swarmd); [ -n "$p" ] && "$p" --version"#);
    let output = match ssh(host, &probe).await {
        Ok(o) => o,
        Err(e) => return HostCheck::Unreachable(format!("{e:#}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.code() == Some(255) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return HostCheck::Unreachable(stderr.trim().lines().last().unwrap_or("ssh failed").to_string());
    }
    if stdout.lines().any(|l| l.starts_with("swarmd ")) {
        return HostCheck::Ready;
    }
    let os = stdout.lines().next().unwrap_or("unknown").trim().to_string();
    let local = format!(
        "{} {}",
        if cfg!(target_os = "macos") { "Darwin" } else { "Linux" },
        if cfg!(target_arch = "aarch64") {
            if cfg!(target_os = "macos") { "arm64" } else { "aarch64" }
        } else {
            "x86_64"
        }
    );
    HostCheck::NeedsInstall { installable: os == local, os }
}

/// Copies this app's `swarmd`, `swarm-shim` and `swarmctl` to the per-user
/// bin dir on `host`. Only valid when [`check_host`] said `installable`.
pub async fn install_swarmd(host: &str) -> Result<()> {
    let exe_dir = std::env::current_exe()?.parent().context("app has no bin dir")?.to_path_buf();
    let bins: Vec<_> = ["swarmd", "swarm-shim", "swarmctl"].iter().map(|b| exe_dir.join(b)).collect();
    for b in &bins {
        if !b.exists() {
            bail!("{} isn't bundled with this app", b.display());
        }
    }
    let mkdir = ssh(host, &format!("mkdir -p \"$HOME/{REMOTE_BIN_DIR}\"")).await?;
    if !mkdir.status.success() {
        bail!("{}", String::from_utf8_lossy(&mkdir.stderr).trim());
    }
    let status = tokio::process::Command::new("scp")
        .args(["-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .args(&bins)
        .arg(format!("{host}:{REMOTE_BIN_DIR}/"))
        .status()
        .await
        .context("run scp")?;
    if !status.success() {
        bail!("copying swarmd to {host} failed ({status})");
    }
    Ok(())
}

/// Hosts the user has connected to, oldest first, stored in Swarm's state
/// dir so their sessions can be reopened at launch.
fn known_hosts_file() -> std::path::PathBuf {
    swarm_client::swarm_proto::paths::state_dir().join("hosts")
}

pub fn known_hosts() -> Vec<String> {
    std::fs::read_to_string(known_hosts_file())
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn remember_host(host: &str) {
    let mut hosts = known_hosts();
    if hosts.iter().any(|h| h == host) {
        return;
    }
    hosts.push(host.to_string());
    let path = known_hosts_file();
    let result = std::fs::create_dir_all(path.parent().unwrap_or(&path))
        .and_then(|_| std::fs::write(&path, hosts.join("\n") + "\n"));
    if let Err(e) = result {
        log::warn!("swarm: couldn't remember host {host}: {e}");
    }
}

/// `host`'s running sessions marked to reopen that nothing is attached to, in
/// tab order (what to bring back as tabs at launch).
pub async fn reopenable_sessions(host: &str) -> Result<Vec<DaemonSession>> {
    let mut sessions = tokio::time::timeout(Duration::from_secs(20), list_sessions(Some(host)))
        .await
        .context("timed out")??;
    sessions.retain(|s| {
        s.is_running() && s.clients.is_empty() && s.ui.get(META_AUTO_ATTACH).map(String::as_str) == Some("true")
    });
    sessions.sort_by_key(|s| (s.ui.get(META_TAB_ORDER).and_then(|v| v.parse::<u64>().ok()).unwrap_or(u64::MAX), s.created_ms));
    Ok(sessions)
}

/// Sessions on `host`, oldest first.
pub async fn list_sessions(host: Option<&str>) -> Result<Vec<DaemonSession>> {
    let daemons = daemons().context("daemon sessions are off")?;
    daemons.client(host).await?.list().await
}

/// Starts a login shell on `host`, marked as this app's and to reopen.
pub async fn create_session(host: Option<&str>, cols: u16, rows: u16) -> Result<DaemonSession> {
    let daemons = daemons().context("daemon sessions are off")?;
    let mut ui = BTreeMap::new();
    ui.insert(META_CREATED_BY.to_string(), "warp".to_string());
    ui.insert(META_AUTO_ATTACH.to_string(), "true".to_string());
    let opts = CreateOpts {
        command: Vec::new(),
        cwd: None,
        env: Vec::new(),
        size: Size { cols: cols.max(1), rows: rows.max(1) },
        name: None,
        ui,
    };
    daemons.client(host).await?.create(opts).await
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
