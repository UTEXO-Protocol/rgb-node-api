pub mod number_from_string {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Display,
        S: Serializer,
    {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

pub mod bytevec_as_hex {
    use std::fmt;

    use serde::{Deserializer, Serializer, de};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_string = hex::encode(bytes);
        serializer.serialize_str(&hex_string)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HexStringVisitor;

        impl de::Visitor<'_> for HexStringVisitor {
            type Value = Vec<u8>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a hex-encoded string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Vec<u8>, E>
            where
                E: de::Error,
            {
                hex::decode(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(HexStringVisitor)
    }
}

/// (De)serialise a `std::time::Duration` as a human string: `"500ms"`,
/// `"5s"`, `"30m"`, `"1h"`, `"2d"`. Single-unit only.
///
/// Use with `#[serde(with = "api_core::serde_utils::duration_str")]`.
pub mod duration_str {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format(*value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        parse(&raw).map_err(de::Error::custom)
    }

    fn parse(input: &str) -> Result<Duration, String> {
        let s = input.trim();
        let split_at = s
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("duration missing unit: {input:?}"))?;
        if split_at == 0 {
            return Err(format!("duration missing number: {input:?}"));
        }
        let (num, unit) = (s[..split_at].parse::<u64>(), s[split_at..].trim());
        let num = num.map_err(|e| format!("invalid duration number in {input:?}: {e}"))?;

        match unit {
            "ns" => Ok(Duration::from_nanos(num)),
            "us" | "µs" => Ok(Duration::from_micros(num)),
            "ms" => Ok(Duration::from_millis(num)),
            "s" => Ok(Duration::from_secs(num)),
            "m" => Ok(Duration::from_secs(num * 60)),
            "h" => Ok(Duration::from_secs(num * 3_600)),
            "d" => Ok(Duration::from_secs(num * 86_400)),
            other => Err(format!("unknown duration unit {other:?} in {input:?}")),
        }
    }

    fn format(d: Duration) -> String {
        let secs = d.as_secs();
        let nanos = d.subsec_nanos();

        if nanos == 0 && secs > 0 {
            if secs.is_multiple_of(86_400) {
                return format!("{}d", secs / 86_400);
            }
            if secs.is_multiple_of(3_600) {
                return format!("{}h", secs / 3_600);
            }
            if secs.is_multiple_of(60) {
                return format!("{}m", secs / 60);
            }
            return format!("{secs}s");
        }
        if nanos.is_multiple_of(1_000_000) {
            return format!("{}ms", d.as_millis());
        }
        if nanos.is_multiple_of(1_000) {
            return format!("{}us", d.as_micros());
        }
        format!("{}ns", d.as_nanos())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn roundtrip() {
            let cases = [
                ("500ms", Duration::from_millis(500)),
                ("5s", Duration::from_secs(5)),
                ("30m", Duration::from_secs(30 * 60)),
                ("1h", Duration::from_secs(3_600)),
                ("2d", Duration::from_secs(2 * 86_400)),
                ("250us", Duration::from_micros(250)),
            ];
            for (s, d) in cases {
                assert_eq!(parse(s).unwrap(), d, "parse {s}");
                assert_eq!(format(d), s, "format {d:?}");
            }
        }

        #[test]
        fn rejects_bare_number() {
            assert!(parse("5").is_err());
        }

        #[test]
        fn rejects_empty_number() {
            assert!(parse("ms").is_err());
        }
    }
}

pub mod bigdecimal_plain_str {
    use std::fmt;

    use bigdecimal::BigDecimal;
    use serde::{Deserializer, Serializer, de};

    pub fn serialize<S>(value: &BigDecimal, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_plain_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BigDecimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BigDecimalVisitor;

        impl de::Visitor<'_> for BigDecimalVisitor {
            type Value = BigDecimal;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a hex-encoded string")
            }

            fn visit_str<E>(self, value: &str) -> Result<BigDecimal, E>
            where
                E: de::Error,
            {
                use std::str::FromStr;
                BigDecimal::from_str(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(BigDecimalVisitor)
    }
}
