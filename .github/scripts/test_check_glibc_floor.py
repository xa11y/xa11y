"""Self-tests for check_glibc_floor.py.

The ELF fixtures are synthesized here rather than checked in: each test
states exactly which version needs and symbol bindings the object carries,
so a parser bug shows up as a wrong answer about a known input instead of a
drifted golden file. One test also reads the running interpreter, so the
parser is held to a real linker's output and not only to this file's idea
of the format.
"""

from __future__ import annotations

import importlib.util
import os
import struct
import sys
from pathlib import Path

import pytest

_spec = importlib.util.spec_from_file_location(
    "check_glibc_floor", Path(__file__).with_name("check_glibc_floor.py")
)
cgf = importlib.util.module_from_spec(_spec)
sys.modules["check_glibc_floor"] = cgf
_spec.loader.exec_module(cgf)


def build_elf(
    needs: dict[str, list[str]],
    symbols: list[tuple[str, str | None, bool]],
    *,
    is64: bool = True,
    little: bool = True,
) -> bytes:
    """Build a section-only ELF with `.gnu.version_r`, `.dynsym`, `.gnu.version`.

    `needs` maps library name to its required version names, in order.
    `symbols` lists (name, version or None, hidden) for each dynamic symbol
    after the mandatory null entry.
    """
    end = "<" if little else ">"
    strtab = bytearray(b"\0")

    def s(text: str) -> int:
        off = len(strtab)
        strtab.extend(text.encode() + b"\0")
        return off

    # Version indexes 0 and 1 are reserved (local / global); needs start at 2.
    index_of: dict[str, int] = {}
    verneed = bytearray()
    libs = list(needs.items())
    for li, (lib, versions) in enumerate(libs):
        last_lib = li == len(libs) - 1
        verneed += struct.pack(
            end + "HHIII", 1, len(versions), s(lib), 16, 0 if last_lib else 16 + 16 * len(versions)
        )
        for vi, ver in enumerate(versions):
            index_of[ver] = len(index_of) + 2
            last_aux = vi == len(versions) - 1
            verneed += struct.pack(
                end + "IHHII", 0, 0, index_of[ver], s(ver), 0 if last_aux else 16
            )

    sym_fmt = end + ("IBBHQQ" if is64 else "IIIBBH")
    dynsym = bytearray(struct.pack(sym_fmt, *([0] * 6)))
    versym = bytearray(struct.pack(end + "H", 0))
    for name, ver, hidden in symbols:
        dynsym += struct.pack(sym_fmt, s(name), *([0] * 5))
        idx = index_of[ver] if ver is not None else 1
        versym += struct.pack(end + "H", idx | (cgf.VERSYM_HIDDEN if hidden else 0))

    ehsize = 64 if is64 else 52
    blobs = [bytes(strtab), bytes(verneed), bytes(dynsym), bytes(versym)]
    offsets = []
    pos = ehsize
    for b in blobs:
        offsets.append(pos)
        pos += len(b)
    shoff = pos
    sym_ent = struct.calcsize(sym_fmt)
    # (type, offset, size, link, entsize) for sections 1..4; 0 is SHT_NULL.
    shdrs = [
        (0, 0, 0, 0, 0),
        (3, offsets[0], len(blobs[0]), 0, 0),
        (cgf.SHT_GNU_VERNEED, offsets[1], len(blobs[1]), 1, 0),
        (cgf.SHT_DYNSYM, offsets[2], len(blobs[2]), 1, sym_ent),
        (cgf.SHT_GNU_VERSYM, offsets[3], len(blobs[3]), 3, 2),
    ]
    if is64:
        shentsize, sh_fmt = 64, end + "IIQQQQIIQQ"
        header = struct.pack(
            end + "HHIQQQIHHHHHH", 3, 62, 1, 0, 0, shoff, 0, ehsize, 0, 0, shentsize, len(shdrs), 0
        )
    else:
        shentsize, sh_fmt = 40, end + "IIIIIIIIII"
        header = struct.pack(
            end + "HHIIIIIHHHHHH", 3, 3, 1, 0, 0, shoff, 0, ehsize, 0, 0, shentsize, len(shdrs), 0
        )
    ident = b"\x7fELF" + bytes([2 if is64 else 1, 1 if little else 2, 1]) + b"\0" * 9
    out = bytearray(ident + header)
    assert len(out) == ehsize
    for b in blobs:
        out += b
    for t, off, size, link, ent in shdrs:
        out += struct.pack(sh_fmt, 0, t, 0, 0, off, size, link, 0, 0, ent)
    return bytes(out)


TYPICAL = (
    {"libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.39"], "libxkbcommon.so.0": ["V_0.5.0"]},
    [
        ("malloc", "GLIBC_2.2.5", False),
        ("pidfd_spawnp", "GLIBC_2.39", False),
        ("pidfd_getpid", "GLIBC_2.39", True),
        ("xkb_context_new", "V_0.5.0", False),
        ("unversioned", None, False),
    ],
)


@pytest.mark.parametrize("is64", [True, False], ids=["elf64", "elf32"])
@pytest.mark.parametrize("little", [True, False], ids=["le", "be"])
def test_reads_versions_and_symbol_bindings(is64: bool, little: bool) -> None:
    needs = cgf.read_version_needs(build_elf(*TYPICAL, is64=is64, little=little))
    assert needs.versions == {
        "GLIBC_2.2.5": "libc.so.6",
        "GLIBC_2.39": "libc.so.6",
        "V_0.5.0": "libxkbcommon.so.0",
    }
    # The hidden bit on a versym entry must not detach a symbol from its version.
    assert sorted(needs.symbols["GLIBC_2.39"]) == ["pidfd_getpid", "pidfd_spawnp"]
    assert needs.symbols["V_0.5.0"] == ["xkb_context_new"]


def test_above_floor_fails_and_names_the_symbols() -> None:
    report = cgf.check("x.node", build_elf(*TYPICAL), (2, 17))
    assert not report.ok
    assert report.highest == "GLIBC_2.39"
    assert report.violations == {"GLIBC_2.39": ["pidfd_getpid", "pidfd_spawnp"]}
    text = cgf.render(report, "2.17")
    assert "FAIL x.node: requires GLIBC_2.39" in text
    assert "pidfd_spawnp" in text


def test_at_or_below_floor_passes_and_ignores_other_libraries() -> None:
    elf = build_elf(
        {"libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.17"], "libgcc_s.so.1": ["GCC_4.2.0"]}, []
    )
    report = cgf.check("x.node", elf, (2, 17))
    assert report.ok, cgf.render(report, "2.17")
    assert report.highest == "GLIBC_2.17"
    assert report.other == {"GCC_4.2.0": "libgcc_s.so.1"}


def test_versions_compare_numerically_not_lexically() -> None:
    # "2.9" > "2.17" as strings; as versions it is the other way round.
    elf = build_elf({"libc.so.6": ["GLIBC_2.9", "GLIBC_2.17"]}, [])
    report = cgf.check("x.node", elf, (2, 17))
    assert report.ok
    assert report.highest == "GLIBC_2.17"
    assert not cgf.check("x.node", elf, (2, 9)).ok


def test_glibc_private_is_a_violation() -> None:
    elf = build_elf(
        {"libc.so.6": ["GLIBC_2.2.5", "GLIBC_PRIVATE"]}, [("_dl_x", "GLIBC_PRIVATE", False)]
    )
    report = cgf.check("x.node", elf, (2, 17))
    assert not report.ok
    assert report.violations == {"GLIBC_PRIVATE": ["_dl_x"]}


def test_no_glibc_references_fails_rather_than_passing() -> None:
    report = cgf.check("x.node", build_elf({"libgcc_s.so.1": ["GCC_3.0"]}, []), (2, 17))
    assert not report.ok
    assert "no GLIBC_* version references" in cgf.render(report, "2.17")


def test_non_elf_input_is_an_error() -> None:
    with pytest.raises(cgf.ElfError):
        cgf.read_version_needs(b"#!/bin/sh\n")


def test_main_exit_codes(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    good = tmp_path / "good.node"
    good.write_bytes(build_elf({"libc.so.6": ["GLIBC_2.17"]}, []))
    bad = tmp_path / "bad.node"
    bad.write_bytes(build_elf(*TYPICAL))

    assert cgf.main(["--max", "2.17", str(good)]) == 0
    # One failing file fails the run even when another passes.
    assert cgf.main(["--max", "2.17", str(good), str(bad)]) == 1
    assert cgf.main(["--max", "2.17", str(tmp_path / "missing.node")]) == 1
    with pytest.raises(SystemExit) as exc:
        cgf.main(["--max", "latest", str(good)])
    assert exc.value.code == 2
    capsys.readouterr()


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="needs a glibc-linked ELF")
def test_reads_a_real_linker_output() -> None:
    exe = Path(os.path.realpath(sys.executable)).read_bytes()
    needs = cgf.read_version_needs(exe)
    if not any(v.startswith("GLIBC_") for v in needs.versions):
        pytest.skip("interpreter is not linked against glibc (musl or static build)")
    assert any(needs.symbols[v] for v in needs.versions if v.startswith("GLIBC_"))
