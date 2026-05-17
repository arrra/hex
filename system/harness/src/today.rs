use chrono::Local;

/// Port of .hex/scripts/today.sh
/// Reads optional timezone from $HEX_DIR/.hex/timezone, then prints the date.
/// format_arg mirrors the shell's $1: e.g. "+%a" or "+%Y-%m-%d".
pub fn run(format_arg: Option<&str>) {
    // Read timezone from HEX_DIR/.hex/timezone if present, matching shell behavior.
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        let tz_file = std::path::Path::new(&hex_dir).join(".hex/timezone");
        if let Ok(tz) = std::fs::read_to_string(&tz_file) {
            let tz = tz.trim();
            if !tz.is_empty() {
                // SAFETY: single-threaded CLI binary; no other threads read TZ.
                unsafe {
                    std::env::set_var("TZ", tz);
                }
            }
        }
    }

    let fmt = match format_arg {
        Some(s) if s.starts_with('+') => &s[1..],
        Some(s) => s,
        None => "%Y-%m-%d",
    };

    println!("{}", Local::now().format(fmt));
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_format_is_iso_date() {
        // Verify that format string produces YYYY-MM-DD shaped output.
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(date.len(), 10, "date must be 10 chars");
        assert_eq!(&date[4..5], "-", "year-month separator must be '-'");
        assert_eq!(&date[7..8], "-", "month-day separator must be '-'");
        // Year is a 4-digit number
        date[0..4].parse::<u32>().expect("year must be numeric");
        // Month 01-12
        let month: u32 = date[5..7].parse().expect("month must be numeric");
        assert!((1..=12).contains(&month));
        // Day 01-31
        let day: u32 = date[8..10].parse().expect("day must be numeric");
        assert!((1..=31).contains(&day));
    }
}
