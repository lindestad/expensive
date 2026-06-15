use chrono::{DateTime, Local};

pub fn cost(value: f64) -> String {
    format!("${value:.2}")
}

pub fn precise_cost(value: f64) -> String {
    format!("${value:.4}")
}

pub fn integer(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    let first_group = digits.len() % 3;

    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0
            && (idx == first_group || (idx > first_group && (idx - first_group).is_multiple_of(3)))
        {
            output.push(',');
        }
        output.push(ch);
    }

    output
}

pub fn tokens(value: u64) -> String {
    const UNITS: &[(&str, f64)] = &[
        ("T", 1_000_000_000_000.0),
        ("B", 1_000_000_000.0),
        ("M", 1_000_000.0),
        ("K", 1_000.0),
    ];

    for (suffix, size) in UNITS {
        if value as f64 >= *size {
            let scaled = value as f64 / size;
            return if scaled >= 100.0 {
                format!("{scaled:.0}{suffix}")
            } else {
                format!("{scaled:.1}{suffix}")
            };
        }
    }

    value.to_string()
}

pub fn timestamp(value: DateTime<Local>) -> String {
    value.format("%H:%M:%S").to_string()
}

pub fn percent(value: f64, max: f64) -> String {
    if max <= 0.0 {
        return "0.0%".to_string();
    }
    format!("{:.1}%", value / max * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_integer_with_commas() {
        assert_eq!(integer(0), "0");
        assert_eq!(integer(999), "999");
        assert_eq!(integer(1_000), "1,000");
        assert_eq!(integer(1_234_567), "1,234,567");
    }

    #[test]
    fn formats_tokens_compactly() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_500), "1.5K");
        assert_eq!(tokens(7_500_000), "7.5M");
        assert_eq!(tokens(262_700_000), "263M");
    }
}
