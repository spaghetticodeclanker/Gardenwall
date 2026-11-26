use super::*;

fn own_exe() -> Vec<u8> {
    std::fs::read(std::env::current_exe().unwrap()).unwrap()
}

#[test]
fn describes_its_own_executable() {
    let bytes = own_exe();
    let info = run(&bytes, &Op::Info).unwrap();
    if cfg!(windows) {
        assert!(info.starts_with("Format: PE32+ executable"), "{info}");
        assert!(info.contains("Machine: x64") || info.contains("Machine: ARM64"));
        assert!(info.contains("Imports: "));
        assert!(info.contains("kernel32.dll") || info.contains("KERNEL32.dll"));
        assert!(info.contains("Security flags:"));
    } else if cfg!(target_os = "linux") {
        assert!(info.starts_with("Format: ELF64"), "{info}");
    } else if cfg!(target_os = "macos") {
        assert!(info.contains("Mach-O"), "{info}");
    }
    assert!(info.contains("entropy"));
    assert!(info.len() <= MAX_OUTPUT + 100);
}

#[test]
fn lists_functions_and_disassembles_the_entry_point() {
    let bytes = own_exe();
    let functions = run(
        &bytes,
        &Op::Functions {
            offset: 0,
            limit: 400,
            filter: None,
        },
    )
    .unwrap();
    assert!(functions.contains("known functions"), "{functions}");
    if cfg!(target_arch = "x86_64") {
        let asm = run(
            &bytes,
            &Op::Disasm {
                target: "entry".into(),
                count: 20,
            },
        )
        .unwrap();
        // An address, raw bytes and a mnemonic on each line.
        let first = asm.lines().find(|l| l.starts_with("0x")).unwrap();
        assert!(first.split_whitespace().count() >= 3, "{asm}");
    }
    if cfg!(all(windows, target_arch = "x86_64")) {
        // Calls through the import table are named.
        let image = Image::parse(&bytes).unwrap();
        let named = image.symbols.iter().any(|s| {
            image
                .disassemble(&bytes, &format!("{:#x}", s.va), 400)
                .is_ok_and(|asm| asm.contains(".dll!"))
        });
        assert!(named, "no import call was annotated in any function");
    }
}

#[test]
fn strings_are_found_with_offsets_and_escaped() {
    let mut data = b"\x00\x01hello world\x00\x02".to_vec();
    data.extend("wide text".encode_utf16().flat_map(|u| u.to_le_bytes()));
    data.extend(b"\x00\x00\x07ab\x00line\nbreak!!\x00");
    let found = find_strings(&data, 4);
    let texts: Vec<(&str, bool)> = found.iter().map(|s| (s.text.as_str(), s.wide)).collect();
    assert!(texts.contains(&("hello world", false)));
    assert!(texts.contains(&("wide text", true)));
    assert!(found.iter().any(|s| s.offset == 2 && !s.wide));
    let out = run(
        &data,
        &Op::Strings {
            min_len: 4,
            offset: 0,
            limit: 10,
            filter: Some("WIDE".into()),
        },
    )
    .unwrap();
    assert!(out.starts_with("1 strings containing \"wide\""), "{out}");
    assert!(out.contains(" u wide text"));
    assert_eq!(clean("a\nb\u{1b}[31m", 50), "a\\nb\\u{1b}[31m");
}

#[test]
fn hex_dump_and_limits() {
    let data: Vec<u8> = (0u8..=40).collect();
    let out = run(
        &data,
        &Op::Hex {
            address: None,
            offset: 16,
            length: 20,
        },
    )
    .unwrap();
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("00000010  10 11 12"), "{out}");
    assert_eq!(out.lines().count(), 2);
    assert!(hexdump(&data, 999, 10).contains("past the end"));
    assert_eq!(
        hexdump(&vec![0u8; 100_000], 0, 1 << 20).lines().count(),
        MAX_HEX / 16
    );
}

#[test]
fn garbage_is_refused_without_panicking() {
    for bad in [
        &b""[..],
        b"MZ",
        b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff\x00\x00PE\x00\x00garbage",
        b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00",
        &[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1, 3, 0, 0, 0],
        &[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 200],
    ] {
        let err = run(bad, &Op::Info).unwrap_err();
        assert!(err.contains("strings and hex still work") || !err.is_empty());
        assert!(
            run(
                bad,
                &Op::Disasm {
                    target: "entry".into(),
                    count: 10
                }
            )
            .is_err()
        );
    }
    // Truncated versions of a real executable never panic.
    let bytes = own_exe();
    for cut in [64, 512, 1024, 4096, bytes.len() / 2] {
        let _ = run(&bytes[..cut.min(bytes.len())], &Op::Info);
        let _ = run(
            &bytes[..cut.min(bytes.len())],
            &Op::Functions {
                offset: 0,
                limit: 10,
                filter: None,
            },
        );
    }
}

#[test]
fn entropy_ranges() {
    assert_eq!(entropy(&[]), 0.0);
    assert_eq!(entropy(&[7; 100]), 0.0);
    let all: Vec<u8> = (0..=255).collect();
    assert!((entropy(&all) - 8.0).abs() < 1e-9);
}

/// Version info and the manifest of real Windows programs (skipped
/// elsewhere): notepad claims Microsoft as its company, and regedit's
/// manifest asks for the highest available rights.
#[cfg(windows)]
#[test]
fn windows_programs_show_version_info_and_manifest() {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:/Windows".into());
    let Ok(notepad) = std::fs::read(format!("{root}/System32/notepad.exe")) else {
        return;
    };
    let info = run(&notepad, &Op::Info).unwrap();
    assert!(
        info.contains("Version info (claimed by the file, not verified)"),
        "{info}"
    );
    assert!(info.contains("company \"Microsoft Corporation\""), "{info}");
    if let Ok(regedit) = std::fs::read(format!("{root}/regedit.exe")) {
        let info = run(&regedit, &Op::Info).unwrap();
        assert!(
            info.contains("Manifest: requested execution level"),
            "{info}"
        );
    }
}
