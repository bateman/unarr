//! `GET /feed/v3/calendar/Sonarr.ics` (`CalendarFeedController`): the
//! calendar as iCalendar, for the calendar applications that subscribe to
//! the link the Calendar page hands out. Opened with the API key, in the
//! query or in the header.
//!
//! The text is what the originals' library (ical.net 4) writes: the
//! properties of a component in alphabetical order, lines folded at 74
//! characters, CRLF line ends.

use axum::extract::State;
use axum::http::{header, Uri};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use unarr_core::model::{Episode, Series, SeriesType};
use unarr_core::store;

use crate::error::ApiResult;
use crate::query::Params;
use crate::state::AppState;

const CALENDAR_NAME: &str = "Sonarr TV Schedule";
pub(crate) const PRODUCT_ID: &str = "-//github.com/ical-org/ical.net//NONSGML ical.net 4.0//EN";

/// A text value as iCalendar wants it: backslash, semicolon and comma
/// escaped, a line break as `\n`.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// A content line, folded: 74 characters, then lines of a space and 73.
pub(crate) fn line(out: &mut String, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let mut rest = &chars[..];
    let mut first = true;
    loop {
        let room = if first { 74 } else { 73 };
        let take = rest.len().min(room);
        if !first {
            out.push(' ');
        }
        out.extend(&rest[..take]);
        out.push_str("\r\n");
        rest = &rest[take..];
        first = false;
        if rest.is_empty() {
            break;
        }
    }
}

pub(crate) fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// One episode as a `VEVENT`.
fn event(out: &mut String, series: &Series, episode: &Episode, all_day: bool, now: DateTime<Utc>) {
    let Some(airs) = episode.air_date_utc else {
        return;
    };
    line(out, "BEGIN:VEVENT");
    if let Some(network) = series.network.as_deref() {
        line(out, &format!("CATEGORIES:{}", escape(network)));
    }
    if let Some(overview) = episode.overview.as_deref().filter(|o| !o.is_empty()) {
        line(out, &format!("DESCRIPTION:{}", escape(overview)));
    }
    if !all_day {
        line(
            out,
            &format!("DTEND:{}", stamp(airs + Duration::minutes(series.runtime))),
        );
    }
    line(out, &format!("DTSTAMP:{}", stamp(now)));
    if all_day {
        // The day it airs where the server is.
        let day: NaiveDate = airs.with_timezone(&Local).date_naive();
        line(out, &format!("DTSTART;VALUE=DATE:{}", day.format("%Y%m%d")));
    } else {
        line(out, &format!("DTSTART:{}", stamp(airs)));
    }
    line(out, "SEQUENCE:0");
    line(
        out,
        if episode.episode_file_id > 0 {
            "STATUS:CONFIRMED"
        } else {
            "STATUS:TENTATIVE"
        },
    );
    let title = episode.title.as_deref().unwrap_or("");
    let summary = match series.series_type {
        SeriesType::Daily => format!("{} - {title}", series.title),
        _ => format!(
            "{} - {}x{:02} - {title}",
            series.title, episode.season_number, episode.episode_number
        ),
    };
    line(out, &format!("SUMMARY:{}", escape(&summary)));
    line(out, &format!("UID:NzbDrone_episode_{}", episode.id));
    line(out, "END:VEVENT");
}

/// The feed for the episodes given, in the order given.
pub fn render(episodes: &[(Series, Episode)], all_day: bool, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, &format!("NAME:{CALENDAR_NAME}"));
    line(&mut out, &format!("PRODID:{PRODUCT_ID}"));
    line(&mut out, "VERSION:2.0");
    line(&mut out, &format!("X-WR-CALNAME:{CALENDAR_NAME}"));
    for (series, episode) in episodes {
        event(&mut out, series, episode, all_day, now);
    }
    line(&mut out, "END:VCALENDAR");
    out
}

/// Midnight today where the server is, as an instant.
pub(crate) fn local_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let today = now.with_timezone(&Local).date_naive();
    Local
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).expect("midnight exists"))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(now)
}

pub async fn calendar(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    let now = state.now();
    let today = local_midnight(now);
    // Numbers no calendar reaches are refused, not computed with.
    let days = |name: &str, default: i64| Duration::try_days(p.i64(name).unwrap_or(default));
    let range = days("pastDays", 7)
        .and_then(|past| today.checked_sub_signed(past))
        .zip(days("futureDays", 28).and_then(|future| today.checked_add_signed(future)));
    let Some((start, end)) = range else {
        return Err(crate::error::ApiError::BadRequest(
            "pastDays and futureDays are out of range".into(),
        ));
    };
    let unmonitored = p.bool("unmonitored", false);
    let premieres_only = p.bool("premieresOnly", false);
    let all_day = p.bool("asAllDay", false);
    let tags: Vec<String> = p
        .get("tags")
        .unwrap_or("")
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let episodes = state
        .db()
        .call(move |conn| {
            let all_tags = store::catalog::list_tags(conn)?;
            let tag_ids: Vec<i64> = tags
                .iter()
                .filter_map(|t| {
                    t.parse::<i64>().ok().or_else(|| {
                        all_tags
                            .iter()
                            .find(|x| x.label.eq_ignore_ascii_case(t))
                            .map(|x| x.id)
                    })
                })
                .collect();
            let series: std::collections::HashMap<i64, Series> = store::series::list(conn)?
                .into_iter()
                .map(|s| (s.id, s))
                .collect();
            let mut out = Vec::new();
            for episode in store::episodes::episodes_between(conn, start, end, unmonitored)? {
                let Some(series) = series.get(&episode.series_id) else {
                    continue;
                };
                if premieres_only && (episode.season_number == 0 || episode.episode_number != 1) {
                    continue;
                }
                if !tag_ids.is_empty() && !tag_ids.iter().any(|t| series.tags.contains(t)) {
                    continue;
                }
                out.push((series.clone(), episode));
            }
            Ok(out)
        })
        .await?;
    Ok((
        [(header::CONTENT_TYPE, "text/calendar")],
        render(&episodes, all_day, now),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_fold_at_seventy_four_characters() {
        let mut out = String::new();
        line(&mut out, &format!("DESCRIPTION:{}", "x".repeat(200)));
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert_eq!(lines[0].chars().count(), 74);
        assert!(lines[1].starts_with(' ') && lines[1].chars().count() == 74);
        assert_eq!(lines.concat().replace(' ', "").len(), 212);
        // Characters, not bytes.
        let mut out = String::new();
        line(&mut out, &"è".repeat(80));
        assert_eq!(out.split("\r\n").next().unwrap().chars().count(), 74);
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape("One, two; three\\four\r\nfive\nsix"),
            "One\\, two\\; three\\\\four\\nfive\\nsix"
        );
    }
}
