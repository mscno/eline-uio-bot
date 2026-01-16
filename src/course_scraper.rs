use anyhow::{Context, Result};
use scraper::{ElementRef, Html, Selector};
use std::time::Instant;
use tracing::{debug, info, instrument, warn};

use crate::models::Course;

pub struct CourseScraper {
    client: reqwest::Client,
    url: String,
}

impl CourseScraper {
    pub fn new(url: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("UiOBot/1.0 (Course Availability Monitor)")
            .build()
            .expect("Failed to create HTTP client");

        info!(url = %url, "Scraper initialized");
        Self { client, url }
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn fetch_courses(&self) -> Result<Vec<Course>> {
        let start = Instant::now();
        info!(url = %self.url, "Starting HTTP fetch");

        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .context("Failed to fetch URL")?;

        let status = response.status();
        let status_code = status.as_u16();

        if !status.is_success() {
            warn!(
                status_code = status_code,
                status_text = %status,
                url = %self.url,
                "HTTP request failed"
            );
            anyhow::bail!("HTTP error: {}", status);
        }

        let content_length = response.content_length();
        let html = response.text().await.context("Failed to read response body")?;
        let fetch_duration_ms = start.elapsed().as_millis();

        info!(
            status_code = status_code,
            content_length_header = ?content_length,
            body_bytes = html.len(),
            fetch_duration_ms = fetch_duration_ms,
            "HTTP fetch completed"
        );

        let parse_start = Instant::now();
        let courses = self.parse_courses(&html)?;
        let parse_duration_ms = parse_start.elapsed().as_millis();

        info!(
            courses_parsed = courses.len(),
            parse_duration_ms = parse_duration_ms,
            total_duration_ms = start.elapsed().as_millis(),
            "Fetch and parse completed"
        );

        Ok(courses)
    }

    fn parse_courses(&self, html: &str) -> Result<Vec<Course>> {
        let document = Html::parse_document(html);
        let mut courses = Vec::new();

        // Find the main content area
        let content_selector = Selector::parse("#vrtx-content, main, article, .vrtx-content, body")
            .expect("Invalid content selector");

        let content = document.select(&content_selector).next();
        let content_element = match content {
            Some(el) => el,
            None => {
                warn!("Could not find main content area in HTML document");
                return Ok(courses);
            }
        };

        // Build a map of h2 IDs to their text (faculty names)
        // The structure is: h2 with id like "det-humanistiske-fakultet" followed by table
        let h2_selector = Selector::parse("h2[id]").expect("Invalid h2 selector");
        let table_selector = Selector::parse("table").expect("Invalid table selector");

        // Collect all h2 elements with their positions
        let mut faculty_map: Vec<(usize, String)> = Vec::new();
        for h2 in content_element.select(&h2_selector) {
            if let Some(id) = h2.value().attr("id") {
                // Skip navigation-related h2s
                if id.contains("sporsmal") || id.contains("kontakt") {
                    debug!(h2_id = %id, "Skipping navigation h2 element");
                    continue;
                }
                let faculty_name = h2.text().collect::<String>().trim().to_string();
                if !faculty_name.is_empty() {
                    debug!(
                        faculty_index = faculty_map.len(),
                        faculty_name = %faculty_name,
                        h2_id = %id,
                        "Found faculty section"
                    );
                    faculty_map.push((faculty_map.len(), faculty_name));
                }
            }
        }

        info!(
            faculty_count = faculty_map.len(),
            faculties = ?faculty_map.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(),
            "Identified faculty sections"
        );

        // Now process tables - each table corresponds to a faculty in order
        let mut table_idx = 0;
        let mut courses_by_faculty: Vec<(String, usize)> = Vec::new();

        for table in content_element.select(&table_selector) {
            let faculty = if table_idx < faculty_map.len() {
                faculty_map[table_idx].1.clone()
            } else {
                "Unknown Faculty".to_string()
            };

            let table_courses = self.parse_table(table, &faculty);
            if !table_courses.is_empty() {
                debug!(
                    faculty = %faculty,
                    courses_in_table = table_courses.len(),
                    table_index = table_idx,
                    "Parsed faculty table"
                );
                courses_by_faculty.push((faculty.clone(), table_courses.len()));
                courses.extend(table_courses);
                table_idx += 1;
            }
        }

        info!(
            total_courses = courses.len(),
            tables_processed = table_idx,
            courses_by_faculty = ?courses_by_faculty,
            "HTML parsing completed"
        );

        Ok(courses)
    }

    fn parse_table(&self, table: ElementRef, faculty: &str) -> Vec<Course> {
        let mut courses = Vec::new();
        let tr_selector = Selector::parse("tr").expect("Invalid tr selector");
        let td_selector = Selector::parse("td").expect("Invalid td selector");
        let a_selector = Selector::parse("a").expect("Invalid a selector");

        let mut rows_processed = 0;
        let mut rows_skipped = 0;
        let mut parse_errors = 0;

        for row in table.select(&tr_selector) {
            let tds: Vec<_> = row.select(&td_selector).collect();
            if tds.len() < 2 {
                rows_skipped += 1;
                continue;
            }
            rows_processed += 1;

            // First td contains the link with course code and name
            let first_td = &tds[0];
            let link = first_td.select(&a_selector).next();

            let (url, code, name) = if let Some(a) = link {
                let href = a.value().attr("href").unwrap_or("").to_string();
                let text = a.text().collect::<String>();
                let (code, name) = parse_course_text(&text);
                (href, code, name)
            } else {
                // No link, try to get text directly
                let text = first_td.text().collect::<String>();
                let (code, name) = parse_course_text(&text);
                (String::new(), code, name)
            };

            if code.is_empty() {
                debug!(
                    faculty = %faculty,
                    raw_text = %first_td.text().collect::<String>().trim(),
                    "Skipping row with empty course code"
                );
                rows_skipped += 1;
                continue;
            }

            // Second td contains points
            let points_text = tds[1].text().collect::<String>();
            let points = parse_points(&points_text);

            if let Some(points) = points {
                let course = Course::new(
                    code.clone(),
                    name.clone(),
                    points,
                    url.clone(),
                    faculty.to_string(),
                );
                debug!(
                    course_code = %code,
                    course_name = %name,
                    points = points,
                    faculty = %faculty,
                    has_url = !url.is_empty(),
                    "Parsed course"
                );
                courses.push(course);
            } else {
                warn!(
                    course_code = %code,
                    faculty = %faculty,
                    raw_points_text = %points_text.trim(),
                    "Failed to parse points value"
                );
                parse_errors += 1;
            }
        }

        debug!(
            faculty = %faculty,
            courses_found = courses.len(),
            rows_processed = rows_processed,
            rows_skipped = rows_skipped,
            parse_errors = parse_errors,
            "Table parsing completed"
        );

        courses
    }
}

/// Parse course code and name from link text
/// Format: "CODE - Course Name" or just "CODE"
fn parse_course_text(text: &str) -> (String, String) {
    let text = text.trim();
    if let Some(pos) = text.find(" - ") {
        let code = text[..pos].trim().to_string();
        let name = text[pos + 3..].trim().to_string();
        (code, name)
    } else {
        (text.to_string(), String::new())
    }
}

/// Parse points from text, handling both integers and decimals
fn parse_points(text: &str) -> Option<f32> {
    let text = text.trim().replace(',', ".");
    text.parse::<f32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_course_text() {
        let (code, name) = parse_course_text("IN1000 - Introduksjon til programmering");
        assert_eq!(code, "IN1000");
        assert_eq!(name, "Introduksjon til programmering");

        let (code, name) = parse_course_text("IN1000");
        assert_eq!(code, "IN1000");
        assert_eq!(name, "");
    }

    #[test]
    fn test_parse_points() {
        assert_eq!(parse_points("10"), Some(10.0));
        assert_eq!(parse_points("2.5"), Some(2.5));
        assert_eq!(parse_points("2,5"), Some(2.5));
        assert_eq!(parse_points("  10  "), Some(10.0));
        assert_eq!(parse_points("invalid"), None);
    }
}
