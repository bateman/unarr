//! `GET /feed/v3/calendar/Radarr.ics` (`CalendarFeedController`): the
//! movie calendar as iCalendar, for the calendar applications that
//! subscribe to the link the Calendar page hands out. Opened with the API
//! key, in the query or in the header.
//!
//! A movie with a cinema, digital or physical release in the range gets an
//! all-day event for each of the three dates it has, in the range or not.
//! The text is written as the series feed writes it (`routes::feed`): what
//! the originals' library produces, line for line.

use axum::extract::State;
use axum::http::{header, Uri};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Local, Utc};
use unarr_core::movies::model::{Movie, MovieStatus};
use unarr_core::movies::store as movies;
use unarr_core::store;

use crate::error::ApiResult;
use crate::query::Params;
use crate::routes::feed::{escape, line, local_midnight, stamp, PRODUCT_ID};
use crate::state::AppState;

const CALENDAR_NAME: &str = "Radarr Movies Calendar";

/// One release of a movie as a `VEVENT`, when the movie has that date.
fn event(
    out: &mut String,
    movie: &Movie,
    date: Option<DateTime<Utc>>,
    kind: &str,
    label: &str,
    now: DateTime<Utc>,
) {
    let Some(date) = date else {
        return;
    };
    let md = &movie.metadata;
    // The day where the server is, and the day after it: an all-day event.
    let day = date.with_timezone(&Local).date_naive();
    line(out, "BEGIN:VEVENT");
    if let Some(studio) = md.studio.as_deref() {
        line(out, &format!("CATEGORIES:{}", escape(studio)));
    }
    if let Some(overview) = md.overview.as_deref().filter(|o| !o.is_empty()) {
        line(out, &format!("DESCRIPTION:{}", escape(overview)));
    }
    line(
        out,
        &format!("DTEND;VALUE=DATE:{}", (day + Duration::days(1)).format("%Y%m%d")),
    );
    line(out, &format!("DTSTAMP:{}", stamp(now)));
    line(out, &format!("DTSTART;VALUE=DATE:{}", day.format("%Y%m%d")));
    line(out, "SEQUENCE:0");
    line(
        out,
        if md.status == MovieStatus::Announced {
            "STATUS:TENTATIVE"
        } else {
            "STATUS:CONFIRMED"
        },
    );
    line(
        out,
        &format!("SUMMARY:{}", escape(&format!("{} {label}", md.title))),
    );
    line(out, &format!("UID:Radarr_movie_{}_{kind}", md.id));
    line(out, "END:VEVENT");
}

/// The feed for the movies given, in the order given.
pub fn render(found: &[Movie], now: DateTime<Utc>) -> String {
    let mut out = String::new();
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, &format!("NAME:{CALENDAR_NAME}"));
    line(&mut out, &format!("PRODID:{PRODUCT_ID}"));
    line(&mut out, "VERSION:2.0");
    line(&mut out, &format!("X-WR-CALNAME:{CALENDAR_NAME}"));
    for movie in found {
        let md = &movie.metadata;
        event(
            &mut out,
            movie,
            md.in_cinemas,
            "cinemas",
            "(Theatrical Release)",
            now,
        );
        event(
            &mut out,
            movie,
            md.digital_release,
            "digital",
            "(Digital Release)",
            now,
        );
        event(
            &mut out,
            movie,
            md.physical_release,
            "physical",
            "(Physical Release)",
            now,
        );
    }
    line(&mut out, "END:VCALENDAR");
    out
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
    let tags: Vec<String> = p
        .get("tags")
        .unwrap_or("")
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let found = state
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
            // The range and the monitored flag are the query's.
            let mut found: Vec<Movie> = movies::movies_released_between(conn, start, end, unmonitored)?
                .into_iter()
                .filter(|m| tag_ids.is_empty() || tag_ids.iter().any(|t| m.tags.contains(t)))
                .collect();
            // As they were added.
            found.sort_by_key(|m| (m.added, m.id));
            Ok(found)
        })
        .await?;
    Ok(([(header::CONTENT_TYPE, "text/calendar")], render(&found, now)).into_response())
}
