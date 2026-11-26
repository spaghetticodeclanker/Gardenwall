//! Pages and documents are hostile input to the sandboxed reader; it must
//! still never crash or run away (the sandbox bounds it too).

use std::io::{Cursor, Write};

use gardenwall_parse::documents::{docx_to_text, pdf_to_text};
use gardenwall_parse::html_to_text;

fn docx(xml: &str) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        w.write_all(xml.as_bytes()).unwrap();
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn pdf(content: &[u8], compress: bool) -> Vec<u8> {
    let (dict, data) = if compress {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(content).unwrap();
        ("<< /Length 0 /Filter /FlateDecode >>", e.finish().unwrap())
    } else {
        ("<< /Length 0 >>", content.to_vec())
    };
    let mut out = b"%PDF-1.4\n1 0 obj\n".to_vec();
    out.extend_from_slice(dict.as_bytes());
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
    out
}

#[test]
fn html_survives_mutation() {
    gardenwall_fuzzkit::run_text(
        "html",
        &[
            "<html><head><style>p{}</style><script>alert(1)</script></head><body><h1>T&amp;itle</h1><p>One <a href='x'>link</a></p><ul><li>a<li>b</ul><table><tr><td>1<td>2</table><!-- c --><pre>  x</pre></body></html>",
            "<div><<<>>>&#x41;&#999999999;&nbsp;<br/><img src=x onerror=y>",
        ],
        5000,
        |t| {
            let out = html_to_text(t, 10_000);
            assert!(out.chars().count() <= 10_100);
        },
    );
}

#[test]
fn documents_survive_mutation() {
    let seeds = vec![
        docx(
            "<w:document><w:body><w:p><w:r><w:t>Hello</w:t><w:tab/><w:t>world</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>",
        ),
        pdf(
            b"BT /F1 12 Tf (Hello \\(world\\)) Tj [(A) -20 (B)] TJ ET",
            true,
        ),
        pdf(b"BT <48656c6c6f> Tj T* (x) ' ET", false),
    ];
    gardenwall_fuzzkit::run("documents", &seeds, 3000, |bytes| {
        for t in [docx_to_text(bytes, 10_000), pdf_to_text(bytes, 10_000)]
            .into_iter()
            .flatten()
        {
            assert!(t.chars().count() <= 10_100);
        }
    });
}
