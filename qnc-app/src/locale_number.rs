pub fn parse_decimal(raw: &str) -> Option<f64> {
    let normalized = normalize_decimal(raw)?;
    normalized
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn normalize_decimal(raw: &str) -> Option<String> {
    let compact: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{00a0}')
        .collect();
    if compact.is_empty() {
        return None;
    }

    let split = compact
        .char_indices()
        .find(|(_, c)| *c == 'e' || *c == 'E')
        .map(|(index, _)| index);
    let (mantissa, exponent) = match split {
        Some(index) => compact.split_at(index),
        None => (compact.as_str(), ""),
    };

    let decimal_index = mantissa
        .char_indices()
        .filter_map(|(index, c)| (c == '.' || c == ',').then_some(index))
        .last();

    let mut out = String::with_capacity(compact.len());
    for (index, c) in mantissa.char_indices() {
        match c {
            '.' | ',' if Some(index) == decimal_index => out.push('.'),
            '.' | ',' => {}
            '\'' | '_' => {}
            _ => out.push(c),
        }
    }
    out.push_str(exponent);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::parse_decimal;

    #[test]
    fn accepts_decimal_dot_and_comma() {
        assert_eq!(parse_decimal("50"), Some(50.0));
        assert_eq!(parse_decimal("29.97"), Some(29.97));
        assert_eq!(parse_decimal("29,97"), Some(29.97));
        assert_eq!(parse_decimal("1.234,56"), Some(1234.56));
    }
}
