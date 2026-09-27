use iroh::EndpointAddr;
use notebook::{
    model::*,
    storage::{Command, Event, Repository, spawn_worker},
    sync::{self, Control, Options, Pairing, Status, SyncHandle},
};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

struct Device {
    db: PathBuf,
    config: PathBuf,
    options: Options,
    commands: Sender<Command>,
    _events: Receiver<Event>,
    sync: Option<SyncHandle>,
    status: Status,
    _dir: tempfile::TempDir,
}

impl Device {
    fn new(name: &str) -> Self {
        let options = Options {
            mdns: false,
            ..Default::default()
        };
        Self::with(name, options, None)
    }

    fn with(name: &str, options: Options, relay: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notebook.db");
        let config = dir.path().join("sync.json");
        let mut settings = sync::Config::load(&config);
        settings.device_name = name.into();
        settings.relay_url = relay.map(Into::into);
        settings.save(&config).unwrap();
        let (commands, events) = spawn_worker(db.clone());
        // The storage worker opens the database on its first command.
        commands.send(Command::Initialize).unwrap();
        let mut device = Device {
            db,
            config,
            options,
            commands,
            _events: events,
            sync: None,
            status: Status::default(),
            _dir: dir,
        };
        device.start_sync();
        device
    }

    fn start_sync(&mut self) {
        self.sync = Some(sync::spawn(
            self.config.clone(),
            self.db.clone(),
            self.commands.clone(),
            self.options,
        ));
    }

    fn stop_sync(&mut self) {
        self.sync = None;
        self.status = Status::default();
    }

    fn sync(&self) -> &SyncHandle {
        self.sync.as_ref().expect("sync is running")
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.sync().status() {
                self.status = status;
            }
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
    // Stand in for the local network: each learns where the other is.
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

    // Both edit at once; each keeps the other's words.
    desktop.save(
        &note.id,
        "Lately: Written on the desktop, finished on the laptop",
    );
    laptop.save(&note.id, "Written on the desktop, finished on the laptop.");
    let merged = "Lately: Written on the desktop, finished on the laptop.";
    desktop.wait_body(&note.id, merged);
    laptop.wait_body(&note.id, merged);

    // A forgotten device can no longer connect.
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
