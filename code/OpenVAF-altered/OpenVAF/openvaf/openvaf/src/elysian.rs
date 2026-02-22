// NOTE: pun on "elision", named after an excellent beer brand from Seattle
use std::collections::HashMap;
use std::io::{self, BufRead};
use std::fs::File;
use camino::Utf8PathBuf;
use basedb::{CliParamDefault, CliParamDefaultValue};
use syntax::name::Name;

#[derive(Debug, Clone, PartialEq)]
pub enum NumericValue {
    Int(i32),
    Float(f64),
}

/// Parses a file with lines of the form:
/// ```text
/// var_name = value
/// ```
/// where value is an integer, float, or scientific notation.
/// Returns a mapping from variable names to their numeric values.
pub fn parse_file(path: &Utf8PathBuf) -> io::Result<HashMap<String, NumericValue>> {
    let file = File::open(&path)?;
    let reader = io::BufReader::new(file);
    let mut map = HashMap::new();

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split('=').collect();
        if parts.len() != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Line {}: expected format 'var = value'", i + 1),
            ));
        }

        let var = parts[0].trim().to_string();
        let val_str = parts[1].trim();

        let value = if let Ok(i_val) = val_str.parse::<i32>() {
            NumericValue::Int(i_val)
        } else if let Ok(f_val) = val_str.parse::<f64>() {
            NumericValue::Float(f_val)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Line {}: cannot parse value '{}'", i + 1, val_str),
            ));
        };

        map.insert(var, value);
    }


    Ok(map)
}

pub fn to_cli_defaults(
    params: &HashMap<String, NumericValue>,
) -> Vec<CliParamDefault> {
    params
        .iter()
        .map(|(name, value)| {
            let value = match value {
                NumericValue::Int(v) => CliParamDefaultValue::Int(*v),
                NumericValue::Float(v) => CliParamDefaultValue::from_float(*v),
            };
            CliParamDefault { name: Name::resolve(name), value }
        })
        .collect()
}

