//! Integers that do not survive a round trip through JavaScript.
//!
//! Sequence numbers and i128 token amounts routinely exceed 2^53, so JSON
//! numbers silently lose precision in any browser client. They travel as
//! strings; deserialisation still accepts plain numbers so older quotes and
//! hand-written requests keep working.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod stringly {
    use super::*;

    pub fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: std::fmt::Display,
        S: Serializer,
    {
        value.to_string().serialize(serializer)
    }

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Text(String),
            Number(i64),
        }
        match Either::deserialize(deserializer)? {
            Either::Text(s) => s.parse().map_err(serde::de::Error::custom),
            Either::Number(n) => n.to_string().parse().map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        #[serde(with = "super::stringly")]
        sequence: i64,
    }

    #[test]
    fn large_integers_survive_as_strings() {
        let s = Sample {
            sequence: 19_363_546_521_403_393,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"sequence":"19363546521403393"}"#);
        assert_eq!(serde_json::from_str::<Sample>(&json).unwrap(), s);
        // Plain numbers still deserialise.
        assert_eq!(
            serde_json::from_str::<Sample>(r#"{"sequence":42}"#)
                .unwrap()
                .sequence,
            42
        );
    }
}
