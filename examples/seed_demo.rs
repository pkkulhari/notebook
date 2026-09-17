//! Replace a closed Notebook database with realistic UI sample data.
//! cargo run --release --example seed_demo -- --reset [database-path]
use notebook::{
    model::*,
    storage::{self, Mutation, Repository, Result},
};
use std::{collections::HashMap, path::PathBuf};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args[0] != "--reset" || args.len() > 2 {
        return Err("Close Notebook, then run: cargo run --release --example seed_demo -- --reset [database-path]".into());
    }
    let path = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(storage::data_path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Build the complete replacement first. A failed seed leaves the old database alone.
    let replacement = tempfile::NamedTempFile::new_in(parent)?;
    let mut repo = Repository::open(replacement.path())?;
    let mut notebooks = HashMap::new();
    notebooks.insert("Default", DEFAULT_NOTEBOOK.to_string());
    for name in ["Work", "Personal", "Reading", "Someday"] {
        let id = uuid::Uuid::new_v4().to_string();
        repo.mutate(&Mutation::CreateNotebook {
            id: id.clone(),
            name: name.into(),
        })?;
        notebooks.insert(name, id);
    }
    let notes = [
        (
            "Default",
            "Coffee shop thought\nThe best ideas usually arrive when I stop trying to have them.",
        ),
        (
            "Default",
            "# This is a deliberately long note name to check how the list handles a title that does not fit\nThe preview should remain readable without pushing the editor out of place.",
        ),
        ("Default", ""),
        (
            "Default",
            "# Small things worth noticing\n\nSunlight across the desk. The first sip of coffee. A message from an old friend.\n\nNot everything needs to become a project.",
        ),
        (
            "Default",
            "# Weekend list\n\n- [x] Buy coffee beans\n- [x] Water the plants\n- [ ] Pick up a library book\n- [ ] Take the long route home\n- [ ] Leave Sunday afternoon unplanned",
        ),
        (
            "Default",
            "# Markdown, without the ceremony\n\nWrite **something important**, add a little *emphasis*, or cross out ~~an old idea~~.\n\n## A few things to try\n\n1. Click into this paragraph to reveal its Markdown.\n2. Select some formatted text and copy it.\n3. Move the cursor to another paragraph.\n\n> A note is a place to begin, not a thing to finish.\n\nAn inline example: `let thought = \"hello\";`\n\nCtrl+click [the Rust website](https://www.rust-lang.org/) to open a link.",
        ),
        (
            "Default",
            "# A tiny Rust example\n\nKeep useful snippets close by.\n\n```rust\nfn main() {\n    let ideas = [\"write\", \"walk\", \"read\"];\n    for idea in ideas {\n        println!(\"Make time to {idea}\");\n    }\n}\n```\n\nThe source stays intact when the formatting changes.",
        ),
        (
            "Default",
            "# Different words, same page 🌿\n\nनमस्ते दुनिया — एक नई शुरुआत।\n\nこんにちは、世界。\n\nCafé, crème brûlée, déjà vu.\n\nIdeas don't need to arrive in just one language.",
        ),
        ("Default", "One line is enough.\n"),
        (
            "Default",
            "# A quick comparison\n\nTables stay as readable Markdown source in this version.\n\n| Idea | Next step |\n| --- | --- |\n| Reading corner | Move the lamp |\n| Balcony herbs | Find two small pots |\n| Photo journal | Choose ten pictures |",
        ),
        (
            "Default",
            "# Before I forget\n\nAsk about the little bookshop near the station.\n\nFind the recipe with roasted tomatoes and white beans.\n\nRemember the name of that song.",
        ),
        (
            "Work",
            "# Monday planning\n\n## The one thing that matters\n\nFinish the onboarding flow before adding more settings.\n\n- [x] Review last week's feedback\n- [ ] Sketch the empty state\n- [ ] Check keyboard navigation\n- [ ] Share a short walkthrough\n\nKeep the afternoon clear for focused work.",
        ),
        (
            "Work",
            "# Project notes\n\nThe first screen should make the next action obvious.\n\n**Decision:** keep the primary action visible, and move occasional actions into a menu.\n\n**Open question:** what can we remove without losing clarity?",
        ),
        (
            "Work",
            "# Meeting — product review\n\n## What we heard\n\n- People want to get started quickly.\n- Empty screens need a little guidance.\n- Quiet save feedback is enough.\n\n## Next steps\n\nTry the revised flow with three people, then compare notes.",
        ),
        (
            "Work",
            "# Release checklist\n\n- [x] Build the release binary\n- [x] Check a fresh database\n- [ ] Try the app with a large collection\n- [ ] Review light and dark appearance\n- [ ] Walk through the keyboard shortcuts",
        ),
        (
            "Work",
            "# A useful terminal command\n\nFind a phrase in a project:\n\n```sh\nrg --line-number 'search phrase' src/\n```\n\nSmall tools, used often, save a surprising amount of time.",
        ),
        (
            "Personal",
            "# A slower Sunday\n\nBreakfast without a screen. A walk before it gets too warm. Something simple for dinner.\n\nLeave a little space between things.",
        ),
        (
            "Personal",
            "# Lemon pasta\n\n## Ingredients\n\n- Pasta\n- One lemon\n- Olive oil\n- Parmesan\n- Black pepper\n\nSave a cup of pasta water. Toss everything together while the pasta is hot. Add water a little at a time until the sauce comes together.",
        ),
        (
            "Personal",
            "# A weekend away\n\nA small bag, comfortable shoes, and no ambitious itinerary.\n\n- [ ] Book a room near the old town\n- [ ] Download a map\n- [ ] Bring the paperback\n\nLook for a quiet café on the first morning.",
        ),
        (
            "Personal",
            "# Things I want to make\n\nA wooden shelf for the plants.\nA short photo book for a friend.\nA better place to keep the little ideas.",
        ),
        (
            "Reading",
            "# The reading pile\n\nThree books on the bedside table, each for a different kind of evening.\n\n- An essay collection\n- A novel I can get lost in\n- A book that teaches me something unfamiliar",
        ),
        (
            "Reading",
            "# A passage to return to\n\nSome books are better read slowly, with a pencil nearby.\n\n**Question for later:** what changed in the way I saw the subject after reading this chapter?",
        ),
        (
            "Reading",
            "# Notes after chapter three\n\nThe author keeps coming back to attention: what we choose to notice, and what we let pass by.\n\nTry paying attention to one ordinary thing for a little longer tomorrow.",
        ),
    ];
    for (notebook, body) in notes {
        add_note(&mut repo, &notebooks[notebook], body)?;
    }
    let mut long = String::from(
        "# Notes from a long walk\n\nA longer entry for trying the scrollbar and reading width.\n",
    );
    for (heading, paragraph) in [
        (
            "Leaving the house",
            "I left without a destination. At the corner I turned toward the quieter street, the one with trees along both sides. There was no reason to hurry, so I didn't.",
        ),
        (
            "The market",
            "The market was just waking up. Someone was arranging flowers in buckets. A bicycle leaned against the wall of a bakery. I stopped for bread and stayed for a conversation.",
        ),
        (
            "By the water",
            "Past the bridge, the path narrowed. The water caught the light in little fragments. I sat on a bench and wrote down a few things that had been circling in my head all week.",
        ),
        (
            "A useful question",
            "What would this look like if it were simple? Not effortless, and not perfect. Just simple enough that I could begin without making another plan.",
        ),
        (
            "On the way back",
            "I noticed a street I had walked past dozens of times without seeing it. There was a small garden behind a low gate. Sometimes a different pace is enough to make a familiar place feel new.",
        ),
        (
            "Back at the desk",
            "The work was still there when I returned, but it felt smaller. I opened a fresh page, wrote one sentence, and let the next sentence follow.",
        ),
    ] {
        long.push_str(&format!("\n## {heading}\n\n{paragraph}\n\n{paragraph}\n"));
    }
    add_note(&mut repo, DEFAULT_NOTEBOOK, &long)?;
    for body in [
        "# An earlier draft\n\nThis version is in Trash. Restore it to try editing again.",
        "Old shopping list\nMilk, apples, oats, and a loaf of bread.",
    ] {
        let id = add_note(&mut repo, DEFAULT_NOTEBOOK, body)?;
        repo.mutate(&Mutation::Trash { id })?;
    }
    let welcome = "# A place for your thoughts\n\nA quick idea, a useful snippet, a list for later. It all starts with a blank page.\n\n## Make yourself at home\n\n- Open a few notes from the list.\n- Try **bold**, *emphasis*, and `inline code`.\n- Browse Work, Personal, and Reading.\n- Open Someday to see an empty notebook.\n\n## A few shortcuts\n\n**Ctrl+N** starts a fresh note in Default.\n**Ctrl+F** searches your notes.\n**F9** gives the page more room.\n\nThere are two sample notes in Trash if you want to try restoring one.\n\nStart anywhere.";
    let selected = add_note(&mut repo, DEFAULT_NOTEBOOK, welcome)?;
    repo.save_preferences(&Preferences {
        selected_note: Some(selected),
        cursor: welcome.chars().count() as i32,
        width: 1200,
        height: 800,
        sidebar_width: 192,
        list_width: 290,
    })?;
    let active = repo.list(&Filter::All, "")?.len();
    let trash = repo.list(&Filter::Trash, "")?.len();
    let books = repo.notebooks()?.len();
    drop(repo); // Close/checkpoint the replacement before installing it.
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        match std::fs::remove_file(sidecar) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    replacement.persist(&path)?;
    println!(
        "Replaced {} with {active} active notes, {trash} trashed notes, and {books} notebooks.",
        path.display()
    );
    Ok(())
}

fn add_note(repo: &mut Repository, notebook: &str, body: &str) -> Result<String> {
    let mut note = Note::blank();
    note.body = body.into();
    repo.create_note(&note)?;
    if notebook != DEFAULT_NOTEBOOK {
        repo.mutate(&Mutation::Move {
            id: note.id.clone(),
            notebook_id: notebook.into(),
        })?;
    }
    Ok(note.id)
}
