//! Run with `cargo run --release --example benchmark`.
use notebook_core::{editor::Drafts, markdown, model::*, storage::Repository};
use std::time::Instant;

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benchmark.db");
    let mut repo = Repository::open(&path).unwrap();
    let seed = Instant::now();
    for i in 0..10_000 {
        let mut note = Note::blank();
        note.body = format!("# Note {i}\n\nA thought about gardens, writing, and Rust.\n");
        repo.create_note(&note).unwrap();
    }
    let mut large = Note::blank();
    large.body = "# A working document\n\n".to_string()
        + &"A **bold** thought with _emphasis_, `code`, and [a link](https://example.org).\n\n"
            .repeat(1_350);
    repo.create_note(&large).unwrap();
    repo.save_preferences(&Preferences {
        selected_note: Some(large.id.clone()),
        ..Default::default()
    })
    .unwrap();
    println!("Seed 10,001 notes: {:.2?}", seed.elapsed());
    drop(repo);
    let start = Instant::now();
    let mut repo = Repository::open(&path).unwrap();
    let (_, note, _) = repo.bootstrap().unwrap();
    println!(
        "Open database and restore {} byte note: {:.2?}",
        note.body.len(),
        start.elapsed()
    );
    let start = Instant::now();
    let notes = repo.list(&Filter::All, "").unwrap();
    println!(
        "List {} note summaries: {:.2?}",
        notes.len(),
        start.elapsed()
    );
    let start = Instant::now();
    let hits = repo.list(&Filter::All, "gardens").unwrap();
    println!("Search {} matches: {:.2?}", hits.len(), start.elapsed());
    let mut timings = vec![];
    for _ in 0..50 {
        let start = Instant::now();
        std::hint::black_box(markdown::parse(&note.body, Units::Chars));
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "Markdown parse median: {:.2?}; p95: {:.2?}",
        timings[25], timings[47]
    );
    // Typing at the end of the large note, counted in UTF-16 as on Android.
    let mut drafts = Drafts::new(Units::Utf16);
    let snapshot = repo.snapshot(&note.id).unwrap().unwrap();
    let draft = drafts.open(note.clone(), &snapshot, true);
    let end = note.body.encode_utf16().count();
    let mut timings = vec![];
    for at in end..end + 1_000 {
        let start = Instant::now();
        draft.replace(at, 0, "x").unwrap();
        timings.push(start.elapsed());
    }
    timings.sort();
    println!(
        "Keystroke into the large note's draft median: {:.2?}; p95: {:.2?}",
        timings[500], timings[950]
    );
    let start = Instant::now();
    repo.save(&note.id, &(note.body + "One more thought."))
        .unwrap();
    println!(
        "Save large note, FTS index, and journal: {:.2?}",
        start.elapsed()
    );
}
