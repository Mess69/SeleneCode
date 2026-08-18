//! Wave B binary-document readers (doc-ingestion PRD §5.2/§5.5 as amended
//! 2026-08-18): bytes → extracted text, at the READ seam — the parser branch
//! (`docparse`) only ever sees clean text.
//!
//! # PDF: isolation is an obligation, not a precaution
//!
//! `pdf-extract` has a documented panic record on malformed input (its issue
//! #141: "~50 panic/crash fixes for untrusted PDF input"). Every call runs in
//! a DEDICATED THREAD wrapped in `catch_unwind`; any panic is "unextractable
//! file", a collected diagnostic, never a dead index (the resolver's
//! catch_unwind precedent, RESUME §3 ⛔ `panic = 'abort'`).
//! **Plan B (recorded):** `lopdf::Document::extract_text` — same parse family,
//! simpler failure surface, lower layout fidelity — tried when pdf-extract
//! panics or errors. The 10-real-PDF quality spike remains OPEN
//! (`docs/benchmarks/`); this ladder is the shipping shape either way.
//!
//! # DOCX: a zip of XML, parsed deterministically
//!
//! `word/document.xml`, `w:p` paragraphs, `w:t` text runs; a `w:pStyle` of
//! `Heading<N>` renders the paragraph as a `# `-prefixed markdown heading so
//! the wave-A markdown sectionizer gives docx real sections for free.

use std::io::Read as _;

/// Per-file byte cap (PRD §5.2): past this, the file is diagnosed, not read.
pub const MAX_DOC_BYTES: u64 = 20 * 1024 * 1024;
/// Extracted-text cap: past this, text is truncated at a char boundary.
pub const MAX_DOC_TEXT: usize = 512 * 1024;

fn cap_text(mut s: String) -> String {
    if s.len() > MAX_DOC_TEXT {
        let mut cut = MAX_DOC_TEXT;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    // Normalize away NULs and lone CRs the graph never wants to store.
    s.replace('\u{0}', "").replace('\r', "")
}

/// PDF bytes → text. `Err` is a *message* for `FileRecord.errors` — callers
/// never fail an index over it.
pub fn pdf_to_text(bytes: &[u8]) -> Result<String, String> {
    // Step 1: pdf-extract, in its own thread so a panic dies there.
    let owned = bytes.to_vec();
    let primary = std::thread::spawn(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem(&owned)
        }))
    })
    .join();
    if let Ok(Ok(Ok(text))) = primary {
        let text = cap_text(text);
        if !text.trim().is_empty() {
            return Ok(text);
        }
    }
    // Step 2 — Plan B: lopdf's own extractor (also isolated; same crate family
    // but a different failure surface).
    let owned = bytes.to_vec();
    let fallback = std::thread::spawn(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let doc = lopdf::Document::load_mem(&owned).map_err(|e| e.to_string())?;
            let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
            doc.extract_text(&pages).map_err(|e| e.to_string())
        }))
    })
    .join();
    match fallback {
        Ok(Ok(Ok(text))) if !text.trim().is_empty() => Ok(cap_text(text)),
        Ok(Ok(Err(e))) => Err(format!("pdf text extraction failed: {e}")),
        _ => Err("pdf text extraction failed (no text layer, or malformed file)".into()),
    }
}

/// DOCX bytes → markdown-ish text (`# ` headings). `Err` is a diagnostic.
pub fn docx_to_text(bytes: &[u8]) -> Result<String, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("not a docx zip: {e}"))?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| "no word/document.xml (not a Word document?)".to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| format!("document.xml unreadable: {e}"))?;

    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut out = String::new();
    let mut para = String::new();
    let mut heading_level: usize = 0;
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) => match e.local_name().as_ref() {
                b"p" => {
                    para.clear();
                    heading_level = 0;
                }
                b"t" => in_text = true,
                _ => {}
            },
            Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == b"pStyle"
                    && let Some(v) = e.attributes().flatten().find_map(|a| {
                        (a.key.local_name().as_ref() == b"val")
                            .then(|| String::from_utf8_lossy(&a.value).into_owned())
                    })
                    && let Some(level) = v.strip_prefix("Heading").and_then(|n| n.parse().ok())
                {
                    heading_level = level;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) if in_text => {
                para.push_str(
                    &t.xml_content(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| format!("document.xml text decode: {e}"))?,
                );
            }
            Ok(quick_xml::events::Event::GeneralRef(r)) if in_text => {
                let entity: &[u8] = r.as_ref();
                para.push_str(match entity {
                    b"quot" => "\"",
                    b"lt" => "<",
                    b"gt" => ">",
                    b"amp" => "&",
                    b"apos" => "'",
                    _ => "",
                });
            }
            Ok(quick_xml::events::Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    let text = para.trim();
                    if !text.is_empty() {
                        if heading_level > 0 {
                            out.push_str(&"#".repeat(heading_level.min(6)));
                            out.push(' ');
                        }
                        out.push_str(text);
                        out.push_str("\n\n");
                    }
                }
                _ => {}
            },
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(format!("document.xml parse: {e}")),
            _ => {}
        }
    }
    Ok(cap_text(out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A minimal single-page PDF with one text object — hand-written bytes,
    /// deterministic, ~700 B. (The 10-real-PDF quality spike stays open; this
    /// proves the LADDER, not layout fidelity.)
    fn tiny_pdf() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 72 720 Td (Selene rationale lives here) Tj ET";
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        let mut push = |body: &mut Vec<u8>, offsets: &mut Vec<usize>, s: String| {
            offsets.push(body.len());
            body.extend_from_slice(s.as_bytes());
        };
        let header = b"%PDF-1.4\n";
        push(
            &mut body,
            &mut offsets,
            "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n".into(),
        );
        push(
            &mut body,
            &mut offsets,
            "2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n".into(),
        );
        push(
            &mut body,
            &mut offsets,
            format!(
                "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n"
            ),
        );
        push(
            &mut body,
            &mut offsets,
            format!(
                "4 0 obj<</Length {}>>stream\n{}\nendstream endobj\n",
                content.len(),
                String::from_utf8_lossy(content)
            ),
        );
        push(
            &mut body,
            &mut offsets,
            "5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n".into(),
        );
        let mut pdf = header.to_vec();
        let base = header.len();
        pdf.extend_from_slice(&body);
        let xref_at = pdf.len();
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{:010} 00000 n \n", base + off));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer<</Size 6/Root 1 0 R>>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    fn tiny_docx() -> Vec<u8> {
        let document = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
 <w:body>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Design</w:t></w:r></w:p>
  <w:p><w:r><w:t>The resolver binds refs &amp; more.</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Deferred</w:t></w:r></w:p>
  <w:p><w:r><w:t>Later.</w:t></w:r></w:p>
 </w:body>
</w:document>"#;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("word/document.xml", opts).unwrap();
            std::io::Write::write_all(&mut w, document.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn docx_headings_become_markdown_headings() {
        let text = docx_to_text(&tiny_docx()).unwrap();
        assert!(text.contains("# Design"), "{text}");
        assert!(text.contains("## Deferred"), "{text}");
        assert!(text.contains("binds refs & more"), "entity decoded: {text}");
    }

    #[test]
    fn pdf_ladder_extracts_the_text_layer() {
        let text = pdf_to_text(&tiny_pdf()).expect("tiny pdf should extract");
        assert!(text.contains("Selene rationale"), "{text}");
    }

    #[test]
    fn garbage_bytes_are_a_diagnostic_never_a_panic() {
        assert!(pdf_to_text(b"not a pdf at all").is_err());
        assert!(docx_to_text(b"not a zip").is_err());
    }
}
