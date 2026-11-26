//! Describe an executable without running it
//! (built-in analyzer in a sandboxed worker).
//!
//! Everything here treats the input as hostile: parsing is bounded, output
//! is capped, and nothing is ever executed or written. The worker that calls
//! this runs with no network, no file access and one process (ADR-012).
//!
//! Operations:
//! - `info`: format, architecture, entry point, security flags, sections
//!   (with entropy), imports, exports, debug and signature presence.
//! - `strings`: ASCII and UTF-16LE strings with their offsets.
//! - `functions`: known function starts (exports, symbols, x64 unwind data).
//! - `disasm`: x86/x64 disassembly at an address or symbol, with import,
//!   symbol and string references noted.
//! - `hex`: a hex dump at a file offset or virtual address.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use goblin::Object;

/// Most characters returned by one operation (Pi's read limit is 50 KB).
pub const MAX_OUTPUT: usize = 48 * 1024;
/// Most instructions decoded per request.
pub const MAX_INSTRUCTIONS: usize = 2000;
/// Largest hex dump per request (bytes).
pub const MAX_HEX: usize = 4096;
/// Most entries listed per page (strings, functions).
pub const MAX_PAGE: usize = 2000;
/// Imports and exports listed in `info` before "… more".
const MAX_LISTED: usize = 300;

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Info,
    Strings {
        min_len: usize,
        offset: usize,
        limit: usize,
        filter: Option<String>,
    },
    Functions {
        offset: usize,
        limit: usize,
        filter: Option<String>,
    },
    Disasm {
        target: String,
        count: usize,
    },
    Hex {
        /// `Some` for a virtual address, else `offset` is a file offset.
        address: Option<u64>,
        offset: u64,
        length: usize,
    },
}

/// Run one operation over the file's bytes.
pub fn run(bytes: &[u8], op: &Op) -> Result<String, String> {
    let out = match op {
        Op::Strings {
            min_len,
            offset,
            limit,
            filter,
        } => strings(bytes, *min_len, *offset, *limit, filter.as_deref()),
        Op::Hex {
            address,
            offset,
            length,
        } => {
            let file_offset = match address {
                Some(va) => Image::parse(bytes)?
                    .file_offset(*va)
                    .ok_or_else(|| format!("address {va:#x} is not backed by file data"))?,
                None => *offset,
            };
            hexdump(bytes, file_offset, *length)
        }
        Op::Info => Image::parse(bytes)?.info,
        Op::Functions {
            offset,
            limit,
            filter,
        } => Image::parse(bytes)?.functions(*offset, *limit, filter.as_deref()),
        Op::Disasm { target, count } => Image::parse(bytes)?.disassemble(bytes, target, *count)?,
    };
    Ok(cap(out))
}

fn cap(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n[output truncated; ask for a smaller range]");
    }
    s
}

/// Shannon entropy in bits per byte (0–8). Packed or encrypted data is
/// close to 8.
pub fn entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for b in data {
        counts[*b as usize] += 1;
    }
    let n = data.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Version information and the manifest's requested privileges from a PE's
/// resources. Every value comes from the file, so it is cleaned and is only
/// what the file claims about itself (it isn't verified).
fn pe_resource_lines(pe: &goblin::pe::PE) -> Vec<String> {
    let mut out = Vec::new();
    let Some(res) = &pe.resource_data else {
        return out;
    };
    if let Some(v) = &res.version_info {
        let mut parts: Vec<String> = Vec::new();
        if let Some(fixed) = v.fixed_info.as_ref().filter(|f| f.is_valid()) {
            parts.push(format!("file {}", fixed.file_version().to_string()));
            parts.push(format!("product {}", fixed.product_version().to_string()));
        }
        let s = &v.string_info;
        for (label, value) in [
            ("product name", s.product_name()),
            ("description", s.file_description()),
            ("company", s.company_name()),
            ("original file name", s.original_filename()),
            ("internal name", s.internal_name()),
            ("copyright", s.legal_copyright()),
        ] {
            if let Some(value) = value.map(|x| x.trim_end_matches('\0').trim().to_string())
                && !value.is_empty()
            {
                parts.push(format!("{label} \"{}\"", clean(&value, 120)));
            }
        }
        if !parts.is_empty() {
            out.push(format!(
                "Version info (claimed by the file, not verified): {}",
                parts.join("; ")
            ));
        }
    }
    if let Some(m) = &res.manifest_data {
        let text = String::from_utf8_lossy(m.data);
        // Only the requestedExecutionLevel element's own attributes.
        let element = text
            .find("requestedExecutionLevel")
            .map(|at| &text[at..])
            .map(|rest| &rest[..rest.find('>').unwrap_or(rest.len())])
            .unwrap_or_default();
        let attr = |name: &str| {
            let at = element.find(&format!(" {name}"))? + name.len() + 1;
            let rest = element[at..].trim_start().strip_prefix('=')?.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let rest = &rest[1..];
            Some(clean(&rest[..rest.find(quote)?], 40))
        };
        if let Some(level) = attr("level") {
            let ui = attr("uiAccess").unwrap_or_else(|| "false".into());
            out.push(format!(
                "Manifest: requested execution level \"{level}\", uiAccess {ui}{}",
                if level == "requireAdministrator" {
                    " (asks for administrator rights when started)"
                } else {
                    ""
                }
            ));
        }
    }
    out
}

/// Model text from a hostile file: control characters are shown escaped
/// so the output can't fake structure.
fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (n, ch) in s.chars().enumerate() {
        if n >= max {
            out.push('…');
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// A string found in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundString {
    pub offset: usize,
    pub wide: bool,
    pub text: String,
}

/// ASCII and UTF-16LE runs of printable characters, in file order.
pub fn find_strings(bytes: &[u8], min_len: usize) -> Vec<FoundString> {
    let min_len = min_len.clamp(3, 64);
    let printable = |b: u8| (0x20..0x7f).contains(&b) || b == b'\t';
    let mut out = Vec::new();
    // ASCII.
    let mut start = None;
    for (i, &b) in bytes
        .iter()
        .enumerate()
        .chain(std::iter::once((bytes.len(), &0u8)))
    {
        if i < bytes.len() && printable(b) {
            start.get_or_insert(i);
        } else if let Some(s) = start.take()
            && i - s >= min_len
        {
            out.push(FoundString {
                offset: s,
                wide: false,
                text: String::from_utf8_lossy(&bytes[s..i]).into_owned(),
            });
        }
    }
    // UTF-16LE (ASCII-range characters), at both alignments.
    for align in 0..2 {
        let mut start = None;
        let mut text = String::new();
        let mut i = align;
        while i + 1 < bytes.len() + 1 {
            let unit = if i + 1 < bytes.len() {
                Some((bytes[i], bytes[i + 1]))
            } else {
                None
            };
            match unit {
                Some((lo, 0)) if printable(lo) => {
                    start.get_or_insert(i);
                    text.push(lo as char);
                }
                _ => {
                    if let Some(s) = start.take()
                        && text.len() >= min_len
                    {
                        out.push(FoundString {
                            offset: s,
                            wide: true,
                            text: std::mem::take(&mut text),
                        });
                    }
                    text.clear();
                }
            }
            i += 2;
        }
    }
    out.sort_by_key(|s| (s.offset, s.wide));
    out
}

fn strings(
    bytes: &[u8],
    min_len: usize,
    offset: usize,
    limit: usize,
    filter: Option<&str>,
) -> String {
    let image = Image::parse(bytes).ok();
    let filter = filter.map(str::to_lowercase);
    let all: Vec<FoundString> = find_strings(bytes, min_len)
        .into_iter()
        .filter(|s| {
            filter
                .as_deref()
                .is_none_or(|f| s.text.to_lowercase().contains(f))
        })
        .collect();
    let limit = limit.clamp(1, MAX_PAGE);
    let mut out = format!(
        "{} strings{} (showing {}–{}). Columns: file offset, [virtual address], a=ASCII u=UTF-16, text.\n",
        all.len(),
        filter
            .as_deref()
            .map(|f| format!(" containing \"{}\"", clean(f, 80)))
            .unwrap_or_default(),
        offset.min(all.len()),
        (offset + limit).min(all.len())
    );
    for s in all.iter().skip(offset).take(limit) {
        let va = image
            .as_ref()
            .and_then(|i| i.virtual_address(s.offset as u64))
            .map(|va| format!(" [{va:#x}]"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{:#08x}{va} {} {}",
            s.offset,
            if s.wide { 'u' } else { 'a' },
            clean(&s.text, 200)
        );
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    if offset + limit < all.len() {
        let _ = writeln!(out, "[more: use offset={}]", offset + limit);
    }
    out
}

/// A classic hex dump.
pub fn hexdump(bytes: &[u8], offset: u64, length: usize) -> String {
    let start = offset.min(bytes.len() as u64) as usize;
    let end = start
        .saturating_add(length.clamp(1, MAX_HEX))
        .min(bytes.len());
    if start >= bytes.len() {
        return format!(
            "offset {offset:#x} is past the end of the file ({} bytes)",
            bytes.len()
        );
    }
    let mut out = String::new();
    for (row, chunk) in bytes[start..end].chunks(16).enumerate() {
        let _ = write!(out, "{:08x}  ", start + row * 16);
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => {
                    let _ = write!(out, "{b:02x} ");
                }
                None => out.push_str("   "),
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        for b in chunk {
            out.push(if (0x20..0x7f).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    X86,
    X64,
    Other,
}

/// File-backed part of the address space.
#[derive(Debug, Clone)]
struct Region {
    va: u64,
    file_offset: u64,
    file_size: u64,
}

/// A known function or data symbol.
#[derive(Debug, Clone)]
struct Symbol {
    va: u64,
    /// Known size (x64 unwind data, ELF symbols), else 0.
    size: u64,
    name: String,
    kind: &'static str,
}

/// What the analyzer needs from a parsed executable.
struct Image {
    arch: Arch,
    entry: Option<u64>,
    regions: Vec<Region>,
    symbols: Vec<Symbol>,
    /// Import address table slots (PE): address → `dll!name`.
    import_slots: HashMap<u64, String>,
    info: String,
}

impl Image {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        match Object::parse(bytes) {
            Ok(Object::PE(pe)) => Ok(pe_image(bytes, &pe)),
            Ok(Object::Elf(elf)) => Ok(elf_image(bytes, &elf)),
            Ok(Object::Mach(goblin::mach::Mach::Binary(m))) => Ok(mach_image(bytes, &m, None)),
            Ok(Object::Mach(goblin::mach::Mach::Fat(fat))) => {
                let arches: Vec<String> = fat
                    .iter_arches()
                    .filter_map(Result::ok)
                    .map(|a| {
                        format!(
                            "{} ({} bytes at {:#x})",
                            cpu_name(a.cputype),
                            a.size,
                            a.offset
                        )
                    })
                    .collect();
                // Describe the first slice; the rest are listed.
                match fat.get(0) {
                    Ok(goblin::mach::SingleArch::MachO(m)) => {
                        Ok(mach_image(bytes, &m, Some(arches.join(", "))))
                    }
                    _ => Err(format!(
                        "universal Mach-O with slices: {}",
                        arches.join(", ")
                    )),
                }
            }
            Ok(Object::Archive(a)) => Err(format!(
                "a static library archive with {} members; strings and hex still work",
                a.len()
            )),
            Ok(_) => Err(
                "an object format the analyzer doesn't describe; strings and hex still work".into(),
            ),
            Err(e) => Err(format!(
                "not a recognised executable (PE, ELF or Mach-O): {}; strings and hex still work",
                clean(&e.to_string(), 200)
            )),
        }
    }

    fn file_offset(&self, va: u64) -> Option<u64> {
        self.regions
            .iter()
            .find(|r| va >= r.va && va < r.va.saturating_add(r.file_size))
            .map(|r| r.file_offset.saturating_add(va - r.va))
    }

    fn virtual_address(&self, offset: u64) -> Option<u64> {
        self.regions
            .iter()
            .find(|r| offset >= r.file_offset && offset < r.file_offset.saturating_add(r.file_size))
            .map(|r| r.va.saturating_add(offset - r.file_offset))
    }

    fn symbol_at(&self, va: u64) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.va == va)
    }

    fn functions(&self, offset: usize, limit: usize, filter: Option<&str>) -> String {
        let filter = filter.map(str::to_lowercase);
        let list: Vec<&Symbol> = self
            .symbols
            .iter()
            .filter(|s| {
                filter
                    .as_deref()
                    .is_none_or(|f| s.name.to_lowercase().contains(f))
            })
            .collect();
        let limit = limit.clamp(1, MAX_PAGE);
        let mut out = format!(
            "{} known functions and symbols (showing {}–{}). Columns: address, size, kind, name.\n",
            list.len(),
            offset.min(list.len()),
            (offset + limit).min(list.len())
        );
        if let Some(e) = self.entry {
            let _ = writeln!(out, "entry point: {e:#x}");
        }
        for s in list.iter().skip(offset).take(limit) {
            let _ = writeln!(
                out,
                "{:#x} {} {} {}",
                s.va,
                if s.size > 0 {
                    s.size.to_string()
                } else {
                    "?".into()
                },
                s.kind,
                clean(&s.name, 160)
            );
            if out.len() > MAX_OUTPUT {
                break;
            }
        }
        if offset + limit < list.len() {
            let _ = writeln!(out, "[more: use offset={}]", offset + limit);
        }
        out
    }

    fn resolve_target(&self, target: &str) -> Result<(u64, Option<u64>), String> {
        let t = target.trim();
        if t.eq_ignore_ascii_case("entry") {
            let e = self.entry.ok_or("this file has no entry point")?;
            let size = self.symbol_at(e).map(|s| s.size).filter(|s| *s > 0);
            return Ok((e, size));
        }
        let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
        if let Some(h) = hex
            && let Ok(va) = u64::from_str_radix(h, 16)
        {
            let size = self.symbol_at(va).map(|s| s.size).filter(|s| *s > 0);
            return Ok((va, size));
        }
        if let Some(s) = self.symbols.iter().find(|s| s.name == t) {
            return Ok((s.va, Some(s.size).filter(|s| *s > 0)));
        }
        let lower = t.to_lowercase();
        let matches: Vec<&Symbol> = self
            .symbols
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&lower))
            .collect();
        match matches.as_slice() {
            [one] => Ok((one.va, Some(one.size).filter(|s| *s > 0))),
            [] => Err(format!(
                "no symbol matches \"{}\"; use an address like 0x401000, \"entry\", or op=functions to list names",
                clean(t, 80)
            )),
            many => Err(format!(
                "\"{}\" matches {} symbols, for example {}; be more specific",
                clean(t, 80),
                many.len(),
                many.iter()
                    .take(5)
                    .map(|s| clean(&s.name, 60))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn disassemble(&self, bytes: &[u8], target: &str, count: usize) -> Result<String, String> {
        use iced_x86::{
            Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind,
        };
        let bitness = match self.arch {
            Arch::X86 => 32,
            Arch::X64 => 64,
            Arch::Other => {
                return Err(
                    "disassembly supports x86 and x64 only; use hex, strings or decompile".into(),
                );
            }
        };
        let (va, size) = self.resolve_target(target)?;
        let start = self
            .file_offset(va)
            .ok_or_else(|| format!("address {va:#x} is not backed by file data"))?;
        // Headers are hostile: every bound is checked against the file.
        let len = bytes.len() as u64;
        let region_end = self
            .regions
            .iter()
            .find(|r| va >= r.va && va < r.va.saturating_add(r.file_size))
            .map(|r| r.file_offset.saturating_add(r.file_size))
            .unwrap_or(len)
            .min(len);
        let mut end = region_end;
        if let Some(size) = size {
            end = end.min(start.saturating_add(size));
        }
        if start >= end {
            return Err(format!(
                "address {va:#x} is past the end of the file's data"
            ));
        }
        let data = &bytes[start as usize..end as usize];
        let count = count.clamp(1, MAX_INSTRUCTIONS);
        let mut decoder = Decoder::with_ip(bitness, data, va, DecoderOptions::NONE);
        let mut formatter = IntelFormatter::new();
        formatter.options_mut().set_first_operand_char_index(8);
        let mut out = String::new();
        if let Some(s) = self.symbol_at(va) {
            let _ = writeln!(out, "{}:", clean(&s.name, 160));
        }
        let mut instr = Instruction::default();
        let mut text = String::new();
        let mut n = 0;
        while decoder.can_decode() && n < count {
            decoder.decode_out(&mut instr);
            n += 1;
            text.clear();
            formatter.format(&instr, &mut text);
            let at = (instr.ip() - va) as usize;
            let raw: String = data[at..at + instr.len()]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let _ = write!(out, "{:#x}  {:<20} {}", instr.ip(), raw, text);
            // Notes: where branches and memory references point.
            let mut notes = Vec::new();
            let target = match instr.op0_kind() {
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    Some(instr.near_branch_target())
                }
                _ => None,
            };
            if let Some(t) = target
                && let Some(s) = self.symbol_at(t)
            {
                notes.push(clean(&s.name, 120));
            }
            for i in 0..instr.op_count() {
                if instr.op_kind(i) != OpKind::Memory {
                    continue;
                }
                let addr = if instr.is_ip_rel_memory_operand() {
                    Some(instr.ip_rel_memory_address())
                } else if instr.memory_base() == iced_x86::Register::None
                    && instr.memory_index() == iced_x86::Register::None
                {
                    Some(instr.memory_displacement64())
                } else {
                    None
                };
                if let Some(a) = addr {
                    notes.extend(self.describe_address(bytes, a));
                }
            }
            if matches!(
                instr.op1_kind(),
                OpKind::Immediate32 | OpKind::Immediate64 | OpKind::Immediate32to64
            ) && let imm = instr.immediate(1)
                && imm > 0x1000
            {
                notes.extend(self.describe_address(bytes, imm));
            }
            if !notes.is_empty() {
                let _ = write!(out, "    ; {}", notes.join(", "));
            }
            out.push('\n');
            if instr.is_invalid() {
                out.push_str(
                    "[invalid instruction: this may be data, or the address is not a code start]\n",
                );
                break;
            }
            if size.is_none()
                && matches!(
                    instr.flow_control(),
                    FlowControl::Return | FlowControl::UnconditionalBranch | FlowControl::Interrupt
                )
            {
                // Without a known size, stop where straight-line code ends.
                out.push_str(
                    "[stopped at the end of straight-line code; ask for a later address to continue]\n",
                );
                break;
            }
            if out.len() > MAX_OUTPUT {
                break;
            }
        }
        if n == count {
            let _ = writeln!(out, "[stopped after {count} instructions]");
        }
        Ok(out)
    }

    /// What an address refers to: an import slot, a symbol, or a string.
    fn describe_address(&self, bytes: &[u8], addr: u64) -> Option<String> {
        if let Some(name) = self.import_slots.get(&addr) {
            return Some(clean(name, 120));
        }
        if let Some(s) = self.symbol_at(addr) {
            return Some(clean(&s.name, 120));
        }
        let off = self.file_offset(addr)? as usize;
        let tail = bytes.get(off..off.saturating_add(256).min(bytes.len()))?;
        let ascii: Vec<u8> = tail
            .iter()
            .copied()
            .take_while(|b| (0x20..0x7f).contains(b))
            .collect();
        if ascii.len() >= 4 && tail.get(ascii.len()) == Some(&0) {
            return Some(format!(
                "\"{}\"",
                clean(&String::from_utf8_lossy(&ascii), 80)
            ));
        }
        let wide: String = tail
            .as_chunks::<2>()
            .0
            .iter()
            .take_while(|[lo, hi]| (0x20..0x7f).contains(lo) && *hi == 0)
            .map(|[lo, _]| *lo as char)
            .collect();
        (wide.len() >= 4).then(|| format!("L\"{}\"", clean(&wide, 80)))
    }
}

fn section_line(out: &mut String, name: &str, va: u64, vsize: u64, raw: &[u8], flags: &str) {
    let _ = writeln!(
        out,
        "  {:<10} va {:#010x} size {:>9} file {:>9}  entropy {:.2}  {}",
        clean(name, 16),
        va,
        vsize,
        raw.len(),
        entropy(raw),
        flags
    );
}

fn slice(bytes: &[u8], offset: u64, size: u64) -> &[u8] {
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    &bytes[start..end]
}

fn pe_image(bytes: &[u8], pe: &goblin::pe::PE) -> Image {
    use goblin::pe::section_table::*;
    let mut info = String::new();
    let machine = pe.header.coff_header.machine;
    let arch = match machine {
        0x14c => Arch::X86,
        0x8664 => Arch::X64,
        _ => Arch::Other,
    };
    let machine_name = match machine {
        0x14c => "x86 (i386)",
        0x8664 => "x64 (AMD64)",
        0xaa64 => "ARM64",
        0x1c4 => "ARM Thumb-2",
        0xa641 => "ARM64EC",
        _ => "other",
    };
    let opt = pe.header.optional_header.as_ref();
    let subsystem = opt.map(|o| o.windows_fields.subsystem).unwrap_or(0);
    let dllchar = opt
        .map(|o| o.windows_fields.dll_characteristics)
        .unwrap_or(0);
    let kind = if pe.is_lib {
        "DLL"
    } else if subsystem == 1 {
        "native image (driver or boot program)"
    } else {
        "executable"
    };
    let _ = writeln!(
        info,
        "Format: PE{} {kind}",
        if pe.is_64 { "32+" } else { "32" }
    );
    let _ = writeln!(info, "Machine: {machine_name} ({machine:#x})");
    let _ = writeln!(
        info,
        "Subsystem: {}",
        match subsystem {
            1 => "native",
            2 => "Windows GUI",
            3 => "Windows console",
            9 => "Windows CE",
            10..=13 => "EFI",
            _ => "other",
        }
    );
    if let Some(name) = pe.name {
        let _ = writeln!(info, "Internal name: {}", clean(name, 120));
    }
    let _ = writeln!(info, "Image base: {:#x}", pe.image_base);
    let entry = (pe.entry != 0).then(|| pe.image_base.wrapping_add(pe.entry as u64));
    if let Some(e) = entry {
        let _ = writeln!(info, "Entry point: {e:#x}");
    }
    let _ = writeln!(
        info,
        "Linker timestamp: {} (seconds since 1970; often faked or a build hash)",
        pe.header.coff_header.time_date_stamp
    );
    let flag = |bit: u16, name: &str| {
        if dllchar & bit != 0 {
            Some(name.to_string())
        } else {
            None
        }
    };
    let flags: Vec<String> = [
        flag(0x0040, "ASLR"),
        flag(0x0020, "high-entropy ASLR"),
        flag(0x0100, "DEP/NX"),
        flag(0x4000, "Control Flow Guard"),
        flag(0x0080, "force integrity"),
        flag(0x0400, "no SEH"),
        flag(0x1000, "AppContainer"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let _ = writeln!(
        info,
        "Security flags: {}",
        if flags.is_empty() {
            "none".into()
        } else {
            flags.join(", ")
        }
    );
    let _ = writeln!(
        info,
        "Authenticode signature: {}",
        if pe.certificates.is_empty() {
            "none".to_string()
        } else {
            format!(
                "present ({} certificate entr{}; not verified here)",
                pe.certificates.len(),
                if pe.certificates.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        }
    );
    for line in pe_resource_lines(pe) {
        let _ = writeln!(info, "{line}");
    }
    if pe.clr_data.is_some() {
        let _ = writeln!(
            info,
            ".NET: managed assembly (CLR header present); native disassembly shows only the loader stub, decompile it instead"
        );
    }
    if let Some(d) = &pe.debug_data
        && let Some(cv) = &d.codeview_pdb70_debug_info
    {
        let _ = writeln!(
            info,
            "Debug symbols: {}",
            clean(
                String::from_utf8_lossy(cv.filename).trim_end_matches('\0'),
                200
            )
        );
    }
    if pe.tls_data.is_some() {
        let _ = writeln!(
            info,
            "TLS directory: present (TLS callbacks can run before the entry point)"
        );
    }

    let mut regions = Vec::new();
    let mut last_end = 0u64;
    let _ = writeln!(info, "Sections ({}):", pe.sections.len());
    for s in &pe.sections {
        let name = s.real_name.clone().unwrap_or_else(|| {
            String::from_utf8_lossy(&s.name)
                .trim_end_matches('\0')
                .to_string()
        });
        let raw = slice(
            bytes,
            s.pointer_to_raw_data as u64,
            s.size_of_raw_data as u64,
        );
        let c = s.characteristics;
        let mut perms = String::new();
        perms.push(if c & IMAGE_SCN_MEM_READ != 0 {
            'r'
        } else {
            '-'
        });
        perms.push(if c & IMAGE_SCN_MEM_WRITE != 0 {
            'w'
        } else {
            '-'
        });
        perms.push(if c & IMAGE_SCN_MEM_EXECUTE != 0 {
            'x'
        } else {
            '-'
        });
        if c & IMAGE_SCN_MEM_WRITE != 0 && c & IMAGE_SCN_MEM_EXECUTE != 0 {
            perms.push_str(" (writable and executable)");
        }
        let va = pe.image_base.wrapping_add(s.virtual_address as u64);
        section_line(&mut info, &name, va, s.virtual_size as u64, raw, &perms);
        let file_size = (s.size_of_raw_data as u64).min(if s.virtual_size > 0 {
            s.virtual_size as u64
        } else {
            u64::MAX
        });
        regions.push(Region {
            va,
            file_offset: s.pointer_to_raw_data as u64,
            file_size,
        });
        last_end = last_end.max(s.pointer_to_raw_data as u64 + s.size_of_raw_data as u64);
    }
    let cert_bytes: u64 = pe.certificates.iter().map(|c| c.length as u64).sum();
    let overlay = (bytes.len() as u64)
        .saturating_sub(last_end)
        .saturating_sub(cert_bytes);
    if overlay > 0 {
        let _ = writeln!(
            info,
            "Overlay: {overlay} bytes after the last section (installers and packed files keep data here; entropy {:.2})",
            entropy(slice(bytes, last_end, overlay))
        );
    }

    let mut import_slots = HashMap::new();
    let mut by_dll: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for imp in &pe.imports {
        by_dll
            .entry(imp.dll)
            .or_default()
            .push(imp.name.to_string());
        import_slots.insert(
            pe.image_base.wrapping_add(imp.offset as u64),
            format!("{}!{}", imp.dll, imp.name),
        );
    }
    let _ = writeln!(
        info,
        "Imports: {} functions from {} libraries",
        pe.imports.len(),
        by_dll.len()
    );
    let mut listed = 0;
    for (dll, names) in &by_dll {
        let room = MAX_LISTED.saturating_sub(listed);
        let shown: Vec<String> = names.iter().take(room).map(|n| clean(n, 80)).collect();
        listed += shown.len();
        let more = names.len() - shown.len();
        let _ = writeln!(
            info,
            "  {} ({}): {}{}",
            clean(dll, 80),
            names.len(),
            shown.join(", "),
            if more > 0 {
                format!(", … {more} more")
            } else {
                String::new()
            }
        );
    }

    let mut symbols = Vec::new();
    let _ = writeln!(info, "Exports: {}", pe.exports.len());
    for (i, e) in pe.exports.iter().enumerate() {
        let name = e
            .name
            .map(str::to_string)
            .unwrap_or_else(|| format!("export_{i}"));
        if i < MAX_LISTED {
            let _ = writeln!(
                info,
                "  {:#x} {}{}",
                pe.image_base.wrapping_add(e.rva as u64),
                clean(&name, 120),
                if e.reexport.is_some() {
                    " (forwarded)"
                } else {
                    ""
                }
            );
        }
        if e.reexport.is_none() {
            symbols.push(Symbol {
                va: pe.image_base.wrapping_add(e.rva as u64),
                size: 0,
                name,
                kind: "export",
            });
        }
    }
    if pe.exports.len() > MAX_LISTED {
        let _ = writeln!(
            info,
            "  … {} more (op=functions lists them)",
            pe.exports.len() - MAX_LISTED
        );
    }
    // x64 unwind data gives every non-leaf function's start and size.
    if let Some(ex) = &pe.exception_data {
        for f in ex.functions().filter_map(Result::ok).take(200_000) {
            let va = pe.image_base.wrapping_add(f.begin_address as u64);
            if let Some(s) = symbols.iter_mut().find(|s| s.va == va) {
                s.size = (f.end_address.saturating_sub(f.begin_address)) as u64;
            } else {
                symbols.push(Symbol {
                    va,
                    size: (f.end_address.saturating_sub(f.begin_address)) as u64,
                    name: format!("sub_{va:x}"),
                    kind: "function",
                });
            }
        }
    }
    if let Some(e) = entry {
        match symbols.iter_mut().find(|s| s.va == e) {
            Some(s) if s.name.starts_with("sub_") => s.name = "entry".into(),
            Some(_) => {}
            None => symbols.push(Symbol {
                va: e,
                size: 0,
                name: "entry".into(),
                kind: "entry",
            }),
        }
    }
    symbols.sort_by_key(|s| s.va);
    symbols.dedup_by_key(|s| s.va);
    Image {
        arch,
        entry,
        regions,
        symbols,
        import_slots,
        info,
    }
}

fn elf_image(bytes: &[u8], elf: &goblin::elf::Elf) -> Image {
    use goblin::elf::header::*;
    use goblin::elf::program_header::*;
    let mut info = String::new();
    let arch = match elf.header.e_machine {
        EM_386 => Arch::X86,
        EM_X86_64 => Arch::X64,
        _ => Arch::Other,
    };
    let _ = writeln!(
        info,
        "Format: ELF{} {} {}",
        if elf.is_64 { "64" } else { "32" },
        if elf.little_endian {
            "little-endian"
        } else {
            "big-endian"
        },
        match elf.header.e_type {
            ET_EXEC => "executable",
            ET_DYN if elf.interpreter.is_some() => "position-independent executable",
            ET_DYN => "shared library",
            ET_REL => "relocatable object",
            ET_CORE => "core dump",
            _ => "other",
        }
    );
    let _ = writeln!(info, "Machine: {}", machine_to_str(elf.header.e_machine));
    let entry = (elf.entry != 0).then_some(elf.entry);
    if let Some(e) = entry {
        let _ = writeln!(info, "Entry point: {e:#x}");
    }
    if let Some(i) = elf.interpreter {
        let _ = writeln!(info, "Interpreter: {}", clean(i, 200));
    }
    if let Some(s) = elf.soname {
        let _ = writeln!(info, "Shared object name: {}", clean(s, 200));
    }
    let nx = elf
        .program_headers
        .iter()
        .find(|p| p.p_type == PT_GNU_STACK)
        .map(|p| p.p_flags & PF_X == 0);
    let relro = elf.program_headers.iter().any(|p| p.p_type == PT_GNU_RELRO);
    let _ = writeln!(
        info,
        "Security flags: {}{}",
        match nx {
            Some(true) => "non-executable stack",
            Some(false) => "EXECUTABLE stack",
            None => "stack flags unknown",
        },
        if relro { ", RELRO" } else { "" }
    );
    let _ = writeln!(
        info,
        "Symbols: {} ({})",
        elf.syms.len(),
        if elf.syms.is_empty() {
            "stripped"
        } else {
            "not stripped"
        }
    );
    let mut regions = Vec::new();
    for p in elf.program_headers.iter().filter(|p| p.p_type == PT_LOAD) {
        regions.push(Region {
            va: p.p_vaddr,
            file_offset: p.p_offset,
            file_size: p.p_filesz,
        });
    }
    let _ = writeln!(info, "Sections ({}):", elf.section_headers.len());
    for s in elf.section_headers.iter().take(200) {
        let name = elf.shdr_strtab.get_at(s.sh_name).unwrap_or("");
        let raw = if s.sh_type == goblin::elf::section_header::SHT_NOBITS {
            &[][..]
        } else {
            slice(bytes, s.sh_offset, s.sh_size)
        };
        let mut perms = String::from("r");
        perms.push(if s.is_writable() { 'w' } else { '-' });
        perms.push(if s.is_executable() { 'x' } else { '-' });
        section_line(&mut info, name, s.sh_addr, s.sh_size, raw, &perms);
    }
    let _ = writeln!(
        info,
        "Needed libraries: {}",
        elf.libraries
            .iter()
            .map(|l| clean(l, 80))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut symbols = Vec::new();
    for (tab, strtab) in [(&elf.dynsyms, &elf.dynstrtab), (&elf.syms, &elf.strtab)] {
        for sym in tab.iter().take(200_000) {
            let Some(name) = strtab.get_at(sym.st_name).filter(|n| !n.is_empty()) else {
                continue;
            };
            if sym.is_import() {
                imports.push(name.to_string());
            } else if sym.st_value != 0 {
                if sym.is_function() {
                    symbols.push(Symbol {
                        va: sym.st_value,
                        size: sym.st_size,
                        name: name.to_string(),
                        kind: "function",
                    });
                }
                if std::ptr::eq(tab, &elf.dynsyms) {
                    exports.push(name.to_string());
                }
            }
        }
    }
    imports.sort();
    imports.dedup();
    let _ = writeln!(
        info,
        "Imported symbols ({}): {}{}",
        imports.len(),
        imports
            .iter()
            .take(MAX_LISTED)
            .map(|n| clean(n, 80))
            .collect::<Vec<_>>()
            .join(", "),
        if imports.len() > MAX_LISTED {
            ", …"
        } else {
            ""
        }
    );
    let _ = writeln!(
        info,
        "Exported symbols ({}): {}{}",
        exports.len(),
        exports
            .iter()
            .take(MAX_LISTED)
            .map(|n| clean(n, 80))
            .collect::<Vec<_>>()
            .join(", "),
        if exports.len() > MAX_LISTED {
            ", …"
        } else {
            ""
        }
    );
    if let Some(e) = entry
        && !symbols.iter().any(|s| s.va == e)
    {
        symbols.push(Symbol {
            va: e,
            size: 0,
            name: "entry".into(),
            kind: "entry",
        });
    }
    symbols.sort_by_key(|s| s.va);
    symbols.dedup_by_key(|s| s.va);
    Image {
        arch,
        entry,
        regions,
        symbols,
        import_slots: HashMap::new(),
        info,
    }
}

fn cpu_name(cpu: u32) -> &'static str {
    use goblin::mach::cputype::*;
    match cpu {
        CPU_TYPE_X86 => "x86",
        CPU_TYPE_X86_64 => "x64",
        CPU_TYPE_ARM => "ARM",
        CPU_TYPE_ARM64 => "ARM64",
        CPU_TYPE_ARM64_32 => "ARM64_32",
        CPU_TYPE_POWERPC => "PowerPC",
        CPU_TYPE_POWERPC64 => "PowerPC64",
        _ => "other",
    }
}

fn mach_image(bytes: &[u8], m: &goblin::mach::MachO, fat: Option<String>) -> Image {
    use goblin::mach::cputype::*;
    let mut info = String::new();
    let arch = match m.header.cputype() {
        CPU_TYPE_X86 => Arch::X86,
        CPU_TYPE_X86_64 => Arch::X64,
        _ => Arch::Other,
    };
    if let Some(slices) = &fat {
        let _ = writeln!(
            info,
            "Universal binary; slices: {slices}. Describing the first slice."
        );
    }
    let _ = writeln!(
        info,
        "Format: Mach-O {} {}",
        if m.is_64 { "64-bit" } else { "32-bit" },
        goblin::mach::header::filetype_to_str(m.header.filetype)
    );
    let _ = writeln!(info, "Machine: {}", cpu_name(m.header.cputype()));
    let entry = (m.entry != 0).then_some(m.entry);
    if let Some(e) = entry {
        let _ = writeln!(info, "Entry point: {e:#x}");
    }
    let _ = writeln!(
        info,
        "Security flags: {}",
        if m.header.flags & goblin::mach::header::MH_PIE != 0 {
            "PIE"
        } else {
            "not PIE"
        }
    );
    let _ = writeln!(
        info,
        "Libraries: {}",
        m.libs
            .iter()
            .map(|l| clean(l, 120))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut regions = Vec::new();
    let _ = writeln!(info, "Segments:");
    // A fat file's slices are at an offset; this describes a thin file or
    // the first slice as its own buffer.
    for seg in m.segments.iter() {
        let name = seg.name().unwrap_or("");
        let raw = slice(bytes, seg.fileoff, seg.filesize);
        let p = seg.initprot;
        let perms = format!(
            "{}{}{}",
            if p & 1 != 0 { 'r' } else { '-' },
            if p & 2 != 0 { 'w' } else { '-' },
            if p & 4 != 0 { 'x' } else { '-' }
        );
        section_line(&mut info, name, seg.vmaddr, seg.vmsize, raw, &perms);
        if fat.is_none() {
            regions.push(Region {
                va: seg.vmaddr,
                file_offset: seg.fileoff,
                file_size: seg.filesize,
            });
        }
    }
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    if let Some(syms) = &m.symbols {
        for (name, nlist) in syms.iter().filter_map(Result::ok).take(200_000) {
            if nlist.is_undefined() {
                imports.push(name.to_string());
            } else if nlist.n_value != 0 && !nlist.is_stab() {
                symbols.push(Symbol {
                    va: nlist.n_value,
                    size: 0,
                    name: name.to_string(),
                    kind: "symbol",
                });
            }
        }
    }
    let _ = writeln!(
        info,
        "Imported symbols ({}): {}{}",
        imports.len(),
        imports
            .iter()
            .take(MAX_LISTED)
            .map(|n| clean(n, 80))
            .collect::<Vec<_>>()
            .join(", "),
        if imports.len() > MAX_LISTED {
            ", …"
        } else {
            ""
        }
    );
    symbols.sort_by_key(|s| s.va);
    symbols.dedup_by_key(|s| s.va);
    Image {
        arch: if fat.is_some() { Arch::Other } else { arch },
        entry,
        regions,
        symbols,
        import_slots: HashMap::new(),
        info,
    }
}

#[cfg(test)]
mod tests;
