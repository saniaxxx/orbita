//! Core sessions per station conversation.
//!
//! A day-long session grew to 15–30k tokens and the 9B model drowned in it
//! ("ты кто?" → "Чем могу помочь?"). A station now starts a new core session
//! after a pause: follow-ups inside one conversation keep their context
//! ("Хочешь ещё сказку?" → "да"), an unrelated request an hour later starts
//! clean and with a short prompt.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use chrono::{DateTime, FixedOffset, Utc};

/// Russian TTS speaks ~14 characters per second; the pause is counted from
/// the end of playback, not from the end of the turn, so a long tale does not
/// expire the conversation while it is still being read out.
const SPOKEN_MS_PER_CHAR: i64 = 75;
/// The first streamed sentence is not part of the final text; cover it.
const SPOKEN_MARGIN_MS: i64 = 3_000;

#[derive(Debug)]
pub struct StationSessions {
    idle: Duration,
    active: Mutex<HashMap<String, Active>>,
}

#[derive(Debug, Clone)]
struct Active {
    key: String,
    /// When the station is expected to fall silent after the last reply.
    quiet_since: DateTime<Utc>,
}

impl StationSessions {
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            active: Mutex::new(HashMap::new()),
        }
    }

    /// The session key for a turn starting `now`: the current one while the
    /// conversation goes on, a new one after `idle` of silence.
    pub fn begin(&self, duid: &str, now: DateTime<Utc>) -> String {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let idle = chrono::Duration::from_std(self.idle).unwrap_or(chrono::Duration::MAX);
        if let Some(current) = active.get_mut(duid)
            && now - current.quiet_since <= idle
        {
            // Keep the conversation alive while the turn runs.
            current.quiet_since = current.quiet_since.max(now);
            return current.key.clone();
        }
        let key = new_key(duid, now);
        active.insert(
            duid.to_owned(),
            Active {
                key: key.clone(),
                quiet_since: now,
            },
        );
        key
    }

    /// The turn is over at `now` and the station speaks `reply`.
    pub fn finish(&self, duid: &str, now: DateTime<Utc>, reply: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(current) = active.get_mut(duid) {
            let spoken = reply.chars().count() as i64 * SPOKEN_MS_PER_CHAR + SPOKEN_MARGIN_MS;
            current.quiet_since = now + chrono::Duration::milliseconds(spoken);
        }
    }
}

impl Default for StationSessions {
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

/// `station:<duid>:<Moscow start time>`: readable in the OpenClaw dashboard
/// and old sessions can be cleaned up by prefix and age.
fn new_key(duid: &str, now: DateTime<Utc>) -> String {
    let moscow = now.with_timezone(&FixedOffset::east_opt(3 * 3600).expect("valid offset"));
    format!("station:{duid}:{}", moscow.format("%Y-%m-%d-%H%M%S"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_follow_up_within_the_pause_keeps_the_session() {
        let sessions = StationSessions::new(Duration::from_secs(60));
        let first = sessions.begin("d", at("2026-11-08T16:00:00Z"));
        assert_eq!(first, "station:d:2026-11-08-190000");
        sessions.finish("d", at("2026-11-08T16:00:05Z"), "Свет включён.");
        assert_eq!(sessions.begin("d", at("2026-11-08T16:00:50Z")), first);
    }

    #[test]
    fn silence_longer_than_the_pause_starts_a_new_session() {
        let sessions = StationSessions::new(Duration::from_secs(60));
        let first = sessions.begin("d", at("2026-11-08T16:00:00Z"));
        sessions.finish("d", at("2026-11-08T16:00:05Z"), "Свет включён.");
        let next = sessions.begin("d", at("2026-11-08T16:02:00Z"));
        assert_ne!(next, first);
        assert_eq!(next, "station:d:2026-11-08-190200");
    }

    #[test]
    fn the_pause_counts_from_the_end_of_a_long_reply() {
        let sessions = StationSessions::new(Duration::from_secs(60));
        let first = sessions.begin("d", at("2026-11-08T16:00:00Z"));
        // ~1000 characters of tale: ~75 s of speech.
        let tale = "Жили-были кот и пёс. ".repeat(48) + "Хочешь ещё сказку?";
        sessions.finish("d", at("2026-11-08T16:00:06Z"), &tale);
        // The answer comes 2 minutes after the turn, right after the tale ends.
        assert_eq!(sessions.begin("d", at("2026-11-08T16:02:00Z")), first);
    }

    #[test]
    fn stations_have_separate_sessions() {
        let sessions = StationSessions::default();
        let now = at("2026-11-08T16:00:00Z");
        assert_ne!(sessions.begin("a", now), sessions.begin("b", now));
    }
}
