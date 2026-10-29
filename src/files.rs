//! Files attached to a message, read on the phone and sent as text: PDFs, spreadsheets (CSV, TSV,
//! Excel, OpenDocument), Word, PowerPoint and OpenDocument text, and anything that is plain text
//! (code, JSON, Markdown, logs…). The file itself never leaves the phone, so every model can use it.

use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};

/// Files larger than this aren't read.
pub const MAX_BYTES: usize = 25 << 20;
/// Characters of each file sent with the message (about 30k tokens).
pub const MAX_CHARS: usize = 120_000;

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Doc {
    pub name: String,
    /// What it is, for people and the model: "PDF", "spreadsheet", "Word document"…
    pub kind: String,
    pub text: String,
    /// The text was cut at `MAX_CHARS`; `chars` is how long it was.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub chars: usize,
}

/// Reads a file into text. Errors are sentences for the person who picked the file.
pub fn read(name: &str, bytes: &[u8]) -> Result<Doc, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("{name} is over {} MB.", MAX_BYTES >> 20));
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    // Parsers of untrusted files can panic; a bad file must not take the app down.
    let parsed = std::panic::catch_unwind(|| -> Result<(&str, String), String> {
        Ok(match ext.as_str() {
            "pdf" => ("PDF", pdf(bytes)?),
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => ("spreadsheet", sheets(bytes)?),
            "docx" => ("Word document", office(&mut zip(bytes)?, "word/document.xml", true)?),
            "pptx" => ("slides", slides(bytes)?),
            "odt" => ("document", office(&mut zip(bytes)?, "content.xml", false)?),
            "odp" => ("slides", office(&mut zip(bytes)?, "content.xml", false)?),
            "csv" => ("CSV", plain(bytes)?),
            "tsv" => ("TSV", plain(bytes)?),
            _ => ("text", plain(bytes)?),
        })
    });
    let (kind, text) = match parsed {
        Ok(r) => r.map_err(|e| format!("Couldn't read {name}: {e}"))?,
        Err(_) => return Err(format!("Couldn't read {name}: the file looks damaged.")),
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(if ext == "pdf" {
            format!("{name} has no text to read. It may be scanned pages: attach them as images instead.")
        } else {
            format!("{name} is empty.")
        });
    }
    let chars = text.chars().count();
    let (text, truncated) = match text.char_indices().nth(MAX_CHARS) {
        Some((at, _)) => (text[..at].to_string(), true),
        None => (text, false),
    };
    Ok(Doc { name: name.to_string(), kind: kind.to_string(), text, truncated, chars })
}

/// The files as the model sees them, after the message text.
pub fn for_model(text: &str, docs: &[Doc]) -> String {
    let mut out = text.to_string();
    for d in docs {
        out.push_str(&format!("\n\n<file name=\"{}\" type=\"{}\">\n{}\n</file>", d.name.replace('"', "'"), d.kind, d.text));
        if d.truncated {
            out.push_str(&format!("\n(Only the first {MAX_CHARS} of its {} characters are attached.)", d.chars));
        }
    }
    out
}

/// "12k characters", for the chip under a message.
pub fn size(d: &Doc) -> String {
    let n = d.text.chars().count();
    if n < 1000 { format!("{n} characters") } else { format!("{}k characters", n / 1000) }
}

fn plain(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) }).collect();
        return Ok(String::from_utf16_lossy(&units));
    }
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return Err("Obscurify can read PDFs, spreadsheets, Word and PowerPoint files and text files, not this kind of file.".into());
    }
    let text = String::from_utf8_lossy(bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn pdf(bytes: &[u8]) -> Result<String, String> {
    pdf_extract::extract_text_from_mem(bytes).map_err(|e| {
        let e = e.to_string();
        if e.to_lowercase().contains("encrypt") { "it's password protected.".into() } else { e }
    })
}

/// Every sheet as CSV, each under its name.
fn sheets(bytes: &[u8]) -> Result<String, String> {
    use calamine::{Data, Reader, open_workbook_auto_from_rs};
    let mut book = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec())).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for name in book.sheet_names() {
        let Ok(range) = book.worksheet_range(&name) else { continue };
        if range.is_empty() {
            continue;
        }
        out.push_str(&format!("## Sheet: {name}\n"));
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| {
                let s = match c {
                    Data::Empty => String::new(),
                    Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
                    other => other.to_string(),
                };
                if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
            }).collect();
            let line = cells.join(",");
            out.push_str(line.trim_end_matches(','));
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

fn zip(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, String> {
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "it isn't a valid Office file.".to_string())
}

/// The text of one XML part of an Office file, a line per paragraph. Office Open XML (`only_t`)
/// keeps its text in `<w:t>`/`<a:t>`; OpenDocument puts it straight in its paragraphs.
fn office(z: &mut zip::ZipArchive<Cursor<&[u8]>>, part: &str, only_t: bool) -> Result<String, String> {
    use quick_xml::events::Event;
    let mut xml = String::new();
    z.by_name(part).map_err(|_| "it isn't a valid Office file.".to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    let mut r = quick_xml::Reader::from_str(&xml);
    let mut out = String::new();
    let mut in_t = 0usize;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == "t" => in_t += 1,
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "t" => in_t = in_t.saturating_sub(1),
                "p" | "h" => out.push('\n'),
                "tr" => out.push('\n'),
                "tc" | "table-cell" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                "tab" => out.push('\t'),
                "br" | "line-break" => out.push('\n'),
                "s" => out.push(' '),
                _ => {}
            },
            Ok(Event::Text(t)) if !only_t || in_t > 0 => out.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(g)) if !only_t || in_t > 0 => match g.resolve_char_ref() {
                Ok(Some(c)) => out.push(c),
                _ => out.push_str(match &*g { "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", _ => "" }),
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
    }
    Ok(out)
}

/// Each slide's text, in order.
fn slides(bytes: &[u8]) -> Result<String, String> {
    let mut z = zip(bytes)?;
    let mut nums: Vec<u32> = z.file_names()
        .filter_map(|n| n.ok()?.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok())
        .collect();
    nums.sort();
    let mut out = String::new();
    for n in nums {
        let t = office(&mut z, &format!("ppt/slides/slide{n}.xml"), true)?;
        out.push_str(&format!("## Slide {n}\n{}\n\n", t.trim()));
    }
    Ok(out)
}
