//! WebGL on this PC's GPU: one GPU service owns the [`device::Device`] and every context, as
//! Chrome's GPU process does; the page's thread hands it each list's new commands and waits for the answer.
//!
//! The service is a PROCESS of its own when the binary that embeds the engine says it can be one
//! ([`use_service_process`]: `korvus` re-runs itself with [`SERVICE_ARG`], as Chrome starts `--type=gpu-process`) --
//! one per browser, shared by its page processes ([`start_shared_service`], [`connect_service`]) -- and a question the
//! page's thread waits on crosses to it as frames on a socket; otherwise (a test harness) it is a thread of this process.
//!
//! [`webgl_block`] is the one question the rest of the engine asks first: no usable GPU (see [`device`]) means no
//! WebGL, as in Chrome 150 on Linux; a GPU answers the values a page reads of it ([`oracle`]).

pub mod device;
pub mod exec;
pub mod gles;
pub mod oracle;
pub mod profile;

use std::collections::HashMap;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use spec::{GlContextAttributes, GlOp, GlQuery, GlQueryResult, Rgba8};

pub use device::DeviceInfo;

/// The argument a binary that embeds the engine runs its GPU service under ([`service_main`]).
pub const SERVICE_ARG: &str = "--type=gpu-process";
/// The argument that names the socket a shared service listens at.
#[cfg(unix)]
const SOCKET_ARG: &str = "--gpu-socket=";

/// What a context is asked for: its new commands, then a readback or a query.
#[derive(Serialize, Deserialize)]
enum Ask {
    Raster,
    Query(GlQuery),
}

/// Chrome's two messages for a question: the list's new commands (`FlushDeferredRequests`, no answer), then the wait
/// for their result (`WaitForGetOffsetInRange`, answered).
#[derive(Serialize, Deserialize)]
enum Req {
    Flush { list: u64, width: u32, height: u32, attrs: GlContextAttributes, ops: Vec<GlOp> },
    Wait { list: u64, ask: Ask },
    Drop { list: u64 },
}

#[derive(Serialize, Deserialize)]
enum Answer {
    Pixels(Rgba8),
    Result(GlQueryResult),
    /// the device could not make the list's context
    Failed,
}

/// The device and its `webgl` block, or none.
type Ready = Option<(DeviceInfo, String)>;

/// The service's first word to each client, on the socket before anything else: its device.
#[derive(Clone, Serialize, Deserialize)]
struct Hello {
    device: Ready,
}

/// How the page's side reaches the service.
enum Link {
    Thread { tx: mpsc::Sender<Req>, rx: mpsc::Receiver<Answer> },
    /// Frames on a socket: the asking thread writes its message itself and
    /// the answer comes back through this side's IO thread, which wakes it (`rx`).
    /// Unix only -- the shared multi-process GPU service; Windows runs the GPU in a thread.
    #[cfg(unix)]
    Socket { stream: UnixStream, rx: mpsc::Receiver<Answer>, _child: Option<std::process::Child> },
}

impl Link {
    /// A message with no answer.
    fn flush(&mut self, req: Req) -> Option<()> {
        match self {
            Link::Thread { tx, .. } => tx.send(req).ok(),
            #[cfg(unix)]
            Link::Socket { stream, .. } => write_frame(stream, &req),
        }
    }

    /// A message the asking thread waits on the answer to.
    fn wait(&mut self, req: Req) -> Option<Answer> {
        match self {
            Link::Thread { tx, rx } => {
                tx.send(req).ok()?;
                rx.recv().ok()
            }
            #[cfg(unix)]
            Link::Socket { stream, rx, .. } => {
                write_frame(stream, &req)?;
                rx.recv().ok()
            }
        }
    }
}

/// An IO thread: every frame `r` brings, handed to whoever waits on the other end of `tx`, until either side closes
/// (Chrome's channels are read on their process's IO thread and their messages posted to the thread they are for).
#[cfg(unix)]
fn io_thread<T: for<'de> Deserialize<'de> + Send + 'static>(name: &str, mut r: UnixStream, tx: mpsc::Sender<T>) -> Option<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            while let Some(v) = read_frame::<T>(&mut r) {
                if tx.send(v).is_err() {
                    return;
                }
            }
        })
        .ok()
        .map(|_| ())
}

/// One frame: its length, then its bincode.
#[cfg(unix)]
fn write_frame<T: Serialize>(w: &mut impl Write, v: &T) -> Option<()> {
    let bytes = bincode::serialize(v).ok()?;
    w.write_all(&(bytes.len() as u64).to_le_bytes()).ok()?;
    w.write_all(&bytes).ok()?;
    w.flush().ok()
}

#[cfg(unix)]
fn read_frame<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> Option<T> {
    let mut len = [0u8; 8];
    r.read_exact(&mut len).ok()?;
    let mut bytes = vec![0u8; u64::from_le_bytes(len) as usize];
    r.read_exact(&mut bytes).ok()?;
    bincode::deserialize(&bytes).ok()
}

struct Gpu {
    /// the link and how many of each list's commands the service has been sent, under one lock: a question and its
    /// answer are one exchange
    link: Mutex<(Link, HashMap<u64, usize>)>,
    info: DeviceInfo,
    /// the device's `deviceOracle.webgl` values, as JSON ([`oracle::webgl_block`])
    block: String,
}

#[cfg(unix)]
static SERVICE_EXE: OnceLock<PathBuf> = OnceLock::new();
/// where this process's own service, when it is a browser's shared one, takes its page processes' connections
#[cfg(unix)]
static SHARED_SOCKET: OnceLock<PathBuf> = OnceLock::new();
/// the shared service of the browser this process is a page of
#[cfg(unix)]
static CONNECT_SOCKET: OnceLock<PathBuf> = OnceLock::new();
static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

/// The embedding binary can run the GPU service as a process of its own: `exe` started with [`SERVICE_ARG`] calls
/// [`service_main`]. Before the first WebGL question; without it the service is a thread.
#[cfg(unix)]
pub fn use_service_process(exe: PathBuf) {
    let _ = SERVICE_EXE.set(exe);
}
/// Windows has no shared GPU-service process (the IPC is a Unix socket): the GPU runs in a thread.
#[cfg(not(unix))]
pub fn use_service_process(_exe: PathBuf) {}

/// Start the GPU service now, once, on a thread of its own, so the page that asks first does not wait for the device
/// (Chrome's GPU process is up before any page asks).
pub fn warm() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("korvus-gpu-warm".into()).spawn(|| {
            gpu();
        });
    });
}

/// A browser's GPU service, shared by every page process it starts -- Chrome's model, one GPU process per browser: started
/// now (this process's own link to it too), and the socket a page process hands [`connect_service`]. `None` when this
/// binary cannot run the service as a process ([`use_service_process`]) or this PC has no usable GPU. Before this
/// process's first WebGL question or [`warm`].
#[cfg(unix)]
pub fn start_shared_service() -> Option<PathBuf> {
    SERVICE_EXE.get()?;
    let name = format!("korvus-gpu-{}.sock", std::process::id());
    // a Unix socket's path holds 107 bytes: a long TMPDIR gives way to /tmp
    let mut path = std::env::temp_dir().join(&name);
    if path.as_os_str().len() > 100 {
        path = PathBuf::from("/tmp").join(&name);
    }
    let _ = std::fs::remove_file(&path);
    let _ = SHARED_SOCKET.set(path.clone());
    // Bring the GPU up OFF the cold path (goal 1022). This endpoint never draws -- only its page processes do, at
    // their first canvas -- so CDP-ready must not wait on the device. Spawn the service + this process's own link on
    // a thread and hand the socket to page processes right away: a page connects to it at its first canvas (page
    // load outlasts the bind), and if the service is not up yet or this PC has no usable GPU the page's connect
    // fails and it starts its own GPU (gpu()'s fallback). So nothing on the cold path blocks on Skia/ANGLE.
    std::thread::Builder::new().name("korvus-gpu-bringup".into()).spawn(|| { gpu(); }).ok();
    Some(path)
}
/// Windows has no shared GPU service (Unix-socket IPC): there is no socket to hand a page process.
#[cfg(not(unix))]
pub fn start_shared_service() -> Option<PathBuf> {
    None
}

/// This process is a page of a browser whose shared GPU service listens at `socket` ([`start_shared_service`]): its
/// WebGL goes there. Before its first WebGL question.
#[cfg(unix)]
pub fn connect_service(socket: PathBuf) {
    let _ = CONNECT_SOCKET.set(socket);
}
/// Windows does not connect to a shared GPU service (Unix-socket IPC): the GPU runs in-process.
#[cfg(not(unix))]
pub fn connect_service(_socket: PathBuf) {}

fn gpu() -> Option<&'static Gpu> {
    GPU.get_or_init(|| {
        // `KORVUS_GPU=off`: run as a PC without a usable GPU does (no WebGL), whatever this one has
        if std::env::var("KORVUS_GPU").as_deref() == Ok("off") {
            return None;
        }
        // the browser's shared service; else (none, or it is gone) this process's own. The shared
        // multi-process service is Unix-only (socket IPC); Windows always runs the GPU in a thread.
        #[cfg(unix)]
        let (link, ready) = {
            let shared = CONNECT_SOCKET.get().and_then(|socket| UnixStream::connect(socket).ok()).and_then(|s| join(s, None));
            match (shared, SERVICE_EXE.get()) {
                (Some(link), _) => link,
                (None, Some(exe)) => spawn_process(exe, SHARED_SOCKET.get())?,
                (None, None) => spawn_thread()?,
            }
        };
        #[cfg(not(unix))]
        let (link, ready) = spawn_thread()?;
        let (info, block) = ready?;
        Some(Gpu { link: Mutex::new((link, HashMap::new())), info, block })
    })
    .as_ref()
}

fn spawn_thread() -> Option<(Link, Ready)> {
    let (tx, req_rx) = mpsc::channel::<Req>();
    let (ans_tx, rx) = mpsc::channel::<Answer>();
    let (ready_tx, ready_rx) = mpsc::channel::<Ready>();
    std::thread::Builder::new()
        .name("korvus-gpu".into())
        .spawn(move || {
            let Some((device, block)) = open_device() else {
                let _ = ready_tx.send(None);
                return;
            };
            let _ = ready_tx.send(Some((device.info.clone(), block)));
            let mut service = Service::new(device);
            while let Ok(req) = req_rx.recv() {
                if let Some(answer) = service.handle(0, req) {
                    if ans_tx.send(answer).is_err() {
                        return;
                    }
                }
            }
        })
        .ok()?;
    let ready = ready_rx.recv().ok()?;
    Some((Link::Thread { tx, rx }, ready))
}

/// The GPU service as a process: `exe --type=gpu-process`, this process's link its stdin; listening at `shared` for a
/// browser's page processes too.
#[cfg(unix)]
fn spawn_process(exe: &PathBuf, shared: Option<&PathBuf>) -> Option<(Link, Ready)> {
    let (ours, theirs) = UnixStream::pair().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(SERVICE_ARG);
    if let Some(p) = shared {
        cmd.arg(format!("{SOCKET_ARG}{}", p.display()));
    }
    let child = cmd
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(theirs)))
        .stdout(std::process::Stdio::null())
        .spawn()
        .ok()?;
    join(ours, Some(child))
}

/// A link over a connected socket: the service's [`Hello`], then frames on it.
#[cfg(unix)]
fn join(mut stream: UnixStream, child: Option<std::process::Child>) -> Option<(Link, Ready)> {
    let hello: Hello = read_frame(&mut stream)?;
    let (tx, rx) = mpsc::channel::<Answer>();
    io_thread("korvus-gpu-io", stream.try_clone().ok()?, tx)?;
    Some((Link::Socket { stream, rx, _child: child }, hello.device))
}

/// The GPU service process's whole life (`exe --type=gpu-process [--gpu-socket=<path>]`): its spawner's link is its
/// stdin, a socket, and a browser's page processes connect at `<path>`. Each client is read on an IO thread and its
/// requests posted to this process's main thread, which runs them on the device and answers as frames on the sockets;
/// it ends when its spawner goes.
#[cfg(unix)]
pub fn service_main() {
    use std::os::fd::FromRawFd;
    let shared = std::env::args().find_map(|a| a.strip_prefix(SOCKET_ARG).map(PathBuf::from));
    // SAFETY: fd 0 is the socket `spawn_process` handed this process as its stdin, and nothing else owns it
    let mut spawner = unsafe { UnixStream::from_raw_fd(0) };
    let Some((device, block)) = open_device() else {
        let _ = write_frame(&mut spawner, &Hello { device: None });
        return;
    };
    let hello = Hello { device: Some((device.info.clone(), block)) };
    // listening BEFORE the spawner hears it is ready: once `start_shared_service` returns, a page process may connect
    let listener = shared.as_ref().and_then(|p| UnixListener::bind(p).ok());
    serve_sockets(device, hello, spawner, listener);
    if let Some(p) = shared {
        let _ = std::fs::remove_file(p);
    }
}
/// Windows never runs as a separate GPU-service process (the IPC is a Unix socket); the SERVICE_ARG
/// branch a binary guards with this is never taken there.
#[cfg(not(unix))]
pub fn service_main() {}

/// The service over frames on sockets: each client read on an IO thread of its own, its requests posted to this (main)
/// thread with its id (`None` when it goes), its answers written back on its socket.
#[cfg(unix)]
fn serve_sockets(device: device::Device, hello: Hello, spawner: UnixStream, listener: Option<UnixListener>) {
    let (tx, rx) = mpsc::channel::<(u32, Option<Req>)>();
    let writers: Arc<Mutex<HashMap<u32, UnixStream>>> = Arc::new(Mutex::new(HashMap::new()));
    if !add_client(0, spawner, &hello, &tx, &writers) {
        return;
    }
    if let Some(listener) = listener {
        let (hello, tx, writers) = (hello.clone(), tx.clone(), writers.clone());
        let _ = std::thread::Builder::new().name("korvus-gpu-accept".into()).spawn(move || {
            for (id, conn) in (1u32..).zip(listener.incoming()) {
                if let Ok(conn) = conn {
                    add_client(id, conn, &hello, &tx, &writers);
                }
            }
        });
    }
    let mut service = Service::new(device);
    while let Ok((client, req)) = rx.recv() {
        let Some(req) = req else {
            service.gone(client);
            if client == 0 {
                return;
            }
            continue;
        };
        if let Some(answer) = service.handle(client, req) {
            let sent = writers.lock().ok().and_then(|mut w| w.get_mut(&client).map(|s| write_frame(s, &answer).is_some()));
            // the spawner gone: so is the service (a page client's answer to nowhere is nothing)
            if sent == Some(false) && client == 0 {
                return;
            }
        }
    }
}

/// One socket client of the service: told the device, its writer kept for its answers, and an IO thread posting its
/// requests -- and, at its end, its leaving -- to the main thread.
#[cfg(unix)]
fn add_client(id: u32, stream: UnixStream, hello: &Hello, tx: &mpsc::Sender<(u32, Option<Req>)>, writers: &Arc<Mutex<HashMap<u32, UnixStream>>>) -> bool {
    let Ok(mut w) = stream.try_clone() else { return false };
    if write_frame(&mut w, hello).is_none() {
        return false;
    }
    if let Ok(mut map) = writers.lock() {
        map.insert(id, w);
    }
    let (tx, writers) = (tx.clone(), writers.clone());
    std::thread::Builder::new()
        .name("korvus-gpu-io".into())
        .spawn(move || {
            let mut r = stream;
            while let Some(req) = read_frame::<Req>(&mut r) {
                if tx.send((id, Some(req))).is_err() {
                    return;
                }
            }
            if let Ok(mut map) = writers.lock() {
                map.remove(&id);
            }
            let _ = tx.send((id, None));
        })
        .is_ok()
}

fn ask(list: u64, cmds: &[GlOp], width: u32, height: u32, attrs: &GlContextAttributes, ask: Ask) -> Option<Answer> {
    let g = gpu()?;
    let mut guard = g.link.lock().ok()?;
    let (link, sent) = &mut *guard;
    let n = sent.entry(list).or_insert(0);
    if *n > cmds.len() {
        // the list started over: so does its context
        link.flush(Req::Drop { list })?;
        *n = 0;
    }
    let ops = cmds[*n..].to_vec();
    *n = cmds.len();
    link.flush(Req::Flush { list, width, height, attrs: attrs.clone(), ops })?;
    link.wait(Req::Wait { list, ask })
}

/// The device a page's WebGL draws on, when this PC has a usable GPU.
pub fn available() -> Option<DeviceInfo> {
    gpu().map(|g| g.info.clone())
}

/// The device's half of the `deviceOracle.webgl` block, as JSON; `None` when this PC has no usable GPU.
pub fn webgl_block() -> Option<String> {
    gpu().map(|g| g.block.clone())
}

/// List `list`'s DrawingBuffer after its commands, top row first.
pub fn raster(list: u64, cmds: &[GlOp], width: u32, height: u32, attrs: &GlContextAttributes) -> Option<Rgba8> {
    match ask(list, cmds, width, height, attrs, Ask::Raster)? {
        Answer::Pixels(p) => Some(p),
        _ => None,
    }
}

/// `q`, asked of list `list`'s context after its commands.
pub fn query(list: u64, cmds: &[GlOp], q: &GlQuery, width: u32, height: u32, attrs: &GlContextAttributes) -> Option<GlQueryResult> {
    match ask(list, cmds, width, height, attrs, Ask::Query(q.clone()))? {
        Answer::Result(r) => Some(r),
        _ => None,
    }
}

/// The page let list `list` go (its context was collected): its GPU context goes too.
pub fn release(list: u64) {
    let Some(g) = GPU.get().and_then(|g| g.as_ref()) else { return };
    if let Ok(mut guard) = g.link.lock() {
        let (link, sent) = &mut *guard;
        if sent.remove(&list).is_some() {
            let _ = link.flush(Req::Drop { list });
        }
    }
}

/// The device and its `webgl` block, or none: a PC without a usable GPU.
fn open_device() -> Option<(device::Device, String)> {
    let d = device::Device::open()?;
    let block = oracle::webgl_block(&d)?.to_string();
    Some((d, block))
}

/// The GPU service's state: the device, and every client's contexts, each client's its own.
struct Service {
    device: device::Device,
    gl: Option<gles::Gles>,
    contexts: HashMap<(u32, u64), (device::DeviceContext, exec::Exec)>,
    current: Option<(u32, u64)>,
}

impl Service {
    fn new(device: device::Device) -> Service {
        Service { device, gl: None, contexts: HashMap::new(), current: None }
    }

    /// One request of `client`'s, in order: the answer a `Wait` has (`None` for a `Flush` or a `Drop`).
    fn handle(&mut self, client: u32, req: Req) -> Option<Answer> {
        match req {
            Req::Drop { list } => {
                if let Some((c, _)) = self.contexts.remove(&(client, list)) {
                    self.device.destroy(c);
                    self.current = None;
                }
                None
            }
            Req::Flush { list, width, height, attrs, ops } => {
                let key = (client, list);
                if !self.contexts.contains_key(&key) {
                    // a context the device cannot make: the `Wait` after this answers `Failed`
                    let c = self.device.create_context(if attrs.webgl2 { 3 } else { 2 })?;
                    self.device.make_current(&c);
                    self.current = Some(key);
                    let g = self.gl.get_or_insert_with(|| gles::Gles::load(&self.device));
                    let x = exec::Exec::new(g, width, height, &attrs);
                    self.contexts.insert(key, (c, x));
                }
                let (c, x) = self.contexts.get_mut(&key)?;
                let g = self.gl.as_ref()?;
                if self.current != Some(key) {
                    self.device.make_current(c);
                    self.current = Some(key);
                }
                x.run_new(g, &ops);
                None
            }
            Req::Wait { list, ask } => {
                let key = (client, list);
                let (Some((c, x)), Some(g)) = (self.contexts.get_mut(&key), self.gl.as_ref()) else { return Some(Answer::Failed) };
                if self.current != Some(key) {
                    self.device.make_current(c);
                    self.current = Some(key);
                }
                Some(match ask {
                    Ask::Raster => Answer::Pixels(x.readback(g)),
                    Ask::Query(q) => Answer::Result(x.query(g, &q)),
                })
            }
        }
    }

    /// `client` went away: its contexts go too.
    fn gone(&mut self, client: u32) {
        let gone: Vec<(u32, u64)> = self.contexts.keys().filter(|(c, _)| *c == client).copied().collect();
        for k in gone {
            if let Some((c, _)) = self.contexts.remove(&k) {
                self.device.destroy(c);
            }
        }
        self.current = None;
    }
}
