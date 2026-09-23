//! Bounded subprocess I/O and lifetime management. No implicit shell is involved.
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, OnceLock};
use std::time::{Duration, Instant};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static HANDLER: OnceLock<std::result::Result<(), String>> = OnceLock::new();
const LIMIT: usize = 4 * 1024 * 1024;
const TAIL: usize = 64 * 1024;

pub fn install_signal_handler() -> Result<()> {
    HANDLER.get_or_init(|| ctrlc::set_handler(|| CANCELLED.store(true, Ordering::SeqCst)).map_err(|e| e.to_string()))
        .as_ref().map_err(|e| anyhow::anyhow!("installing cancellation handler: {e}"))?;
    Ok(())
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

enum Message { Data(Vec<u8>), Error(std::io::Error), End }

/// Readers spool bytes before delivering them. The channel, line buffer and stderr tail
/// are bounded; oversized records fail explicitly, with the original bytes retained on disk.
pub struct Process {
    child: Child,
    tree: Tree,
    rx: mpsc::Receiver<Message>,
    stderr: Option<std::thread::JoinHandle<Result<String>>>,
    started: Instant,
    timeout: Duration,
    pending: Vec<u8>,
    scanned: usize,
    ended: bool,
    pub spool: PathBuf,
}

impl Process {
    pub fn spawn(cmd: &mut Command, input: &[u8], timeout: Duration, spool: Option<&Path>) -> Result<Self> {
        install_signal_handler()?;
        if CANCELLED.load(Ordering::SeqCst) { bail!("cancelled"); }
        if timeout.is_zero() { bail!("subprocess timeout must be positive"); }
        let spool = spool.map(Path::to_path_buf).unwrap_or_else(|| crate::util::casimir_home().join("processes").join(uuid::Uuid::new_v4().to_string()));
        crate::util::private_dir(&spool)?;
        let mut stdout_file = crate::util::private_file(&spool.join("stdout.log"))?;
        let mut stderr_file = crate::util::private_file(&spool.join("stderr.log"))?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        Tree::configure(cmd);
        let spawn_started = Instant::now();
        let mut child = loop {
            match cmd.spawn() {
                Ok(child) => break child,
                Err(error) if (cfg!(unix) && error.raw_os_error() == Some(26)
                    || cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))) && spawn_started.elapsed() < Duration::from_secs(2) => {
                    // No process was created and no prompt was sent. Retry a transient
                    // executable sharing conflict, never a completed/ambiguous invocation.
                    std::thread::sleep(Duration::from_millis(10));
                },
                Err(error) => return Err(error).context("starting subprocess (arguments omitted for privacy)"),
            }
        };
        let tree = match Tree::attach(&child) {
            Ok(tree) => tree,
            Err(err) => { let _ = child.kill(); let _ = child.wait(); return Err(err); }
        };
        let mut stdin = child.stdin.take().context("subprocess stdin")?;
        let input = input.to_vec();
        std::thread::spawn(move || { let _ = stdin.write_all(&input); });
        let mut stdout = child.stdout.take().context("subprocess stdout")?;
        let mut stderr = child.stderr.take().context("subprocess stderr")?;
        let (tx, rx) = mpsc::sync_channel(8);
        let error_tx = tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> std::io::Result<()> {
                let mut buf = [0; 8192];
                loop {
                    let n = stdout.read(&mut buf)?;
                    if n == 0 { break; }
                    stdout_file.write_all(&buf[..n])?;
                    stdout_file.sync_data()?;
                    if tx.send(Message::Data(buf[..n].to_vec())).is_err() { return Ok(()); }
                }
                stdout_file.sync_all()?;
                Ok(())
            })();
            if let Err(e) = result { let _ = tx.send(Message::Error(e)); }
            let _ = tx.send(Message::End);
        });
        let err_thread = std::thread::spawn(move || {
            let result = (|| -> Result<String> {
                let mut buf = [0; 8192];
                let mut tail = Vec::new();
                loop {
                    let n = stderr.read(&mut buf)?;
                    if n == 0 { break; }
                    stderr_file.write_all(&buf[..n])?;
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > TAIL { tail.drain(..tail.len() - TAIL); }
                }
                stderr_file.sync_all()?;
                Ok(String::from_utf8_lossy(&tail).into_owned())
            })();
            if result.is_err() { let _ = error_tx.send(Message::Error(std::io::Error::other("stderr capture failed"))); }
            result
        });
        Ok(Self { child, tree, rx, stderr: Some(err_thread), started: Instant::now(), timeout, pending: Vec::new(), scanned: 0, ended: false, spool })
    }

    fn poll(&mut self) -> Result<()> {
        if CANCELLED.load(Ordering::SeqCst) { bail!("subprocess cancelled; raw output: {}", self.spool.display()); }
        if self.started.elapsed() >= self.timeout { bail!("subprocess timed out after {} seconds; raw output: {}", self.timeout.as_secs(), self.spool.display()); }
        // A parent that exits while descendants retain pipe handles must not hang readers.
        if self.child.try_wait()?.is_some() { self.tree.kill(); }
        Ok(())
    }

    pub fn next_line(&mut self) -> Result<Option<String>> {
        loop {
            self.poll()?;
            if let Some(end) = self.pending[self.scanned..].iter().position(|b| *b == b'\n').map(|i| self.scanned + i) {
                let line: Vec<_> = self.pending.drain(..=end).collect();
                self.scanned = 0;
                return Ok(Some(String::from_utf8(line).context("non-UTF-8 subprocess protocol")?));
            }
            self.scanned = self.pending.len();
            if self.ended {
                if self.pending.is_empty() { return Ok(None); }
                self.scanned = 0;
                return Ok(Some(String::from_utf8(std::mem::take(&mut self.pending)).context("non-UTF-8 subprocess protocol")?));
            }
            match self.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Message::Data(bytes)) => {
                    self.pending.extend(bytes);
                    if self.pending.len() > LIMIT { bail!("subprocess record exceeds 4 MiB; raw output: {}", self.spool.display()); }
                }
                Ok(Message::Error(err)) => return Err(err).context("persisting subprocess output"),
                Ok(Message::End) => self.ended = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {},
                Err(mpsc::RecvTimeoutError::Disconnected) => self.ended = true,
            }
        }
    }

    pub fn finish(mut self) -> Result<Output> {
        self.pending.clear();
        while !self.ended {
            self.poll()?;
            match self.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Message::Data(_)) => {},
                Ok(Message::Error(error)) => return Err(error).context("persisting subprocess output"),
                Ok(Message::End) | Err(mpsc::RecvTimeoutError::Disconnected) => self.ended = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {},
            }
        }
        let status = loop {
            self.poll()?;
            if let Some(status) = self.child.try_wait()? { break status; }
            std::thread::sleep(Duration::from_millis(10));
        };
        self.tree.kill();
        let stderr = self.stderr.take().context("stderr reader missing")?.join().map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
        Ok(Output { status, stdout: Vec::new(), stderr })
    }
}

impl Drop for Process {
    fn drop(&mut self) { self.tree.kill(); let _ = self.child.kill(); let _ = self.child.wait(); }
}

pub fn capture(cmd: &mut Command, input: &[u8], timeout: Duration, spool: Option<&Path>) -> Result<Output> {
    let mut process = Process::spawn(cmd, input, timeout, spool)?;
    let mut bytes = Vec::new();
    while let Some(line) = process.next_line()? {
        if bytes.len() + line.len() > LIMIT { bail!("subprocess response exceeds 4 MiB; raw output: {}", process.spool.display()); }
        bytes.extend_from_slice(line.as_bytes());
    }
    let mut output = process.finish()?;
    output.stdout = bytes;
    Ok(output)
}

#[cfg(unix)]
struct Tree { pid: i32 }
#[cfg(unix)]
impl Tree {
    fn configure(cmd: &mut Command) { use std::os::unix::process::CommandExt; cmd.process_group(0); }
    fn attach(child: &Child) -> Result<Self> { Ok(Self { pid: child.id() as i32 }) }
    fn kill(&self) { unsafe { libc::kill(-self.pid, libc::SIGKILL); } }
}

#[cfg(windows)]
struct Tree { job: windows_sys::Win32::Foundation::HANDLE }
#[cfg(windows)]
impl Tree {
    fn configure(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::*, System::{JobObjects::*, Threading::*, Diagnostics::ToolHelp::*}};
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() { return Err(std::io::Error::last_os_error().into()); }
            let tree = Self { job };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as _, std::mem::size_of_val(&limits) as u32) == 0
                || AssignProcessToJobObject(job, child.as_raw_handle()) == 0 { return Err(std::io::Error::last_os_error().into()); }
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE { return Err(std::io::Error::last_os_error().into()); }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            let mut more = Thread32First(snapshot, &mut entry);
            let mut resumed = false;
            while more != 0 {
                if entry.th32OwnerProcessID == child.id() {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() { resumed |= ResumeThread(thread) != u32::MAX; CloseHandle(thread); }
                }
                more = Thread32Next(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
            if !resumed { bail!("could not resume subprocess in Windows Job Object"); }
            Ok(tree)
        }
    }
    fn kill(&self) { unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job, 1); } }
}
#[cfg(windows)]
impl Drop for Tree { fn drop(&mut self) { unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job); } } }
