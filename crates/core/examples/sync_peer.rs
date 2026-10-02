//! A headless device for testing sync with a phone or an emulator.
//!
//! cargo run --example sync_peer -- --dir DIR [--name NAME] [--for SECONDS]
//!     [--introduce ID@HOST:PORT] [--show-code | --join CODE] [--forget]
//!     [--seed COUNT] [--big CHARACTERS] [--note TEXT]...
//! cargo run --example sync_peer -- --key-of SECRET_KEY
//!
//! On a local network, devices find each other with mDNS. `--introduce`
//! stands in for it, such as for an emulator behind NAT with a forwarded UDP
//! port; `--key-of` prints the endpoint ID for the secret key in a device's
//! sync.json. `--forget` removes every paired device. `--seed` and `--big`
//! add many short notes or one long Markdown note before sync starts.
//!
//! Prints each new status with the seconds since starting, and a summary of
//! its notes when it stops.
use iroh::{EndpointAddr, EndpointId, SecretKey};
use notebook_core::{
    model::{Filter, Note},
    storage::{Command, Repository, spawn_worker},
    sync::{self, Control, Options, Pairing},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() {
    let started = Instant::now();
    let mut args = std::env::args().skip(1);
    let (mut dir, mut name, mut introduce, mut join, mut seconds) =
        (None, "Sync peer".to_string(), None, None, 60);
    let (mut notes, mut seed, mut big, mut show_code, mut forget) = (vec![], 0, 0, false, false);
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
            "--show-code" => show_code = true,
            "--forget" => forget = true,
            "--note" => notes.push(value()),
            "--seed" => seed = value().parse().expect("--seed takes a count"),
            "--big" => big = value().parse().expect("--big takes a length"),
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
    if seed > 0 || big > 0 {
        let mut repo = Repository::open(&db).unwrap();
        for i in 0..seed {
            let mut note = Note::blank();
            note.body = format!("Note {i}\n\nA thought about gardens, writing, and Rust.");
            repo.create_note(&note).unwrap();
        }
        if big > 0 {
            let line = "- [ ] A **task** with _emphasis_ and `code` 🌿\n";
            let mut note = Note::blank();
            note.body = format!("# A long note\n\n{}", line.repeat(big / line.len() + 1));
            repo.create_note(&note).unwrap();
        }
        println!(
            "SEEDED {seed} notes and {big} characters in {:.1?}",
            started.elapsed()
        );
    }
    let (commands, _events) = spawn_worker(db.clone());
    commands.send(Command::Initialize).unwrap();
    let config = dir.join("sync.json");
    let mut settings = sync::Config::load(&config);
    settings.enabled = true;
    let paired = std::mem::take(&mut settings.devices);
    if !forget {
        settings.devices = paired.clone();
    }
    settings.save(&config).unwrap();
    let handle = sync::spawn(
        config,
        db.clone(),
        name,
        commands.clone(),
        move |status| {
            let at = started.elapsed().as_secs_f32();
            if let Pairing::Showing(code) = &status.pairing {
                println!("CODE {code}");
            }
            println!("STATUS +{at:.1}s {status:?}");
        },
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
    if show_code {
        handle.send(Control::StartPairing);
    }
    if let Some(code) = join {
        handle.send(Control::JoinPairing(code));
    }
    for body in notes {
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
    drop(handle);
    let repo = Repository::open(&db).unwrap();
    let all = repo.list(&Filter::All, "").unwrap();
    println!("NOTES {}", all.len());
    for summary in all.iter().take(10) {
        println!("NOTE {:?}", summary.label);
    }
}
