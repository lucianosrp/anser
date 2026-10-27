//! A file as a grid of cells, for the format definitions (`format.rs`) and
//! the Upload page's column mapper: CSV (any encoding, `,` `;` tab or `|`
//! separated) and Excel (.xlsx, .xls, via calamine).
//!
//! Excel cells keep their type: a number stays a number (so a sheet's
//! amounts never go through the decimal / thousands rules meant for text),
//! and a date cell becomes ISO text (`2026-03-01`, or `2026-03-01 14:05:00`
//! with a time), which every definition's date parsing accepts.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::csvread;
use super::textfmt;

/// One cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Empty,
    Text(String),
    Num(f64),
}

impl Val {
    /// The cell as text: numbers without a needless `.0`.
    pub fn text(&self) -> String {
        match self {
            Val::Empty => String::new(),
            Val::Text(s) => s.trim().to_string(),
            Val::Num(x) if x.fract() == 0.0 && x.abs() < 1e15 => format!("{}", *x as i64),
            Val::Num(x) => textfmt::float_repr(*x),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Val::Empty => true,
            Val::Text(s) => s.trim().is_empty(),
            Val::Num(_) => false,
        }
    }
}

pub type Row = Vec<Val>;

/// How to read a file: the definition's `encoding`, `delimiter`, `sheet`.
#[derive(Debug, Clone, Default)]
pub struct ReadOpts {
    /// utf-8 (default; a BOM is dropped), latin-1, windows-1252, utf-16
    pub encoding: Option<String>,
    /// one char; None = sniffed from the first lines
    pub delimiter: Option<char>,
    /// Excel: the sheet's name; None = the first sheet
    pub sheet: Option<String>,
}

/// .xlsx / .xls / .xlsm (any case).
pub fn is_excel(path: &Path) -> bool {
    matches!(super::suffix_lower(path).as_str(), ".xlsx" | ".xls" | ".xlsm")
}

/// The tabular extensions a definition can describe.
pub fn is_tabular(path: &Path) -> bool {
    is_excel(path) || matches!(super::suffix_lower(path).as_str(), ".csv" | ".txt" | ".tsv")
}

/// Decode bytes in `encoding`.
pub fn decode(bytes: &[u8], encoding: Option<&str>) -> Result<String> {
    let enc = encoding.unwrap_or("utf-8").trim().to_ascii_lowercase().replace('_', "-");
    match enc.as_str() {
        "utf-8" | "utf8" => {
            let b = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
            Ok(std::str::from_utf8(b).context("not valid UTF-8 (set encoding = \"latin-1\" or \"windows-1252\")")?.to_string())
        }
        "latin-1" | "latin1" | "iso-8859-1" => Ok(bytes.iter().map(|&b| b as char).collect()),
        "windows-1252" | "cp1252" => Ok(bytes.iter().map(|&b| cp1252(b)).collect()),
        "utf-16" | "utf-16le" | "utf-16be" => {
            let (be, body) = match bytes {
                [0xfe, 0xff, rest @ ..] => (true, rest),
                [0xff, 0xfe, rest @ ..] => (false, rest),
                _ => (enc == "utf-16be", bytes),
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
                .collect();
            Ok(String::from_utf16(&units).context("not valid UTF-16")?)
        }
        other => bail!("unknown encoding {other:?} (utf-8, latin-1, windows-1252, utf-16)"),
    }
}

/// Windows-1252: latin-1 except 0x80..0x9F.
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“',
        '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    if (0x80..0xa0).contains(&b) { HIGH[(b - 0x80) as usize] } else { b as char }
}

/// The separator the first lines use most consistently: `,` `;` tab `|`.
pub fn sniff_delimiter(text: &str) -> char {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(10).collect();
    let count = |l: &str, d: char| {
        let mut n = 0;
        let mut q = false;
        for c in l.chars() {
            if c == '"' {
                q = !q;
            } else if c == d && !q {
                n += 1;
            }
        }
        n
    };
    let mut best = (',', 0usize);
    for d in [',', ';', '\t', '|'] {
        // the most lines with the same nonzero count wins
        let counts: Vec<usize> = lines.iter().map(|l| count(l, d)).collect();
        let Some(&mode) = counts.iter().filter(|&&n| n > 0).max_by_key(|&&n| counts.iter().filter(|&&m| m == n).count()) else {
            continue;
        };
        let score = counts.iter().filter(|&&n| n == mode).count() * 1000 + mode;
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// Every row of the file (the header included), at most `limit` rows.
pub fn read(path: &Path, opts: &ReadOpts, limit: Option<usize>) -> Result<Vec<Row>> {
    if is_excel(path) {
        return read_excel(path, opts, limit);
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // a sniff reads a prefix; cut on a line end so no record is torn
    let bytes = match limit {
        Some(n) if bytes.len() > 256 * 1024 && n < 1000 => {
            let cut = bytes[..256 * 1024].iter().rposition(|&b| b == b'\n').unwrap_or(256 * 1024);
            &bytes[..cut]
        }
        _ => &bytes[..],
    };
    let text = decode(bytes, opts.encoding.as_deref())?;
    let delim = opts.delimiter.unwrap_or_else(|| sniff_delimiter(&text));
    let mut rows: Vec<Row> = csvread::plain_records(&text, delim)
        .into_iter()
        .map(|r| r.into_iter().map(|f| if f.trim().is_empty() { Val::Empty } else { Val::Text(f) }).collect())
        .collect();
    if let Some(n) = limit {
        rows.truncate(n);
    }
    Ok(rows)
}

fn read_excel(path: &Path, opts: &ReadOpts, limit: Option<usize>) -> Result<Vec<Row>> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).with_context(|| format!("opening {} as a spreadsheet", path.display()))?;
    let names = wb.sheet_names();
    let sheet = match &opts.sheet {
        Some(s) => names.iter().find(|n| n.eq_ignore_ascii_case(s)).cloned().with_context(|| format!("no sheet named {s:?} (it has {})", names.join(", ")))?,
        None => names.first().cloned().context("the workbook has no sheets")?,
    };
    if let (Some(n), calamine::Sheets::Xlsx(x)) = (limit, &mut wb) {
        return xlsx_head(x, &sheet, n).map_err(calamine::Error::Xlsx).with_context(|| format!("reading sheet {sheet:?}"));
    }
    let range = wb.worksheet_range(&sheet).with_context(|| format!("reading sheet {sheet:?}"))?;
    let mut out = Vec::new();
    // calamine's range starts at the first used cell: pad back to A1 so row
    // numbers and skip_rows mean what they say in the spreadsheet
    let (top, left) = range.start().map(|(r, c)| (r as usize, c as usize)).unwrap_or((0, 0));
    for _ in 0..top {
        out.push(Vec::new());
    }
    for r in range.rows() {
        if limit.is_some_and(|n| out.len() >= n) {
            break;
        }
        let mut row: Row = vec![Val::Empty; left];
        row.extend(r.iter().map(excel_val));
        out.push(row);
    }
    if let Some(n) = limit {
        out.truncate(n);
    }
    Ok(out)
}

fn excel_val(c: &calamine::Data) -> Val {
    use calamine::Data;
    match c {
        Data::Empty | Data::Error(_) => Val::Empty,
        Data::String(s) => if s.trim().is_empty() { Val::Empty } else { Val::Text(s.clone()) },
        Data::Int(i) => Val::Num(*i as f64),
        Data::Float(f) => Val::Num(*f),
        Data::Bool(b) => Val::Text(b.to_string()),
        Data::DateTime(d) => match d.as_datetime() {
            Some(dt) if dt.time() == chrono::NaiveTime::MIN => Val::Text(dt.format("%Y-%m-%d").to_string()),
            Some(dt) => Val::Text(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
            None => Val::Num(d.as_f64()),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => Val::Text(s.replace('T', " ")),
    }
}

/// `read_excel`'s first `limit` rows of an .xlsx sheet, streamed: the same
/// grid (padded back to A1, as wide as the sheet's used columns), but only
/// those rows are kept. Reading the whole range held every cell of the
/// sheet, ~80 MB for a 20 MB export, for each format a sniff tried.
fn xlsx_head<RS: std::io::Read + std::io::Seek>(wb: &mut calamine::Xlsx<RS>, sheet: &str, limit: usize) -> Result<Vec<Row>, calamine::XlsxError> {
    use calamine::{DataRef, XlsxError};
    let mut cells = match wb.worksheet_cells_reader(sheet) {
        Ok(r) => r,
        // what worksheet_range makes of it: an empty sheet
        Err(XlsxError::NotAWorksheet(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // the used range's bounds need every cell, its values only the first rows'
    let (mut top, mut bottom, mut left, mut right) = (u32::MAX, 0, u32::MAX, 0);
    let mut kept = Vec::new();
    while let Some(c) = cells.next_cell()? {
        if matches!(c.get_value(), DataRef::Empty) {
            continue;
        }
        let (r, col) = c.get_position();
        (top, bottom, left, right) = (top.min(r), bottom.max(r), left.min(col), right.max(col));
        if (r as usize) < limit {
            kept.push((r as usize, col as usize, excel_val(&c.get_value().clone().into())));
        }
    }
    if top == u32::MAX {
        return Ok(Vec::new());
    }
    let mut out: Vec<Row> = (0..limit.min(bottom as usize + 1))
        .map(|r| if r < top as usize { Vec::new() } else { vec![Val::Empty; right as usize + 1] })
        .collect();
    for (r, col, v) in kept {
        out[r][col] = v;
    }
    Ok(out)
}

/// The sheet names of a workbook (the mapper's sheet picker).
pub fn sheet_names(path: &Path) -> Vec<String> {
    use calamine::Reader;
    calamine::open_workbook_auto(path).map(|wb| wb.sheet_names()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(decode(b"\xef\xbb\xbfa;b", None).unwrap(), "a;b");
        assert_eq!(decode(b"Caf\xe9", Some("latin-1")).unwrap(), "Café");
        assert_eq!(decode(b"\x80 5", Some("windows-1252")).unwrap(), "€ 5");
        assert_eq!(decode(b"\xff\xfea\x00b\x00", Some("utf-16")).unwrap(), "ab");
        assert!(decode(b"Caf\xe9", None).is_err());
        assert!(decode(b"x", Some("ebcdic")).is_err());
    }

    #[test]
    fn delimiters() {
        assert_eq!(sniff_delimiter("a;b;c\n1;2,5;3\n"), ';');
        assert_eq!(sniff_delimiter("a,b,c\n1,\"2;5\",3\n"), ',');
        assert_eq!(sniff_delimiter("a\tb\n1\t2\n"), '\t');
        assert_eq!(sniff_delimiter("single\n"), ',');
    }

    #[test]
    fn a_limited_xlsx_read_is_the_full_read_cut() {
        use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        // the used range starts at B3 and is widest far below the cut
        ws.write_string(2, 1, "Date").unwrap();
        ws.write_string(2, 2, "Amount").unwrap();
        ws.write_string(2, 3, " ").unwrap();
        ws.write_datetime_with_format(3, 1, ExcelDateTime::from_ymd(2026, 3, 1).unwrap(), &Format::new().set_num_format("yyyy-mm-dd")).unwrap();
        ws.write_number(3, 2, -4.5).unwrap();
        ws.write_boolean(4, 2, true).unwrap();
        ws.write_number(60, 6, 1.0).unwrap();
        let path = std::env::temp_dir().join(format!("grid-head-{}.xlsx", std::process::id()));
        wb.save(&path).unwrap();
        let opts = ReadOpts::default();
        let full = read(&path, &opts, None).unwrap();
        assert_eq!(full.len(), 61);
        assert_eq!(full[3][1], Val::Text("2026-03-01".into()));
        for n in [0, 1, 2, 3, 5, 40, 61, 100] {
            assert_eq!(read(&path, &opts, Some(n)).unwrap(), full[..n.min(full.len())], "limit {n}");
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cells() {
        assert_eq!(Val::Num(12345678.0).text(), "12345678");
        assert_eq!(Val::Num(-12.5).text(), "-12.5");
        assert!(Val::Text("  ".into()).is_empty());
    }
}
