use std::fmt;
use std::str::FromStr;

/// An Open Library work key such as `OL45804W`, stored as its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkKey(pub u64);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("not an Open Library work key: {0:?}")]
pub struct KeyError(pub String);

impl FromStr for WorkKey {
    type Err = KeyError;

    /// Accepts `OL45804W` and `/works/OL45804W`.
    fn from_str(s: &str) -> Result<Self, KeyError> {
        let bare = s.strip_prefix("/works/").unwrap_or(s);
        bare.strip_prefix("OL")
            .and_then(|rest| rest.strip_suffix('W'))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse().ok())
            .map(WorkKey)
            .ok_or_else(|| KeyError(s.to_owned()))
    }
}

impl fmt::Display for WorkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OL{}W", self.0)
    }
}

impl serde::Serialize for WorkKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for WorkKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        assert_eq!("OL45804W".parse(), Ok(WorkKey(45804)));
        assert_eq!("/works/OL45804W".parse(), Ok(WorkKey(45804)));
        assert_eq!(WorkKey(45804).to_string(), "OL45804W");
        for bad in ["", "OL", "OLW", "OL12M", "OL1x2W", "45804", "/books/OL1W"] {
            assert!(bad.parse::<WorkKey>().is_err(), "{bad}");
        }
    }
}
