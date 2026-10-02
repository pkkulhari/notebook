use iroh::{EndpointAddr, SecretKey};
use notebook_core::{
    model::*,
    storage::{Command, Event, Repository, spawn_worker},
    sync::{self, Control, Options, Pairing, Status, SyncHandle},
};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, Sender},
};
use std::time::{Duration, Instant};

struct Device {
    name: String,
    db: PathBuf,
    config: PathBuf,
    options: Options,
    commands: Sender<Command>,
    _events: Receiver<Event>,
    sync: Option<SyncHandle>,
    statuses: Receiver<Status>,
    status: Status,
    _dir: tempfile::TempDir,
}

fn local() -> Options {
    Options {
        mdns: false,
        ..Default::default()
    }
}

impl Device {
    fn new(name: &str) -> Self {
        Self::with(name, local(), None)
    }

    fn with(name: &str, options: Options, relay: Option<&str>) -> Self {
        let mut device = Self::stopped(name, options, relay);
        device.start_sync();
        device
    }

    fn stopped(name: &str, options: Options, relay: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notebook.db");
        let config = dir.path().join("sync.json");
        let mut settings = sync::Config::load(&config);
        settings.relay_url = relay.map(Into::into);
        settings.save(&config).unwrap();
        let (commands, events) = spawn_worker(db.clone());
        // The storage worker opens the database on its first command.
        commands.send(Command::Initialize).unwrap();
        Device {
            name: name.into(),
            db,
            config,
            options,
            commands,
            _events: events,
            sync: None,
            statuses: mpsc::channel().1,
            status: Status::default(),
            _dir: dir,
        }
    }

    fn start_sync(&mut self) {
        let (statuses, receiver) = mpsc::channel();
        self.sync = Some(sync::spawn(
            self.config.clone(),
            self.db.clone(),
            self.name.clone(),
            self.commands.clone(),
            move |status| {
                let _ = statuses.send(status);
            },
            self.options,
        ));
        self.statuses = receiver;
    }

    fn stop_sync(&mut self) {
        self.sync = None;
        self.status = Status::default();
    }

    fn sync(&self) -> &SyncHandle {
        self.sync.as_ref().expect("sync is running")
    }

    /// Saves settings as if this device had paired with `other` earlier.
    fn remember(&self, key: &SecretKey, other: &Device, other_key: &SecretKey) {
        let mut settings = sync::Config::load(&self.config);
        settings.enabled = true;
        settings.secret_key = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        settings.devices = vec![sync::PairedDevice {
            id: other_key.public().to_string(),
            name: other.name.clone(),
            relay_url: None,
        }];
        settings.save(&self.config).unwrap();
    }

    fn refresh(&mut self) -> &Status {
        if let Some(status) = self.statuses.try_iter().last() {
            self.status = status;
        }
        &self.status
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.refresh();
            if done(&self.status) {
                return self.status.clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.status
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn show_code(&mut self) -> String {
        self.sync().send(Control::StartPairing);
        let Pairing::Showing(code) = self
            .wait("a pairing code", |s| {
                matches!(s.pairing, Pairing::Showing(_))
            })
            .pairing
        else {
            unreachable!()
        };
        code
    }

    fn body(&self, id: &str) -> Option<String> {
        Repository::open(&self.db)
            .unwrap()
            .load(id)
            .unwrap()
            .map(|note| note.body)
    }

    fn wait_body(&self, id: &str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.body(id).as_deref() != Some(expected) {
            assert!(
                Instant::now() < deadline,
                "note never became {expected:?}; it is {:?}",
                self.body(id)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn save(&self, id: &str, body: &str) {
        self.commands
            .send(Command::Save {
                id: id.into(),
                body: body.into(),
                sequence: 1,
            })
            .unwrap();
    }

    fn create(&self, body: &str) -> String {
        let note = Note::blank();
        self.commands.send(Command::Create(note.clone())).unwrap();
        self.save(&note.id, body);
        note.id
    }
}

/// Bound to an address other devices on this computer can reach.
fn reachable(status: &Status) -> bool {
    status
        .addr
        .as_ref()
        .is_some_and(|a| a.ip_addrs().next().is_some())
}

fn connected(status: &Status) -> bool {
    status.devices.iter().any(|d| d.connected)
}

/// Two devices that paired earlier, with sync on but not started.
fn paired() -> (Device, Device) {
    let desktop = Device::stopped("Desktop", local(), None);
    let laptop = Device::stopped("Laptop", local(), None);
    let mut keys = [SecretKey::generate(), SecretKey::generate()];
    // The laptop's ID is the larger. Of two connections that arrive together,
    // devices keep the one the smaller ID dialed, so a test of the laptop
    // reconnecting can't pass by that rule alone.
    keys.sort_by_key(|key| key.public());
    let [desktop_key, laptop_key] = keys;
    desktop.remember(&desktop_key, &laptop, &laptop_key);
    laptop.remember(&laptop_key, &desktop, &desktop_key);
    (desktop, laptop)
}

/// Stands in for mDNS: each device learns where the other is.
fn introduce(a: &mut Device, b: &mut Device) {
    a.sync()
        .send(Control::Introduce(b.status.addr.clone().unwrap()));
    b.sync()
        .send(Control::Introduce(a.status.addr.clone().unwrap()));
}

fn connected_pair() -> (Device, Device) {
    let (mut desktop, mut laptop) = paired();
    for device in [&mut desktop, &mut laptop] {
        device.start_sync();
        device.wait("sync to start", reachable);
    }
    introduce(&mut desktop, &mut laptop);
    desktop.wait("a connection", connected);
    laptop.wait("a connection", connected);
    (desktop, laptop)
}

#[test]
fn devices_pair_with_a_code_and_sync_both_ways() {
    let mut desktop = Device::new("Desktop");
    let mut laptop = Device::new("Laptop");
    for device in [&mut desktop, &mut laptop] {
        device.sync().send(Control::SetEnabled(true));
        device.wait("sync to start", |s| {
            s.addr
                .as_ref()
                .is_some_and(|a| a.ip_addrs().next().is_some())
        });
    }
    laptop
        .sync()
        .send(Control::Introduce(desktop.status.addr.clone().unwrap()));
    desktop
        .sync()
        .send(Control::Introduce(laptop.status.addr.clone().unwrap()));

    let code = desktop.show_code();
    let wrong = if code.starts_with('9') {
        "000000"
    } else {
        "999999"
    };
    laptop.sync().send(Control::JoinPairing(wrong.into()));
    let failed = laptop.wait("a wrong code to fail", |s| {
        matches!(s.pairing, Pairing::Failed(_))
    });
    assert!(matches!(&failed.pairing, Pairing::Failed(m) if m.contains("didn't match")));
    assert!(matches!(desktop.status.pairing, Pairing::Showing(_)));
    assert!(laptop.status.devices.is_empty());

    laptop.sync().send(Control::JoinPairing(code));
    laptop.wait("pairing", |s| {
        s.pairing == Pairing::Paired("Desktop".into())
    });
    desktop.wait("pairing", |s| s.pairing == Pairing::Paired("Laptop".into()));
    desktop.wait("a connection", |s| s.devices.iter().any(|d| d.connected));
    laptop.wait("a connection", |s| s.devices.iter().any(|d| d.connected));
    assert_eq!(desktop.status.devices[0].name, "Laptop");

    let note = Note::blank();
    desktop
        .commands
        .send(Command::Create(note.clone()))
        .unwrap();
    desktop.save(&note.id, "Written on the desktop");
    laptop.wait_body(&note.id, "Written on the desktop");

    laptop.save(&note.id, "Written on the desktop, finished on the laptop");
    desktop.wait_body(&note.id, "Written on the desktop, finished on the laptop");

    desktop.save(
        &note.id,
        "Lately: Written on the desktop, finished on the laptop",
    );
    laptop.save(&note.id, "Written on the desktop, finished on the laptop.");
    let merged = "Lately: Written on the desktop, finished on the laptop.";
    desktop.wait_body(&note.id, merged);
    laptop.wait_body(&note.id, merged);

    let laptop_id = desktop.status.devices[0].id.clone();
    desktop.sync().send(Control::Forget(laptop_id));
    desktop.wait("forgetting", |s| s.devices.is_empty());
    laptop.wait("disconnecting", |s| s.devices.iter().all(|d| !d.connected));
    desktop.save(&note.id, "Private again");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(laptop.body(&note.id).as_deref(), Some(merged));
}

#[test]
#[ignore = "needs multicast on the local network"]
fn devices_find_each_other_with_mdns() {
    let mut devices: Vec<Device> = ["Desktop", "Laptop"]
        .into_iter()
        .map(|name| Device::with(name, Options::default(), None))
        .collect();
    for device in &mut devices {
        device.sync().send(Control::SetEnabled(true));
        device.wait("sync to start", Status::running);
    }
    let [desktop, laptop] = &mut devices[..] else {
        unreachable!()
    };
    let code = desktop.show_code();
    laptop.sync().send(Control::JoinPairing(code));
    laptop.wait("pairing", |s| {
        s.pairing == Pairing::Paired("Desktop".into())
    });
    laptop.wait("a connection", |s| s.devices.iter().any(|d| d.connected));
    let note = Note::blank();
    laptop.commands.send(Command::Create(note.clone())).unwrap();
    laptop.save(&note.id, "Found over mDNS");
    desktop.wait_body(&note.id, "Found over mDNS");
}

/// A public relay for the internet tests. n0 runs these for testing only;
/// set NOTEBOOK_TEST_RELAY to use another.
fn test_relay() -> String {
    std::env::var("NOTEBOOK_TEST_RELAY")
        .unwrap_or_else(|_| "https://aps1-1.relay.n0.iroh.link.".into())
}

/// Devices on different networks: they pair and sync through a relay, then
/// one goes away and comes back knowing only what it saved.
fn sync_through_relay(options: Options) {
    let relay = test_relay();
    let mut desktop = Device::with("Desktop", options, Some(&relay));
    let mut laptop = Device::with("Laptop", options, Some(&relay));
    desktop.sync().send(Control::SetEnabled(true));
    desktop.wait("the relay", |s| s.running() && s.relay_connected);
    let desktop_addr = desktop.status.addr.clone().unwrap();
    if !options.direct {
        assert_eq!(desktop_addr.ip_addrs().count(), 0, "{desktop_addr:?}");
    }
    let code = desktop.show_code();
    // The laptop turns sync on and enters the code at once, before it has
    // reached the relay.
    laptop.sync().send(Control::SetEnabled(true));
    // In use, pairing happens on one network; here the laptop learns only the
    // desktop's key and relay, so pairing itself also crosses the relay.
    laptop.sync().send(Control::Introduce(
        EndpointAddr::new(desktop_addr.id).with_relay_url(relay.parse().unwrap()),
    ));
    laptop.sync().send(Control::JoinPairing(code));
    laptop.wait("pairing", |s| {
        s.pairing == Pairing::Paired("Desktop".into())
    });
    desktop.wait("pairing", |s| s.pairing == Pairing::Paired("Laptop".into()));
    desktop.wait("a connection", |s| s.devices.iter().any(|d| d.connected));
    laptop.wait("a connection", |s| s.devices.iter().any(|d| d.connected));

    let note = Note::blank();
    desktop
        .commands
        .send(Command::Create(note.clone()))
        .unwrap();
    desktop.save(&note.id, "Written at home");
    laptop.wait_body(&note.id, "Written at home");
    laptop.save(&note.id, "Written at home, read on the train");
    desktop.wait_body(&note.id, "Written at home, read on the train");

    laptop.stop_sync();
    desktop.wait("the laptop to leave", |s| {
        s.devices.iter().all(|d| !d.connected)
    });
    let away = "Written at home, read on the train, finished while apart";
    desktop.save(&note.id, away);
    // Coming back, the laptop knows only the desktop's key and relay.
    let saved = sync::Config::load(&laptop.config);
    assert_eq!(saved.devices[0].relay_url.as_deref(), Some(relay.as_str()));
    laptop.start_sync();
    laptop.wait("reconnecting", |s| s.devices.iter().any(|d| d.connected));
    laptop.wait_body(&note.id, away);
}

#[test]
#[ignore = "needs internet access to n0's public relays"]
fn devices_sync_only_through_a_public_relay() {
    sync_through_relay(Options {
        mdns: false,
        direct: false,
    });
}

#[test]
#[ignore = "needs internet access to n0's public relays"]
fn devices_meet_at_a_public_relay_with_direct_connections_allowed() {
    sync_through_relay(Options {
        mdns: false,
        direct: true,
    });
}

#[test]
fn suspended_sync_catches_up_when_resumed() {
    let (mut desktop, mut laptop) = connected_pair();
    desktop.sync().send(Control::Suspend(true));
    let paused = desktop.wait("suspending", |s| s.suspended && !s.running());
    assert!(paused.enabled);
    laptop.wait("the desktop to leave", |s| !connected(s));
    let id = laptop.create("Written while the desktop slept");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(desktop.body(&id), None);

    desktop.sync().send(Control::Suspend(false));
    desktop.wait("resuming", |s| !s.suspended && reachable(s));
    // The desktop came back on a new port.
    introduce(&mut desktop, &mut laptop);
    desktop.wait_body(&id, "Written while the desktop slept");
    assert!(sync::Config::load(&desktop.config).enabled);
}

#[test]
fn settings_change_while_suspended_without_binding() {
    let mut device = Device::new("Desktop");
    device.sync().send(Control::SetEnabled(true));
    device.wait("sync to start", Status::running);
    device.sync().send(Control::Suspend(true));
    device.wait("suspending", |s| s.suspended && !s.running());
    device.sync().send(Control::NetworkChanged);
    let relay = "https://relay.example.org./";
    device.sync().send(Control::SetRelay(Some(relay.into())));
    let status = device.wait("the new relay", |s| s.relay_url.as_deref() == Some(relay));
    assert!(!status.running() && status.suspended);
    assert_eq!(
        sync::Config::load(&device.config).relay_url.as_deref(),
        Some(relay)
    );
    device.sync().send(Control::SetEnabled(false));
    device.wait("turning off", |s| !s.enabled);
    device.sync().send(Control::SetEnabled(true));
    let status = device.wait("turning on", |s| s.enabled);
    assert!(!status.running());
    device.sync().send(Control::Suspend(false));
    device.wait("resuming with the relay", |s| {
        s.running() && !s.suspended && s.relay_url.as_deref() == Some(relay)
    });
}

#[test]
fn a_network_change_keeps_a_working_session() {
    let (mut desktop, laptop) = connected_pair();
    desktop.sync().send(Control::NetworkChanged);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(connected(desktop.refresh()));
        std::thread::sleep(Duration::from_millis(20));
    }
    let id = desktop.create("Still connected");
    laptop.wait_body(&id, "Still connected");
}

#[test]
fn statuses_arrive_through_the_callback_and_the_name_defaults_once() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = (dir.path().join("notebook.db"), dir.path().join("sync.json"));
    let (commands, _events) = spawn_worker(db.clone());
    let start = |name: &str| {
        let seen = Arc::new(Mutex::new(Vec::<Status>::new()));
        let record = seen.clone();
        let handle = sync::spawn(
            config.clone(),
            db.clone(),
            name.into(),
            commands.clone(),
            move |status| record.lock().unwrap().push(status),
            local(),
        );
        (handle, seen)
    };
    let wait = |seen: &Mutex<Vec<Status>>, done: &dyn Fn(&Status) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !seen.lock().unwrap().last().is_some_and(done) {
            assert!(Instant::now() < deadline, "{:?}", seen.lock().unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    let (handle, seen) = start("Pixel 9");
    wait(&seen, &|s| s.device_name == "Pixel 9");
    let first = seen.lock().unwrap()[0].clone();
    assert!(first.device_name == "Pixel 9" && !first.enabled && !first.suspended);
    assert_eq!(sync::Config::load(&config).device_name, "Pixel 9");
    handle.send(Control::SetDeviceName("Kitchen tablet".into()));
    wait(&seen, &|s| s.device_name == "Kitchen tablet");
    assert!(seen.lock().unwrap().windows(2).all(|w| w[0] != w[1]));
    drop(handle);

    let (_handle, seen) = start("Pixel 9");
    wait(&seen, &|_| true);
    assert_eq!(seen.lock().unwrap()[0].device_name, "Kitchen tablet");
}

/// The child process `restarted_device_reconnects_at_once` kills: one
/// device's sync, run until the process dies. It prints its address once,
/// then `CONNECTED` when it has a session. Ignored, so it only runs when a
/// test starts it.
#[test]
#[ignore = "runs only as a child process of another test"]
fn child_device() {
    let Some(dir) = std::env::var_os("NOTEBOOK_CHILD_DIR").map(PathBuf::from) else {
        return;
    };
    let db = dir.join("child.db");
    let (commands, _events) = spawn_worker(db.clone());
    commands.send(Command::Initialize).unwrap();
    let (statuses, receiver) = mpsc::channel();
    let sync = sync::spawn(
        dir.join("sync.json"),
        db,
        "Laptop".into(),
        commands,
        move |status| {
            let _ = statuses.send(status);
        },
        local(),
    );
    if let Ok(addr) = std::env::var("NOTEBOOK_CHILD_DIAL") {
        sync.send(Control::Introduce(serde_json::from_str(&addr).unwrap()));
    }
    let mut announced = false;
    for status in receiver {
        if !announced && reachable(&status) {
            println!("ADDR {}", serde_json::to_string(&status.addr).unwrap());
            announced = true;
        }
        if connected(&status) {
            println!("CONNECTED");
        }
    }
}

/// Android kills app processes without warning, and the other device keeps
/// the dead session until it times out. When the app restarts, its new
/// connection must replace that session at once.
fn restarted_device_reconnects_at_once(laptop_dials_first: bool) {
    use std::io::BufRead;
    let (mut desktop, mut laptop) = paired();
    desktop.start_sync();
    desktop.wait("sync to start", reachable);
    let desktop_addr = serde_json::to_string(&desktop.status.addr).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["child_device", "--exact", "--ignored", "--nocapture"])
        .env("NOTEBOOK_CHILD_DIR", laptop.config.parent().unwrap())
        .stdout(std::process::Stdio::piped());
    // Only the device that dials first learns where the other is.
    if laptop_dials_first {
        child.env("NOTEBOOK_CHILD_DIAL", &desktop_addr);
    }
    let mut child = child.spawn().unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = |prefix: &str| loop {
        let line = lines.next().expect("the child exited").unwrap();
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    };
    let laptop_addr: EndpointAddr = serde_json::from_str(&next("ADDR")).unwrap();
    if !laptop_dials_first {
        desktop.sync().send(Control::Introduce(laptop_addr));
    }
    next("CONNECTED");
    desktop.wait("a connection", connected);
    // Two connections within a few seconds of each other count as both
    // devices dialing at once.
    std::thread::sleep(Duration::from_secs(6));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(connected(desktop.refresh()), "the desktop noticed the kill");

    laptop.start_sync();
    laptop.wait("restarting", reachable);
    laptop.sync().send(Control::Introduce(
        serde_json::from_str(&desktop_addr).unwrap(),
    ));
    laptop.wait("reconnecting", connected);
    // Well within the 30 seconds the dead session would take to time out.
    let id = desktop.create("Sent after the restart");
    let deadline = Instant::now() + Duration::from_secs(5);
    while laptop.body(&id).as_deref() != Some("Sent after the restart") {
        assert!(
            Instant::now() < deadline,
            "the desktop kept the dead session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_restarted_device_replaces_the_session_it_dialed() {
    restarted_device_reconnects_at_once(true);
}

#[test]
fn a_restarted_device_replaces_the_session_the_other_dialed() {
    restarted_device_reconnects_at_once(false);
}
