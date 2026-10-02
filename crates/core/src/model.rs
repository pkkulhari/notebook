use serde::{Deserialize, Serialize};

/// Reserved ID for the Default notebook.
pub const DEFAULT_NOTEBOOK: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Clone, Debug)]
pub struct Notebook {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub id: String,
    pub notebook_id: String,
    pub body: String,
    pub deleted: bool,
}

impl Note {
    pub fn blank() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            notebook_id: DEFAULT_NOTEBOOK.into(),
            body: String::new(),
            deleted: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSummary {
    pub id: String,
    pub label: String,
    pub preview: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    Notebook(String),
    All,
    Trash,
}

pub type NoteCounts = std::collections::HashMap<Filter, u64>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub selected_note: Option<String>,
    pub cursor: i32,
    pub sidebar_width: i32,
    pub list_width: i32,
    pub width: i32,
    pub height: i32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            selected_note: None,
            cursor: 0,
            sidebar_width: 190,
            list_width: 280,
            width: 1120,
            height: 760,
        }
    }
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
