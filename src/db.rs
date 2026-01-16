use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use libsql::{Builder, Connection};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, instrument};

use crate::models::{Course, CourseChange};

const SCHEMA_VERSION: i32 = 2;

pub struct Database {
    conn: Connection,
    db_type: DatabaseType,
}

#[derive(Debug, Clone)]
pub enum DatabaseType {
    LocalSqlite(String),
    Turso { url: String },
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::LocalSqlite(path) => write!(f, "SQLite({})", path),
            DatabaseType::Turso { url } => write!(f, "Turso({})", url),
        }
    }
}

impl Database {
    /// Open a local SQLite database file
    pub async fn open(path: &Path) -> Result<Self> {
        let path_str = path.to_string_lossy().to_string();
        info!(db_path = %path_str, db_type = "sqlite", "Opening local SQLite database");

        let db = Builder::new_local(path)
            .build()
            .await
            .context("Failed to open local SQLite database")?;

        let conn = db.connect().context("Failed to connect to database")?;
        let mut db = Self {
            conn,
            db_type: DatabaseType::LocalSqlite(path_str.clone()),
        };

        db.run_migrations().await?;

        let count = db.get_course_count().await.unwrap_or(0);
        info!(
            db_path = %path_str,
            db_type = "sqlite",
            existing_courses = count,
            schema_version = SCHEMA_VERSION,
            "Database opened successfully"
        );

        Ok(db)
    }

    /// Open a remote Turso database
    pub async fn open_turso(url: &str, auth_token: &str) -> Result<Self> {
        info!(
            db_url = %url,
            db_type = "turso",
            "Connecting to Turso database"
        );

        let db = Builder::new_remote(url.to_string(), auth_token.to_string())
            .build()
            .await
            .context("Failed to connect to Turso database")?;

        let conn = db.connect().context("Failed to connect to Turso")?;
        let mut db = Self {
            conn,
            db_type: DatabaseType::Turso { url: url.to_string() },
        };

        db.run_migrations().await?;

        let count = db.get_course_count().await.unwrap_or(0);
        info!(
            db_url = %url,
            db_type = "turso",
            existing_courses = count,
            schema_version = SCHEMA_VERSION,
            "Turso database connected successfully"
        );

        Ok(db)
    }

    /// Open an in-memory database for testing
    pub async fn open_in_memory() -> Result<Self> {
        debug!("Opening in-memory database");

        let db = Builder::new_local(":memory:")
            .build()
            .await
            .context("Failed to open in-memory database")?;

        let conn = db.connect().context("Failed to connect to in-memory database")?;
        let mut db = Self {
            conn,
            db_type: DatabaseType::LocalSqlite(":memory:".to_string()),
        };

        db.run_migrations().await?;
        debug!("In-memory database opened successfully");

        Ok(db)
    }

    /// Get the database type description
    pub fn db_type(&self) -> &DatabaseType {
        &self.db_type
    }

    /// Run database migrations
    async fn run_migrations(&mut self) -> Result<()> {
        info!(target_version = SCHEMA_VERSION, "Running database migrations");

        // Create schema version table if it doesn't exist
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version INTEGER PRIMARY KEY
                )",
                (),
            )
            .await?;

        // Get current version
        let current_version: i32 = self
            .conn
            .query("SELECT COALESCE(MAX(version), 0) FROM schema_version", ())
            .await?
            .next()
            .await?
            .map(|row| row.get::<i32>(0).unwrap_or(0))
            .unwrap_or(0);

        debug!(
            current_version = current_version,
            target_version = SCHEMA_VERSION,
            "Migration status"
        );

        if current_version < 1 {
            info!(migration = 1, "Running migration: create initial schema");
            self.migrate_v1().await?;
        }

        if current_version < 2 {
            info!(migration = 2, "Running migration: create run_log table");
            self.migrate_v2().await?;
        }

        info!(
            from_version = current_version,
            to_version = SCHEMA_VERSION,
            "Migrations completed"
        );

        Ok(())
    }

    /// Migration v1: Create initial tables
    async fn migrate_v1(&mut self) -> Result<()> {
        // Create courses table
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS courses (
                    code TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    points REAL NOT NULL,
                    url TEXT NOT NULL,
                    faculty TEXT NOT NULL,
                    first_seen_at TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL
                )",
                (),
            )
            .await?;

        // Create change log table
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS change_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    course_code TEXT NOT NULL,
                    change_type TEXT NOT NULL,
                    course_data TEXT NOT NULL,
                    timestamp TEXT NOT NULL
                )",
                (),
            )
            .await?;

        // Create indexes
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_change_log_timestamp ON change_log(timestamp)",
                (),
            )
            .await?;

        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_change_log_course_code ON change_log(course_code)",
                (),
            )
            .await?;

        // Record migration version
        self.conn
            .execute("INSERT INTO schema_version (version) VALUES (1)", ())
            .await?;

        debug!("Migration v1 completed: initial schema created");
        Ok(())
    }

    /// Migration v2: Create run_log table for tracking deltas
    async fn migrate_v2(&mut self) -> Result<()> {
        // Create run_log table to track each scrape run and its results
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS run_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp TEXT NOT NULL,
                    total_courses_fetched INTEGER NOT NULL,
                    raw_added_count INTEGER NOT NULL,
                    raw_removed_count INTEGER NOT NULL,
                    filtered_added_count INTEGER NOT NULL,
                    filtered_removed_count INTEGER NOT NULL,
                    filter_used TEXT NOT NULL,
                    notification_sent INTEGER NOT NULL,
                    is_first_run INTEGER NOT NULL,
                    added_courses TEXT NOT NULL,
                    removed_courses TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL
                )",
                (),
            )
            .await?;

        // Create index for timestamp queries
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_run_log_timestamp ON run_log(timestamp)",
                (),
            )
            .await?;

        // Record migration version
        self.conn
            .execute("INSERT INTO schema_version (version) VALUES (2)", ())
            .await?;

        debug!("Migration v2 completed: run_log table created");
        Ok(())
    }

    pub async fn get_all_courses(&self) -> Result<HashMap<String, Course>> {
        let mut rows = self
            .conn
            .query("SELECT code, name, points, url, faculty FROM courses", ())
            .await?;

        let mut courses = HashMap::new();
        while let Some(row) = rows.next().await? {
            let course = Course {
                code: row.get::<String>(0)?,
                name: row.get::<String>(1)?,
                points: row.get::<f64>(2)? as f32,
                url: row.get::<String>(3)?,
                faculty: row.get::<String>(4)?,
            };
            courses.insert(course.code.clone(), course);
        }

        Ok(courses)
    }

    pub async fn get_course_count(&self) -> Result<usize> {
        let mut rows = self.conn.query("SELECT COUNT(*) FROM courses", ()).await?;
        let count = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
        Ok(count as usize)
    }

    pub async fn is_first_run(&self) -> Result<bool> {
        Ok(self.get_course_count().await? == 0)
    }

    pub async fn upsert_course(&self, course: &Course, now: DateTime<Utc>) -> Result<bool> {
        let now_str = now.to_rfc3339();

        // Check if course exists
        let mut rows = self
            .conn
            .query(
                "SELECT 1 FROM courses WHERE code = ?",
                libsql::params![course.code.clone()],
            )
            .await?;

        let exists = rows.next().await?.is_some();

        if exists {
            // Update existing course
            self.conn
                .execute(
                    "UPDATE courses SET name = ?, points = ?, url = ?, faculty = ?, last_seen_at = ? WHERE code = ?",
                    libsql::params![
                        course.name.clone(),
                        course.points as f64,
                        course.url.clone(),
                        course.faculty.clone(),
                        now_str.clone(),
                        course.code.clone(),
                    ],
                )
                .await?;
            Ok(false) // Not new
        } else {
            // Insert new course
            self.conn
                .execute(
                    "INSERT INTO courses (code, name, points, url, faculty, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    libsql::params![
                        course.code.clone(),
                        course.name.clone(),
                        course.points as f64,
                        course.url.clone(),
                        course.faculty.clone(),
                        now_str.clone(),
                        now_str.clone(),
                    ],
                )
                .await?;
            Ok(true) // New course
        }
    }

    pub async fn remove_course(&self, code: &str) -> Result<Option<Course>> {
        // Get course before removing
        let mut rows = self
            .conn
            .query(
                "SELECT code, name, points, url, faculty FROM courses WHERE code = ?",
                libsql::params![code.to_string()],
            )
            .await?;

        let course = if let Some(row) = rows.next().await? {
            Some(Course {
                code: row.get::<String>(0)?,
                name: row.get::<String>(1)?,
                points: row.get::<f64>(2)? as f32,
                url: row.get::<String>(3)?,
                faculty: row.get::<String>(4)?,
            })
        } else {
            None
        };

        if course.is_some() {
            self.conn
                .execute(
                    "DELETE FROM courses WHERE code = ?",
                    libsql::params![code.to_string()],
                )
                .await?;
        }

        Ok(course)
    }

    pub async fn log_change(&self, change: &CourseChange) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let course = change.course();
        let change_type = change.change_type();
        let course_json = serde_json::to_string(course)?;

        self.conn
            .execute(
                "INSERT INTO change_log (course_code, change_type, course_data, timestamp) VALUES (?, ?, ?, ?)",
                libsql::params![
                    course.code.clone(),
                    change_type.to_string(),
                    course_json,
                    now.clone(),
                ],
            )
            .await?;

        info!(
            course_code = %course.code,
            course_name = %course.name,
            points = course.points,
            change_type = %change_type,
            timestamp = %now,
            "Change logged to database"
        );

        Ok(())
    }

    /// Log a complete run with all delta information
    #[instrument(skip(self, run_log), fields(
        total_fetched = run_log.total_courses_fetched,
        filtered_added = run_log.filtered_added_count,
        filtered_removed = run_log.filtered_removed_count
    ))]
    pub async fn log_run(&self, run_log: &RunLog) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();

        // Serialize course code lists as JSON
        let added_json = serde_json::to_string(&run_log.added_courses)?;
        let removed_json = serde_json::to_string(&run_log.removed_courses)?;

        self.conn
            .execute(
                "INSERT INTO run_log (
                    timestamp, total_courses_fetched,
                    raw_added_count, raw_removed_count,
                    filtered_added_count, filtered_removed_count,
                    filter_used, notification_sent, is_first_run,
                    added_courses, removed_courses, duration_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                libsql::params![
                    now.clone(),
                    run_log.total_courses_fetched as i64,
                    run_log.raw_added_count as i64,
                    run_log.raw_removed_count as i64,
                    run_log.filtered_added_count as i64,
                    run_log.filtered_removed_count as i64,
                    run_log.filter_used.clone(),
                    if run_log.notification_sent { 1i64 } else { 0i64 },
                    if run_log.is_first_run { 1i64 } else { 0i64 },
                    added_json,
                    removed_json,
                    run_log.duration_ms as i64,
                ],
            )
            .await?;

        // Get the last inserted row ID
        let mut rows = self.conn.query("SELECT last_insert_rowid()", ()).await?;
        let run_id = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        info!(
            run_id = run_id,
            timestamp = %now,
            total_courses_fetched = run_log.total_courses_fetched,
            raw_added = run_log.raw_added_count,
            raw_removed = run_log.raw_removed_count,
            filtered_added = run_log.filtered_added_count,
            filtered_removed = run_log.filtered_removed_count,
            filter = %run_log.filter_used,
            notification_sent = run_log.notification_sent,
            is_first_run = run_log.is_first_run,
            added_codes = ?run_log.added_courses,
            removed_codes = ?run_log.removed_courses,
            duration_ms = run_log.duration_ms,
            "Run logged to database"
        );

        Ok(run_id)
    }

    /// Get all courses for web display, sorted by code
    pub async fn get_courses_for_display(&self) -> Result<Vec<CourseDisplay>> {
        let mut rows = self
            .conn
            .query(
                "SELECT code, name, points, url, faculty, first_seen_at, last_seen_at
                 FROM courses ORDER BY code",
                (),
            )
            .await?;

        let mut courses = Vec::new();
        while let Some(row) = rows.next().await? {
            courses.push(CourseDisplay {
                code: row.get::<String>(0)?,
                name: row.get::<String>(1)?,
                points: row.get::<f64>(2)? as f32,
                url: row.get::<String>(3)?,
                faculty: row.get::<String>(4)?,
                first_seen_at: row.get::<String>(5)?,
                last_seen_at: row.get::<String>(6)?,
            });
        }

        Ok(courses)
    }

    /// Get recent run logs for web display
    pub async fn get_run_logs(&self, limit: usize) -> Result<Vec<RunLogEntry>> {
        let mut rows = self
            .conn
            .query(
                "SELECT id, timestamp, total_courses_fetched, raw_added_count, raw_removed_count,
                        filtered_added_count, filtered_removed_count, filter_used,
                        notification_sent, is_first_run, added_courses, removed_courses, duration_ms
                 FROM run_log ORDER BY id DESC LIMIT ?",
                libsql::params![limit as i64],
            )
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            let added_json: String = row.get(10)?;
            let removed_json: String = row.get(11)?;

            entries.push(RunLogEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                total_courses_fetched: row.get(2)?,
                raw_added_count: row.get(3)?,
                raw_removed_count: row.get(4)?,
                filtered_added_count: row.get(5)?,
                filtered_removed_count: row.get(6)?,
                filter_used: row.get(7)?,
                notification_sent: row.get::<i64>(8)? != 0,
                is_first_run: row.get::<i64>(9)? != 0,
                added_courses: serde_json::from_str(&added_json).unwrap_or_default(),
                removed_courses: serde_json::from_str(&removed_json).unwrap_or_default(),
                duration_ms: row.get(12)?,
            });
        }

        Ok(entries)
    }

    /// Get a single run log by ID
    pub async fn get_run_log(&self, id: i64) -> Result<Option<RunLogEntry>> {
        let mut rows = self
            .conn
            .query(
                "SELECT id, timestamp, total_courses_fetched, raw_added_count, raw_removed_count,
                        filtered_added_count, filtered_removed_count, filter_used,
                        notification_sent, is_first_run, added_courses, removed_courses, duration_ms
                 FROM run_log WHERE id = ?",
                libsql::params![id],
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let added_json: String = row.get(10)?;
            let removed_json: String = row.get(11)?;

            Ok(Some(RunLogEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                total_courses_fetched: row.get(2)?,
                raw_added_count: row.get(3)?,
                raw_removed_count: row.get(4)?,
                filtered_added_count: row.get(5)?,
                filtered_removed_count: row.get(6)?,
                filter_used: row.get(7)?,
                notification_sent: row.get::<i64>(8)? != 0,
                is_first_run: row.get::<i64>(9)? != 0,
                added_courses: serde_json::from_str(&added_json).unwrap_or_default(),
                removed_courses: serde_json::from_str(&removed_json).unwrap_or_default(),
                duration_ms: row.get(12)?,
            }))
        } else {
            Ok(None)
        }
    }

    #[instrument(skip(self, current_courses), fields(incoming_courses = current_courses.len()))]
    pub async fn sync_courses(&self, current_courses: &[Course]) -> Result<SyncResult> {
        let now = Utc::now();
        let is_first_run = self.is_first_run().await?;

        // Get existing courses
        let existing = self.get_all_courses().await?;
        let existing_count = existing.len();
        let current_codes: std::collections::HashSet<_> =
            current_courses.iter().map(|c| c.code.clone()).collect();
        let existing_codes: std::collections::HashSet<_> = existing.keys().cloned().collect();

        info!(
            is_first_run = is_first_run,
            existing_courses_in_db = existing_count,
            incoming_courses = current_courses.len(),
            db_type = %self.db_type,
            "Starting database sync"
        );

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut updated_count = 0;

        // Find new courses
        for course in current_courses {
            let is_new = self.upsert_course(course, now).await?;
            if is_new {
                if !is_first_run {
                    debug!(
                        course_code = %course.code,
                        course_name = %course.name,
                        points = course.points,
                        faculty = %course.faculty,
                        "New course detected"
                    );
                    added.push(course.clone());
                    self.log_change(&CourseChange::Added(course.clone())).await?;
                }
            } else {
                updated_count += 1;
            }
        }

        // Find removed courses
        let codes_to_remove: Vec<_> = existing_codes.difference(&current_codes).cloned().collect();
        debug!(
            courses_to_remove = codes_to_remove.len(),
            codes = ?codes_to_remove,
            "Checking for removed courses"
        );

        for code in codes_to_remove {
            if let Some(course) = self.remove_course(&code).await? {
                if !is_first_run {
                    debug!(
                        course_code = %course.code,
                        course_name = %course.name,
                        points = course.points,
                        faculty = %course.faculty,
                        "Course removed from availability"
                    );
                    self.log_change(&CourseChange::Removed(course.clone())).await?;
                    removed.push(course);
                }
            }
        }

        if is_first_run {
            info!(
                courses_stored = current_courses.len(),
                db_type = %self.db_type,
                "First run completed - database initialized"
            );
        } else {
            info!(
                added_count = added.len(),
                removed_count = removed.len(),
                updated_count = updated_count,
                total_courses = current_courses.len(),
                added_codes = ?added.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(),
                removed_codes = ?removed.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(),
                db_type = %self.db_type,
                "Database sync completed"
            );
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

/// Record of a single scrape run for logging
#[derive(Debug)]
pub struct RunLog {
    pub total_courses_fetched: usize,
    pub raw_added_count: usize,
    pub raw_removed_count: usize,
    pub filtered_added_count: usize,
    pub filtered_removed_count: usize,
    pub filter_used: String,
    pub notification_sent: bool,
    pub is_first_run: bool,
    pub added_courses: Vec<String>,  // Course codes
    pub removed_courses: Vec<String>, // Course codes
    pub duration_ms: u64,
}

/// Course data for web display
#[derive(Debug, Clone)]
pub struct CourseDisplay {
    pub code: String,
    pub name: String,
    pub points: f32,
    pub faculty: String,
    pub url: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

/// Run log entry for web display
#[derive(Debug, Clone)]
pub struct RunLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub total_courses_fetched: i64,
    pub raw_added_count: i64,
    pub raw_removed_count: i64,
    pub filtered_added_count: i64,
    pub filtered_removed_count: i64,
    pub filter_used: String,
    pub notification_sent: bool,
    pub is_first_run: bool,
    pub added_courses: Vec<String>,
    pub removed_courses: Vec<String>,
    pub duration_ms: i64,
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

    #[tokio::test]
    async fn test_upsert_course() {
        let db = Database::open_in_memory().await.unwrap();
        let course = test_course();

        // First insert should return true (new)
        let is_new = db.upsert_course(&course, Utc::now()).await.unwrap();
        assert!(is_new);

        // Second insert should return false (existing)
        let is_new = db.upsert_course(&course, Utc::now()).await.unwrap();
        assert!(!is_new);
    }

    #[tokio::test]
    async fn test_sync_courses() {
        let db = Database::open_in_memory().await.unwrap();
        let course1 = test_course();
        let course2 = Course::new(
            "IN2000".to_string(),
            "Advanced Programming".to_string(),
            10.0,
            "https://example.com/2".to_string(),
            "MN Faculty".to_string(),
        );

        // First sync - first run
        let result = db.sync_courses(&[course1.clone(), course2.clone()]).await.unwrap();
        assert!(result.is_first_run);
        assert!(result.added.is_empty()); // First run doesn't report added
        assert_eq!(result.total_courses, 2);

        // Second sync - remove course2
        let result = db.sync_courses(&[course1.clone()]).await.unwrap();
        assert!(!result.is_first_run);
        assert!(result.added.is_empty());
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].code, "IN2000");
    }

    #[tokio::test]
    async fn test_sync_detects_added_and_removed_by_code() {
        let db = Database::open_in_memory().await.unwrap();

        // Initial courses: A (2.5 pts), B (10 pts), C (2.5 pts)
        let course_a = make_course("HFLESER1031", 2.5); // 2.5 point course
        let course_b = make_course("IN1000", 10.0);
        let course_c = make_course("HIS2011M", 2.5); // Another 2.5 point course

        // First sync - populates DB
        let result = db
            .sync_courses(&[course_a.clone(), course_b.clone(), course_c.clone()])
            .await
            .unwrap();
        assert!(result.is_first_run);
        assert_eq!(db.get_course_count().await.unwrap(), 3);

        // Second sync - course_a (2.5pts) removed, new course_d (2.5pts) added
        let course_d = make_course("NEWCOURSE", 2.5); // New 2.5 point course
        let result = db
            .sync_courses(&[course_b.clone(), course_c.clone(), course_d.clone()])
            .await
            .unwrap();

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
        assert_eq!(db.get_course_count().await.unwrap(), 3);
        let all_courses = db.get_all_courses().await.unwrap();
        assert!(all_courses.contains_key("IN1000"));
        assert!(all_courses.contains_key("HIS2011M"));
        assert!(all_courses.contains_key("NEWCOURSE"));
        assert!(!all_courses.contains_key("HFLESER1031")); // Removed
    }

    #[tokio::test]
    async fn test_course_code_is_unique_identifier() {
        let db = Database::open_in_memory().await.unwrap();

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
            10.0,                  // Different points
            "https://example.com/test".to_string(),
            "Faculty".to_string(),
        );

        // Insert first version
        let is_new = db.upsert_course(&course_v1, Utc::now()).await.unwrap();
        assert!(is_new);

        // Insert second version with same code - should update, not create new
        let is_new = db.upsert_course(&course_v2, Utc::now()).await.unwrap();
        assert!(!is_new); // Not new because code already exists

        // Should still have only 1 course
        assert_eq!(db.get_course_count().await.unwrap(), 1);

        // Course should have updated values
        let courses = db.get_all_courses().await.unwrap();
        let course = courses.get("TEST123").unwrap();
        assert_eq!(course.name, "Updated Name");
        assert_eq!(course.points, 10.0);
    }
}
