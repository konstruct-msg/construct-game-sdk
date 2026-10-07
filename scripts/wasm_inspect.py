#!/usr/bin/env python3
"""Check a built game module's imports and exports; print its SHA-256.

A stop-gap until construct-game-check (plan stage 5) does this with wasmparser, along with
what this cannot: the float-instruction scan, fuel and determinism. It reads only the
section headers, so it needs nothing but the standard library.

Usage: wasm_inspect.py MODULE.wasm...   — exit 1 if any module fails.
"""

import hashlib
import sys

REQUIRED_EXPORTS = {
    "memory",
    "cg_abi_version",
    "cg_alloc",
    "cg_init",
    "cg_apply",
    "cg_legal_moves",
    "cg_view",
    "cg_status",
}
SECTION_IMPORT = 2
SECTION_START = 8
SECTION_EXPORT = 7


def leb128(data, pos):
    result = shift = 0
    while True:
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            return result, pos


def name(data, pos):
    length, pos = leb128(data, pos)
    return data[pos : pos + length].decode(), pos + length


def sections(data):
    if data[:8] != b"\0asm\x01\0\0\0":
        raise ValueError("not a wasm module (or not version 1)")
    pos = 8
    while pos < len(data):
        section_id = data[pos]
        size, pos = leb128(data, pos + 1)
        yield section_id, data[pos : pos + size]
        pos += size


def imports(body):
    count, pos = leb128(body, 0)
    found = []
    for _ in range(count):
        module, pos = name(body, pos)
        field, pos = name(body, pos)
        found.append(f"{module}.{field}")
        kind = body[pos]
        pos += 1
        if kind == 0:  # func: type index
            _, pos = leb128(body, pos)
        else:  # table / memory / global: listing them is enough, the module fails anyway
            break
    return found


def exports(body):
    count, pos = leb128(body, 0)
    found = set()
    for _ in range(count):
        field, pos = name(body, pos)
        pos += 1  # kind
        _, pos = leb128(body, pos)
        found.add(field)
    return found


def inspect(path):
    data = open(path, "rb").read()
    problems = []
    found_exports = set()
    for section_id, body in sections(data):
        if section_id == SECTION_IMPORT:
            listed = imports(body)
            if listed:
                problems.append(f"imports {', '.join(listed)}")
        elif section_id == SECTION_START:
            problems.append("has a start function")
        elif section_id == SECTION_EXPORT:
            found_exports = exports(body)
    missing = REQUIRED_EXPORTS - found_exports
    if missing:
        problems.append(f"missing exports {', '.join(sorted(missing))}")
    return hashlib.sha256(data).hexdigest(), len(data), problems


def main(paths):
    failed = False
    for path in paths:
        digest, size, problems = inspect(path)
        print(f"{digest}  {size:>7}  {path}")
        for problem in problems:
            print(f"  FAIL: {problem}")
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]) if len(sys.argv) > 1 else 2)
