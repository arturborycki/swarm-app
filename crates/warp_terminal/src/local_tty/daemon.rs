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

use std::collections::BTreeMap;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Arc, Mutex};
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

pub fn enabled() -> bool {
    std::env::var("SWARM_DAEMON").map(|v| v != "0").unwrap_or(true)
}

pub struct DaemonSpawner {
    rt: tokio::runtime::Runtime,
    client: tokio::sync::Mutex<Option<Client>>,
}

enum Ctl {
    Resize(Size),
    Detach,
}

impl DaemonSpawner {
    pub fn new() -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("swarm-daemon")
            .enable_all()
            .build()?;
        Ok(DaemonSpawner { rt, client: tokio::sync::Mutex::new(None) })
    }

    /// The daemon connection, (re)connecting and starting the daemon if needed.
    async fn client(&self) -> Result<Client> {
        let mut guard = self.client.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.ping().await.is_ok() {
                return Ok(c.clone());
            }
        }
        let c = Client::connect_local("warp").await.context("connect to swarmd")?;
        *guard = Some(c.clone());
        Ok(c)
    }

    /// Creates a daemon session running the shell described by `options`.
    pub fn spawn(&self, options: PtyOptions) -> Result<(PtySpawnResult, Box<dyn PtyHandle>)> {
        let PtyOptions {
            size,
            window_id,
            shell_starter,
            start_dir,
            env_vars,
            enable_ssh_wrapper,
            reuse_ssh_control_master,
            shell_debug_mode,
            honor_ps1,
            node_version_chip_enabled,
            close_fds: _,
        } = options;
        let session_id = shell_starter.session_id();
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

        let (client, info, att) = self.rt.block_on(async {
            let client = self.client().await?;
            let info = client.create(opts).await.context("create session")?;
            let att = client.attach(&info.id, AttachMode::Full, Some(to_size(&size)), false).await?;
            anyhow::Ok((client, info, att))
        })?;
        log::info!("swarm: created daemon session {} (pid {})", info.id, info.pid);
        self.bridge(client, info, att, false)
    }

    fn bridge(
        &self,
        client: Client,
        info: SessionInfo,
        att: Attachment,
        guard_replay: bool,
    ) -> Result<(PtySpawnResult, Box<dyn PtyHandle>)> {
        let (ours, theirs) = StdUnixStream::pair()?;
        ours.set_nonblocking(true)?;
        let status = Arc::new(Mutex::new(None));
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        self.rt.spawn(run_bridge(client, att, ours, ctl_rx, status.clone(), guard_replay));
        let handle = DaemonPtyHandle { pid: info.pid, status, ctl: ctl_tx };
        Ok((PtySpawnResult { pid: info.pid, leader_fd: theirs.into_raw_fd() }, Box::new(handle)))
    }
}

fn to_size(size: &SizeInfo) -> Size {
    Size { cols: size.columns().clamp(1, u16::MAX as usize) as u16, rows: size.rows().clamp(1, u16::MAX as usize) as u16 }
}

async fn run_bridge(
    client: Client,
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
                    break;
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
                Some(Ctl::Detach) | None => break,
            },
        }
    }
    // Dropping the attachment detaches; the session keeps running.
    drop(att);
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

    /// Detaches only: the daemon keeps the session for the next attach.
    fn kill(&mut self) -> Result<()> {
        let _ = self.ctl.send(Ctl::Detach);
        Ok(())
    }

    fn resize(&self, size: &SizeInfo) -> Option<Result<()>> {
        Some(self.ctl.send(Ctl::Resize(to_size(size))).map_err(|_| anyhow::anyhow!("session bridge closed")))
    }
}
