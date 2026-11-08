//! Deterministic fast paths for the most common device commands.
//!
//! The parsers live in `orbita-intents` (shared with the station adapter);
//! this module maps them onto the legacy agent's [`Action`]s.

use crate::action::{Action, VolumeChange};

/// Parse a volume command: "громче", "сделай потише", "громкость 5".
pub fn volume(text: &str) -> Option<VolumeChange> {
    orbita_intents::volume(text)
}

/// Parse a native timer: "поставь таймер на 5 минут".
pub fn timer(text: &str) -> Option<Action> {
    orbita_intents::timer(text).map(|(seconds, label)| Action::SetTimer { seconds, label })
}

/// Fast path for any supported action, in priority order.
pub fn action(text: &str) -> Option<Action> {
    if let Some(volume) = volume(text) {
        return Some(Action::SetVolume(volume));
    }
    timer(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_volume_commands() {
        assert_eq!(volume("громче"), Some(VolumeChange::Louder));
        assert_eq!(volume("Сделай погромче!"), Some(VolumeChange::Louder));
        assert_eq!(volume("потише"), Some(VolumeChange::Quieter));
        assert_eq!(volume("убавь громкость"), Some(VolumeChange::Quieter));
        assert_eq!(volume("громкость 5"), Some(VolumeChange::Set(5)));
        assert_eq!(
            volume("установи громкость на 7"),
            Some(VolumeChange::Set(7))
        );
        assert_eq!(volume("громкость 50"), Some(VolumeChange::Set(10)));
        assert_eq!(volume("напомни через 5 минут громче"), None);
        assert_eq!(volume("привет"), None);
    }

    #[test]
    fn reads_timers() {
        assert_eq!(
            timer("поставь таймер на 5 минут"),
            Some(Action::SetTimer {
                seconds: 300,
                label: None
            })
        );
        assert_eq!(
            timer("таймер 30 секунд"),
            Some(Action::SetTimer {
                seconds: 30,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на час"),
            Some(Action::SetTimer {
                seconds: 3600,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на 2 минуты 30 секунд"),
            Some(Action::SetTimer {
                seconds: 150,
                label: None
            })
        );
        assert_eq!(
            timer("поставь таймер на две минуты"),
            Some(Action::SetTimer {
                seconds: 120,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на двадцать пять минут"),
            Some(Action::SetTimer {
                seconds: 1500,
                label: None
            })
        );
        assert_eq!(
            timer("таймер на полчаса"),
            Some(Action::SetTimer {
                seconds: 1800,
                label: None
            })
        );
        assert_eq!(timer("отмени таймер"), None);
        assert_eq!(timer("напомни через 5 минут"), None);
        assert_eq!(timer("привет"), None);
    }

    #[test]
    fn action_prefers_volume() {
        assert_eq!(
            action("громкость 3"),
            Some(Action::SetVolume(VolumeChange::Set(3)))
        );
        assert_eq!(
            action("таймер на 10 минут"),
            Some(Action::SetTimer {
                seconds: 600,
                label: None
            })
        );
        assert_eq!(action("расскажи сказку"), None);
    }
}
