//! Pure overlap scoring for the auto-match flow.
//!
//! Given a recording's wall-clock window `[rec_start, rec_end]` and the
//! candidate events from `list_upcoming`, score each candidate by
//! seconds of intersection with the recording. The winner is the
//! candidate with the most overlap, *iff* its overlap exceeds the
//! configured threshold of the recording duration.
//!
//! Stays calendar-provider-agnostic — operates on
//! `(start_iso, end_iso)` strings and returns a chosen index. The
//! Tauri command in `commands.rs` does the I/O.

use chrono::{DateTime, Utc};

/// An event must overlap at least this fraction of the recording
/// duration to be considered a match. The user's stated default is
/// 50%; tightening or loosening this is a one-line change.
pub const MATCH_THRESHOLD: f64 = 0.5;

/// Result of scoring all candidates against a recording window.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    /// Index into the input slice. `None` when no candidate overlapped
    /// the threshold.
    pub winner_index: Option<usize>,
    /// Overlap in seconds for the winner (0 if none).
    pub overlap_seconds: i64,
}

/// Score `(start_iso, end_iso)` candidates against the recording
/// window and pick the highest-overlap candidate that exceeds
/// `MATCH_THRESHOLD * recording_duration`.
///
/// Times are parsed as RFC3339 (what Google's calendar API returns,
/// what we capture from `Date.toISOString()` on the frontend).
/// Candidates with malformed timestamps are silently skipped — a
/// single bad row from Google must not block matching against the
/// rest.
///
/// Tied scores deterministically prefer the earlier index, which
/// equals the order Google returned (start-time-ascending per the
/// `orderBy=startTime` query).
pub fn score_candidates(
    rec_start: DateTime<Utc>,
    rec_end: DateTime<Utc>,
    candidates: &[(String, String)],
) -> MatchResult {
    if rec_end <= rec_start {
        return MatchResult { winner_index: None, overlap_seconds: 0 };
    }
    let rec_duration = (rec_end - rec_start).num_seconds().max(1);
    let threshold_seconds = ((rec_duration as f64) * MATCH_THRESHOLD).ceil() as i64;

    let mut best: Option<(usize, i64)> = None;
    for (i, (s, e)) in candidates.iter().enumerate() {
        let Some((ev_start, ev_end)) = parse_pair(s, e) else {
            continue;
        };
        if ev_end <= ev_start {
            continue;
        }
        let overlap_start = ev_start.max(rec_start);
        let overlap_end = ev_end.min(rec_end);
        if overlap_end <= overlap_start {
            continue;
        }
        let overlap = (overlap_end - overlap_start).num_seconds();
        if overlap < threshold_seconds {
            continue;
        }
        match best {
            None => best = Some((i, overlap)),
            Some((_, b)) if overlap > b => best = Some((i, overlap)),
            _ => {}
        }
    }

    match best {
        Some((i, overlap)) => MatchResult {
            winner_index: Some(i),
            overlap_seconds: overlap,
        },
        None => MatchResult { winner_index: None, overlap_seconds: 0 },
    }
}

fn parse_pair(s: &str, e: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc);
    let end = DateTime::parse_from_rfc3339(e).ok()?.with_timezone(&Utc);
    Some((start, end))
}

/// Returns true when a meeting title matches the auto-generated shape
/// `Meeting DD_MM_YY_HH_MM_SS` produced by `useRecordingStart`'s
/// `generateMeetingTitle`. We only overwrite titles that match this
/// pattern — never a title the user (or a summary's
/// `extract_meeting_name_from_markdown`) has already set.
pub fn is_auto_generated_title(title: &str) -> bool {
    let prefix = "Meeting ";
    if !title.starts_with(prefix) {
        return false;
    }
    let stamp = &title[prefix.len()..];
    if stamp.len() != 17 {
        return false;
    }
    stamp.bytes().enumerate().all(|(i, b)| match i {
        2 | 5 | 8 | 11 | 14 => b == b'_',
        _ => b.is_ascii_digit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn no_candidates_yields_no_match() {
        let r = score_candidates(t("2026-05-04T15:00:00Z"), t("2026-05-04T16:00:00Z"), &[]);
        assert_eq!(r.winner_index, None);
    }

    #[test]
    fn single_full_overlap_wins() {
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T16:00:00Z"),
            &[(
                "2026-05-04T15:00:00Z".into(),
                "2026-05-04T16:00:00Z".into(),
            )],
        );
        assert_eq!(r.winner_index, Some(0));
        assert_eq!(r.overlap_seconds, 3600);
    }

    #[test]
    fn below_threshold_is_rejected() {
        // 60-min recording, 20-min event overlap = 33% < 50% threshold.
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T16:00:00Z"),
            &[(
                "2026-05-04T14:50:00Z".into(),
                "2026-05-04T15:20:00Z".into(),
            )],
        );
        assert_eq!(r.winner_index, None);
    }

    #[test]
    fn highest_overlap_wins_among_two_above_threshold() {
        // Recording: 15:00–16:00 (60 min)
        // Event A:  14:30–15:45 → overlap 45 min (75%) ← wins
        // Event B:  15:30–17:00 → overlap 30 min (50%) ← also above threshold
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T16:00:00Z"),
            &[
                ("2026-05-04T14:30:00Z".into(), "2026-05-04T15:45:00Z".into()),
                ("2026-05-04T15:30:00Z".into(), "2026-05-04T17:00:00Z".into()),
            ],
        );
        assert_eq!(r.winner_index, Some(0));
        assert_eq!(r.overlap_seconds, 45 * 60);
    }

    #[test]
    fn ties_go_to_earlier_index() {
        // Both events overlap 60 minutes exactly.
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T16:00:00Z"),
            &[
                ("2026-05-04T15:00:00Z".into(), "2026-05-04T16:00:00Z".into()),
                ("2026-05-04T14:00:00Z".into(), "2026-05-04T17:00:00Z".into()),
            ],
        );
        assert_eq!(r.winner_index, Some(0));
    }

    #[test]
    fn malformed_timestamps_are_skipped_not_fatal() {
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T16:00:00Z"),
            &[
                ("not a date".into(), "also not a date".into()),
                (
                    "2026-05-04T15:00:00Z".into(),
                    "2026-05-04T16:00:00Z".into(),
                ),
            ],
        );
        assert_eq!(r.winner_index, Some(1));
    }

    #[test]
    fn zero_or_negative_recording_duration_yields_no_match() {
        let r = score_candidates(
            t("2026-05-04T15:00:00Z"),
            t("2026-05-04T15:00:00Z"),
            &[("2026-05-04T15:00:00Z".into(), "2026-05-04T16:00:00Z".into())],
        );
        assert_eq!(r.winner_index, None);
    }

    #[test]
    fn recognizes_auto_generated_meeting_title() {
        assert!(is_auto_generated_title("Meeting 04_05_26_15_03_47"));
        assert!(is_auto_generated_title("Meeting 31_12_99_23_59_59"));
    }

    #[test]
    fn rejects_user_edited_titles() {
        assert!(!is_auto_generated_title("Sprint Review"));
        assert!(!is_auto_generated_title("Meeting"));
        assert!(!is_auto_generated_title("Meeting 04_05_26"));
        assert!(!is_auto_generated_title("Meeting 04-05-26-15-03-47"));
        assert!(!is_auto_generated_title("Meeting xx_05_26_15_03_47"));
        // Length matches but contains non-digits in number positions.
        assert!(!is_auto_generated_title("Meeting 04_05_26_15_03_4x"));
    }
}
