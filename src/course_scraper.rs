use anyhow::{Context, Result};
use scraper::{ElementRef, Html, Selector};
use tracing::{debug, warn};

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

        Self { client, url }
    }

    pub async fn fetch_courses(&self) -> Result<Vec<Course>> {
        debug!("Fetching courses from {}", self.url);

        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .context("Failed to fetch URL")?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("HTTP error: {}", status);
        }

        let html = response.text().await.context("Failed to read response body")?;
        debug!("Received {} bytes of HTML", html.len());

        self.parse_courses(&html)
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
                warn!("Could not find main content area");
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
                    continue;
                }
                let faculty_name = h2.text().collect::<String>().trim().to_string();
                if !faculty_name.is_empty() {
                    // Use a simple counter for ordering
                    faculty_map.push((faculty_map.len(), faculty_name));
                }
            }
        }

        debug!("Found {} faculty sections", faculty_map.len());

        // Now process tables - each table corresponds to a faculty in order
        let mut table_idx = 0;
        for table in content_element.select(&table_selector) {
            let faculty = if table_idx < faculty_map.len() {
                faculty_map[table_idx].1.clone()
            } else {
                "Unknown Faculty".to_string()
            };

            let table_courses = self.parse_table(table, &faculty);
            if !table_courses.is_empty() {
                debug!("Parsed {} courses from {}", table_courses.len(), faculty);
                courses.extend(table_courses);
                table_idx += 1;
            }
        }

        debug!("Parsed {} courses total", courses.len());
        Ok(courses)
    }

    fn parse_table(&self, table: ElementRef, faculty: &str) -> Vec<Course> {
        let mut courses = Vec::new();
        let tr_selector = Selector::parse("tr").expect("Invalid tr selector");
        let td_selector = Selector::parse("td").expect("Invalid td selector");
        let a_selector = Selector::parse("a").expect("Invalid a selector");

        for row in table.select(&tr_selector) {
            let tds: Vec<_> = row.select(&td_selector).collect();
            if tds.len() < 2 {
                continue;
            }

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
                continue;
            }

            // Second td contains points
            let points_text = tds[1].text().collect::<String>();
            let points = parse_points(&points_text);

            if let Some(points) = points {
                let course = Course::new(
                    code.clone(),
                    name,
                    points,
                    url,
                    faculty.to_string(),
                );
                debug!(
                    "Parsed: {} ({} pts) - {}",
                    course.code, course.points, faculty
                );
                courses.push(course);
            } else {
                warn!("Failed to parse points from: {}", points_text.trim());
            }
        }

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
