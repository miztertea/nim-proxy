#!/usr/bin/env python3
"""locale-v1: validate a translated message catalog against its en-US source.

Stdlib only, like the other scripts here.

    python3 scripts/locale_v1.py path/to/de-DE.json
    python3 scripts/locale_v1.py --all
    python3 scripts/locale_v1.py --selftest

Every check below has a matching deliberately-broken fixture in
`tests/fixtures/locales/`, and `--selftest` asserts each fixture fails for its
own reason while `valid.json` passes. Checks were written against those
fixtures rather than the other way round: a check nobody has watched fail is
decoration.

The checks, and why each exists:

- **completeness** — a missing id renders the raw id string to the operator,
  because the runtime's fallback is the id itself.
- **no orphans** — an id absent from en-US is either a typo or a leftover from
  a removed feature; both are silent.
- **placeholder parity** — the runtime substitutes by exact name. A renamed or
  dropped placeholder leaves `{count}` visible in the interface.
- **formatter syntax** — an unbalanced brace parses as literal text and ships
  as literal text.
- **no catalog markup** — catalog values are plain Unicode text. Raw markup
  and entity-encoded markup are rejected independently so neither can become
  executable or double-encoded when a value reaches the wrong sink.
  See knowledge/decisions/message-catalog-and-escaping.md.
- **inline structure** — removed, duplicated, or reordered `{b}`/`{/b}`
  markers describe a shape the fixed-node runtime does not accept.
- **source-hash freshness** — the hash records which en-US text a translation
  was made from. When the source changes and the hash does not, the translation
  is stale: still valid, no longer correct. This is the check that makes a
  regenerate-on-drift pipeline possible.
- **length caps** — opt-in per key, for strings in width-constrained elements.
"""
import argparse
import html
import json
import pathlib
import re
import sys

# One definition of the never-translate list, not two. check_i18n guards its
# main(), so importing it is free of side effects, and a drifting second copy
# would be worse than the coupling.
from check_i18n import NEVER_TRANSLATE

ROOT = pathlib.Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests/fixtures/locales"
INLINE_OPEN = {"{b}"}
INLINE_CLOSE = {"{/b}"}
INLINE_TOKEN = re.compile(r"\{/?b\}")
PLACEHOLDER = re.compile(r"\{[^{}]*\}")


def placeholders(text: str) -> set:
    """Value placeholders only — inline markup tags are checked separately."""
    return {p for p in PLACEHOLDER.findall(text) if p not in INLINE_OPEN | INLINE_CLOSE}


def unbalanced_braces(text: str) -> bool:
    depth = 0
    for ch in text:
        if ch == "{":
            if depth:
                return True  # nested { — never valid
            depth = 1
        elif ch == "}":
            if not depth:
                return True
            depth = 0
    return depth != 0


def validate(source: dict, candidate: dict, name: str) -> list:
    """Return a list of (check, message). Empty means the locale is sound."""
    problems = []
    src, cand = source["messages"], candidate["messages"]

    for mid in sorted(set(src) - set(cand)):
        problems.append(("completeness", f"{name}: missing {mid}"))
    for mid in sorted(set(cand) - set(src)):
        problems.append(("orphan", f"{name}: {mid} is not in the source catalog"))

    for mid in sorted(set(src) & set(cand)):
        s, c = src[mid], cand[mid]
        text = c.get("en", "")

        if unbalanced_braces(text):
            problems.append(("syntax", f"{name}: {mid} has an unbalanced brace"))
        else:
            want, got = placeholders(s["en"]), placeholders(text)
            if want != got:
                miss = ", ".join(sorted(want - got)) or "none"
                extra = ", ".join(sorted(got - want)) or "none"
                problems.append((
                    "placeholders",
                    f"{name}: {mid} placeholder mismatch (missing {miss}; unexpected {extra})",
                ))

        if "<" in text or ">" in text:
            problems.append(("catalog-markup", f"{name}: {mid} contains raw markup"))
        decoded = html.unescape(text)
        if decoded != text and ("<" in decoded or ">" in decoded):
            problems.append((
                "catalog-entity-markup",
                f"{name}: {mid} contains entity-encoded markup",
            ))

        source_inline = INLINE_TOKEN.findall(s["en"])
        candidate_inline = INLINE_TOKEN.findall(text)
        if candidate_inline != source_inline:
            problems.append((
                "inline",
                f"{name}: {mid} inline structure {candidate_inline!r} "
                f"does not match source {source_inline!r}",
            ))

        if c.get("hash") != s["hash"]:
            problems.append((
                "stale",
                f"{name}: {mid} was translated from a different source text "
                f"(recorded {c.get('hash')}, source is {s['hash']})",
            ))

        cap = s.get("maxLen")
        if cap and len(text) > cap:
            problems.append(("length", f"{name}: {mid} is {len(text)} chars, cap is {cap}"))

        # Units, HTTP status codes and API identifiers carry the same meaning in
        # every language. A machine translator will happily render "rpm" as
        # "tr/min" or "429 retry" as "429 réessai", and the result validates
        # against every other check while being wrong. Only tokens the SOURCE
        # actually uses are required, so this never invents a constraint.
        for token in sorted(NEVER_TRANSLATE):
            if token in s["en"] and token not in text:
                problems.append((
                    "frozen",
                    f"{name}: {mid} dropped or translated {token!r}, "
                    f"which must survive verbatim in every locale",
                ))

    return problems


def public_projection_problems(source_messages: dict, candidate: dict) -> list:
    """Return named problems in the locked public plain-string projection."""
    problems = []
    expected = {
        mid: message["en"]
        for mid, message in source_messages.items()
        if mid == "common.app_name"
        or mid.startswith("login.")
        or mid.startswith("setup.")
    }
    messages = candidate.get("messages")
    if not isinstance(messages, dict):
        return [("catalog-schema", "public catalog messages must be an object")]
    for mid in sorted(set(messages) - set(expected)):
        problems.append((
            "public-catalog-extra-id",
            f"public catalog exposes operator-only id {mid}",
        ))
    for mid in sorted(set(expected) - set(messages)):
        problems.append((
            "public-catalog-missing-id",
            f"public catalog is missing {mid}",
        ))
    for mid in sorted(set(expected) & set(messages)):
        if messages[mid] != expected[mid]:
            problems.append((
                "public-catalog-value-drift",
                f"public catalog value for {mid} differs from the source",
            ))
    return problems


def public_projection(source: dict) -> dict:
    return {
        "locale": "en-US",
        "messages": {
            mid: message["en"]
            for mid, message in source["messages"].items()
            if mid == "common.app_name"
            or mid.startswith("login.")
            or mid.startswith("setup.")
        },
    }


def public_projection_text(source: dict) -> str:
    return json.dumps(
        public_projection(source),
        ensure_ascii=False,
        separators=(",", ":"),
    )


def production_locale_problems(locale_names: list[str]) -> list:
    """Production embeds exactly en-US; generated en-XA never ships."""
    unsupported = sorted({name for name in locale_names if name != "en-US"})
    return [
        (
            "production-pseudolocale",
            f"production locale registry contains {name}; only en-US may ship",
        )
        for name in unsupported
    ]


def catalog_text_problems(raw: str, name: str, rich: bool) -> list:
    """Preserve JSON pairs so duplicate ids and authored order stay observable."""
    class Pairs(list):
        pass

    try:
        root = json.loads(raw, object_pairs_hook=Pairs)
    except json.JSONDecodeError as error:
        return [("catalog-schema", f"{name}: invalid JSON: {error.msg}")]
    problems = []

    def duplicate_keys(value):
        if isinstance(value, Pairs):
            keys = [key for key, _ in value]
            for key in sorted({key for key in keys if keys.count(key) > 1}):
                problems.append((
                    "catalog-duplicate-id",
                    f"{name}: duplicate object key {key}",
                ))
            for _, child in value:
                duplicate_keys(child)
        elif isinstance(value, list):
            for child in value:
                duplicate_keys(child)

    duplicate_keys(root)
    if not isinstance(root, Pairs):
        return problems + [("catalog-schema", f"{name}: catalog must be an object")]
    top = dict(root)
    expected_top = {"locale", "messages", "state"} if rich else {"locale", "messages"}
    if set(top) != expected_top or top.get("locale") != "en-US":
        problems.append((
            "catalog-schema",
            f"{name}: top-level catalog schema is invalid",
        ))
    if rich and top.get("state") != "Source":
        problems.append((
            "catalog-schema",
            f"{name}: rich source state must be Source",
        ))
    messages = top.get("messages")
    if not isinstance(messages, Pairs):
        problems.append((
            "catalog-schema",
            f"{name}: messages must be an object",
        ))
        return problems
    ids = [mid for mid, _ in messages]
    if ids != sorted(ids) or any(not mid.isascii() for mid in ids):
        problems.append((
            "catalog-key-order",
            f"{name}: message ids must be ASCII and ASCII-sorted",
        ))
    for mid, message in messages:
        if not isinstance(mid, str):
            problems.append(("catalog-schema", f"{name}: message id must be text"))
            continue
        if rich:
            if not isinstance(message, Pairs):
                problems.append((
                    "catalog-schema",
                    f"{name}: {mid} rich message must be an object",
                ))
                continue
            entry = dict(message)
            if (
                not {"en", "hash"} <= set(entry)
                or not set(entry) <= {"en", "desc", "hash", "maxLen"}
                or not isinstance(entry.get("en"), str)
                or not isinstance(entry.get("hash"), str)
                or ("desc" in entry and not isinstance(entry["desc"], str))
                or ("maxLen" in entry and not isinstance(entry["maxLen"], int))
            ):
                problems.append((
                    "catalog-schema",
                    f"{name}: {mid} rich message schema is invalid",
                ))
        elif not isinstance(message, str):
            problems.append((
                "catalog-schema",
                f"{name}: {mid} public value must be a string",
            ))
    return problems


def load(path: pathlib.Path) -> dict:
    return json.loads(path.read_text())


def selftest() -> int:
    """Each fixture must fail for its OWN reason, and valid.json must pass."""
    source = load(FIXTURES / "_source-en-US.json")
    expected = {
        "valid.json": None,
        "missing-key.json": "completeness",
        "orphan-key.json": "orphan",
        "placeholder-mismatch.json": "placeholders",
        "bad-formatter-syntax.json": "syntax",
        "raw-html.json": "catalog-markup",
        "entity-html.json": "catalog-entity-markup",
        "stale-hash.json": "stale",
        "too-long.json": "length",
        "inline-dropped.json": "inline",
        "unbalanced-inline.json": "inline",
        "frozen-token-dropped.json": "frozen",
    }
    failures = []
    for fixture, want in sorted(expected.items()):
        got = validate(source, load(FIXTURES / fixture), fixture)
        checks = {c for c, _ in got}
        if want is None:
            if got:
                failures.append(f"{fixture}: expected to pass, got {sorted(checks)}")
            else:
                print(f"  ok  {fixture:28} passes")
        elif want not in checks:
            failures.append(f"{fixture}: expected check {want!r}, got {sorted(checks) or 'nothing'}")
        else:
            print(f"  ok  {fixture:28} trips {want}")

    source_messages = {
        "common.app_name": {"en": "NIM Proxy"},
        "dashboard.nav.overview": {"en": "Overview"},
        "login.submit": {"en": "Sign in"},
        "setup.wizard.continue": {"en": "Continue"},
    }
    public = {
        "locale": "en-US",
        "messages": {
            "common.app_name": "NIM Proxy",
            "login.submit": "Sign in",
            "setup.wizard.continue": "Continue",
        },
    }
    projection_cases = {
        "public-catalog-extra-id": {
            **public,
            "messages": {**public["messages"], "dashboard.nav.overview": "Overview"},
        },
        "public-catalog-missing-id": {
            **public,
            "messages": {
                "common.app_name": "NIM Proxy",
                "setup.wizard.continue": "Continue",
            },
        },
        "public-catalog-value-drift": {
            **public,
            "messages": {**public["messages"], "login.submit": "Log in"},
        },
    }
    for want, candidate in projection_cases.items():
        checks = {
            check
            for check, _ in public_projection_problems(source_messages, candidate)
        }
        if want not in checks:
            failures.append(
                f"{want}: expected check {want!r}, got {sorted(checks) or 'nothing'}"
            )
        else:
            print(f"  ok  {want:28} trips {want}")

    production_checks = {
        check
        for check, _ in production_locale_problems(["en-US", "en-XA"])
    }
    if "production-pseudolocale" not in production_checks:
        failures.append(
            "production-pseudolocale: expected check "
            "'production-pseudolocale', got "
            f"{sorted(production_checks) or 'nothing'}"
        )
    else:
        print("  ok  production-pseudolocale    trips production-pseudolocale")

    text_cases = {
        "catalog-duplicate-id": (
            '{"locale":"en-US","messages":{"common.app_name":"NIM Proxy",'
            '"common.app_name":"NIM Proxy"}}',
            False,
        ),
        "catalog-key-order": (
            '{"locale":"en-US","messages":{"setup.z":"Z","login.a":"A"}}',
            False,
        ),
        "catalog-schema": (
            '{"locale":"en-US","messages":[]}',
            False,
        ),
    }
    for want, (raw, rich) in text_cases.items():
        checks = {
            check
            for check, _ in catalog_text_problems(raw, f"{want}.json", rich)
        }
        if want not in checks:
            failures.append(
                f"{want}: expected check {want!r}, got {sorted(checks) or 'nothing'}"
            )
        else:
            print(f"  ok  {want:28} trips {want}")

    if failures:
        print("\nselftest FAILED:")
        for f in failures:
            print("  -", f)
        return 1
    total = len(expected) + len(projection_cases) + 1 + len(text_cases)
    print(f"\nselftest ok — {total} cases, every check observed to fail")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("locale", nargs="*", type=pathlib.Path)
    ap.add_argument("--all", action="store_true", help="validate the repository catalogs")
    ap.add_argument("--selftest", action="store_true", help="prove each check can fail")
    ap.add_argument(
        "--update-public",
        action="store_true",
        help="regenerate the public en-US fixture from the canonical source",
    )
    args = ap.parse_args()

    if args.selftest:
        return selftest()

    source_path = ROOT / "src/web/locales/en-US.json"
    source = load(source_path)
    public_path = FIXTURES / "public-en-US.json"
    if args.update_public:
        public_path.write_text(public_projection_text(source))
        print(
            "wrote "
            f"{len(public_projection(source)['messages'])} messages to "
            f"{public_path.relative_to(ROOT)}"
        )
        return 0
    targets = list(args.locale)
    problems = []
    if args.all:
        problems += catalog_text_problems(
            source_path.read_text(),
            str(source_path.relative_to(ROOT)),
            True,
        )
        problems += catalog_text_problems(
            public_path.read_text(),
            str(public_path.relative_to(ROOT)),
            False,
        )
        problems += validate(source, source, str(source_path.relative_to(ROOT)))
        problems += public_projection_problems(
            source["messages"],
            load(public_path),
        )
        production_names = [
            path.stem
            for path in sorted((ROOT / "src/web/locales").glob("*.json"))
        ]
        production_names += [
            path.stem.removeprefix("setup-")
            for path in sorted((ROOT / "locales").glob("*.json"))
        ]
        problems += production_locale_problems(production_names)
    for path in targets:
        problems += catalog_text_problems(path.read_text(), path.name, True)
        problems += validate(source, load(path), path.name)

    if problems:
        print(f"{len(problems)} problem(s):")
        for check, msg in problems:
            print(f"  [{check}] {msg}")
        return 1
    checked = len(targets) + int(args.all)
    if not checked:
        print("no locale supplied (use --all for the repository contract)")
        return 0
    print(f"locale-v1 ok — {checked} catalog set(s) clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
