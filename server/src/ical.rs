//! Reading iCalendar (.ics) text and working out when its events happen.
//!
//! Nothing here touches the network or the disk: [`parse`] turns the text of a
//! calendar into [`Event`]s, and [`Event::between`] lists when one happens in a
//! stretch of time, repeats included.
//!
//! Times come in three kinds and each stays what it is all the way to the page:
//! a whole day, a clock time with no zone (the same on every wall), and a moment
//! in a named zone. The first two are laid on the UTC time line only to do
//! arithmetic with them.

use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};
use serde_json::{json, Value};

/// Most events kept from one calendar.
const MAX_EVENTS: usize = 20_000;
/// Most times one repeating event is listed in a single answer.
const MAX_REPEATS: u16 = 400;
const MAX_TEXT: usize = 200;
const DAY: i64 = 24 * 60 * 60;

/// When something starts or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// A whole day, wherever the reader is.
    Day(NaiveDate),
    /// A time on the clock, wherever the reader is.
    Local(NaiveDateTime),
    /// A moment in time, in seconds since 1970.
    Instant(i64),
}

impl Moment {
    /// For the page: a day as `YYYY-MM-DD`, a clock time as `YYYY-MM-DDTHH:MM:SS`, a moment as a number.
    pub fn view(self) -> Value {
        match self {
            Moment::Day(day) => json!(day.format("%Y-%m-%d").to_string()),
            Moment::Local(time) => json!(time.format("%Y-%m-%dT%H:%M:%S").to_string()),
            Moment::Instant(secs) => json!(secs),
        }
    }
}

/// When an event first starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Day(NaiveDate),
    Local(NaiveDateTime),
    Zoned(DateTime<Tz>),
}

impl Start {
    /// Where this sits on the time line that repeats are worked out on.
    fn on_line(self) -> DateTime<Tz> {
        match self {
            Start::Day(day) => Tz::UTC.from_utc_datetime(&day.and_time(NaiveTime::MIN)),
            Start::Local(time) => Tz::UTC.from_utc_datetime(&time),
            Start::Zoned(at) => at,
        }
    }

    /// A point on that time line, as the same kind of moment this start is.
    fn moment(self, at: DateTime<Tz>) -> Moment {
        match self {
            Start::Day(_) => Moment::Day(at.naive_utc().date()),
            Start::Local(_) => Moment::Local(at.naive_utc()),
            Start::Zoned(_) => Moment::Instant(at.timestamp()),
        }
    }
}

/// Something on a calendar, with every time it happens.
#[derive(Clone, Debug)]
pub struct Event {
    pub title: String,
    pub place: Option<String>,
    start: Start,
    /// How long it lasts each time.
    length: Duration,
    /// How it repeats, if it does.
    rule: Option<RRuleSet>,
    /// Further times it happens, beyond the start and the rule.
    extra: Vec<DateTime<Tz>>,
    /// Times it would happen but does not.
    skipped: Vec<DateTime<Tz>>,
}

/// One time an event happens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub title: String,
    pub place: Option<String>,
    pub start: Moment,
    /// For whole days, the day after the last one.
    pub end: Moment,
    /// Where it falls in time, near enough to put occurrences in order.
    pub order: i64,
}

impl Event {
    /// An event with a title and a start, repeating by `rule` (the text of an RRULE) if one is given.
    pub fn new(title: &str, start: Start, rule: Option<&str>) -> Event {
        let length = if matches!(start, Start::Day(_)) { Duration::days(1) } else { Duration::zero() };
        Event {
            title: title.to_string(),
            place: None,
            start,
            length,
            rule: rule.and_then(|text| repeats(text, start)),
            extra: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// Every time this happens between `from` and `to` (seconds since 1970).
    ///
    /// Whole days and clock times have no zone, so they are matched a day generously at each
    /// end; whoever shows them knows which days they mean.
    pub fn between(&self, from: i64, to: i64) -> Vec<Occurrence> {
        let on_line = |secs: i64| Tz::UTC.timestamp_opt(secs, 0).single();
        let slack = if matches!(self.start, Start::Zoned(_)) { 0 } else { DAY };
        let earliest = from.saturating_sub(slack).saturating_sub(self.length.num_seconds());
        let (Some(after), Some(before)) = (on_line(earliest), on_line(to.saturating_add(slack))) else {
            return Vec::new();
        };
        let mut starts = match &self.rule {
            Some(rule) => rule.clone().after(after).before(before).all(MAX_REPEATS).dates,
            None => vec![self.start.on_line()],
        };
        starts.extend(self.extra.iter().copied());
        starts.retain(|at| (after..=before).contains(at) && !self.skipped.contains(at));
        starts.sort();
        starts.dedup();
        starts
            .into_iter()
            .map(|at| Occurrence {
                title: self.title.clone(),
                place: self.place.clone(),
                start: self.start.moment(at),
                end: self.start.moment(at + self.length),
                order: at.timestamp(),
            })
            .collect()
    }
}

/// The rule in `text` as a set of times starting at `start`, or None if it cannot be followed
/// (the event then happens once, which is better than not at all).
fn repeats(text: &str, start: Start) -> Option<RRuleSet> {
    let rule = RRule::<Unvalidated>::from_str(&utc_until(text)).ok()?;
    rule.build(start.on_line()).ok()
}

/// A rule's UNTIL has to be in UTC for the rule to be accepted, and many calendars write a
/// bare date or clock time there. Such a value is taken as UTC, a date to the end of its day.
fn utc_until(rule: &str) -> String {
    rule.split(';')
        .map(|part| match part.split_once('=') {
            Some((name, value)) if name.eq_ignore_ascii_case("UNTIL") && !value.ends_with(['Z', 'z']) => {
                if value.contains(['T', 't']) {
                    format!("{name}={value}Z")
                } else {
                    format!("{name}={value}T235959Z")
                }
            }
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

// --- reading the text -------------------------------------------------------------

/// What a calendar file holds.
#[derive(Clone, Debug, Default)]
pub struct Calendar {
    /// What the calendar calls itself, if it says.
    pub name: Option<String>,
    pub events: Vec<Event>,
}

/// One line of a calendar: `NAME;PARAMETER=value:value`.
struct Line<'a> {
    name: &'a str,
    parameters: &'a str,
    value: &'a str,
}

impl<'a> Line<'a> {
    fn parse(text: &'a str) -> Option<Line<'a>> {
        // The value starts at the first colon outside quotes (a quoted parameter may hold one).
        let mut quoted = false;
        let colon = text.char_indices().find_map(|(i, c)| {
            quoted ^= c == '"';
            (c == ':' && !quoted).then_some(i)
        })?;
        let (head, value) = (&text[..colon], &text[colon + 1..]);
        let (name, parameters) = head.split_once(';').unwrap_or((head, ""));
        Some(Line { name, parameters, value })
    }

    fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    fn parameter(&self, wanted: &str) -> Option<&'a str> {
        self.parameters.split(';').find_map(|parameter| {
            let (name, value) = parameter.split_once('=')?;
            name.eq_ignore_ascii_case(wanted).then(|| value.trim_matches('"'))
        })
    }

    /// The times this line gives; most give one, EXDATE and RDATE may list several.
    fn times(&self) -> Vec<Start> {
        let zone = self.parameter("TZID");
        self.value.split(',').filter_map(|text| time(text.trim(), zone)).collect()
    }
}

/// `YYYYMMDD`, `YYYYMMDDTHHMMSS` or `YYYYMMDDTHHMMSSZ`, in `zone` if one is named and known.
fn time(text: &str, zone: Option<&str>) -> Option<Start> {
    if !text.contains(['T', 't']) {
        return NaiveDate::parse_from_str(text, "%Y%m%d").ok().map(Start::Day);
    }
    let (clock, utc) = match text.strip_suffix(['Z', 'z']) {
        Some(clock) => (clock, true),
        None => (text, false),
    };
    let naive = NaiveDateTime::parse_from_str(&clock.to_ascii_uppercase(), "%Y%m%dT%H%M%S").ok()?;
    if utc {
        return Some(Start::Zoned(Tz::UTC.from_utc_datetime(&naive)));
    }
    let Some(zone) = zone.and_then(|name| chrono_tz::Tz::from_str(name).ok()) else {
        // No zone, or one we do not know: the clock time is the best reading there is.
        return Some(Start::Local(naive));
    };
    let zone = Tz::Tz(zone);
    // A time the clocks skip in spring is taken an hour later, where they land.
    let at = zone
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| zone.from_local_datetime(&(naive + Duration::hours(1))).earliest())?;
    Some(Start::Zoned(at))
}

/// A length of time such as `PT1H30M`, `P2D` or `P1W`.
fn duration(text: &str) -> Option<Duration> {
    let rest = text.strip_prefix(['P', 'p'])?;
    let (mut seconds, mut digits, mut in_time) = (0i64, String::new(), false);
    for c in rest.chars() {
        match c.to_ascii_uppercase() {
            'T' => in_time = true,
            c if c.is_ascii_digit() => digits.push(c),
            unit => {
                let count: i64 = digits.parse().ok()?;
                digits.clear();
                let each = match (unit, in_time) {
                    ('W', false) => 7 * DAY,
                    ('D', false) => DAY,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                seconds = seconds.checked_add(count.checked_mul(each)?)?;
            }
        }
    }
    digits.is_empty().then(|| Duration::try_seconds(seconds)).flatten()
}

/// Text as written in a calendar, with its escapes undone and squeezed onto one short line.
fn text(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n' | 'N') => out.push(' '),
                Some(other) => out.push(other),
                None => {}
            },
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    let line = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then(|| line.chars().take(MAX_TEXT).collect())
}

/// The lines of a calendar, with long lines that were folded over several put back together.
fn unfolded(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.lines() {
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(more), Some(last)) => last.push_str(more),
            _ => lines.push(raw.to_string()),
        }
    }
    lines
}

/// An event as it is being read, before we know whether it is complete.
#[derive(Default)]
struct Draft {
    uid: Option<String>,
    title: Option<String>,
    place: Option<String>,
    start: Option<Start>,
    end: Option<Start>,
    length: Option<Duration>,
    rule: Option<String>,
    extra: Vec<Start>,
    skipped: Vec<Start>,
    /// Set when this stands in for one time of a repeating event (a moved or edited one).
    replaces: Option<Start>,
    cancelled: bool,
}

impl Draft {
    fn read(&mut self, line: &Line) {
        if line.is("SUMMARY") {
            self.title = text(line.value);
        } else if line.is("LOCATION") {
            self.place = text(line.value);
        } else if line.is("UID") {
            self.uid = Some(line.value.to_string());
        } else if line.is("DTSTART") {
            self.start = line.times().first().copied();
        } else if line.is("DTEND") {
            self.end = line.times().first().copied();
        } else if line.is("DURATION") {
            self.length = duration(line.value);
        } else if line.is("RRULE") {
            self.rule = Some(line.value.to_string());
        } else if line.is("RDATE") {
            self.extra.extend(line.times());
        } else if line.is("EXDATE") {
            self.skipped.extend(line.times());
        } else if line.is("RECURRENCE-ID") {
            self.replaces = line.times().first().copied();
        } else if line.is("STATUS") {
            self.cancelled = line.value.eq_ignore_ascii_case("CANCELLED");
        }
    }

    /// The finished event, if it has a start.
    fn finish(self) -> Option<Finished> {
        let start = self.start?;
        let first = start.on_line();
        let given = self.end.map(|end| end.on_line() - first).or(self.length);
        let whole_day = if matches!(start, Start::Day(_)) { Duration::days(1) } else { Duration::zero() };
        let length = given.filter(|length| *length > Duration::zero()).unwrap_or(whole_day);
        // A stand-in happens once, whatever the event it stands in for does.
        let rule = self.rule.filter(|_| self.replaces.is_none()).and_then(|rule| repeats(&rule, start));
        let event = Event {
            title: self.title.unwrap_or_else(|| "(no title)".to_string()),
            place: self.place,
            start,
            length,
            rule,
            extra: self.extra.into_iter().map(Start::on_line).collect(),
            skipped: self.skipped.into_iter().map(Start::on_line).collect(),
        };
        Some(Finished { event, uid: self.uid, replaces: self.replaces.map(Start::on_line), cancelled: self.cancelled })
    }
}

/// An event that has been read, with what is needed to fit it in among the others.
struct Finished {
    event: Event,
    uid: Option<String>,
    /// The time of the repeating event with the same UID that this one stands in for.
    replaces: Option<DateTime<Tz>>,
    cancelled: bool,
}

/// Read the text of an iCalendar file. Whatever cannot be understood is left out; an event
/// whose repeat rule cannot be followed is kept as happening once.
pub fn parse(ics: &str) -> Calendar {
    let mut calendar = Calendar::default();
    // Events by UID, so that a moved or cancelled time can be struck from the event it belongs to.
    let mut with_uid: Vec<(String, usize)> = Vec::new();
    let mut struck: Vec<(String, DateTime<Tz>)> = Vec::new();
    let mut draft: Option<Draft> = None;
    // How deep we are inside parts of an event (such as its alarms), whose lines are not the event's.
    let mut nested = 0usize;
    for content in unfolded(ics) {
        let Some(line) = Line::parse(&content) else { continue };
        if line.is("BEGIN") {
            if line.value.eq_ignore_ascii_case("VEVENT") && draft.is_none() {
                draft = Some(Draft::default());
            } else if draft.is_some() {
                nested += 1;
            }
        } else if line.is("END") {
            if nested > 0 {
                nested -= 1;
            } else if line.value.eq_ignore_ascii_case("VEVENT") {
                let Some(read) = draft.take().and_then(Draft::finish) else { continue };
                // A stand-in strikes the time it stands in for, even when it is itself cancelled.
                struck.extend(read.uid.clone().zip(read.replaces));
                if read.cancelled || calendar.events.len() >= MAX_EVENTS {
                    continue;
                }
                if let (Some(uid), None) = (read.uid, read.replaces) {
                    with_uid.push((uid, calendar.events.len()));
                }
                calendar.events.push(read.event);
            }
        } else if let Some(event) = draft.as_mut().filter(|_| nested == 0) {
            event.read(&line);
        } else if draft.is_none() && line.is("X-WR-CALNAME") {
            calendar.name = text(line.value);
        }
    }
    for (uid, at) in struck {
        for (_, index) in with_uid.iter().filter(|(known, _)| *known == uid) {
            calendar.events[*index].skipped.push(at);
        }
    }
    calendar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn clock(year: i32, month: u32, date: u32, hour: u32, minute: u32) -> NaiveDateTime {
        day(year, month, date).and_hms_opt(hour, minute, 0).unwrap()
    }

    /// Seconds since 1970 of a UTC time.
    fn utc(year: i32, month: u32, date: u32, hour: u32, minute: u32) -> i64 {
        clock(year, month, date, hour, minute).and_utc().timestamp()
    }

    fn calendar(events: &str) -> Calendar {
        parse(&format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Family\r\n{events}END:VCALENDAR\r\n"))
    }

    fn event(lines: &str) -> String {
        format!("BEGIN:VEVENT\r\n{}\r\nEND:VEVENT\r\n", lines.trim().replace('\n', "\r\n"))
    }

    /// When the only event in `lines` starts during October 2026.
    fn starts_in_october(lines: &str) -> Vec<Moment> {
        let found = calendar(&event(lines));
        assert_eq!(found.events.len(), 1, "{lines}");
        found.events[0].between(utc(2026, 10, 1, 0, 0), utc(2026, 11, 1, 0, 0)).into_iter().map(|o| o.start).collect()
    }

    #[test]
    fn a_plain_event_keeps_its_title_place_and_times() {
        let found = calendar(&event(
            "UID:1\nSUMMARY:Dentist\\, again\nLOCATION:12 High St\\nLisbon\nDTSTART:20261013T140000Z\nDTEND:20261013T150000Z",
        ));
        assert_eq!(found.name.as_deref(), Some("Family"));
        let all = found.events[0].between(utc(2026, 10, 13, 0, 0), utc(2026, 10, 14, 0, 0));
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].title.as_str(), all[0].place.as_deref()), ("Dentist, again", Some("12 High St Lisbon")));
        assert_eq!(all[0].start, Moment::Instant(utc(2026, 10, 13, 14, 0)));
        assert_eq!(all[0].end, Moment::Instant(utc(2026, 10, 13, 15, 0)));
        assert_eq!(all[0].order, utc(2026, 10, 13, 14, 0));
        assert!(found.events[0].between(utc(2026, 11, 1, 0, 0), utc(2026, 12, 1, 0, 0)).is_empty());
    }

    #[test]
    fn the_three_kinds_of_time_stay_what_they_are() {
        assert_eq!(starts_in_october("DTSTART;VALUE=DATE:20261013"), [Moment::Day(day(2026, 10, 13))]);
        assert_eq!(starts_in_october("DTSTART:20261013T090000"), [Moment::Local(clock(2026, 10, 13, 9, 0))]);
        // Lisbon is an hour ahead of UTC until the clocks go back on 25 October 2026.
        assert_eq!(
            starts_in_october("DTSTART;TZID=Europe/Lisbon:20261013T090000"),
            [Moment::Instant(utc(2026, 10, 13, 8, 0))]
        );
        assert_eq!(
            starts_in_october("DTSTART;TZID=\"Europe/Lisbon\":20261027T090000"),
            [Moment::Instant(utc(2026, 10, 27, 9, 0))]
        );
        assert_eq!(
            starts_in_october("DTSTART;TZID=GMT Standard Time:20261013T090000"),
            [Moment::Local(clock(2026, 10, 13, 9, 0))],
            "a zone we do not know is read as the clock time"
        );
        assert_eq!(Moment::Day(day(2026, 10, 3)).view(), json!("2026-10-03"));
        assert_eq!(Moment::Local(clock(2026, 10, 3, 9, 5)).view(), json!("2026-10-03T09:05:00"));
        assert_eq!(Moment::Instant(5).view(), json!(5));
    }

    #[test]
    fn a_whole_day_event_ends_the_day_after_its_last_day() {
        let found = calendar(&event("SUMMARY:Holiday\nDTSTART;VALUE=DATE:20261010\nDTEND;VALUE=DATE:20261013"));
        let all = found.events[0].between(utc(2026, 10, 12, 0, 0), utc(2026, 10, 12, 12, 0));
        assert_eq!(all.len(), 1, "an event that began before the window and is still on is listed");
        assert_eq!((all[0].start, all[0].end), (Moment::Day(day(2026, 10, 10)), Moment::Day(day(2026, 10, 13))));
        let one = calendar(&event("DTSTART;VALUE=DATE:20261010"));
        let all = one.events[0].between(utc(2026, 10, 10, 0, 0), utc(2026, 10, 11, 0, 0));
        assert_eq!(all[0].end, Moment::Day(day(2026, 10, 11)), "one day when no end is given");
        assert_eq!(all[0].title, "(no title)");
    }

    #[test]
    fn length_can_be_given_as_a_duration() {
        let found = calendar(&event("DTSTART:20261013T140000Z\nDURATION:PT1H30M"));
        let all = found.events[0].between(utc(2026, 10, 13, 0, 0), utc(2026, 10, 14, 0, 0));
        assert_eq!(all[0].end, Moment::Instant(utc(2026, 10, 13, 15, 30)));
        assert_eq!(duration("P1W"), Some(Duration::weeks(1)));
        assert_eq!(duration("P2DT3H4M5S"), Some(Duration::seconds(2 * 86_400 + 3 * 3600 + 4 * 60 + 5)));
        for bad in ["", "1H", "PT", "P1H", "PT1D", "PT5", "PTxM", "P99999999999999999999D"] {
            assert!(duration(bad).is_none_or(|d| d.is_zero()), "{bad:?}");
        }
    }

    #[test]
    fn repeating_events_happen_every_time() {
        let weekly = starts_in_october("DTSTART;TZID=Europe/Lisbon:20260907T180000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE");
        assert_eq!(weekly.len(), 8, "Mondays and Wednesdays of October 2026, less the edges: {weekly:?}");
        assert_eq!(weekly[0], Moment::Instant(utc(2026, 10, 5, 17, 0)));
        assert_eq!(
            weekly[7],
            Moment::Instant(utc(2026, 10, 28, 18, 0)),
            "still 18:00 in Lisbon after the clocks change"
        );

        let birthday = starts_in_october("DTSTART;VALUE=DATE:19900113\nRRULE:FREQ=YEARLY");
        assert!(birthday.is_empty());
        let birthday = starts_in_october("DTSTART;VALUE=DATE:19901013\nRRULE:FREQ=YEARLY");
        assert_eq!(birthday, [Moment::Day(day(2026, 10, 13))]);

        let counted = starts_in_october("DTSTART:20261001T080000\nRRULE:FREQ=DAILY;COUNT=3");
        assert_eq!(counted.len(), 3);
        let monthly = starts_in_october("DTSTART:20260106T200000Z\nRRULE:FREQ=MONTHLY;BYDAY=1TU");
        assert_eq!(monthly, [Moment::Instant(utc(2026, 10, 6, 20, 0))], "the first Tuesday");
    }

    #[test]
    fn a_rule_stops_at_its_until_however_that_is_written() {
        for until in ["20261008", "20261008T235959", "20261008T235959Z"] {
            let found = starts_in_october(&format!("DTSTART;VALUE=DATE:20261005\nRRULE:FREQ=DAILY;UNTIL={until}"));
            assert_eq!(found.len(), 4, "{until}: {found:?}");
        }
        let zoned = starts_in_october("DTSTART;TZID=Europe/Lisbon:20261005T090000\nRRULE:FREQ=DAILY;UNTIL=20261007");
        assert_eq!(zoned.len(), 3);
        assert_eq!(utc_until("FREQ=DAILY;until=20261008;INTERVAL=2"), "FREQ=DAILY;until=20261008T235959Z;INTERVAL=2");
    }

    #[test]
    fn a_rule_that_cannot_be_followed_leaves_the_event_happening_once() {
        for rule in ["FREQ=SOMETIMES", "nonsense", "FREQ=DAILY;UNTIL=19990101T000000Z"] {
            let found = starts_in_october(&format!("DTSTART:20261013T090000Z\nRRULE:{rule}"));
            assert_eq!(found, [Moment::Instant(utc(2026, 10, 13, 9, 0))], "{rule}");
        }
    }

    #[test]
    fn skipped_and_extra_times_are_honoured() {
        let found = starts_in_october(
            "DTSTART;TZID=Europe/Lisbon:20261005T090000\nRRULE:FREQ=DAILY;COUNT=4\n\
             EXDATE;TZID=Europe/Lisbon:20261006T090000,20261007T090000\nRDATE;TZID=Europe/Lisbon:20261020T100000",
        );
        assert_eq!(
            found,
            [
                Moment::Instant(utc(2026, 10, 5, 8, 0)),
                Moment::Instant(utc(2026, 10, 8, 8, 0)),
                Moment::Instant(utc(2026, 10, 20, 9, 0))
            ]
        );
        let extra_only = starts_in_october("DTSTART;VALUE=DATE:20261005\nRDATE;VALUE=DATE:20261009");
        assert_eq!(extra_only, [Moment::Day(day(2026, 10, 5)), Moment::Day(day(2026, 10, 9))]);
    }

    #[test]
    fn a_moved_time_replaces_the_one_it_stands_in_for() {
        let found = calendar(&format!(
            "{}{}{}",
            event("UID:swim\nSUMMARY:Swimming\nDTSTART:20261005T170000Z\nRRULE:FREQ=WEEKLY;COUNT=3"),
            event("UID:swim\nSUMMARY:Swimming (moved)\nRECURRENCE-ID:20261012T170000Z\nDTSTART:20261013T180000Z\nRRULE:FREQ=WEEKLY"),
            event("UID:swim\nSTATUS:CANCELLED\nRECURRENCE-ID:20261019T170000Z\nDTSTART:20261019T170000Z"),
        ));
        assert_eq!(found.events.len(), 2, "the cancelled time is not an event");
        let mut all: Vec<(String, Moment)> = found
            .events
            .iter()
            .flat_map(|e| e.between(utc(2026, 10, 1, 0, 0), utc(2026, 11, 1, 0, 0)))
            .map(|o| (o.title, o.start))
            .collect();
        all.sort_by_key(|(title, _)| title.clone());
        assert_eq!(
            all,
            [
                ("Swimming".to_string(), Moment::Instant(utc(2026, 10, 5, 17, 0))),
                ("Swimming (moved)".to_string(), Moment::Instant(utc(2026, 10, 13, 18, 0))),
            ]
        );
    }

    #[test]
    fn cancelled_events_and_events_without_a_start_are_left_out() {
        let found = calendar(&format!(
            "{}{}{}",
            event("SUMMARY:Off\nSTATUS:CANCELLED\nDTSTART:20261013T090000Z"),
            event("SUMMARY:No start"),
            event("SUMMARY:Bad start\nDTSTART:tomorrow"),
        ));
        assert!(found.events.is_empty());
    }

    #[test]
    fn folded_lines_alarms_and_untidy_text_are_read_correctly() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:A very long\n  title that was\n\t folded\n\
                   DTSTART:20261013T090000Z\nBEGIN:VALARM\nSUMMARY:Reminder\nDTSTART:20200101T000000Z\n\
                   TRIGGER:-PT15M\nEND:VALARM\nDESCRIPTION:see https://example.org/a:b\nEND:VEVENT\n\
                   this line means nothing\nBEGIN:VEVENT\nsummary;LANGUAGE=en:lower case names\ndtstart:20261014t100000z\nEND:VEVENT\n";
        let found = parse(ics);
        assert_eq!(found.name, None);
        let titles: Vec<&str> = found.events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["A very long title that was folded", "lower case names"]);
        let first = found.events[0].between(utc(2026, 10, 1, 0, 0), utc(2026, 11, 1, 0, 0));
        assert_eq!(first[0].start, Moment::Instant(utc(2026, 10, 13, 9, 0)), "the alarm's lines are not the event's");
        assert_eq!(text("a\\\\b\\;c\\"), Some("a\\b;c".to_string()));
        assert_eq!(text(" \\n "), None);
        assert_eq!(text(&"x".repeat(500)).unwrap().len(), MAX_TEXT);
        assert!(parse("").events.is_empty());
        assert!(parse("not a calendar at all").events.is_empty());
    }

    #[test]
    fn a_time_the_clocks_skip_lands_an_hour_later() {
        // Lisbon skips 01:00 to 02:00 on 29 March 2026.
        let skipped = time("20260329T013000", Some("Europe/Lisbon"));
        assert_eq!(skipped.map(Start::on_line).map(|at| at.timestamp()), Some(utc(2026, 3, 29, 1, 30)));
        assert_eq!(time("2026-10-13", None), None);
        assert_eq!(time("20261340T000000", None), None);
    }

    #[test]
    fn events_made_here_repeat_by_a_simple_rule() {
        let party = Event::new("Party", Start::Local(clock(2026, 10, 13, 19, 30)), None);
        let once = party.between(utc(2026, 10, 1, 0, 0), utc(2026, 11, 1, 0, 0));
        assert_eq!(
            (once.len(), once[0].start, once[0].end),
            (1, once[0].end, Moment::Local(clock(2026, 10, 13, 19, 30)))
        );
        let bins = Event::new("Bins", Start::Day(day(2026, 10, 1)), Some("FREQ=WEEKLY"));
        let weeks = bins.between(utc(2026, 10, 1, 0, 0), utc(2026, 10, 31, 0, 0));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[4].end, Moment::Day(day(2026, 10, 30)));
        assert!(bins.between(i64::MAX - 5, i64::MAX).is_empty(), "a window beyond the calendar is simply empty");
    }
}
