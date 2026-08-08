#!/usr/bin/env python3
"""Generate the test-only en-XA pseudolocale from the canonical source.

    python3 scripts/gen_pseudolocale.py --check
    python3 scripts/gen_pseudolocale.py --stdout

en-XA is not a language. It is a rendering test that answers three questions no
English screenshot can:

- **Is anything still hardcoded?** Untranslated text stays plain ASCII and
  stands out immediately against the accented text around it.
- **Does the layout survive longer strings?** Real translations of English run
  20-40% longer; German and Finnish more. Padding to 135% makes a container
  that only fits English fail here rather than after a locale ships.
- **Is anything truncated?** The brackets mark each message's boundaries, so a
  clipped string is visible as a missing `]`.

Deterministic and generated only inside tests, so it is always complete and
never becomes a production locale. Placeholders and inline markers pass through
untouched — an accented `{çôûñt}` would not substitute, which would test the
pseudolocale instead of the layout.
"""
import argparse
import json
import pathlib
import re
import sys

# One definition of the never-translate list, shared with check_i18n and
# locale_v1 rather than copied a third time.
from check_i18n import NEVER_TRANSLATE

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE = ROOT / "src/web/locales/en-US.json"
PRODUCTION_PSEUDOLOCALES = (
    ROOT / "src/web/locales/en-XA.json",
    ROOT / "locales/en-XA.json",
    ROOT / "locales/setup-en-XA.json",
)

# Latin-1/Latin-Extended-A lookalikes: legible as English, obviously not English.
ACCENTS = str.maketrans(
    "aeiouAEIOUbcdfghjklmnpqrstvwxyzBCDFGHJKLMNPQRSTVWXYZ",
    "áéíóúÁÉÍÓÚƀçđƒǥĥĵķŀɱñƥɋŕşŧṽŵxŷẑƁÇĐƑǤĤĴĶĿMÑƤɊŔŞŦṼŴXŶẐ",
)
# Padding is Latin text, not filler glyphs, so a reviewer can tell an
# over-long string from a rendering fault.
PAD = "escamotable"
# Two kinds of run survive untouched: {placeholders}, which must still
# substitute, and the never-translate tokens, which carry the same meaning in
# every language. Accenting `NIM` to `NÎM` made en-XA render a string no real
# locale would ever produce, and hid the fact that the tokens need protecting
# in translations too. Longest-first so `p50 / p95` wins over `p95`, and
# `requests/min` over `req/min`.
_TOKENS = "|".join(
    re.escape(t) for t in sorted(NEVER_TRANSLATE, key=len, reverse=True)
)
SEGMENT = re.compile(r"(\{[^{}]*\}|" + _TOKENS + r")")


def pseudo(text: str) -> str:
    """Accent the prose, leave placeholders and frozen tokens alone, pad ~135%."""
    parts = SEGMENT.split(text)
    accented = "".join(p if SEGMENT.fullmatch(p) else p.translate(ACCENTS) for p in parts)

    # Pad on the *prose* length: padding a string that is mostly placeholders
    # would stretch it far past what any real translation would do.
    prose = sum(len(p) for p in parts if not SEGMENT.fullmatch(p))
    need = max(1, round(prose * 0.35))
    tail = (PAD * (need // len(PAD) + 1))[:need]
    return f"[{accented} {tail}]"


def build(source: pathlib.Path) -> dict:
    src = json.loads(source.read_text())
    messages = {}
    for mid, msg in src["messages"].items():
        out = {"en": pseudo(msg["en"])}
        if "desc" in msg:
            out["desc"] = msg["desc"]
        # The hash records the SOURCE text this was generated from, so en-XA
        # goes Stale through the same mechanism as a real locale.
        out["hash"] = msg["hash"]
        if "maxLen" in msg:
            out["maxLen"] = msg["maxLen"]
        messages[mid] = out
    return {"locale": "en-XA", "state": "Generated", "messages": messages}


def wire_catalog(catalog: dict) -> dict:
    return {
        "locale": catalog["locale"],
        "messages": {
            mid: message["en"]
            for mid, message in catalog["messages"].items()
        },
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    output = ap.add_mutually_exclusive_group(required=True)
    output.add_argument(
        "--check",
        action="store_true",
        help="validate deterministic test-only generation and production absence",
    )
    output.add_argument(
        "--stdout",
        action="store_true",
        help="write the plain-string en-XA wire catalog to stdout for a test",
    )
    args = ap.parse_args()

    catalog = build(SOURCE)
    if args.stdout:
        print(json.dumps(wire_catalog(catalog), ensure_ascii=False))
        return 0

    present = [
        str(path.relative_to(ROOT))
        for path in PRODUCTION_PSEUDOLOCALES
        if path.exists()
    ]
    if present:
        print(
            "production pseudolocale present: "
            + ", ".join(present),
            file=sys.stderr,
        )
        return 1
    repeated = build(SOURCE)
    if catalog != repeated:
        print("en-XA generation is not deterministic", file=sys.stderr)
        return 1
    print(
        "en-XA generation OK — "
        f"{len(catalog['messages'])} messages, test-only, no production file"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
