"""List the DLLs a Windows program loads at start (its PE import table).

Usage: python scripts/pe-imports.py <file.exe|file.dll> ...
       python scripts/pe-imports.py --check <files...>
With --check, exits 1 if any file needs the Visual C++ Redistributable
(VCRUNTIME*.dll, MSVCP*.dll and friends), which a fresh Windows doesn't
always have. The Universal CRT (ucrtbase.dll, api-ms-win-crt-*) is part of
Windows 10 and 11, so it's fine.
"""

import struct
import sys

CRT_PREFIXES = ("vcruntime", "msvcp", "concrt", "vccorlib", "vcomp")


def imports(path):
    with open(path, "rb") as f:
        data = f.read()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("not a PE file")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    dirs = opt + (112 if magic == 0x20B else 96)
    imp_rva = struct.unpack_from("<I", data, dirs + 8)[0]
    secs = []
    s = opt + opt_size
    for i in range(nsec):
        vsize, va, rsize, raw = struct.unpack_from("<IIII", data, s + i * 40 + 8)
        secs.append((va, max(vsize, rsize), raw))

    def off(rva):
        for va, size, raw in secs:
            if va <= rva < va + size:
                return rva - va + raw
        raise ValueError(f"RVA {rva:#x} not in a section")

    names = []
    if imp_rva == 0:
        return names
    d = off(imp_rva)
    while True:
        name_rva = struct.unpack_from("<I", data, d + 12)[0]
        if name_rva == 0:
            break
        o = off(name_rva)
        names.append(data[o:data.index(b"\0", o)].decode("ascii", "replace"))
        d += 20
    return names


def main():
    args = sys.argv[1:]
    check = args[:1] == ["--check"]
    if check:
        args = args[1:]
    bad = False
    for path in args:
        try:
            dlls = imports(path)
        except (OSError, ValueError) as e:
            print(f"{path}: {e}")
            bad = True
            continue
        crt = sorted({d for d in dlls if d.lower().startswith(CRT_PREFIXES)})
        if check:
            print(f"{'NEEDS VC++ RUNTIME' if crt else 'ok'}  {path}" + (f"  ({', '.join(crt)})" if crt else ""))
            bad |= bool(crt)
        else:
            print(f"{path}:\n  " + "\n  ".join(dlls))
    sys.exit(1 if check and bad else 0)


if __name__ == "__main__":
    main()
