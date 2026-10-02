//! A headless device for testing sync with a phone or an emulator.
//!
//! cargo run --example sync_peer -- --dir DIR [--name NAME] [--introduce ID@HOST:PORT]
//!     [--join CODE] [--note TEXT] [--for SECONDS]
//! cargo run --example sync_peer -- --key-of SECRET_KEY
//!
//! `--introduce` stands in for mDNS, such as for an emulator behind NAT with a
//! forwarded UDP port. `--key-of` prints the endpoint ID for the secret key in
//! a device's sync.json. Prints each new status, and the notes it holds when
//! it stops.
use iroh::{EndpointAddr, EndpointId, SecretKey};
use notebook_core::{
    model::{Filter, Note},
    storage::{Command, Repository, spawn_worker},
    sync::{self, Control, Options},
};
use std::{path::PathBuf, time::Duration};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut dir, mut name, mut introduce, mut join, mut note, mut seconds) =
        (None, "Sync peer".to_string(), None, None, None, 60);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| panic!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--dir" => dir = Some(PathBuf::from(value())),
            "--name" => name = value(),
            "--introduce" => introduce = Some(value()),
            "--join" => join = Some(value()),
            "--note" => note = Some(value()),
            "--for" => seconds = value().parse().expect("--for takes seconds"),
            "--key-of" => {
                let key: SecretKey = value().parse().expect("not a secret key");
                println!("{}", key.public());
                return;
            }
            other => panic!("unknown flag {other}"),
        }
    }
    let dir = dir.expect("--dir is required");
    let db = dir.join("notebook.db");
    let (commands, _events) = spawn_worker(db.clone());
    commands.send(Command::Initialize).unwrap();
    let mut settings = sync::Config::load(&dir.join("sync.json"));
    settings.enabled = true;
    settings.save(&dir.join("sync.json")).unwrap();
    let handle = sync::spawn(
        dir.join("sync.json"),
        db.clone(),
        name,
        commands.clone(),
        |status| println!("STATUS {status:?}"),
        Options {
            mdns: introduce.is_none(),
            ..Options::default()
        },
    );
    if let Some(target) = introduce {
        let (id, address) = target
            .split_once('@')
            .expect("--introduce takes ID@HOST:PORT");
        let id: EndpointId = id.parse().expect("not an endpoint ID");
        let address = address.parse().expect("not a socket address");
        handle.send(Control::Introduce(
            EndpointAddr::new(id).with_ip_addr(address),
        ));
    }
    if let Some(code) = join {
        handle.send(Control::JoinPairing(code));
    }
    if let Some(body) = note {
        let note = Note::blank();
        commands.send(Command::Create(note.clone())).unwrap();
        commands
            .send(Command::Save {
                id: note.id,
                body,
                sequence: 1,
            })
            .unwrap();
    }
    std::thread::sleep(Duration::from_secs(seconds));
    let repo = Repository::open(&db).unwrap();
    for summary in repo.list(&Filter::All, "").unwrap() {
        let body = repo.load(&summary.id).unwrap().unwrap().body;
        println!("NOTE {body:?}");
    }
}
