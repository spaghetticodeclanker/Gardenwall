//! OCR for scanned PDFs: Windows' own PDF
//! renderer draws each page and Windows' built-in OCR reads it. Both are
//! WinRT components made for sandboxed apps, and both run here, inside the
//! parse worker's AppContainer (no network, no files), so the trusted core
//! never touches the document. Offline, nothing to download. Windows only;
//! other systems report that OCR isn't available.

/// Most pages read by OCR in one document (about a second each).
pub const MAX_OCR_PAGES: u32 = 300;

/// Width pages are drawn at, in pixels: about 200 dpi for A4 or Letter,
/// enough for body text without huge bitmaps.
#[cfg(windows)]
const RENDER_WIDTH: u32 = 1700;

/// The text of every page of `bytes`, read by OCR, pages separated by a
/// blank line and marked `[Page N, read by OCR]`. Stops at `max_chars` or
/// [`MAX_OCR_PAGES`].
#[cfg(windows)]
pub fn pdf_ocr(bytes: &[u8], max_chars: usize) -> Result<String, String> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    let fail = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| fail("Windows OCR isn't available", e))?;

    let input = InMemoryRandomAccessStream::new().map_err(|e| fail("memory stream", e))?;
    let writer = DataWriter::CreateDataWriter(&input).map_err(|e| fail("memory stream", e))?;
    writer
        .WriteBytes(bytes)
        .map_err(|e| fail("memory stream", e))?;
    writer
        .StoreAsync()
        .and_then(|op| op.join())
        .map_err(|e| fail("memory stream", e))?;
    writer
        .DetachStream()
        .map_err(|e| fail("memory stream", e))?;
    input.Seek(0).map_err(|e| fail("memory stream", e))?;

    let document = PdfDocument::LoadFromStreamAsync(&input)
        .and_then(|op| op.join())
        .map_err(|e| fail("Windows couldn't open this PDF", e))?;
    let pages = document.PageCount().map_err(|e| fail("PDF", e))?;
    let max_dim = OcrEngine::MaxImageDimension().unwrap_or(10_000).max(1);
    let width = RENDER_WIDTH.min(max_dim);

    let mut out = String::new();
    for index in 0..pages.min(MAX_OCR_PAGES) {
        let page = document.GetPage(index).map_err(|e| fail("PDF page", e))?;
        let options = PdfPageRenderOptions::new().map_err(|e| fail("PDF page", e))?;
        options
            .SetDestinationWidth(width)
            .map_err(|e| fail("PDF page", e))?;
        // A very tall page would exceed what OCR accepts: draw it narrower.
        if let Ok(size) = page.Size()
            && size.Width > 0.0
            && size.Height / size.Width * width as f32 > max_dim as f32
        {
            let narrow = (max_dim as f32 * size.Width / size.Height).floor() as u32;
            options
                .SetDestinationWidth(narrow.max(1))
                .map_err(|e| fail("PDF page", e))?;
        }
        let image = InMemoryRandomAccessStream::new().map_err(|e| fail("memory stream", e))?;
        page.RenderWithOptionsToStreamAsync(&image, &options)
            .and_then(|op| op.join())
            .map_err(|e| fail("Windows couldn't draw a page", e))?;
        let decoder = BitmapDecoder::CreateAsync(&image)
            .and_then(|op| op.join())
            .map_err(|e| fail("page image", e))?;
        let bitmap = decoder
            .GetSoftwareBitmapAsync()
            .and_then(|op| op.join())
            .map_err(|e| fail("page image", e))?;
        let bitmap = SoftwareBitmap::Convert(&bitmap, BitmapPixelFormat::Gray8)
            .map_err(|e| fail("page image", e))?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .and_then(|op| op.join())
            .map_err(|e| fail("OCR", e))?;
        let mut text = String::new();
        if let Ok(lines) = result.Lines() {
            for line in &lines {
                if let Ok(t) = line.Text() {
                    text.push_str(&t.to_string_lossy());
                    text.push('\n');
                }
            }
        }
        if !text.trim().is_empty() {
            out.push_str(&format!("[Page {}, read by OCR]\n", index + 1));
            out.push_str(text.trim_end());
            out.push_str("\n\n");
        }
        if out.len() >= max_chars {
            break;
        }
    }
    if pages > MAX_OCR_PAGES && out.len() < max_chars {
        out.push_str(&format!(
            "[Only the first {MAX_OCR_PAGES} of {pages} pages were read by OCR.]\n"
        ));
    }
    let text: String = out
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max_chars)
        .collect();
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 10 {
        return Err("no text was found in this PDF, even with OCR".into());
    }
    Ok(text)
}

#[cfg(not(windows))]
pub fn pdf_ocr(_bytes: &[u8], _max_chars: usize) -> Result<String, String> {
    Err("no readable text was found in this PDF (it may be scanned images); OCR is available on Windows only".into())
}
