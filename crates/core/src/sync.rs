//! Peer-to-peer sync between one person's own devices.
//!
//! Devices find each other on the local network with mDNS and connect over
//! iroh's encrypted QUIC connections, dialing each other by public key. Only
//! devices paired with a short code may connect. Setting a relay server lets
//! paired devices also reach each other across the internet; without one,
//! nothing leaves the local network.
//!
//! A session reconciles by comparing 256 bucket digests, then the versions of
//! documents in differing buckets, and then each side sends the changes the
//! other lacks. Afterwards new changes are pushed as they happen, and the
//! digests are compared again every minute to heal anything missed.
use crate::crdt;
use crate::storage::{self, Command, DocChange, DocVersion, Origin, RemoteDoc};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, Watcher,
    endpoint::{Connection, PortmapperConfig, RecvStream, SendStream, presets},
    endpoint_info::UserData,
};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use n0_future::task::AbortOnDropHandle;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub type Result<T> = crdt::Result<T>;

pub const SYNC_ALPN: &[u8] = b"notebook/sync/1";
pub const PAIR_ALPN: &[u8] = b"notebook/pair/1";
const PROTOCOL: u32 = 1;
const SERVICE: &str = "notebook-sync";
/// Advertised while a device shows a pairing code.
const PAIRING: &str = "pairing";
const MAX_FRAME: usize = 64 << 20;
const RESYNC: Duration = Duration::from_secs(60);
const REDIAL: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
/// A second connection from a device within this long of the first means both
/// dialed at once. Later, it means the device lost the first, usually because
/// its app restarted.
const SIMULTANEOUS_DIAL: Duration = Duration::from_secs(5);
const PAIRING_TIME: Duration = Duration::from_secs(300);
const JOIN_TIME: Duration = Duration::from_secs(30);
const PAIRING_ATTEMPTS: u32 = 3;
const BATCH_DOCS: usize = 256;
const BATCH_BYTES: usize = 4 << 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub device_name: String,
    /// This device's private key, in hex. It lives outside the notes database,
    /// so copying that database to another computer never shares it.
    pub secret_key: String,
    pub relay_url: Option<String>,
    pub devices: Vec<PairedDevice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub relay_url: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temporary, path)
    }

    /// This device's key, and whether it was just created and needs saving.
    fn key(&mut self) -> (SecretKey, bool) {
        if let Ok(key) = self.secret_key.parse() {
            return (key, false);
        }
        let key = SecretKey::generate();
        self.secret_key = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        (key, true)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub enabled: bool,
    /// Paused by the app, such as while it's in the background. Sync stays
    /// enabled, and resumes where it left off.
    pub suspended: bool,
    pub device_name: String,
    pub relay_url: Option<String>,
    pub relay_connected: bool,
    pub devices: Vec<DeviceStatus>,
    pub pairing: Pairing,
    pub problem: Option<String>,
    pub addr: Option<EndpointAddr>,
}

impl Status {
    pub fn running(&self) -> bool {
        self.addr.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub connected: bool,
    /// Milliseconds since the Unix epoch.
    pub last_synced: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Pairing {
    #[default]
    Idle,
    Showing(String),
    Searching,
    Paired(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Control {
    SetEnabled(bool),
    SetDeviceName(String),
    SetRelay(Option<String>),
    StartPairing,
    JoinPairing(String),
    CancelPairing,
    Forget(String),
    /// Adds an address for a device, as if found on the local network.
    Introduce(EndpointAddr),
    /// Closes or reopens every connection without changing the settings.
    Suspend(bool),
    /// The device's network changed. iroh can't notice this by itself on
    /// Android.
    NetworkChanged,
    Shutdown,
}

pub struct SyncHandle {
    control: UnboundedSender<Control>,
}

impl SyncHandle {
    pub fn send(&self, control: Control) {
        let _ = self.control.send(control);
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub mdns: bool,
    /// Tests turn it off to send everything through the relay.
    pub direct: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mdns: true,
            direct: true,
        }
    }
}

/// Starts the sync thread. It only touches the network while sync is enabled.
///
/// `default_device_name` names this device until someone picks a name.
/// `on_status` runs on the sync thread with each new status, starting with
/// the first.
pub fn spawn(
    config_path: PathBuf,
    db_path: PathBuf,
    default_device_name: String,
    storage: std::sync::mpsc::Sender<Command>,
    on_status: impl Fn(Status) + Send + 'static,
    options: Options,
) -> SyncHandle {
    let (control, control_rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("notebook-sync".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("could not start the sync runtime");
            runtime.block_on(async move {
                let (internal, internal_rx) = mpsc::unbounded_channel();
                let (outbox, changes) = mpsc::unbounded_channel();
                let mut config = Config::load(&config_path);
                let unnamed = config.device_name.trim().is_empty();
                if unnamed {
                    config.device_name = default_device_name;
                }
                let mut sync = Sync {
                    allowed: Arc::new(RwLock::new(
                        config
                            .devices
                            .iter()
                            .filter_map(|d| d.id.parse().ok())
                            .collect(),
                    )),
                    config,
                    config_path,
                    reader: Reader::new(db_path),
                    storage,
                    on_status: Box::new(on_status),
                    last_status: None,
                    internal,
                    outbox,
                    options,
                    node: None,
                    sessions: HashMap::new(),
                    dialing: HashSet::new(),
                    discovered: Arc::new(Mutex::new(HashMap::new())),
                    offer: Arc::new(Mutex::new(None)),
                    last_synced: HashMap::new(),
                    pairing: Pairing::Idle,
                    joining: None,
                    problem: None,
                    suspended: false,
                };
                if unnamed {
                    sync.save_config();
                }
                sync.run(control_rx, internal_rx, changes).await;
            });
        })
        .expect("could not start the sync thread");
    SyncHandle { control }
}

#[derive(Debug, Serialize, Deserialize)]
enum Msg {
    Hello {
        protocol: u32,
        name: String,
        relay_url: Option<String>,
    },
    Digest(Vec<[u8; 32]>),
    Versions {
        buckets: Vec<u8>,
        docs: Vec<DocVersion>,
    },
    Docs(Vec<RemoteDoc>),
}

#[derive(Debug, Serialize, Deserialize)]
enum PairMsg {
    Spake(Vec<u8>),
    Confirm([u8; 32]),
    Welcome {
        name: String,
        relay_url: Option<String>,
    },
    WrongCode,
}

async fn write_frame<T: Serialize>(send: &mut SendStream, message: &T) -> Result<()> {
    let bytes = postcard::to_stdvec(message)?;
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(recv: &mut RecvStream) -> Result<T> {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err("A device sent a message that is too large".into());
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes).await?;
    Ok(postcard::from_bytes(&bytes)?)
}

#[derive(Clone)]
struct Reader {
    path: PathBuf,
    connection: Arc<Mutex<Option<rusqlite::Connection>>>,
}

impl Reader {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            connection: Arc::new(Mutex::new(None)),
        }
    }

    async fn with<T: Send + 'static>(
        &self,
        work: impl FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let reader = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = reader.connection.lock().unwrap();
            if connection.is_none() {
                *connection = Some(storage::open_reader(&reader.path)?);
            }
            work(connection.as_ref().expect("opened above"))
        })
        .await?
    }
}

#[derive(Clone)]
struct Offer {
    code: String,
    until: Instant,
    attempts: u32,
    name: String,
    relay_url: Option<String>,
}

enum Internal {
    Connected {
        conn: Connection,
        dialer: EndpointId,
    },
    DialFailed(EndpointId),
    Closed {
        peer: EndpointId,
        id: usize,
        error: Option<String>,
    },
    Hello {
        peer: EndpointId,
        name: String,
        relay_url: Option<String>,
    },
    Synced(EndpointId),
    /// Devices appeared or left, or the relay connection changed.
    NetworkChanged,
    Paired(PairedDevice),
    PairingFailed(String),
}

struct Session {
    outgoing: UnboundedSender<Msg>,
    id: usize,
    dialer: EndpointId,
    started: Instant,
    _task: AbortOnDropHandle<()>,
}

struct Node {
    endpoint: Endpoint,
    tasks: Vec<AbortOnDropHandle<()>>,
}

struct Sync {
    config: Config,
    config_path: PathBuf,
    reader: Reader,
    storage: std::sync::mpsc::Sender<Command>,
    on_status: Box<dyn Fn(Status) + Send>,
    last_status: Option<Status>,
    internal: UnboundedSender<Internal>,
    outbox: storage::Outbox,
    options: Options,
    node: Option<Node>,
    allowed: Arc<RwLock<HashSet<EndpointId>>>,
    sessions: HashMap<EndpointId, Session>,
    dialing: HashSet<EndpointId>,
    /// Devices seen on the network, and whether each is showing a pairing code.
    discovered: Arc<Mutex<HashMap<EndpointId, (EndpointAddr, bool)>>>,
    offer: Arc<Mutex<Option<Offer>>>,
    last_synced: HashMap<EndpointId, i64>,
    pairing: Pairing,
    joining: Option<AbortOnDropHandle<()>>,
    problem: Option<String>,
    suspended: bool,
}

impl Sync {
    async fn run(
        &mut self,
        mut control: UnboundedReceiver<Control>,
        mut internal: UnboundedReceiver<Internal>,
        mut changes: UnboundedReceiver<DocChange>,
    ) {
        if self.config.enabled {
            self.start().await;
        }
        self.publish();
        let mut tick = tokio::time::interval(REDIAL);
        loop {
            tokio::select! {
                message = control.recv() => match message {
                    None | Some(Control::Shutdown) => break,
                    Some(message) => self.control(message).await,
                },
                Some(event) = internal.recv() => self.internal(event),
                Some(change) = changes.recv() => {
                    let mut batch = vec![change];
                    while let Ok(change) = changes.try_recv() {
                        batch.push(change);
                    }
                    self.forward(batch);
                }
                _ = tick.tick() => self.tick(),
            }
            self.publish();
        }
        self.stop().await;
    }

    fn save_config(&mut self) {
        if let Err(error) = self.config.save(&self.config_path) {
            self.problem = Some(format!("Could not save sync settings: {error}"));
        }
    }

    async fn start(&mut self) {
        match self.bind().await {
            Ok(node) => {
                self.node = Some(node);
                self.problem = None;
                self.tick();
            }
            Err(error) => self.problem = Some(format!("Sync could not start: {error}")),
        }
    }

    async fn bind(&mut self) -> Result<Node> {
        let (key, created) = self.config.key();
        if created {
            self.save_config();
        }
        // Without a relay, sync stays on this network: no relay, and no asking
        // the router to open a port to the internet.
        let (relay, portmapper) = match &self.config.relay_url {
            Some(url) => (
                RelayMode::custom([url.parse::<RelayUrl>()?]),
                PortmapperConfig::default(),
            ),
            None => (RelayMode::Disabled, PortmapperConfig::Disabled),
        };
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .alpns(vec![SYNC_ALPN.to_vec(), PAIR_ALPN.to_vec()])
            .relay_mode(relay)
            .portmapper_config(portmapper);
        if !self.options.direct {
            builder = builder.clear_ip_transports();
        }
        let endpoint = builder.bind().await?;
        let mut tasks = Vec::new();
        // Paired devices meet at the relay, so retry them once it's reachable.
        let mut relays = endpoint.home_relay_status();
        let internal = self.internal.clone();
        tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
            while relays.updated().await.is_ok() {
                let _ = internal.send(Internal::NetworkChanged);
            }
        })));
        if self.options.mdns {
            let mdns = MdnsAddressLookup::builder()
                .service_name(SERVICE)
                .build(endpoint.id())?;
            endpoint.address_lookup()?.add(mdns.clone());
            let (discovered, internal) = (self.discovered.clone(), self.internal.clone());
            tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
                use n0_future::StreamExt;
                let mut events = mdns.subscribe().await;
                while let Some(event) = events.next().await {
                    match event {
                        DiscoveryEvent::Discovered { endpoint_info, .. } => {
                            let pairing = endpoint_info
                                .user_data()
                                .is_some_and(|data| data.to_string() == PAIRING);
                            let addr = EndpointAddr::from_parts(
                                endpoint_info.endpoint_id,
                                endpoint_info.addrs().cloned(),
                            );
                            discovered
                                .lock()
                                .unwrap()
                                .insert(endpoint_info.endpoint_id, (addr, pairing));
                        }
                        DiscoveryEvent::Expired { endpoint_id } => {
                            discovered.lock().unwrap().remove(&endpoint_id);
                        }
                        _ => continue,
                    }
                    let _ = internal.send(Internal::NetworkChanged);
                }
            })));
        }
        let (me, allowed, offer, internal) = (
            endpoint.id(),
            self.allowed.clone(),
            self.offer.clone(),
            self.internal.clone(),
        );
        let accepting = endpoint.clone();
        tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let (allowed, offer, internal) = (allowed.clone(), offer.clone(), internal.clone());
                tokio::spawn(async move {
                    let Ok(accepting) = incoming.accept() else {
                        return;
                    };
                    let Ok(conn) = accepting.await else {
                        return;
                    };
                    let peer = conn.remote_id();
                    if conn.alpn() == SYNC_ALPN {
                        if allowed.read().unwrap().contains(&peer) {
                            let _ = internal.send(Internal::Connected { conn, dialer: peer });
                        } else {
                            conn.close(1u32.into(), b"not paired");
                        }
                    } else if conn.alpn() == PAIR_ALPN {
                        invite(conn, me, offer, internal).await;
                    }
                });
            }
        })));
        let _ = self
            .storage
            .send(Command::Attach(Some(self.outbox.clone())));
        Ok(Node { endpoint, tasks })
    }

    async fn stop(&mut self) {
        let _ = self.storage.send(Command::Attach(None));
        self.sessions.clear();
        self.cancel_pairing();
        self.dialing.clear();
        self.discovered.lock().unwrap().clear();
        if matches!(self.pairing, Pairing::Showing(_) | Pairing::Searching) {
            self.pairing = Pairing::Idle;
        }
        if let Some(Node { endpoint, tasks }) = self.node.take() {
            drop(tasks);
            endpoint.close().await;
        }
    }

    async fn restart(&mut self) {
        self.stop().await;
        if self.config.enabled && !self.suspended {
            self.start().await;
        }
    }

    async fn control(&mut self, message: Control) {
        match message {
            Control::SetEnabled(enabled) => {
                self.config.enabled = enabled;
                self.save_config();
                self.problem = None;
                self.restart().await;
            }
            Control::SetDeviceName(name) => {
                let name = name.trim();
                if !name.is_empty() && name.chars().count() <= 60 && name != self.config.device_name
                {
                    self.config.device_name = name.into();
                    self.save_config();
                }
            }
            Control::SetRelay(url) => {
                let url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
                if let Some(url) = &url
                    && let Err(error) = url.parse::<RelayUrl>()
                {
                    self.problem = Some(format!("That relay address isn't valid: {error}"));
                    return;
                }
                self.config.relay_url = url;
                self.save_config();
                self.problem = None;
                self.restart().await;
            }
            Control::StartPairing => {
                let Some(node) = &self.node else {
                    self.pairing = Pairing::Failed("Turn on sync first".into());
                    return;
                };
                let code = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
                *self.offer.lock().unwrap() = Some(Offer {
                    code: code.clone(),
                    until: Instant::now() + PAIRING_TIME,
                    attempts: 0,
                    name: self.config.device_name.clone(),
                    relay_url: self.config.relay_url.clone(),
                });
                node.endpoint
                    .set_user_data_for_address_lookup(UserData::try_from(PAIRING.to_string()).ok());
                self.pairing = Pairing::Showing(format!("{} {}", &code[..3], &code[3..]));
            }
            Control::JoinPairing(code) => {
                let code: String = code.chars().filter(char::is_ascii_digit).collect();
                let Some(node) = &self.node else {
                    self.pairing = Pairing::Failed("Turn on sync first".into());
                    return;
                };
                if code.len() != 6 {
                    self.pairing = Pairing::Failed("Enter the six-digit code".into());
                    return;
                }
                self.pairing = Pairing::Searching;
                self.joining = Some(AbortOnDropHandle::new(tokio::spawn(join(
                    node.endpoint.clone(),
                    code,
                    self.config.device_name.clone(),
                    self.config.relay_url.clone(),
                    self.discovered.clone(),
                    self.internal.clone(),
                ))));
            }
            Control::CancelPairing => {
                self.cancel_pairing();
                self.pairing = Pairing::Idle;
            }
            Control::Forget(id) => {
                self.config.devices.retain(|d| d.id != id);
                self.save_config();
                if let Ok(peer) = id.parse::<EndpointId>() {
                    self.allowed.write().unwrap().remove(&peer);
                    self.sessions.remove(&peer);
                    self.last_synced.remove(&peer);
                }
            }
            Control::Introduce(addr) => {
                self.discovered
                    .lock()
                    .unwrap()
                    .insert(addr.id, (addr, true));
                self.tick();
            }
            Control::Suspend(suspend) => {
                if suspend != self.suspended {
                    self.suspended = suspend;
                    // Each new session starts by comparing digests, which
                    // catches up on everything changed while suspended.
                    self.restart().await;
                }
            }
            Control::NetworkChanged => {
                if let Some(node) = &self.node {
                    node.endpoint.network_change().await;
                }
                self.tick();
            }
            Control::Shutdown => {}
        }
    }

    fn cancel_pairing(&mut self) {
        *self.offer.lock().unwrap() = None;
        self.joining = None;
        if let Some(node) = &self.node {
            node.endpoint.set_user_data_for_address_lookup(None);
        }
    }

    fn internal(&mut self, event: Internal) {
        match event {
            Internal::Connected { conn, dialer } => {
                let peer = conn.remote_id();
                self.dialing.remove(&peer);
                self.adopt(conn, dialer);
            }
            Internal::DialFailed(peer) => {
                self.dialing.remove(&peer);
            }
            Internal::Closed { peer, id, error } => {
                if self.sessions.get(&peer).is_some_and(|s| s.id == id) {
                    self.sessions.remove(&peer);
                    if let Some(error) = error {
                        self.problem = Some(error);
                    }
                }
            }
            Internal::Hello {
                peer,
                name,
                relay_url,
            } => {
                let id = peer.to_string();
                if let Some(device) = self.config.devices.iter_mut().find(|d| d.id == id)
                    && (device.name != name || device.relay_url != relay_url)
                {
                    device.name = name;
                    device.relay_url = relay_url;
                    self.save_config();
                }
            }
            Internal::Synced(peer) => {
                self.last_synced.insert(peer, crate::model::now_millis());
                self.problem = None;
            }
            Internal::NetworkChanged => self.tick(),
            Internal::Paired(device) => {
                self.cancel_pairing();
                if let Ok(peer) = device.id.parse::<EndpointId>() {
                    self.allowed.write().unwrap().insert(peer);
                }
                self.pairing = Pairing::Paired(device.name.clone());
                self.config.devices.retain(|d| d.id != device.id);
                self.config.devices.push(device);
                self.save_config();
                self.tick();
            }
            Internal::PairingFailed(message) => {
                self.cancel_pairing();
                self.pairing = Pairing::Failed(message);
            }
        }
    }

    /// When both devices dialed at once, each keeps the connection dialed by
    /// the smaller ID, so they agree. A connection from a device that already
    /// has an older session replaces it: the device lost that session, and
    /// otherwise would wait for it to time out.
    fn adopt(&mut self, conn: Connection, dialer: EndpointId) {
        let Some(node) = &self.node else {
            return;
        };
        let (me, peer) = (node.endpoint.id(), conn.remote_id());
        if let Some(existing) = self.sessions.get(&peer)
            && existing.started.elapsed() < SIMULTANEOUS_DIAL
        {
            let preferred = me.min(peer);
            if existing.dialer == preferred || dialer != preferred {
                conn.close(0u32.into(), b"duplicate");
                return;
            }
        }
        let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
        let id = conn.stable_id();
        let hello = Msg::Hello {
            protocol: PROTOCOL,
            name: self.config.device_name.clone(),
            relay_url: self.config.relay_url.clone(),
        };
        let (reader, storage, internal) = (
            self.reader.clone(),
            self.storage.clone(),
            self.internal.clone(),
        );
        let task = tokio::spawn(async move {
            let result = session(
                conn,
                dialer == me,
                hello,
                reader,
                storage,
                outgoing_rx,
                internal.clone(),
            )
            .await;
            let _ = internal.send(Internal::Closed {
                peer,
                id,
                error: result
                    .err()
                    .and_then(|e| e.downcast::<Incompatible>().ok())
                    .map(|e| e.0),
            });
        });
        self.sessions.insert(
            peer,
            Session {
                outgoing,
                id,
                dialer,
                started: Instant::now(),
                _task: AbortOnDropHandle::new(task),
            },
        );
    }

    fn forward(&mut self, changes: Vec<DocChange>) {
        for (peer, session) in &self.sessions {
            let peer = peer.to_string();
            let docs: Vec<RemoteDoc> = changes
                .iter()
                .filter(|c| !matches!(&c.origin, Origin::Remote(from) if *from == peer))
                .map(|c| RemoteDoc {
                    id: c.id.clone(),
                    kind: c.kind,
                    bytes: c.delta.clone(),
                })
                .collect();
            for batch in batches(docs) {
                let _ = session.outgoing.send(Msg::Docs(batch));
            }
        }
    }

    fn tick(&mut self) {
        let expired = self
            .offer
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|offer| offer.until <= Instant::now());
        if expired {
            self.cancel_pairing();
            self.pairing = Pairing::Failed("The pairing code expired".into());
        }
        let Some(node) = &self.node else {
            return;
        };
        for device in &self.config.devices {
            let Ok(peer) = device.id.parse::<EndpointId>() else {
                continue;
            };
            if self.sessions.contains_key(&peer) || !self.dialing.insert(peer) {
                continue;
            }
            let mut addr = self
                .discovered
                .lock()
                .unwrap()
                .get(&peer)
                .map(|(addr, _)| addr.clone())
                .unwrap_or_else(|| EndpointAddr::new(peer));
            if self.config.relay_url.is_some()
                && let Some(url) = device.relay_url.as_ref().and_then(|u| u.parse().ok())
            {
                addr = addr.with_relay_url(url);
            }
            let (endpoint, internal, me) = (
                node.endpoint.clone(),
                self.internal.clone(),
                node.endpoint.id(),
            );
            tokio::spawn(async move {
                match tokio::time::timeout(DIAL_TIMEOUT, endpoint.connect(addr, SYNC_ALPN)).await {
                    Ok(Ok(conn)) => {
                        let _ = internal.send(Internal::Connected { conn, dialer: me });
                    }
                    _ => {
                        let _ = internal.send(Internal::DialFailed(peer));
                    }
                }
            });
        }
    }

    fn publish(&mut self) {
        let status = Status {
            enabled: self.config.enabled,
            suspended: self.suspended,
            device_name: self.config.device_name.clone(),
            relay_url: self.config.relay_url.clone(),
            relay_connected: self.node.as_ref().is_some_and(|node| {
                node.endpoint
                    .home_relay_status()
                    .get()
                    .iter()
                    .any(|relay| relay.is_connected())
            }),
            devices: self
                .config
                .devices
                .iter()
                .map(|device| {
                    let peer = device.id.parse::<EndpointId>().ok();
                    DeviceStatus {
                        id: device.id.clone(),
                        name: device.name.clone(),
                        connected: peer.is_some_and(|p| self.sessions.contains_key(&p)),
                        last_synced: peer.and_then(|p| self.last_synced.get(&p).copied()),
                    }
                })
                .collect(),
            pairing: self.pairing.clone(),
            problem: self.problem.clone(),
            addr: self.node.as_ref().map(|node| {
                let mut addr = node.endpoint.addr();
                // Also reachable on this computer, for a second instance.
                for socket in node.endpoint.bound_sockets() {
                    let ip: std::net::IpAddr = match socket.ip() {
                        std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                            std::net::Ipv4Addr::LOCALHOST.into()
                        }
                        std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                            std::net::Ipv6Addr::LOCALHOST.into()
                        }
                        ip => ip,
                    };
                    addr = addr.with_ip_addr((ip, socket.port()).into());
                }
                addr
            }),
        };
        if self.last_status.as_ref() != Some(&status) {
            (self.on_status)(status.clone());
            self.last_status = Some(status);
        }
    }
}

fn batches(docs: Vec<RemoteDoc>) -> Vec<Vec<RemoteDoc>> {
    let mut batches = vec![];
    let mut batch = vec![];
    let mut bytes = 0;
    for doc in docs {
        if !batch.is_empty() && (batch.len() >= BATCH_DOCS || bytes + doc.bytes.len() > BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        bytes += doc.bytes.len();
        batch.push(doc);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// The one session error shown to the person using the app.
#[derive(Debug)]
struct Incompatible(String);

impl std::fmt::Display for Incompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Incompatible {}

async fn session(
    conn: Connection,
    dialed: bool,
    hello: Msg,
    reader: Reader,
    storage: std::sync::mpsc::Sender<Command>,
    mut outgoing: UnboundedReceiver<Msg>,
    internal: UnboundedSender<Internal>,
) -> Result<()> {
    let peer = conn.remote_id();
    let (mut send, mut recv) = if dialed {
        conn.open_bi().await?
    } else {
        conn.accept_bi().await?
    };
    write_frame(&mut send, &hello).await?;
    write_frame(&mut send, &Msg::Digest(reader.with(storage::digest).await?)).await?;
    // Reading isn't cancellation-safe, so it gets its own task.
    let (incoming_tx, mut incoming) = mpsc::unbounded_channel();
    let reading = tokio::spawn(async move {
        loop {
            let message = read_frame::<Msg>(&mut recv).await;
            let failed = message.is_err();
            if incoming_tx.send(message).is_err() || failed {
                break;
            }
        }
    });
    let _stop_reading = AbortOnDropHandle::new(reading);
    let mut resync = tokio::time::interval(RESYNC);
    resync.tick().await;
    loop {
        tokio::select! {
            message = incoming.recv() => {
                let Some(message) = message else { return Ok(()) };
                match message? {
                    Msg::Hello { protocol, name, relay_url } => {
                        if protocol != PROTOCOL {
                            return Err(Incompatible(format!("{name} runs a different version of Notebook's sync; update both devices")).into());
                        }
                        let _ = internal.send(Internal::Hello { peer, name, relay_url });
                    }
                    Msg::Digest(theirs) => {
                        let ours = reader.with(storage::digest).await?;
                        let buckets = crdt::differing_buckets(&ours, &theirs);
                        if buckets.is_empty() {
                            let _ = internal.send(Internal::Synced(peer));
                        } else {
                            let wanted = buckets.clone();
                            let docs = reader.with(move |db| storage::versions(db, &wanted)).await?;
                            write_frame(&mut send, &Msg::Versions { buckets, docs }).await?;
                        }
                    }
                    Msg::Versions { buckets, docs } => {
                        let missing = reader
                            .with(move |db| storage::missing(db, &buckets, &docs))
                            .await?;
                        for batch in batches(missing) {
                            write_frame(&mut send, &Msg::Docs(batch)).await?;
                        }
                        let _ = internal.send(Internal::Synced(peer));
                    }
                    Msg::Docs(docs) => {
                        if !docs.is_empty() {
                            let from = peer.to_string();
                            if storage.send(Command::Remote { docs, from }).is_err() {
                                return Ok(());
                            }
                        }
                        let _ = internal.send(Internal::Synced(peer));
                    }
                }
            }
            Some(message) = outgoing.recv() => write_frame(&mut send, &message).await?,
            _ = resync.tick() => {
                write_frame(&mut send, &Msg::Digest(reader.with(storage::digest).await?)).await?;
            }
        }
    }
}

/// Derives a confirmation both sides can only compute with the same code.
fn confirmation(key: &[u8], role: &str, joiner: EndpointId, inviter: EndpointId) -> [u8; 32] {
    let key = blake3::derive_key("notebook pairing confirmation v1", key);
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(role.as_bytes());
    hasher.update(joiner.as_bytes());
    hasher.update(inviter.as_bytes());
    *hasher.finalize().as_bytes()
}

fn spake_identities(
    joiner: EndpointId,
    inviter: EndpointId,
) -> (spake2::Identity, spake2::Identity) {
    (
        spake2::Identity::new(joiner.as_bytes()),
        spake2::Identity::new(inviter.as_bytes()),
    )
}

/// Answers a device that entered a code. A PAKE means a wrong guess teaches
/// the guesser nothing, and each code allows only a few guesses.
async fn invite(
    conn: Connection,
    me: EndpointId,
    offer: Arc<Mutex<Option<Offer>>>,
    internal: UnboundedSender<Internal>,
) {
    let Some(current) = offer
        .lock()
        .unwrap()
        .clone()
        .filter(|o| o.until > Instant::now())
    else {
        conn.close(1u32.into(), b"not pairing");
        return;
    };
    let joiner = conn.remote_id();
    let outcome = async {
        let (mut send, mut recv) = conn.accept_bi().await?;
        let PairMsg::Spake(theirs) = read_frame(&mut recv).await? else {
            return Err("unexpected message".into());
        };
        let (a, b) = spake_identities(joiner, me);
        let (state, ours) = spake2::Spake2::<spake2::Ed25519Group>::start_b(
            &spake2::Password::new(current.code.as_bytes()),
            &a,
            &b,
        );
        write_frame(&mut send, &PairMsg::Spake(ours)).await?;
        let key = state.finish(&theirs).map_err(|_| "pairing failed")?;
        let PairMsg::Confirm(proof) = read_frame(&mut recv).await? else {
            return Err("unexpected message".into());
        };
        if blake3::Hash::from(proof) != blake3::Hash::from(confirmation(&key, "joiner", joiner, me))
        {
            let _ = write_frame(&mut send, &PairMsg::WrongCode).await;
            let _ = send.finish();
            return Ok(None);
        }
        write_frame(
            &mut send,
            &PairMsg::Confirm(confirmation(&key, "inviter", joiner, me)),
        )
        .await?;
        write_frame(
            &mut send,
            &PairMsg::Welcome {
                name: current.name.clone(),
                relay_url: current.relay_url.clone(),
            },
        )
        .await?;
        let PairMsg::Welcome { name, relay_url } = read_frame(&mut recv).await? else {
            return Err("unexpected message".into());
        };
        let _ = send.finish();
        Ok::<_, crdt::Error>(Some(PairedDevice {
            id: joiner.to_string(),
            name,
            relay_url,
        }))
    }
    .await;
    match outcome {
        Ok(Some(device)) => {
            // The joiner waits for this close to know its welcome arrived.
            conn.close(0u32.into(), b"paired");
            let _ = internal.send(Internal::Paired(device));
            return;
        }
        Ok(None) => {
            let mut offer = offer.lock().unwrap();
            if let Some(offer) = offer.as_mut() {
                offer.attempts += 1;
                if offer.attempts >= PAIRING_ATTEMPTS {
                    let _ = internal.send(Internal::PairingFailed(
                        "Too many wrong codes were entered; start pairing again".into(),
                    ));
                }
            }
        }
        Err(_) => {}
    }
    // Let the joiner read the verdict and hang up first.
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
}

async fn join(
    endpoint: Endpoint,
    code: String,
    name: String,
    relay_url: Option<String>,
    discovered: Arc<Mutex<HashMap<EndpointId, (EndpointAddr, bool)>>>,
    internal: UnboundedSender<Internal>,
) {
    let deadline = Instant::now() + JOIN_TIME;
    let mut tried = HashSet::new();
    let mut wrong = false;
    while Instant::now() < deadline {
        let candidates: Vec<EndpointAddr> = discovered
            .lock()
            .unwrap()
            .values()
            .filter(|(addr, pairing)| *pairing && !tried.contains(&addr.id))
            .map(|(addr, _)| addr.clone())
            .collect();
        for addr in candidates {
            let id = addr.id;
            let attempt = tokio::time::timeout(
                DIAL_TIMEOUT,
                pair_with(&endpoint, addr, &code, &name, &relay_url),
            )
            .await;
            match attempt {
                Ok(Ok(Some(device))) => {
                    let _ = internal.send(Internal::Paired(device));
                    return;
                }
                Ok(Ok(None)) => {
                    tried.insert(id);
                    wrong = true;
                }
                // It may not be reachable yet, such as before it reaches its relay.
                _ => {}
            }
        }
        if wrong {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = internal.send(Internal::PairingFailed(if wrong {
        "That code didn't match. Check the code on your other device".into()
    } else {
        "No device showing a pairing code was found on this network".into()
    }));
}

async fn pair_with(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    code: &str,
    name: &str,
    relay_url: &Option<String>,
) -> Result<Option<PairedDevice>> {
    let conn = endpoint.connect(addr, PAIR_ALPN).await?;
    let (me, inviter) = (endpoint.id(), conn.remote_id());
    let (mut send, mut recv) = conn.open_bi().await?;
    let (a, b) = spake_identities(me, inviter);
    let (state, ours) = spake2::Spake2::<spake2::Ed25519Group>::start_a(
        &spake2::Password::new(code.as_bytes()),
        &a,
        &b,
    );
    write_frame(&mut send, &PairMsg::Spake(ours)).await?;
    let PairMsg::Spake(theirs) = read_frame(&mut recv).await? else {
        return Err("unexpected message".into());
    };
    let key = state.finish(&theirs).map_err(|_| "pairing failed")?;
    write_frame(
        &mut send,
        &PairMsg::Confirm(confirmation(&key, "joiner", me, inviter)),
    )
    .await?;
    let proof = match read_frame(&mut recv).await? {
        PairMsg::Confirm(proof) => proof,
        PairMsg::WrongCode => return Ok(None),
        _ => return Err("unexpected message".into()),
    };
    if blake3::Hash::from(proof) != blake3::Hash::from(confirmation(&key, "inviter", me, inviter)) {
        return Ok(None);
    }
    let PairMsg::Welcome {
        name: their_name,
        relay_url: their_relay,
    } = read_frame(&mut recv).await?
    else {
        return Err("unexpected message".into());
    };
    write_frame(
        &mut send,
        &PairMsg::Welcome {
            name: name.into(),
            relay_url: relay_url.clone(),
        },
    )
    .await?;
    send.finish()?;
    // Let the inviter read the welcome before the connection goes away.
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    Ok(Some(PairedDevice {
        id: inviter.to_string(),
        name: their_name,
        relay_url: their_relay,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_bound_count_and_size() {
        let doc = |len| RemoteDoc {
            id: "x".into(),
            kind: crdt::Kind::Note,
            bytes: vec![0; len],
        };
        let sizes: Vec<usize> = batches((0..600).map(|_| doc(10)).collect())
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(sizes, [256, 256, 88]);
        let big = batches(vec![doc(BATCH_BYTES), doc(1), doc(BATCH_BYTES)]);
        assert_eq!(big.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 1]);
        assert!(batches(vec![]).is_empty());
    }

    #[test]
    fn config_keeps_its_key_and_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notebook/sync.json");
        let mut config = Config::load(&path);
        assert!(config.device_name.is_empty());
        let (key, created) = config.key();
        assert!(created);
        config.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let mut reloaded = Config::load(&path);
        let (again, created) = reloaded.key();
        assert!(!created);
        assert_eq!(again.public(), key.public());
    }

    #[test]
    fn confirmations_bind_the_code_and_both_devices() {
        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let key = [7u8; 32];
        assert_eq!(
            confirmation(&key, "joiner", a, b),
            confirmation(&key, "joiner", a, b)
        );
        assert_ne!(
            confirmation(&key, "joiner", a, b),
            confirmation(&key, "inviter", a, b)
        );
        assert_ne!(
            confirmation(&key, "joiner", a, b),
            confirmation(&key, "joiner", b, a)
        );
        assert_ne!(
            confirmation(&key, "joiner", a, b),
            confirmation(&[8u8; 32], "joiner", a, b)
        );
    }
}
