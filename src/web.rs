use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::Html,
    routing::get,
    Router,
};
use tracing::info;

use crate::db::{CourseDisplay, Database, RunLogEntry};

/// Application state shared between handlers
pub struct AppState {
    pub db: Database,
}

/// Create the Axum router with all routes
pub fn create_router(db: Database) -> Router {
    let state = Arc::new(AppState { db });

    Router::new()
        .route("/", get(dashboard))
        .route("/runs", get(run_logs))
        .route("/runs/{id}", get(run_detail))
        .with_state(state)
}

/// Start the web server on the given port
pub async fn start_server(router: Router, port: u16) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!(
        port = port,
        addr = %addr,
        "Web server started"
    );

    axum::serve(listener, router).await?;
    Ok(())
}

/// Dashboard page showing current courses
async fn dashboard(State(state): State<Arc<AppState>>) -> Html<String> {
    let courses = state.db.get_courses_for_display().await.unwrap_or_default();
    Html(render_dashboard(&courses))
}

/// Run logs list page
async fn run_logs(State(state): State<Arc<AppState>>) -> Html<String> {
    let runs = state.db.get_run_logs(100).await.unwrap_or_default();
    Html(render_run_logs(&runs))
}

/// Run log detail page
async fn run_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Html<String> {
    match state.db.get_run_log(id).await {
        Ok(Some(run)) => Html(render_run_detail(&run)),
        Ok(None) => Html(render_error("Run log not found")),
        Err(e) => Html(render_error(&format!("Error: {}", e))),
    }
}

/// Render the dashboard HTML
fn render_dashboard(courses: &[CourseDisplay]) -> String {
    let mut rows = String::new();
    for course in courses {
        rows.push_str(&format!(
            r#"<tr>
                <td><a href="{}" target="_blank">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
            </tr>"#,
            html_escape(&course.url),
            html_escape(&course.code),
            html_escape(&course.name),
            course.points,
            html_escape(&course.faculty),
            format_timestamp(&course.first_seen_at),
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>UiOBot Dashboard</title>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/milligram/1.4.1/milligram.min.css">
    <style>
        body {{ padding: 2rem 0; }}
        nav {{ margin-bottom: 2rem; }}
        nav a {{ margin-right: 1rem; }}
        table {{ width: 100%; }}
        .count {{ color: #606c76; font-weight: normal; }}
    </style>
</head>
<body>
    <main class="container">
        <h1>UiOBot Dashboard</h1>
        <nav>
            <a href="/" class="button button-outline">Courses</a>
            <a href="/runs" class="button button-clear">Run Logs</a>
        </nav>

        <h2>Current Courses <span class="count">({} total)</span></h2>
        <table>
            <thead>
                <tr>
                    <th>Code</th>
                    <th>Name</th>
                    <th>Points</th>
                    <th>Faculty</th>
                    <th>First Seen</th>
                </tr>
            </thead>
            <tbody>
                {}
            </tbody>
        </table>
    </main>
</body>
</html>"#,
        courses.len(),
        rows
    )
}

/// Render the run logs list HTML
fn render_run_logs(runs: &[RunLogEntry]) -> String {
    let mut rows = String::new();
    for run in runs {
        let notified = if run.notification_sent { "Yes" } else { "No" };
        let first_run = if run.is_first_run { " (first run)" } else { "" };

        rows.push_str(&format!(
            r#"<tr>
                <td><a href="/runs/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td style="color: green;">+{}</td>
                <td style="color: red;">-{}</td>
                <td>{}</td>
                <td>{}ms</td>
            </tr>"#,
            run.id,
            run.id,
            format_timestamp(&run.timestamp),
            run.total_courses_fetched,
            run.filtered_added_count,
            run.filtered_removed_count,
            format!("{}{}", notified, first_run),
            run.duration_ms,
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Run Logs - UiOBot</title>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/milligram/1.4.1/milligram.min.css">
    <style>
        body {{ padding: 2rem 0; }}
        nav {{ margin-bottom: 2rem; }}
        nav a {{ margin-right: 1rem; }}
        table {{ width: 100%; }}
        .count {{ color: #606c76; font-weight: normal; }}
    </style>
</head>
<body>
    <main class="container">
        <h1>UiOBot Dashboard</h1>
        <nav>
            <a href="/" class="button button-clear">Courses</a>
            <a href="/runs" class="button button-outline">Run Logs</a>
        </nav>

        <h2>Run Logs <span class="count">({} shown)</span></h2>
        <table>
            <thead>
                <tr>
                    <th>ID</th>
                    <th>Timestamp</th>
                    <th>Fetched</th>
                    <th>Added</th>
                    <th>Removed</th>
                    <th>Notified</th>
                    <th>Duration</th>
                </tr>
            </thead>
            <tbody>
                {}
            </tbody>
        </table>
    </main>
</body>
</html>"#,
        runs.len(),
        rows
    )
}

/// Render the run detail HTML
fn render_run_detail(run: &RunLogEntry) -> String {
    let added_list = if run.added_courses.is_empty() {
        "<li>None</li>".to_string()
    } else {
        run.added_courses
            .iter()
            .map(|c| format!("<li>{}</li>", html_escape(c)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let removed_list = if run.removed_courses.is_empty() {
        "<li>None</li>".to_string()
    } else {
        run.removed_courses
            .iter()
            .map(|c| format!("<li>{}</li>", html_escape(c)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Run #{} - UiOBot</title>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/milligram/1.4.1/milligram.min.css">
    <style>
        body {{ padding: 2rem 0; }}
        nav {{ margin-bottom: 2rem; }}
        nav a {{ margin-right: 1rem; }}
        .detail-grid {{ display: grid; grid-template-columns: auto 1fr; gap: 0.5rem 2rem; }}
        .detail-grid dt {{ font-weight: bold; }}
        .badge {{ display: inline-block; padding: 0.2rem 0.5rem; border-radius: 3px; font-size: 0.9rem; }}
        .badge-success {{ background: #d4edda; color: #155724; }}
        .badge-info {{ background: #cce5ff; color: #004085; }}
        .lists {{ display: grid; grid-template-columns: 1fr 1fr; gap: 2rem; margin-top: 2rem; }}
        .lists h4 {{ margin-bottom: 0.5rem; }}
        .added {{ color: green; }}
        .removed {{ color: red; }}
    </style>
</head>
<body>
    <main class="container">
        <h1>UiOBot Dashboard</h1>
        <nav>
            <a href="/" class="button button-clear">Courses</a>
            <a href="/runs" class="button button-outline">Run Logs</a>
        </nav>

        <h2>Run #{}</h2>

        <dl class="detail-grid">
            <dt>Timestamp</dt>
            <dd>{}</dd>

            <dt>Duration</dt>
            <dd>{}ms</dd>

            <dt>Filter Used</dt>
            <dd>{}</dd>

            <dt>Courses Fetched</dt>
            <dd>{}</dd>

            <dt>Raw Changes</dt>
            <dd>+{} / -{}</dd>

            <dt>Filtered Changes</dt>
            <dd>+{} / -{}</dd>

            <dt>Notification Sent</dt>
            <dd>{}</dd>

            <dt>First Run</dt>
            <dd>{}</dd>
        </dl>

        <div class="lists">
            <div>
                <h4 class="added">Added Courses (+{})</h4>
                <ul>{}</ul>
            </div>
            <div>
                <h4 class="removed">Removed Courses (-{})</h4>
                <ul>{}</ul>
            </div>
        </div>

        <p><a href="/runs">&larr; Back to Run Logs</a></p>
    </main>
</body>
</html>"#,
        run.id,
        run.id,
        format_timestamp(&run.timestamp),
        run.duration_ms,
        html_escape(&run.filter_used),
        run.total_courses_fetched,
        run.raw_added_count,
        run.raw_removed_count,
        run.filtered_added_count,
        run.filtered_removed_count,
        if run.notification_sent {
            "<span class=\"badge badge-success\">Yes</span>"
        } else {
            "No"
        },
        if run.is_first_run {
            "<span class=\"badge badge-info\">Yes</span>"
        } else {
            "No"
        },
        run.filtered_added_count,
        added_list,
        run.filtered_removed_count,
        removed_list,
    )
}

/// Render an error page
fn render_error(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Error - UiOBot</title>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/milligram/1.4.1/milligram.min.css">
    <style>
        body {{ padding: 2rem 0; }}
        .error {{ color: #dc3545; }}
    </style>
</head>
<body>
    <main class="container">
        <h1>UiOBot Dashboard</h1>
        <p class="error">{}</p>
        <p><a href="/">&larr; Back to Dashboard</a></p>
    </main>
</body>
</html>"#,
        html_escape(message)
    )
}

/// Simple HTML escaping
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Format timestamp for display (truncate to readable format)
fn format_timestamp(ts: &str) -> String {
    // RFC3339 format: 2024-01-15T10:30:00+00:00
    // We want: 2024-01-15 10:30:00
    ts.replace('T', " ")
        .chars()
        .take(19)
        .collect()
}
