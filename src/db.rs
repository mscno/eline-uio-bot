use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info};

use crate::models::{Course, CourseChange};

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open database")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS courses (
                code TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                points REAL NOT NULL,
                url TEXT NOT NULL,
                faculty TEXT NOT NULL,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS change_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                course_code TEXT NOT NULL,
                change_type TEXT NOT NULL,
                course_data TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_change_log_timestamp
                ON change_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_change_log_course_code
                ON change_log(course_code);
            "#,
        )?;

        debug!("Database schema initialized");
        Ok(())
    }

    pub fn get_all_courses(&self) -> Result<HashMap<String, Course>> {
        let mut stmt = self.conn.prepare(
            "SELECT code, name, points, url, faculty FROM courses",
        )?;

        let courses = stmt
            .query_map([], |row| {
                Ok(Course {
                    code: row.get(0)?,
                    name: row.get(1)?,
                    points: row.get(2)?,
                    url: row.get(3)?,
                    faculty: row.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .map(|c| (c.code.clone(), c))
            .collect();

        Ok(courses)
    }

    pub fn get_course_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM courses", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn is_first_run(&self) -> Result<bool> {
        Ok(self.get_course_count()? == 0)
    }

    pub fn upsert_course(&self, course: &Course, now: DateTime<Utc>) -> Result<bool> {
        let now_str = now.to_rfc3339();

        // Check if course exists
        let exists: bool = self.conn.query_row(
            "SELECT 1 FROM courses WHERE code = ?",
            [&course.code],
            |_| Ok(true),
        ).unwrap_or(false);

        if exists {
            // Update last_seen_at
            self.conn.execute(
                "UPDATE courses SET
                    name = ?, points = ?, url = ?, faculty = ?, last_seen_at = ?
                 WHERE code = ?",
                params![
                    course.name,
                    course.points,
                    course.url,
                    course.faculty,
                    now_str,
                    course.code,
                ],
            )?;
            Ok(false) // Not new
        } else {
            // Insert new course
            self.conn.execute(
                "INSERT INTO courses (code, name, points, url, faculty, first_seen_at, last_seen_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    course.code,
                    course.name,
                    course.points,
                    course.url,
                    course.faculty,
                    now_str,
                    now_str,
                ],
            )?;
            Ok(true) // New course
        }
    }

    pub fn remove_course(&self, code: &str) -> Result<Option<Course>> {
        // Get course before removing
        let course = self.conn.query_row(
            "SELECT code, name, points, url, faculty FROM courses WHERE code = ?",
            [code],
            |row| {
                Ok(Course {
                    code: row.get(0)?,
                    name: row.get(1)?,
                    points: row.get(2)?,
                    url: row.get(3)?,
                    faculty: row.get(4)?,
                })
            },
        ).ok();

        if course.is_some() {
            self.conn.execute("DELETE FROM courses WHERE code = ?", [code])?;
        }

        Ok(course)
    }

    pub fn log_change(&self, change: &CourseChange) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let course = change.course();
        let change_type = change.change_type();
        let course_json = serde_json::to_string(course)?;

        self.conn.execute(
            "INSERT INTO change_log (course_code, change_type, course_data, timestamp)
             VALUES (?, ?, ?, ?)",
            params![course.code, change_type, course_json, now],
        )?;

        debug!("Logged change: {} {}", change_type, course.code);
        Ok(())
    }

    pub fn sync_courses(&self, current_courses: &[Course]) -> Result<SyncResult> {
        let now = Utc::now();
        let is_first_run = self.is_first_run()?;

        // Get existing courses
        let existing = self.get_all_courses()?;
        let current_codes: std::collections::HashSet<_> =
            current_courses.iter().map(|c| c.code.clone()).collect();
        let existing_codes: std::collections::HashSet<_> =
            existing.keys().cloned().collect();

        let mut added = Vec::new();
        let mut removed = Vec::new();

        // Find new courses
        for course in current_courses {
            let is_new = self.upsert_course(course, now)?;
            if is_new && !is_first_run {
                added.push(course.clone());
                self.log_change(&CourseChange::Added(course.clone()))?;
            }
        }

        // Find removed courses
        for code in existing_codes.difference(&current_codes) {
            if let Some(course) = self.remove_course(code)? {
                if !is_first_run {
                    self.log_change(&CourseChange::Removed(course.clone()))?;
                    removed.push(course);
                }
            }
        }

        if is_first_run {
            info!("First run: populated database with {} courses", current_courses.len());
        }

        Ok(SyncResult {
            added,
            removed,
            is_first_run,
            total_courses: current_courses.len(),
        })
    }
}

#[derive(Debug)]
pub struct SyncResult {
    pub added: Vec<Course>,
    pub removed: Vec<Course>,
    pub is_first_run: bool,
    pub total_courses: usize,
}

impl SyncResult {
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_course() -> Course {
        Course::new(
            "IN1000".to_string(),
            "Intro to Programming".to_string(),
            10.0,
            "https://example.com".to_string(),
            "MN Faculty".to_string(),
        )
    }

    fn make_course(code: &str, points: f32) -> Course {
        Course::new(
            code.to_string(),
            format!("Course {}", code),
            points,
            format!("https://example.com/{}", code),
            "Faculty".to_string(),
        )
    }

    #[test]
    fn test_upsert_course() {
        let db = Database::open_in_memory().unwrap();
        let course = test_course();

        // First insert should return true (new)
        let is_new = db.upsert_course(&course, Utc::now()).unwrap();
        assert!(is_new);

        // Second insert should return false (existing)
        let is_new = db.upsert_course(&course, Utc::now()).unwrap();
        assert!(!is_new);
    }

    #[test]
    fn test_sync_courses() {
        let db = Database::open_in_memory().unwrap();
        let course1 = test_course();
        let course2 = Course::new(
            "IN2000".to_string(),
            "Advanced Programming".to_string(),
            10.0,
            "https://example.com/2".to_string(),
            "MN Faculty".to_string(),
        );

        // First sync - first run
        let result = db.sync_courses(&[course1.clone(), course2.clone()]).unwrap();
        assert!(result.is_first_run);
        assert!(result.added.is_empty()); // First run doesn't report added
        assert_eq!(result.total_courses, 2);

        // Second sync - remove course2
        let result = db.sync_courses(&[course1.clone()]).unwrap();
        assert!(!result.is_first_run);
        assert!(result.added.is_empty());
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].code, "IN2000");
    }

    #[test]
    fn test_sync_detects_added_and_removed_by_code() {
        let db = Database::open_in_memory().unwrap();

        // Initial courses: A (2.5 pts), B (10 pts), C (2.5 pts)
        let course_a = make_course("HFLESER1031", 2.5); // 2.5 point course
        let course_b = make_course("IN1000", 10.0);
        let course_c = make_course("HIS2011M", 2.5); // Another 2.5 point course

        // First sync - populates DB
        let result = db.sync_courses(&[course_a.clone(), course_b.clone(), course_c.clone()]).unwrap();
        assert!(result.is_first_run);
        assert_eq!(db.get_course_count().unwrap(), 3);

        // Second sync - course_a (2.5pts) removed, new course_d (2.5pts) added
        let course_d = make_course("NEWCOURSE", 2.5); // New 2.5 point course
        let result = db.sync_courses(&[course_b.clone(), course_c.clone(), course_d.clone()]).unwrap();

        assert!(!result.is_first_run);

        // Verify added: NEWCOURSE (2.5 pts) should be detected by code
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].code, "NEWCOURSE");
        assert_eq!(result.added[0].points, 2.5);

        // Verify removed: HFLESER1031 (2.5 pts) should be detected by code
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].code, "HFLESER1031");
        assert_eq!(result.removed[0].points, 2.5);

        // Verify DB state: should have B, C, D
        assert_eq!(db.get_course_count().unwrap(), 3);
        let all_courses = db.get_all_courses().unwrap();
        assert!(all_courses.contains_key("IN1000"));
        assert!(all_courses.contains_key("HIS2011M"));
        assert!(all_courses.contains_key("NEWCOURSE"));
        assert!(!all_courses.contains_key("HFLESER1031")); // Removed
    }

    #[test]
    fn test_course_code_is_unique_identifier() {
        let db = Database::open_in_memory().unwrap();

        // Two courses with same code but different names/points
        let course_v1 = Course::new(
            "TEST123".to_string(),
            "Original Name".to_string(),
            5.0,
            "https://example.com/test".to_string(),
            "Faculty".to_string(),
        );

        let course_v2 = Course::new(
            "TEST123".to_string(), // Same code
            "Updated Name".to_string(), // Different name
            10.0, // Different points
            "https://example.com/test".to_string(),
            "Faculty".to_string(),
        );

        // Insert first version
        let is_new = db.upsert_course(&course_v1, Utc::now()).unwrap();
        assert!(is_new);

        // Insert second version with same code - should update, not create new
        let is_new = db.upsert_course(&course_v2, Utc::now()).unwrap();
        assert!(!is_new); // Not new because code already exists

        // Should still have only 1 course
        assert_eq!(db.get_course_count().unwrap(), 1);

        // Course should have updated values
        let courses = db.get_all_courses().unwrap();
        let course = courses.get("TEST123").unwrap();
        assert_eq!(course.name, "Updated Name");
        assert_eq!(course.points, 10.0);
    }
}
