use tracing::{debug, info, instrument};

use crate::config::PointsFilter;
use crate::db::SyncResult;
use crate::models::{Course, ScrapeDiff};

/// Filter sync results based on points criteria
#[instrument(skip(result), fields(
    input_added = result.added.len(),
    input_removed = result.removed.len(),
    filter = %filter.description()
))]
pub fn filter_changes(result: &SyncResult, filter: &PointsFilter) -> ScrapeDiff {
    let added: Vec<Course> = result
        .added
        .iter()
        .filter(|c| {
            let matches = filter.matches(c.points);
            if !matches {
                debug!(
                    course_code = %c.code,
                    points = c.points,
                    filter = %filter.description(),
                    "Added course filtered out"
                );
            }
            matches
        })
        .cloned()
        .collect();

    let removed: Vec<Course> = result
        .removed
        .iter()
        .filter(|c| {
            let matches = filter.matches(c.points);
            if !matches {
                debug!(
                    course_code = %c.code,
                    points = c.points,
                    filter = %filter.description(),
                    "Removed course filtered out"
                );
            }
            matches
        })
        .cloned()
        .collect();

    let diff = ScrapeDiff::new(added.clone(), removed.clone());

    info!(
        filter = %filter.description(),
        input_added = result.added.len(),
        input_removed = result.removed.len(),
        output_added = diff.added.len(),
        output_removed = diff.removed.len(),
        filtered_out_added = result.added.len() - diff.added.len(),
        filtered_out_removed = result.removed.len() - diff.removed.len(),
        added_codes = ?added.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
        removed_codes = ?removed.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
        "Filter applied to changes"
    );

    diff
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_filter_exact() {
        let result = SyncResult {
            added: vec![make_course("A", 2.5), make_course("B", 10.0)],
            removed: vec![make_course("C", 2.5), make_course("D", 5.0)],
            is_first_run: false,
            total_courses: 10,
        };

        let filter = PointsFilter::Exact(2.5);
        let diff = filter_changes(&result, &filter);

        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].code, "A");
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].code, "C");
    }

    #[test]
    fn test_filter_range() {
        let result = SyncResult {
            added: vec![
                make_course("A", 2.5),
                make_course("B", 5.0),
                make_course("C", 10.0),
            ],
            removed: vec![],
            is_first_run: false,
            total_courses: 10,
        };

        let filter = PointsFilter::Range {
            min: None,
            max: Some(5.0),
        };
        let diff = filter_changes(&result, &filter);

        assert_eq!(diff.added.len(), 2);
        assert!(diff.added.iter().any(|c| c.code == "A"));
        assert!(diff.added.iter().any(|c| c.code == "B"));
    }

    #[test]
    fn test_filter_none() {
        let result = SyncResult {
            added: vec![make_course("A", 2.5), make_course("B", 10.0)],
            removed: vec![make_course("C", 5.0)],
            is_first_run: false,
            total_courses: 10,
        };

        let filter = PointsFilter::None;
        let diff = filter_changes(&result, &filter);

        assert_eq!(diff.added.len(), 2);
        assert_eq!(diff.removed.len(), 1);
    }

    /// Test that 2.5 point filter catches both added AND removed courses
    #[test]
    fn test_2_5_point_filter_catches_both_added_and_removed() {
        // Simulate a scenario where:
        // - HFLESER1031 (2.5 pts) was removed from the website
        // - NEWCOURSE (2.5 pts) was added to the website
        // - IN1000 (10 pts) was also added but should NOT be in filtered results
        let result = SyncResult {
            added: vec![
                make_course("NEWCOURSE", 2.5),  // Should be included
                make_course("IN1000", 10.0),    // Should be filtered out
            ],
            removed: vec![
                make_course("HFLESER1031", 2.5), // Should be included
                make_course("JUR1120", 10.0),    // Should be filtered out
            ],
            is_first_run: false,
            total_courses: 100,
        };

        let filter = PointsFilter::Exact(2.5);
        let diff = filter_changes(&result, &filter);

        // Should notify about the NEW 2.5 point course
        assert_eq!(diff.added.len(), 1, "Should have exactly 1 added 2.5pt course");
        assert_eq!(diff.added[0].code, "NEWCOURSE");
        assert_eq!(diff.added[0].points, 2.5);

        // Should notify about the REMOVED 2.5 point course
        assert_eq!(diff.removed.len(), 1, "Should have exactly 1 removed 2.5pt course");
        assert_eq!(diff.removed[0].code, "HFLESER1031");
        assert_eq!(diff.removed[0].points, 2.5);

        // Total changes should be 2 (1 added + 1 removed)
        assert_eq!(diff.total_changes(), 2);
        assert!(!diff.is_empty());
    }

    /// Test that course codes are used for identity (not names or other fields)
    #[test]
    fn test_filter_uses_course_code_for_identity() {
        // Two different courses that happen to have the same name
        let course1 = Course::new(
            "CODE1".to_string(),
            "Same Name".to_string(),
            2.5,
            "https://example.com/1".to_string(),
            "Faculty A".to_string(),
        );
        let course2 = Course::new(
            "CODE2".to_string(),
            "Same Name".to_string(), // Same name, different code
            2.5,
            "https://example.com/2".to_string(),
            "Faculty B".to_string(),
        );

        let result = SyncResult {
            added: vec![course1],
            removed: vec![course2],
            is_first_run: false,
            total_courses: 10,
        };

        let filter = PointsFilter::Exact(2.5);
        let diff = filter_changes(&result, &filter);

        // Both should be present because they have different codes
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].code, "CODE1");
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].code, "CODE2");
    }
}
