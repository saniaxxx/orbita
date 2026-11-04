//! Russian plurals and spoken durations shared by the fast paths.

/// Pick the plural form for `n` (`1 минуту`, `2 минуты`, `5 минут`).
pub(crate) fn plural<'a>(n: i64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let tail = n.rem_euclid(100);
    if (11..=14).contains(&tail) {
        return many;
    }
    match n.rem_euclid(10) {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

/// `на 2 минуты`, `на 1 час`, `на 30 секунд`.
pub(crate) fn duration_phrase(seconds: u64) -> String {
    if seconds.is_multiple_of(3600) {
        let hours = (seconds / 3600) as i64;
        format!("на {hours} {}", plural(hours, "час", "часа", "часов"))
    } else if seconds.is_multiple_of(60) {
        let minutes = (seconds / 60) as i64;
        format!(
            "на {minutes} {}",
            plural(minutes, "минуту", "минуты", "минут")
        )
    } else {
        let seconds = seconds as i64;
        format!(
            "на {seconds} {}",
            plural(seconds, "секунду", "секунды", "секунд")
        )
    }
}

/// `1 минуту` / `2 часа` for a reminder amount and its unit word.
pub(crate) fn amount_phrase(amount: i64, unit: &str) -> String {
    if unit.starts_with("час") {
        format!("{amount} {}", plural(amount, "час", "часа", "часов"))
    } else {
        format!("{amount} {}", plural(amount, "минуту", "минуты", "минут"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_plural_forms() {
        assert_eq!(plural(1, "час", "часа", "часов"), "час");
        assert_eq!(plural(3, "час", "часа", "часов"), "часа");
        assert_eq!(plural(11, "час", "часа", "часов"), "часов");
        assert_eq!(plural(21, "минуту", "минуты", "минут"), "минуту");
    }

    #[test]
    fn speaks_durations() {
        assert_eq!(duration_phrase(120), "на 2 минуты");
        assert_eq!(duration_phrase(60), "на 1 минуту");
        assert_eq!(duration_phrase(3600), "на 1 час");
        assert_eq!(duration_phrase(30), "на 30 секунд");
        assert_eq!(amount_phrase(25, "минут"), "25 минут");
        assert_eq!(amount_phrase(2, "часа"), "2 часа");
    }
}
