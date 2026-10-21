#!/usr/bin/env python3
"""Fail if an ELF shared object needs a newer glibc than the supported floor.

A Linux binary records every versioned libc symbol it links against in its
`.gnu.version_r` section (`pidfd_spawnp@GLIBC_2.39`, ...). The dynamic
loader refuses to load the object on any system whose glibc predates the
highest of those versions, so that maximum *is* the binary's glibc floor,
whatever the build machine intended. This script reads it straight from the
ELF and fails when it exceeds `--max`.

Issue #462 is why it exists: the npm `*-linux-*-gnu` prebuilds were built on
bare `ubuntu-latest`, picked up glibc 2.39 from Rust std's weak `pidfd_*`
references, and stopped loading on RHEL 9 / AL2023 / Debian 12 while every
check in CI stayed green.

The ELF is parsed here rather than with `objdump -T` so the check has no
binutils dependency, reads a cross-compiled aarch64 object on an x86_64 host,
and is unit-testable (`test_check_glibc_floor.py`). On failure it names each
offending symbol, so the fix does not start with re-running objdump by hand.

Usage:
    python .github/scripts/check_glibc_floor.py --max 2.17 xa11y-js/*.node
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path

SHT_DYNSYM = 11
SHT_GNU_VERNEED = 0x6FFFFFFE
SHT_GNU_VERSYM = 0x6FFFFFFF
VERSYM_HIDDEN = 0x8000

GLIBC_RE = re.compile(r"^GLIBC_(\d+(?:\.\d+)*)$")


class ElfError(Exception):
    """The file is not an ELF object this script can read."""


@dataclass
class VersionNeeds:
    """Versioned dependencies of one ELF object.

    `versions` maps each required version name (`GLIBC_2.17`, `V_0.5.0`) to
    the library that provides it; `symbols` maps it to the dynamic symbols
    bound to that version.
    """

    versions: dict[str, str] = field(default_factory=dict)
    symbols: dict[str, list[str]] = field(default_factory=dict)


def _cstr(blob: bytes, offset: int) -> str:
    end = blob.find(b"\0", offset)
    if end < 0:
        raise ElfError(f"unterminated string at offset {offset}")
    return blob[offset:end].decode("utf-8", "replace")


def read_version_needs(data: bytes) -> VersionNeeds:
    """Parse the version-needed and symbol-version tables of an ELF image."""
    if data[:4] != b"\x7fELF":
        raise ElfError("not an ELF file (bad magic)")
    ei_class, ei_data = data[4], data[5]
    if ei_class not in (1, 2):
        raise ElfError(f"unknown ELF class {ei_class}")
    if ei_data not in (1, 2):
        raise ElfError(f"unknown ELF data encoding {ei_data}")
    end = "<" if ei_data == 1 else ">"
    is64 = ei_class == 2

    if is64:
        shoff, shentsize, shnum = (
            struct.unpack_from(end + "Q", data, 0x28)[0],
            *struct.unpack_from(end + "HH", data, 0x3A),
        )
        sh_fmt = end + "IIQQQQIIQQ"
        sym_fmt, sym_name_at = end + "IBBHQQ", 0
    else:
        shoff, shentsize, shnum = (
            struct.unpack_from(end + "I", data, 0x20)[0],
            *struct.unpack_from(end + "HH", data, 0x2E),
        )
        sh_fmt = end + "IIIIIIIIII"
        sym_fmt, sym_name_at = end + "IIIBBH", 0
    if shoff == 0 or shnum == 0:
        raise ElfError("no section header table (stripped of sections?)")

    # (type, offset, size, link, entsize) per section.
    sections = []
    for i in range(shnum):
        f = struct.unpack_from(sh_fmt, data, shoff + i * shentsize)
        _name, sh_type, _flags, _addr, off, size, link, _info, _align, entsize = f
        sections.append((sh_type, off, size, link, entsize))

    def blob(index: int) -> bytes:
        _t, off, size, _l, _e = sections[index]
        return data[off : off + size]

    needs = VersionNeeds()
    by_index: dict[int, str] = {}

    for sh_type, off, _size, link, _e in sections:
        if sh_type != SHT_GNU_VERNEED:
            continue
        strtab = blob(link)
        pos = off
        while True:
            _ver, cnt, file_off, aux, nxt = struct.unpack_from(end + "HHIII", data, pos)
            lib = _cstr(strtab, file_off)
            apos = pos + aux
            for _ in range(cnt):
                _hash, _flags, other, name_off, anext = struct.unpack_from(
                    end + "IHHII", data, apos
                )
                name = _cstr(strtab, name_off)
                needs.versions[name] = lib
                needs.symbols.setdefault(name, [])
                by_index[other] = name
                if anext == 0:
                    break
                apos += anext
            if nxt == 0:
                break
            pos += nxt

    versym = next((s for s in sections if s[0] == SHT_GNU_VERSYM), None)
    dynsym_index = next((i for i, s in enumerate(sections) if s[0] == SHT_DYNSYM), None)
    if versym is not None and dynsym_index is not None:
        _t, sym_off, sym_size, sym_link, sym_ent = sections[dynsym_index]
        sym_ent = sym_ent or struct.calcsize(sym_fmt)
        dynstr = blob(sym_link)
        count = sym_size // sym_ent
        for i in range(count):
            (idx,) = struct.unpack_from(end + "H", data, versym[1] + 2 * i)
            name = by_index.get(idx & ~VERSYM_HIDDEN)
            if name is None:
                continue
            fields = struct.unpack_from(sym_fmt, data, sym_off + i * sym_ent)
            needs.symbols[name].append(_cstr(dynstr, fields[sym_name_at]))

    return needs


def parse_version(text: str) -> tuple[int, ...]:
    if not re.fullmatch(r"\d+(?:\.\d+)*", text):
        raise ValueError(f"not a dotted version: {text!r}")
    return tuple(int(p) for p in text.split("."))


@dataclass
class Report:
    path: str
    highest: str | None
    violations: dict[str, list[str]]
    other: dict[str, str]

    @property
    def ok(self) -> bool:
        return self.highest is not None and not self.violations


def check(path: str, data: bytes, floor: tuple[int, ...]) -> Report:
    """Compare one object's GLIBC_* needs against `floor`.

    A non-numeric GLIBC version (`GLIBC_PRIVATE`) is a violation: it is an
    internal interface no distribution promises to keep. An object with no
    GLIBC_* needs at all fails too (`highest is None`): every Rust cdylib
    linked against glibc has some, so finding none means the input is wrong
    or the parser is, and a guard must not pass on either.
    """
    needs = read_version_needs(data)
    glibc: dict[str, tuple[int, ...] | None] = {}
    other: dict[str, str] = {}
    for name, lib in needs.versions.items():
        if name.startswith("GLIBC_"):
            m = GLIBC_RE.match(name)
            glibc[name] = parse_version(m.group(1)) if m else None
        else:
            other[name] = lib

    numeric = [(v, n) for n, v in glibc.items() if v is not None]
    highest = max(numeric)[1] if numeric else None
    violations = {
        n: sorted(needs.symbols.get(n, [])) for n, v in glibc.items() if v is None or v > floor
    }
    return Report(path, highest, violations, other)


def _version_order(name: str) -> tuple[int, tuple[int, ...], str]:
    """Numeric GLIBC versions first, in version order; anything else last."""
    m = GLIBC_RE.match(name)
    return (0, parse_version(m.group(1)), name) if m else (1, (), name)


def render(report: Report, floor_text: str) -> str:
    lines = []
    if report.highest is None:
        lines.append(
            f"FAIL {report.path}: no GLIBC_* version references found; is this a glibc-linked ELF object?"
        )
    elif report.violations:
        lines.append(
            f"FAIL {report.path}: requires {report.highest}, above the GLIBC_{floor_text} floor"
        )
        for name in sorted(report.violations, key=_version_order):
            syms = report.violations[name]
            shown = ", ".join(syms[:10]) + (
                f", ... ({len(syms) - 10} more)" if len(syms) > 10 else ""
            )
            lines.append(f"  {name}: {shown or '(no dynamic symbol bound to it)'}")
    else:
        lines.append(f"ok   {report.path}: highest is {report.highest} (floor GLIBC_{floor_text})")
    if report.other:
        others = ", ".join(f"{n} ({lib})" for n, lib in sorted(report.other.items()))
        lines.append(f"     other versioned deps: {others}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--max", required=True, help="highest glibc version allowed, e.g. 2.17")
    parser.add_argument("files", nargs="+", help=".node / .so files to check")
    args = parser.parse_args(argv)
    try:
        floor = parse_version(args.max)
    except ValueError as e:
        parser.error(str(e))

    ok = True
    for path in args.files:
        try:
            data = Path(path).read_bytes()
            report = check(path, data, floor)
        except (OSError, ElfError, struct.error) as e:
            print(f"FAIL {path}: {e}", file=sys.stderr)
            ok = False
            continue
        print(render(report, args.max))
        ok = ok and report.ok
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
