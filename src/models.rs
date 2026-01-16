use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Course {
    pub code: String,
    pub name: String,
    pub points: f32,
    pub url: String,
    pub faculty: String,
}

impl Course {
    pub fn new(code: String, name: String, points: f32, url: String, faculty: String) -> Self {
        Self {
            code,
            name,
            points,
            url,
            faculty,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScrapeDiff {
    pub added: Vec<Course>,
    pub removed: Vec<Course>,
}

impl ScrapeDiff {
    pub fn new(added: Vec<Course>, removed: Vec<Course>) -> Self {
        Self { added, removed }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len()
    }
}
