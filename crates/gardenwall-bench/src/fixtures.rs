//! Deterministic fixture generators: large repositories and files for the
//! agent tasks, and documents and images for the RAG and vision suites.
//! Everything comes from a seed, so nothing large is committed and every
//! machine gets the same bytes.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// A small, fast, reproducible generator (xorshift64*).
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

pub const WORDS: &[&str] = &[
    "amber", "basin", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper",
    "kestrel", "lagoon", "meadow", "nimbus", "orchid", "pebble", "quarry", "raven", "saffron",
    "timber", "umber", "valley", "willow", "yarrow", "zephyr", "anchor", "beacon", "canyon",
    "dune", "estuary", "falcon", "glacier", "heron", "island", "jasper", "kiln", "lantern",
    "marble", "nectar", "onyx", "prairie", "quill", "ridge", "summit", "tundra", "upland",
    "vertex", "wharf", "yonder", "zenith",
];

const VERBS: &[&str] = &[
    "compute",
    "load",
    "parse",
    "render",
    "merge",
    "filter",
    "resolve",
    "encode",
    "decode",
    "index",
    "score",
    "split",
    "format",
    "validate",
    "normalize",
    "collect",
];

/// A generated part of a fixture.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Generator {
    /// `files` Python modules of plausible noise code under `dir`.
    PythonPackage {
        dir: String,
        files: usize,
        seed: u64,
    },
    /// A Python file of `lines` lines; `plant` puts text at line numbers
    /// (1-based; each entry may hold several lines).
    LongFile {
        path: String,
        lines: usize,
        seed: u64,
        #[serde(default)]
        plant: BTreeMap<String, String>,
    },
    /// A log of `lines` lines, with planted lines as for `long-file`.
    LogFile {
        path: String,
        lines: usize,
        seed: u64,
        #[serde(default)]
        plant: BTreeMap<String, String>,
    },
    /// One note file per part (`<dir>/note-NN.md`), each with
    /// `noise_lines` lines of noise and, somewhere inside, the line
    /// `Part N of M: <part>`. Files are written in a shuffled order of
    /// names so the parts can't be read off the listing.
    Notes {
        dir: String,
        parts: Vec<String>,
        noise_lines: usize,
        seed: u64,
    },
}

fn safe_rel(p: &str) -> bool {
    !p.is_empty() && !p.contains("..") && !Path::new(p).is_absolute()
}

impl Generator {
    pub fn validate(&self) -> Result<(), String> {
        let (path, size) = match self {
            Self::PythonPackage { dir, files, .. } => (dir, *files),
            Self::LongFile { path, lines, .. } | Self::LogFile { path, lines, .. } => {
                (path, *lines)
            }
            Self::Notes { dir, parts, .. } => (dir, parts.len()),
        };
        if !safe_rel(path) {
            return Err(format!("generated path {path} must be relative"));
        }
        if size == 0 || size > 20_000 {
            return Err("generated sizes must be between 1 and 20,000".into());
        }
        if let Self::LongFile { plant, .. } | Self::LogFile { plant, .. } = self
            && plant.keys().any(|k| k.parse::<usize>().is_err())
        {
            return Err("planted lines are keyed by line number".into());
        }
        Ok(())
    }

    pub fn write(&self, root: &Path) -> std::io::Result<()> {
        match self {
            Self::PythonPackage { dir, files, seed } => {
                let mut rng = Rng::new(*seed);
                let base = root.join(dir);
                std::fs::create_dir_all(&base)?;
                std::fs::write(base.join("__init__.py"), "")?;
                for i in 0..*files {
                    // Some modules live in subpackages, as in real projects.
                    let sub = WORDS[i % 7];
                    let folder = base.join(sub);
                    std::fs::create_dir_all(&folder)?;
                    let init = folder.join("__init__.py");
                    if !init.exists() {
                        std::fs::write(&init, "")?;
                    }
                    let name = format!("{}_{i:03}.py", rng.pick(WORDS));
                    let functions = 6 + rng.below(8);
                    std::fs::write(folder.join(name), python_module(&mut rng, functions))?;
                }
            }
            Self::LongFile {
                path,
                lines,
                seed,
                plant,
            } => {
                let mut rng = Rng::new(*seed);
                let mut out = Vec::with_capacity(*lines);
                while out.len() < *lines {
                    out.extend(python_function(&mut rng).lines().map(str::to_string));
                    out.push(String::new());
                }
                out.truncate(*lines);
                write_planted(&root.join(path), out, plant)?;
            }
            Self::LogFile {
                path,
                lines,
                seed,
                plant,
            } => {
                let mut rng = Rng::new(*seed);
                let levels = ["INFO", "INFO", "INFO", "DEBUG", "WARN"];
                let out = (0..*lines)
                    .map(|i| {
                        format!(
                            "2026-11-{:02} {:02}:{:02}:{:02} {} [{}] {} {} completed in {} ms",
                            1 + i * 17 / lines.max(&1),
                            (i / 3600) % 24,
                            (i / 60) % 60,
                            i % 60,
                            rng.pick(&levels),
                            rng.pick(WORDS),
                            rng.pick(VERBS),
                            rng.pick(WORDS),
                            rng.below(900) + 10
                        )
                    })
                    .collect();
                write_planted(&root.join(path), out, plant)?;
            }
            Self::Notes {
                dir,
                parts,
                noise_lines,
                seed,
            } => {
                let mut rng = Rng::new(*seed);
                let base = root.join(dir);
                std::fs::create_dir_all(&base)?;
                let mut names: Vec<usize> = (0..parts.len()).collect();
                crate::shuffle(&mut names, *seed);
                for (n, part) in parts.iter().enumerate() {
                    let at = rng.below(*noise_lines + 1);
                    let mut text = format!("# Field notes, sheet {}\n\n", names[n] + 1);
                    for i in 0..*noise_lines {
                        if i == at {
                            text.push_str(&format!("Part {} of {}: {part}\n", n + 1, parts.len()));
                        }
                        text.push_str(&sentence(&mut rng));
                        text.push('\n');
                    }
                    if at == *noise_lines {
                        text.push_str(&format!("Part {} of {}: {part}\n", n + 1, parts.len()));
                    }
                    std::fs::write(base.join(format!("note-{:02}.md", names[n] + 1)), text)?;
                }
            }
        }
        Ok(())
    }
}

fn write_planted(
    path: &Path,
    mut lines: Vec<String>,
    plant: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let mut planted: Vec<(usize, &String)> = plant
        .iter()
        .filter_map(|(k, v)| k.parse().ok().map(|n| (n, v)))
        .collect();
    planted.sort_by_key(|(n, _)| *n);
    // From the end, so earlier line numbers stay put.
    for (at, text) in planted.iter().rev() {
        let i = at.saturating_sub(1).min(lines.len());
        for (k, l) in text.lines().enumerate() {
            lines.insert(i + k, l.to_string());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text)
}

pub fn sentence(rng: &mut Rng) -> String {
    let n = 8 + rng.below(10);
    let mut words: Vec<&str> = (0..n).map(|_| rng.pick(WORDS)).collect();
    let first = words[0];
    let cap = format!("{}{}", first[..1].to_ascii_uppercase(), &first[1..]);
    words[0] = &cap;
    format!("{}.", words.join(" "))
}

fn python_function(rng: &mut Rng) -> String {
    let verb = rng.pick(VERBS);
    let noun = rng.pick(WORDS);
    let n = rng.below(1000);
    let (k, m) = (rng.below(9) + 2, rng.below(50));
    format!(
        "def {verb}_{noun}_{n}(value, scale={k}):\n    \"\"\"{} the {noun} value.\"\"\"\n    result = value * scale + {m}\n    if result > {}:\n        result -= {m}\n    return result\n",
        capital(verb),
        100 + m * 3
    )
}

fn python_module(rng: &mut Rng, functions: usize) -> String {
    let mut text = format!(
        "\"\"\"{} helpers.\"\"\"\n\nimport math\n\n",
        capital(rng.pick(WORDS))
    );
    for _ in 0..functions {
        text.push_str(&python_function(rng));
        text.push_str("\n\n");
    }
    text
}

fn capital(w: &str) -> String {
    format!("{}{}", w[..1].to_ascii_uppercase(), &w[1..])
}

// ---------- documents and images (RAG and vision suites) ----------

/// A one-page-per-paragraph PDF with plain Helvetica text (uncompressed).
pub fn pdf(paragraphs: &[String]) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    let pages = paragraphs.len().max(1);
    // 1: catalog, 2: pages, 3: font, then page + content per paragraph.
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i * 2)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {pages} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for (i, p) in paragraphs.iter().enumerate() {
        let content_id = 5 + i * 2;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
        ));
        let mut stream = String::from("BT /F1 11 Tf 14 TL 50 740 Td\n");
        for line in wrap(p, 90) {
            let escaped = line
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            stream.push_str(&format!("({escaped}) Tj T*\n"));
        }
        stream.push_str("ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for w in text.split_whitespace() {
        if !line.is_empty() && line.len() + w.len() + 1 > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A minimal DOCX (a stored, uncompressed zip) with one paragraph each.
pub fn docx(paragraphs: &[String]) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let body: String = paragraphs
        .iter()
        .map(|p| {
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                esc(p)
            )
        })
        .collect();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>"
    );
    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    stored_zip(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

/// A zip archive with stored (uncompressed) entries.
pub fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend_from_slice(&sig.to_le_bytes());
            if central {
                h.extend_from_slice(&20u16.to_le_bytes()); // made by
            }
            h.extend_from_slice(&20u16.to_le_bytes()); // needed
            h.extend_from_slice(&0u16.to_le_bytes()); // flags
            h.extend_from_slice(&0u16.to_le_bytes()); // stored
            h.extend_from_slice(&0u32.to_le_bytes()); // time, date
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central {
                h.extend_from_slice(&0u16.to_le_bytes()); // comment
                h.extend_from_slice(&0u16.to_le_bytes()); // disk
                h.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                h.extend_from_slice(&0u32.to_le_bytes()); // external attrs
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Standard base64 with padding (for `data:` URLs).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// An RGB PNG (stored deflate blocks) of a pattern from the seed: coloured
/// rectangles, so a vision model has something to describe.
pub fn png(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let rects: Vec<(u32, u32, u32, u32, [u8; 3])> = (0..6)
        .map(|_| {
            let (x, y) = (
                rng.below(width as usize) as u32,
                rng.below(height as usize) as u32,
            );
            let (w, h) = (
                width / 6 + rng.below((width / 3) as usize) as u32,
                height / 6 + rng.below((height / 3) as usize) as u32,
            );
            let c = [
                rng.below(256) as u8,
                rng.below(256) as u8,
                rng.below(256) as u8,
            ];
            (x, y, w, h, c)
        })
        .collect();
    let mut raw = Vec::with_capacity(((width * 3 + 1) * height) as usize);
    for y in 0..height {
        raw.push(0); // no filter
        for x in 0..width {
            let mut px = [245u8, 245, 240];
            for (rx, ry, rw, rh, c) in &rects {
                if x >= *rx && x < rx + rw && y >= *ry && y < ry + rh {
                    px = *c;
                }
            }
            raw.extend_from_slice(&px);
        }
    }
    // zlib with stored blocks.
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(u8::from(last));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        c.extend_from_slice(&body);
        c.extend_from_slice(&crc32(&body).to_be_bytes());
        c
    };
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &z));
    out.extend(chunk(b"IEND", &[]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_deterministic_and_plant_text() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let g = Generator::LongFile {
            path: "x/long.py".into(),
            lines: 500,
            seed: 4,
            plant: [(
                "250".to_string(),
                "PLANTED = 1\nPLANTED_TOO = 2".to_string(),
            )]
            .into(),
        };
        g.write(a.path()).unwrap();
        g.write(b.path()).unwrap();
        let ta = std::fs::read_to_string(a.path().join("x/long.py")).unwrap();
        assert_eq!(
            ta,
            std::fs::read_to_string(b.path().join("x/long.py")).unwrap()
        );
        let lines: Vec<&str> = ta.lines().collect();
        assert_eq!(lines.len(), 502);
        assert_eq!(lines[249], "PLANTED = 1");
        let pkg = Generator::PythonPackage {
            dir: "src/app".into(),
            files: 40,
            seed: 1,
        };
        pkg.write(a.path()).unwrap();
        let count = walk(&a.path().join("src/app"));
        assert!(count >= 40, "{count}");
        let notes = Generator::Notes {
            dir: "notes".into(),
            parts: vec!["al".into(), "pha".into()],
            noise_lines: 20,
            seed: 2,
        };
        notes.write(a.path()).unwrap();
        let all: String = (1..=2)
            .map(|i| {
                std::fs::read_to_string(a.path().join(format!("notes/note-{i:02}.md"))).unwrap()
            })
            .collect();
        assert!(all.contains("Part 1 of 2: al") && all.contains("Part 2 of 2: pha"));
    }

    fn walk(p: &Path) -> usize {
        std::fs::read_dir(p)
            .unwrap()
            .flatten()
            .map(|e| {
                if e.path().is_dir() {
                    walk(&e.path())
                } else {
                    1
                }
            })
            .sum()
    }

    #[test]
    fn documents_and_images_are_well_formed() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let p = pdf(&["Hello (world) from the bench".into(), "Second page".into()]);
        assert!(p.starts_with(b"%PDF-1.4") && p.ends_with(b"%%EOF\n"));
        let d = docx(&["One".into(), "Two & three".into()]);
        assert_eq!(&d[..4], b"PK\x03\x04");
        let img = png(64, 48, 3);
        assert_eq!(&img[..8], b"\x89PNG\r\n\x1a\n");
    }
}
