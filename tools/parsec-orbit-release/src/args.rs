//! Command-line options: `--name value` pairs and positional words.

use parsec_orbit_registry::Error;

/// The options of one command, as parsed by [`Args::parse`].
#[derive(Debug, Default)]
pub struct Args {
    positional: Vec<String>,
    values: Vec<(String, String)>,
}

/// A usage error: the arguments do not match the command.
pub fn usage(text: impl Into<String>) -> Error {
    Error::Usage(text.into())
}

impl Args {
    /// Parses `args`; each of `values` takes the next word as its value, and
    /// any other `--` word is refused.
    pub fn parse(args: &[String], values: &[&str]) -> Result<Args, Error> {
        let mut parsed = Args::default();
        let mut words = args.iter();
        while let Some(word) = words.next() {
            if values.contains(&word.as_str()) {
                let value = words
                    .next()
                    .ok_or_else(|| usage(format!("{word} needs a value")))?;
                parsed.values.push((word.clone(), value.clone()));
            } else if word.starts_with("--") {
                return Err(usage(format!("unknown option {word}")));
            } else {
                parsed.positional.push(word.clone());
            }
        }
        Ok(parsed)
    }

    /// The last value given for `name`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(option, _)| option == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn required(&self, name: &str) -> Result<&str, Error> {
        self.value(name)
            .ok_or_else(|| usage(format!("{name} is required")))
    }

    /// Refuses any positional word.
    pub fn no_words(&self) -> Result<(), Error> {
        match self.positional.first() {
            None => Ok(()),
            Some(word) => Err(usage(format!("unexpected argument {word:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn values_are_paired_with_their_option() {
        let parsed = Args::parse(&words(&["--key", "k.key"]), &["--key"]).unwrap();
        assert_eq!(parsed.value("--key"), Some("k.key"));
        assert_eq!(parsed.value("--other"), None);
        assert!(parsed.no_words().is_ok());
    }

    #[test]
    fn a_required_value_must_be_given() {
        let parsed = Args::parse(&[], &["--key"]).unwrap();
        assert!(matches!(parsed.required("--key"), Err(Error::Usage(_))));
    }

    #[test]
    fn the_last_value_wins() {
        let parsed = Args::parse(&words(&["--key", "a", "--key", "b"]), &["--key"]).unwrap();
        assert_eq!(parsed.value("--key"), Some("b"));
    }

    #[test]
    fn unknown_options_and_missing_values_are_usage_errors() {
        for list in [&["--nope"][..], &["--key"][..]] {
            let error = Args::parse(&words(list), &["--key"]).unwrap_err();
            assert!(matches!(error, Error::Usage(_)), "{list:?}");
        }
    }

    #[test]
    fn positional_words_are_refused_only_where_none_are_taken() {
        let none = Args::parse(&[], &[]).unwrap();
        assert!(none.no_words().is_ok());
        let one = Args::parse(&words(&["a"]), &[]).unwrap();
        assert!(one.no_words().is_err());
    }
}
