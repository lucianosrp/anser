//! A small CSV reader with the exact semantics the stored rows were built
//! with: a table reader (HSBC, N26, Revolut, Trezor; [`read_table`]) and a
//! header-keyed record reader (the HSBC card CSV; [`dict_rows`]).
//!
//! Why not the csv crate: the table reader distinguishes an unquoted empty
//! field (null) from a quoted one ("" -> empty string), keeps a blank line
//! as an all-null row, and de-duplication treats null and "" as different
//! values. Those details decide `row_count` and which rows survive.

use anyhow::{bail, Result};

/// One field: `None` = null (unquoted empty).
pub type Cell = Option<String>;

/// Tokenize CSV text into records. A blank line is an empty record; the
/// terminator after the last record does not start another one.
fn records(text: &str) -> Vec<Vec<(String, bool)>> {
    records_with(text, ',')
}

/// [`records`] with another field separator (`;` and tab exports).
fn records_with(text: &str, delim: char) -> Vec<Vec<(String, bool)>> {
    let mut out: Vec<Vec<(String, bool)>> = Vec::new();
    let mut rec: Vec<(String, bool)> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut at_field_start = true;
    let mut line_has_content = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                in_quotes = true;
                quoted = true;
                at_field_start = false;
                line_has_content = true;
            }
            c if c == delim => {
                rec.push((std::mem::take(&mut field), quoted));
                quoted = false;
                at_field_start = true;
                line_has_content = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if line_has_content {
                    rec.push((std::mem::take(&mut field), quoted));
                }
                out.push(std::mem::take(&mut rec));
                quoted = false;
                at_field_start = true;
                line_has_content = false;
            }
            _ => {
                // after a closing quote both readers append the rest
                // literally ("-550.00"\t -> -550.00\t)
                field.push(c);
                at_field_start = false;
                line_has_content = true;
            }
        }
    }
    if line_has_content || in_quotes {
        rec.push((field, quoted));
        out.push(rec);
    }
    out
}

fn decode(bytes: &[u8]) -> Result<&str> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    Ok(std::str::from_utf8(bytes)?)
}

/// A table: header names + rows of cells aligned to the header.
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn col(&self, name: &str) -> Result<usize> {
        match self.header.iter().position(|h| h == name) {
            Some(i) => Ok(i),
            None => bail!("column {name:?} not found"),
        }
    }
}

/// The whole file as a table, every cell a string or null.
pub fn read_table(bytes: &[u8]) -> Result<Table> {
    let text = decode(bytes)?;
    let mut recs = records(text).into_iter();
    let header: Vec<String> = match recs.next() {
        Some(h) => h.into_iter().map(|(f, _)| f).collect(),
        None => return Ok(Table { header: vec![], rows: vec![] }),
    };
    let mut rows = Vec::new();
    for rec in recs {
        if rec.len() > header.len() {
            bail!("found more fields than defined in 'Schema'");
        }
        let mut row: Vec<Cell> = rec
            .into_iter()
            .map(|(f, q)| if f.is_empty() && !q { None } else { Some(f) })
            .collect();
        row.resize(header.len(), None);
        rows.push(row);
    }
    Ok(Table { header, rows })
}

/// Type inference over a column's first 100 values, for a column
/// later cast to a string: a column whose sampled values all look like
/// integers is read as Int64 and re-stringified ("007" -> "7"), likewise
/// floats and booleans. Applied to the N26 columns that feed the row key, so
/// such a file hashes exactly as its stored rows were hashed.
pub fn infer_str_column(col: &mut [Cell]) -> Result<()> {
    let sample: Vec<&str> = col.iter().take(100).flatten().map(|s| s.as_str()).collect();
    if sample.is_empty() {
        return Ok(());
    }
    let is_int = |s: &str| {
        let d = s.strip_prefix('-').unwrap_or(s);
        !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
    };
    let is_float = |s: &str| {
        let re = float_re();
        re.is_match(s)
    };
    let is_bool = |s: &str| s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false");
    if sample.iter().all(|s| is_int(s)) {
        for c in col.iter_mut().flatten() {
            match c.parse::<i64>() {
                Ok(v) => *c = v.to_string(),
                Err(_) => bail!("could not parse {c:?} as dtype i64"),
            }
        }
    } else if sample.iter().all(|s| is_int(s) || is_float(s)) {
        for c in col.iter_mut().flatten() {
            match c.parse::<f64>() {
                Ok(v) => *c = super::textfmt::float_repr(v),
                Err(_) => bail!("could not parse {c:?} as dtype f64"),
            }
        }
    } else if sample.iter().all(|s| is_bool(s)) {
        for c in col.iter_mut().flatten() {
            if !is_bool(c) {
                bail!("could not parse {c:?} as dtype bool");
            }
            *c = c.to_ascii_lowercase();
        }
    }
    Ok(())
}

fn float_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^[-+]?((\d*\.\d+)([eE][-+]?\d+)?|inf|NaN|(\d+)[eE][-+]?\d+|\d+\.)$").unwrap()
    })
}

/// Strict string -> f64 cast: null stays null, anything that is not a
/// number is an error (the file fails to parse).
pub fn cast_f64(v: &Cell) -> Result<Option<f64>> {
    match v {
        None => Ok(None),
        Some(s) => match s.parse::<f64>() {
            Ok(x) if !s.is_empty() && !s.eq_ignore_ascii_case("infinity") && !s.contains('_') => Ok(Some(x)),
            _ => bail!("conversion from `str` to `f64` failed for value {s:?}"),
        },
    }
}

/// Every record as plain strings (quoted or not), blank lines kept as empty
/// records: the format definitions' reader (`sources::grid`).
pub fn plain_records(text: &str, delim: char) -> Vec<Vec<String>> {
    records_with(text, delim).into_iter().map(|r| r.into_iter().map(|(f, _)| f).collect()).collect()
}

/// Header-keyed rows: blank lines skipped, short rows padded with None.
pub fn dict_rows(bytes: &[u8]) -> Result<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let text = decode(bytes)?;
    let mut recs = records(text).into_iter().filter(|r| !r.is_empty());
    let header: Vec<String> = match recs.next() {
        Some(h) => h.into_iter().map(|(f, _)| f).collect(),
        None => return Ok((vec![], vec![])),
    };
    let rows = recs
        .map(|rec| {
            let mut row: Vec<Option<String>> = rec.into_iter().map(|(f, _)| Some(f)).collect();
            row.resize(header.len(), None);
            row
        })
        .collect();
    Ok((header, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_and_blank_lines() {
        let t = read_table(b"\xef\xbb\xbfa,b\r\n1,\r\n\r\n\"\",\"x,\"\"y\"\r\n").unwrap();
        assert_eq!(t.header, vec!["a", "b"]);
        assert_eq!(
            t.rows,
            vec![
                vec![Some("1".into()), None],
                vec![None, None],
                vec![Some("".into()), Some("x,\"y".into())],
            ]
        );
        assert!(read_table(b"a,b\n1,2,3\n").is_err());
        let t = read_table(b"a,b\n1\n").unwrap();
        assert_eq!(t.rows, vec![vec![Some("1".into()), None]]);
    }

    #[test]
    fn inference() {
        let mut c = vec![Some("007".into()), None, Some("12".into())];
        infer_str_column(&mut c).unwrap();
        assert_eq!(c, vec![Some("7".into()), None, Some("12".into())]);
        let mut c = vec![Some("1.50".into()), Some("2".into())];
        infer_str_column(&mut c).unwrap();
        assert_eq!(c, vec![Some("1.5".into()), Some("2.0".into())]);
        let mut c = vec![Some("Ref 1".into()), Some("2".into())];
        infer_str_column(&mut c).unwrap();
        assert_eq!(c[1], Some("2".into()));
    }

    #[test]
    fn dict_reader() {
        let (h, rows) = dict_rows(b"a,b\r\n\"-5.00\"\t,x\r\n\r\nz\r\n").unwrap();
        assert_eq!(h, vec!["a", "b"]);
        assert_eq!(rows[0], vec![Some("-5.00\t".into()), Some("x".into())]);
        assert_eq!(rows[1], vec![Some("z".into()), None]);
    }
}
