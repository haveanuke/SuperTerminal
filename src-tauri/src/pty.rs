use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::shell_env;

/// Current working directory of a process via `proc_pidinfo(PROC_PIDVNODEPATHINFO)`.
/// Direct syscall — libproc-the-crate stubs this out on macOS, and the C struct
/// layout (vnode_info 152 bytes + MAXPATHLEN 1024, twice = 2352) is stable ABI.
#[cfg(target_os = "macos")]
mod proc_cwd {
    use std::os::raw::{c_int, c_void};

    const PROC_PIDVNODEPATHINFO: c_int = 9;

    #[repr(C)]
    struct VnodeInfoPath {
        _vi: [u8; 152],
        vip_path: [u8; 1024],
    }

    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    pub fn pid_cwd(pid: i32) -> Option<String> {
        let mut info = std::mem::MaybeUninit::<ProcVnodePathInfo>::uninit();
        let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
        let ret = unsafe {
            proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr() as *mut c_void, size)
        };
        if ret <= 0 {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let path = &info.pvi_cdir.vip_path;
        let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        std::str::from_utf8(&path[..len]).ok().map(String::from)
    }
}

/// Reservation-based slot: `Creating` blocks duplicate creates without holding
/// the map lock across the (slow) spawn. Every reservation carries a unique
/// generation so a create that lost a dispose/recreate race can detect it —
/// without the generation, create A could install its record over create B's
/// reservation after A was disposed mid-flight, leaving the bridge holding B's
/// channel while the backend streams to A's.
enum PtySlot {
    Creating(u64),
    Live(u64, PtyRecord),
}

struct PtyRecord {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    pid: Option<u32>,
}

#[derive(Default)]
pub struct PtyManager {
    slots: Arc<Mutex<HashMap<String, PtySlot>>>,
    next_gen: std::sync::atomic::AtomicU64,
    /// Test-only: one-shot artificial delay between reservation and spawn,
    /// used to open the dispose-during-create race window deterministically.
    #[cfg(test)]
    spawn_delay: Mutex<Option<std::time::Duration>>,
}

#[cfg(test)]
impl PtyManager {
    fn slot_is_creating(&self, id: &str) -> bool {
        matches!(self.slots.lock().unwrap().get(id), Some(PtySlot::Creating(_)))
    }
}

impl PtyManager {
    pub fn create(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
        channel: Channel<InvokeResponseBody>,
    ) -> Result<bool, String> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        self.create_with_shell(id, cols, rows, cwd, channel, &shell)
    }

    /// Returns Ok(true) if a new PTY was spawned, Ok(false) if the id already existed.
    fn create_with_shell(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
        channel: Channel<InvokeResponseBody>,
        shell: &str,
    ) -> Result<bool, String> {
        use std::sync::atomic::Ordering;
        let gen = self.next_gen.fetch_add(1, Ordering::SeqCst);

        // Reserve the slot (never spawn while holding the lock).
        {
            let mut slots = self.slots.lock().unwrap();
            if slots.contains_key(id) {
                return Ok(false);
            }
            slots.insert(id.to_string(), PtySlot::Creating(gen));
        }

        #[cfg(test)]
        if let Some(d) = self.spawn_delay.lock().unwrap().take() {
            std::thread::sleep(d);
        }

        let spawned = (|| -> Result<PtyRecord, String> {
            let pair = native_pty_system()
                .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
                .map_err(|e| e.to_string())?;
            let mut cmd = CommandBuilder::new(shell);
            cmd.env_clear();
            for (k, v) in shell_env::shell_env() {
                cmd.env(k, v);
            }
            cmd.env("TERM", "xterm-256color");
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            cmd.cwd(cwd.as_deref().unwrap_or(&home));

            let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
            drop(pair.slave);

            let pid = child.process_id();
            let mut killer = child.clone_killer();

            // Any failure after spawn must kill the child, or it leaks.
            let reader = match pair.master.try_clone_reader() {
                Ok(r) => r,
                Err(e) => {
                    let _ = killer.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            };
            let writer = match pair.master.take_writer() {
                Ok(w) => w,
                Err(e) => {
                    let _ = killer.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            };
            let mut reader = reader;

            // Waiter thread: sole owner of child.wait(); hands the code to the
            // reader thread so the exit frame is always the LAST frame sent.
            let (exit_tx, exit_rx) = mpsc::channel::<i32>();
            std::thread::spawn(move || {
                let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
                let _ = exit_tx.send(code);
            });

            // Reader thread: pump raw bytes until EOF, then send the exit frame.
            // recv() has NO timeout: a process can close its PTY fds (reader EOF)
            // and keep running — the exit frame must carry the real code, never a
            // fabricated one. Dispose/shutdown kill the child, which unblocks wait().
            let slots_for_reader = Arc::clone(&self.slots);
            let id_for_reader = id.to_string();
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if channel
                                .send(InvokeResponseBody::Raw(buf[..n].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                let code = exit_rx.recv().unwrap_or(-1);
                let _ = channel.send(InvokeResponseBody::Json(format!("{{\"exit\":{code}}}")));
                // Invalidate the pid so pty_cwd never queries a reused pid —
                // but only on our own generation's record, never a replacement's.
                let mut slots = slots_for_reader.lock().unwrap();
                if let Some(PtySlot::Live(g, rec)) = slots.get_mut(&id_for_reader) {
                    if *g == gen {
                        rec.pid = None;
                    }
                }
            });

            Ok(PtyRecord {
                writer: Arc::new(Mutex::new(writer)),
                master: Arc::new(Mutex::new(pair.master)),
                killer: Arc::new(Mutex::new(killer)),
                pid,
            })
        })();

        match spawned {
            Ok(record) => {
                let mut slots = self.slots.lock().unwrap();
                match slots.get(id) {
                    // Install only over our OWN reservation.
                    Some(PtySlot::Creating(g)) if *g == gen => {
                        slots.insert(id.to_string(), PtySlot::Live(gen, record));
                        Ok(true)
                    }
                    // Disposed (and possibly replaced) while creating: kill the
                    // fresh child; its reader sends the exit frame and unwinds.
                    _ => {
                        drop(slots);
                        let _ = record.killer.lock().unwrap().kill();
                        Ok(true)
                    }
                }
            }
            Err(e) => {
                let mut slots = self.slots.lock().unwrap();
                if let Some(PtySlot::Creating(g)) = slots.get(id) {
                    if *g == gen {
                        slots.remove(id);
                    }
                }
                Err(e)
            }
        }
    }

    fn live_writer(&self, id: &str) -> Option<Arc<Mutex<Box<dyn Write + Send>>>> {
        let slots = self.slots.lock().unwrap();
        match slots.get(id) {
            Some(PtySlot::Live(_, rec)) => Some(Arc::clone(&rec.writer)),
            _ => None,
        }
    }

    pub fn write(&self, id: &str, data: &str) {
        if let Some(writer) = self.live_writer(id) {
            let mut w = writer.lock().unwrap();
            let _ = w.write_all(data.as_bytes());
            let _ = w.flush();
        }
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) {
        let master = {
            let slots = self.slots.lock().unwrap();
            match slots.get(id) {
                Some(PtySlot::Live(_, rec)) => Some(Arc::clone(&rec.master)),
                _ => None,
            }
        };
        if let Some(master) = master {
            let _ = master.lock().unwrap().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn dispose(&self, id: &str) {
        let slot = self.slots.lock().unwrap().remove(id);
        if let Some(PtySlot::Live(_, rec)) = slot {
            let _ = rec.killer.lock().unwrap().kill();
            // reader hits EOF -> sends exit frame -> threads unwind
        }
        // A `Creating` slot is simply removed: the in-flight create detects the
        // missing/foreign reservation at install time and kills its child.
    }

    pub fn dispose_all(&self) {
        let ids: Vec<String> = self.slots.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.dispose(&id);
        }
    }

    pub fn cwd(&self, id: &str) -> Option<String> {
        let pid = {
            let slots = self.slots.lock().unwrap();
            match slots.get(id) {
                Some(PtySlot::Live(_, rec)) => rec.pid,
                _ => None,
            }
        }?;
        #[cfg(target_os = "macos")]
        {
            proc_cwd::pid_cwd(pid as i32)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = pid;
            None
        }
    }
}

#[tauri::command]
pub fn pty_create(
    id: String,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, PtyManager>,
) -> Result<bool, String> {
    state.create(&id, cols, rows, cwd, channel)
}

#[tauri::command]
pub fn pty_write(id: String, data: String, state: State<'_, PtyManager>) {
    state.write(&id, &data);
}

#[tauri::command]
pub fn pty_write_broadcast(ids: Vec<String>, data: String, state: State<'_, PtyManager>) {
    for id in ids {
        state.write(&id, &data);
    }
}

#[tauri::command]
pub fn pty_resize(id: String, cols: u16, rows: u16, state: State<'_, PtyManager>) {
    state.resize(&id, cols, rows);
}

#[tauri::command]
pub fn pty_dispose(id: String, state: State<'_, PtyManager>) {
    state.dispose(&id);
}

#[tauri::command]
pub fn pty_cwd(id: String, state: State<'_, PtyManager>) -> Option<String> {
    state.cwd(&id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_channel() -> (
        Channel<InvokeResponseBody>,
        mpsc::Receiver<InvokeResponseBody>,
    ) {
        let (tx, rx) = mpsc::channel();
        let ch = Channel::new(move |msg| {
            let _ = tx.send(msg);
            Ok(())
        });
        (ch, rx)
    }

    /// Drain frames until the exit frame; return (all_bytes, exit_json).
    fn drain(rx: &mpsc::Receiver<InvokeResponseBody>) -> (Vec<u8>, String) {
        let mut bytes = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(15)).expect("frame") {
                InvokeResponseBody::Raw(b) => bytes.extend_from_slice(&b),
                InvokeResponseBody::Json(j) => return (bytes, j),
            }
        }
    }

    #[test]
    fn streams_output_then_final_exit_frame() {
        let mgr = PtyManager::default();
        let (ch, rx) = test_channel();
        mgr.create_with_shell("t1", 80, 24, None, ch, "/bin/sh")
            .expect("create");
        mgr.write("t1", "printf hello-pty; exit 3\n");
        let (bytes, exit) = drain(&rx);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("hello-pty"), "got: {text}");
        assert_eq!(exit, "{\"exit\":3}");
    }

    #[test]
    fn create_same_id_is_noop_and_dispose_kills() {
        let mgr = PtyManager::default();
        let (ch1, rx1) = test_channel();
        assert!(mgr
            .create_with_shell("t2", 80, 24, None, ch1, "/bin/sh")
            .unwrap());
        let (ch2, _rx2) = test_channel();
        // second create with same id: no-op, returns false, original still alive
        assert!(!mgr
            .create_with_shell("t2", 80, 24, None, ch2, "/bin/sh")
            .unwrap());
        mgr.dispose("t2");
        // dispose leads to reader EOF and a final exit frame on the FIRST channel
        let (_bytes, exit) = drain(&rx1);
        assert!(exit.starts_with("{\"exit\":"));
        // id can be recreated after dispose
        let (ch3, rx3) = test_channel();
        assert!(mgr
            .create_with_shell("t2", 80, 24, None, ch3, "/bin/sh")
            .unwrap());
        mgr.write("t2", "exit\n");
        let _ = drain(&rx3);
    }

    #[test]
    fn dispose_during_create_installs_replacement_only() {
        let mgr = Arc::new(PtyManager::default());
        *mgr.spawn_delay.lock().unwrap() = Some(Duration::from_millis(600));
        let (ch_a, rx_a) = test_channel();
        let mgr2 = Arc::clone(&mgr);
        let t = std::thread::spawn(move || {
            mgr2.create_with_shell("r", 80, 24, None, ch_a, "/bin/sh")
                .unwrap()
        });
        // Deterministic: wait until A's reservation is observably in place
        // (A holds it while sleeping in the spawn_delay hook) before disposing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !mgr.slot_is_creating("r") {
            assert!(
                std::time::Instant::now() < deadline,
                "reservation never appeared"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        mgr.dispose("r"); // removes A's Creating reservation mid-flight
        let (ch_b, rx_b) = test_channel();
        assert!(mgr
            .create_with_shell("r", 80, 24, None, ch_b, "/bin/sh")
            .unwrap());
        t.join().unwrap();
        // B's shell is the live one:
        mgr.write("r", "printf B-alive; exit\n");
        let (bytes, _exit) = drain(&rx_b);
        assert!(String::from_utf8_lossy(&bytes).contains("B-alive"));
        // A's child was killed at install-conflict time; its channel still ends
        // with an exit frame (reader EOF -> real wait code).
        let (_ab, a_exit) = drain(&rx_a);
        assert!(a_exit.starts_with("{\"exit\":"));
    }

    #[test]
    fn exit_frame_carries_real_code_after_fd_close_eof() {
        // The child closes its PTY fds (reader EOF) but keeps running, then
        // exits 5. The exit frame must wait for the REAL code — never fabricate.
        let mgr = PtyManager::default();
        let (ch, rx) = test_channel();
        mgr.create_with_shell("dl", 80, 24, None, ch, "/bin/sh")
            .unwrap();
        mgr.write("dl", "exec 0<&- 1>&- 2>&-; sleep 1; exit 5\n");
        let (_bytes, exit) = drain(&rx);
        assert_eq!(exit, "{\"exit\":5}");
    }

    #[test]
    fn write_and_resize_on_unknown_id_are_noops() {
        let mgr = PtyManager::default();
        mgr.write("nope", "x");
        mgr.resize("nope", 100, 30);
        mgr.dispose("nope"); // no panic
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cwd_reports_spawn_dir_and_none_after_exit() {
        let mgr = PtyManager::default();
        let (ch, rx) = test_channel();
        mgr.create_with_shell("t3", 80, 24, Some("/private/tmp".into()), ch, "/bin/sh")
            .expect("create");
        // give the shell a moment to start
        std::thread::sleep(Duration::from_millis(500));
        let cwd = mgr.cwd("t3").expect("cwd");
        assert!(
            cwd.starts_with("/private/tmp") || cwd.starts_with("/tmp"),
            "got {cwd}"
        );
        mgr.write("t3", "exit\n");
        let _ = drain(&rx);
        // after exit the pid is invalidated
        std::thread::sleep(Duration::from_millis(200));
        assert!(mgr.cwd("t3").is_none());
    }
}
