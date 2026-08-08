#!/usr/bin/env python3
"""Verify the embedded message catalog against the markup it was extracted from.

Stdlib only, like the other scripts here. Run from the repo root:

    python3 scripts/check_i18n.py

Three checks, all of which must hold for an extraction to be non-destructive:

1. **Round-trip.** Every element carrying `data-i18n` / `data-i18n-text` /
   `data-i18n-attr` still contains, in the markup, exactly the text the catalog
   holds for that id. Extraction only *adds attributes* — if a tagged element's
   text and its catalog value ever disagree, the page renders differently after
   the runtime substitutes, which is the bug this catches.
2. **Completeness.** No id referenced from the markup is missing from the
   catalog, and no catalog id is unreferenced (orphan).
3. **Hash freshness.** Each message's recorded hash matches its current text.
   Translated locales record the source hash they were made from, so a stale
   translation is detectable rather than silently wrong.

Exits non-zero on any failure. PR 5 extends this into the full `locale-v1`
validator (placeholder parity, no raw HTML in values, length caps) with the
negative fixtures that prove each check actually fails.
"""
import hashlib
import html as htmlmod
import json
import pathlib
import re
import sys
from html.parser import HTMLParser

ROOT = pathlib.Path(__file__).resolve().parent.parent
EMERGENCY_TEXT = "NIM Proxy interface failed to load."
PAGE_SOURCES = {
    "src/web/dashboard.html": (
        "src/web/shared.js",
        "src/web/dashboard.js",
        "src/web/settings.js",
    ),
    "src/web/setup.html": ("src/web/setup.js",),
    "src/web/login.html": ("src/web/login.js",),
}


def bundled_page(name: str) -> str:
    """Return authored markup plus the real split scripts in one lint input.

    The synthetic wrapper is scanner input only; browser execution always uses
    the separately served production assets.
    """
    page = (ROOT / name).read_text()
    if name == "src/web/dashboard.html":
        shared = (ROOT / "src/web/shared.js").read_text()
        dashboard = (ROOT / "src/web/dashboard.js").read_text()
        settings = (ROOT / "src/web/settings.js").read_text()
        marker = "const RENDERERS ="
        before, after = dashboard.split(marker, 1)
        # Preserve the monolith's scanner order. The existing lint intentionally
        # stops at renderSettings() because settings vocabulary belongs to the
        # later rationalization task; source splitting must not silently widen
        # that scope to code that originally followed the settings block.
        scripts = f"{shared}\n{before}\n{settings}\n{marker}{after}"
    else:
        scripts = "\n".join(
            (ROOT / script).read_text() for script in PAGE_SOURCES[name]
        )
    return f"{page}\n<script>\n{scripts}\n</script>\n"


def load_catalog(source: str):
    m = re.search(
        r'<script type="application/json" id="i18n-catalog">(.*?)</script>', source, re.S
    )
    if not m:
        sys.exit("no inline i18n catalog found")
    return json.loads(m.group(1))["messages"]


def strip_comments(source: str) -> str:
    """Drop JS comments so an id merely *mentioned* in prose does not count as
    a reference and mask an orphan."""
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.S)
    return re.sub(r"(?<![:'\"])//[^\n]*", "", source)


def blank_comments(source: str) -> str:
    """Like strip_comments, but preserves line count so reported line numbers
    still match the file. A multi-line `/* */` collapsed to nothing shifts every
    line after it."""
    source = re.sub(
        r"/\*.*?\*/", lambda m: "\n" * m.group(0).count("\n"), source, flags=re.S
    )
    source = re.sub(r"<!--.*?-->", lambda m: "\n" * m.group(0).count("\n"), source, flags=re.S)
    return re.sub(r"(?<![:'\"])//[^\n]*", "", source)


def strip_scripts(source: str) -> str:
    """Drop executable <script> bodies; the runtime's own doc comment mentions
    these attribute names and would otherwise be scanned as markup."""
    return re.sub(r"<script>.*?</script>", "<script></script>", source, flags=re.S)


def own_text(inner: str) -> str:
    """The element's own first text node, skipping complete child elements.

    `<h2><svg>...</svg>Traffic <span>note</span>` -> "Traffic ". This mirrors
    what the runtime does, which assigns to the first non-blank text-node child
    of the element itself, not of a descendant."""
    depth, buf, out = 0, [], ""
    i = 0
    while i < len(inner):
        ch = inner[i]
        if ch == "<":
            if depth == 0 and "".join(buf).strip():
                return "".join(buf)
            buf = []
            close = inner.find(">", i)
            if close == -1:
                break
            if not inner[i + 1: close].endswith("/") and not inner.startswith("</", i):
                depth += 1
            elif inner.startswith("</", i):
                depth -= 1
            i = close + 1
            continue
        if depth == 0:
            buf.append(ch)
        i += 1
    return "".join(buf)


def tagged(source: str, attr: str):
    """Yield (id, full_element_markup) for each element carrying `attr`."""
    for m in re.finditer(rf'<(\w+)([^>]*\s{attr}="([^"]+)"[^>]*)>', source):
        tag, mid = m.group(1), m.group(3)
        # naive but sufficient: these elements do not nest the same tag
        close = source.find(f"</{tag}>", m.end())
        yield mid, source[m.end():close if close != -1 else m.end()]



# ---------------------------------------------------------------------------
# Untagged-string lint
#
# Without this, English creeps back within two PRs: someone adds a column or a
# metric row, writes the label inline because that is what the surrounding code
# used to look like, and nothing complains. It covers ATTRIBUTES as well as
# text, because a hardcoded title= is just as untranslated and far easier to
# miss in review.
#
# Scoped deliberately:
#   - Settings is included; its static and dynamic strings stay catalog-owned.
#   - chipHtml's interior is excluded. It interpolates into a URL and (before
#     this release) a JS context; routing a catalog value through it is the
#     thing we spent PR 3 removing. Flagging it would invite someone to
#     "fix" it by wrapping it in t(), which is a net loss.
#   - NEVER_TRANSLATE holds the units and codes that must survive verbatim.

NEVER_TRANSLATE = {
    "TTFT", "TPOT", "tok", "tok/s", "Tools/req", "Msgs/req", "p50 / p95", "p50", "p95",
    "requests / min", "req/min", "rpm", "JSON", "HTTP", "POST", "SLO", "NIM",
    "/v1", "%", "429", "401", "503", "504", "5xx",
    "requests/min", "nvapi-\u2026", "npk_\u2026",
}

def frozen(text: str) -> bool:
    """Units, status codes, key prefixes and URLs survive verbatim in every
    locale, so a hardcoded one is correct rather than an oversight."""
    return (
        text in NEVER_TRANSLATE
        or text.startswith(("http://", "https://"))
        or not re.search(r"[A-Za-z]{2}", text)
    )

# JS positions whose first argument is displayed to the operator.
DISPLAY_CALLS = re.compile(
    r"(?:\{\s*label:\s*|metricRow\(\s*|tile\(\s*|prow\(\s*|empty:\s*)'([^']{2,})'"
)

# The allowlist above only sees five call shapes, so every leak that survived
# extraction lived in a shape it does not scan: `{name:'…'}` chart series,
# array-literal taxonomy tables, object-literal reason maps, and bare prose in
# template literals. This is the documented trap "strings passed as arguments
# hide from string sweeps" — a lint that reports clean while ~40 English
# strings render is worse than no lint.
#
# So: find quoted PROSE anywhere in the scanned script, and exclude what is
# provably not display text rather than allowlisting the places prose may sit.
QUOTED = re.compile(r"'([^'\\\n]{2,})'|\"([^\"\\\n]{2,})\"")
# Text nodes inside template literals — `<span class="k">Superuser</span>`.
# Neither the quoted scan nor the markup scan can see these: there are no
# quotes around the text, and strip_scripts() deletes the script that holds
# it. Six English labels and a retired term shipped in setup.html's review
# panel through this hole.
TEMPLATE_LITERAL = re.compile(r"`(?:[^`\\]|\\.)*`", re.S)
INTERPOLATION = re.compile(r"\$\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}")
TEXT_NODE = re.compile(r">([^<>]+)<")
# Attribute values are not text nodes. The localizable ones (title, alt,
# placeholder, aria-label) have their own check; the rest are machinery.
#
# The `=` must ABUT the quote, as it does in markup (`class="x"`). Allowing
# whitespace exempted every JS assignment and comparison — `const x = 'label'`,
# `el.textContent = 'label'`, `if (s === 'label')` — which is the most common
# way to write a string in this file, and `.textContent =` is how the page
# renders labels. Tightening this costs zero findings on either page.
ATTR_VALUE = re.compile(r"[\w-]=$")
# How far back to look for the machinery that governs a quoted string. Wide
# enough to cover `document.querySelector(` and `.getAttribute(`.
NOT_DISPLAY_WINDOW = 30
# ...but the exemption belongs to the string that machinery GOVERNS, not to its
# neighbours. `bar(y.padStart(3), 'label')` put `padStart(` inside the window
# while the string is a different argument entirely, so an argument boundary
# between the two cancels the exemption.
ARG_BOUNDARY = re.compile(r"[,;]")
# Contexts where a quoted string is machinery, not text for a human.
NOT_DISPLAY = re.compile(
    r"querySelector|getElementById|createElementNS|setAttribute\(|getAttribute\(|"
    r"classList|\.style|addEventListener|removeEventListener|localStorage|"
    r"nimproxy_|labels\[|dataset\.|\.dataset|JSON\.|console\.|new Intl|"
    r"new (?:Error|TypeError)|\.split\(|\.join\(|\.replace\(|padStart|toFixed|"
    r"encodeURI|fetch\("
)


def looks_like_prose(text: str) -> bool:
    """Human-facing text: a capitalised word, or two words with a space.

    Deliberately NOT flagged: lowercase single tokens (enum values, metric
    label values, CSS keywords), anything with an underscore or slash-prefix
    (identifiers, selectors, paths), and anything already frozen.
    """
    if frozen(text) or "${" in text or "_" in text:
        return False
    if text.startswith(("#", ".", "/", "--", "http")):
        return False
    # CSS values look like prose to a word counter: `var(--violet, #8B7BB8)`
    # has two words and starts with a letter. They are style, never text.
    if text.startswith("var(") or re.match(r"^[a-z-]+\(", text) or "#" in text:
        return False
    # A whole inline style attribute is also style: `display:flex;gap:20px`
    # reads as prose to a word counter. CSS declarations, never text.
    #
    # `^[a-z-]+:` alone was too greedy — it exempted `note: …`, `error: …`,
    # `avg: …`, which are labels. A CSS declaration list either carries a `;`
    # or its single value has no spaces (`display:flex`, `position:relative`);
    # prose after the colon has spaces and no semicolon.
    if re.match(r"^[a-z-]+\s*:", text) and (
        ";" in text or " " not in text.split(":", 1)[1].strip()
    ):
        return False
    if text == "use strict":
        return False
    if not re.search(r"[A-Za-z]{3}", text):
        return False
    words = text.split()
    if len(words) >= 2 and re.match(r"^[A-Za-z]", text):
        return True
    return bool(re.match(r"^[A-Z][a-z]{2,}$", text))


def lint_runtime_helpers(name: str, raw: str) -> list:
    """Every i18n helper a page CALLS must be defined in that page.

    `node --check` sees only syntax and `cargo test` never parses the script,
    so an undefined helper is silent until the page runs. It bites hard:
    applyStatic aborts on the first throw, so ONE missing helper leaves the
    entire page untranslated rather than one string. That happened — setup.html
    called tRaw() while defining only rawMsg()."""
    body = re.search(r"<script>(.*?)</script>", raw, re.S)
    if not body:
        return []
    js = strip_comments(body.group(1))
    helpers = (
        r"message|setMessageText|setMessageAttr|setMessageWithNodes|"
        r"setEmphasizedMessage|applyStatic|t|tRaw|tHtml|rawMsg"
    )
    called = set(re.findall(rf"\b({helpers})\s*\(", js))
    defined = set(
        re.findall(rf"(?:const|let|var|function)\s+({helpers})\b", js)
    )
    return [
        f"{name}: calls {fn}() but never defines it — the page will not localize"
        for fn in sorted(called - defined)
    ]


I18N_TEXT_ATTRS = {"title", "placeholder", "aria-label", "alt"}
CANONICAL_LOOKUPS = {
    "src/web/dashboard.html": (
        r"""function escapeHtml\(value\) \{
  const text = value && value\.type === CATALOG_MESSAGE
    \? message\(value\.id, value\.params\)
    : String\(value\);
  return text\.replace""",
        r"""function setMessageText\(node, id, params = \{\}\) \{
  assertMessageTextTarget\(node\);
  node\.textContent = message\(id, params\);
\}""",
        r"""function setMessageAttr\(node, attr, id, params = \{\}\) \{
  if \(!I18N_TEXT_ATTRS\.has\(attr\)\)
    throw new Error\(`forbidden catalog attribute: \$\{attr\}`\);
  node\.setAttribute\(attr, message\(id, params\)\);
\}""",
        r"""function confirmMessage\(id, params = \{\}\) \{
  return confirm\(message\(id, params\)\);
\}""",
        r"""function promptMessage\(id, params = \{\}\) \{
  return prompt\(message\(id, params\)\);
\}""",
        r"""const apiError = typeof j\.error === 'string'
    \? j\.error
    : \(j\.error && typeof j\.error\.message === 'string' \? j\.error\.message : ''\);
  if \(!r\.ok \|\| j\.ok === false\)
    throw new Error\(apiError \|\| message\('settings\.error\.request_failed', \{ status: r\.status \}\)\);""",
    ),
    "src/web/setup.html": (
        r"""function setMessageText\(node, id, params = \{\}\) \{
  assertMessageTextTarget\(node\);
  node\.textContent = message\(id, params\);
\}""",
        r"""function setMessageAttr\(node, attr, id, params = \{\}\) \{
  if \(!I18N_TEXT_ATTRS\.has\(attr\)\)
    throw new Error\(`forbidden catalog attribute: \$\{attr\}`\);
  node\.setAttribute\(attr, message\(id, params\)\);
\}""",
        r"""function setMessageWithNodes\(node, id, replacements\) \{
  assertMessageTextTarget\(node\);
  for \(const \[, replacement\] of replacements\) \{
    assertMessageTextTarget\(replacement\);
    if \(replacement\.querySelector\?\.\("script,style,svg"\)\)
      throw new Error\("forbidden catalog text target"\);
  \}
  let text = message\(id\);""",
        r"""function setEmphasizedMessage\(node, id, emphasis\) \{
  assertMessageTextTarget\(node\);
  assertMessageTextTarget\(emphasis\);
  if \(emphasis\.querySelector\?\.\("script,style,svg"\)\)
    throw new Error\("forbidden catalog text target"\);
  const text = message\(id\);""",
    ),
    "src/web/login.html": (
        r"""function setMessageText\(node, id, params = \{\}\) \{
  assertMessageTextTarget\(node\);
  node\.textContent = message\(id, params\);
\}""",
        r"""function setMessageAttr\(node, attr, id, params = \{\}\) \{
  if \(!I18N_TEXT_ATTRS\.has\(attr\)\)
    throw new Error\(`forbidden catalog attribute: \$\{attr\}`\);
  node\.setAttribute\(attr, message\(id, params\)\);
\}""",
    ),
}


def _consume_canonical_lookups(name: str, source: str, problems: list) -> str:
    """Blank only the deliberately tiny set of raw catalog lookup bodies.

    Page code passes ids or inert descriptors. A raw ``message()`` lookup is
    legal only inside one canonical sink helper, whose complete opening shape
    is fixed here. This is intentionally a bounded convention rather than a
    partial JavaScript parser.
    """
    working = source
    for pattern in CANONICAL_LOOKUPS.get(name, ()):
        matches = list(re.finditer(pattern, working))
        if len(matches) != 1:
            problems.append((
                "sink-context",
                f"{name}: canonical catalog sink shape occurs {len(matches)} times",
            ))
            continue
        match = matches[0]
        replacement = match.group(0).replace("message(", "__catalog_lookup__(")
        working = working[:match.start()] + replacement + working[match.end():]

    declarations = list(
        re.finditer(r"\bconst message\s*=\s*\(id, params = \{\}\)\s*=>", working)
    )
    if name in CANONICAL_LOOKUPS and len(declarations) != 1:
        problems.append((
            "sink-context",
            f"{name}: raw catalog lookup declaration occurs {len(declarations)} times",
        ))
    working = re.sub(
        r"\bconst message(\s*=\s*\(id, params = \{\}\)\s*=>)",
        r"const __catalog_lookup__\1",
        working,
    )
    return working


def lint_catalog_sinks(name: str, raw: str) -> list:
    """Enforce the bounded ID/descriptor-to-sink convention."""
    problems = []
    source = strip_comments(raw)

    def add(check: str, detail: str):
        if not any(existing == check for existing, _ in problems):
            problems.append((check, detail))

    if re.search(r'data-i18n-html=|\btHtml\s*\(', source):
        add(
            "structured-message-html",
            f"{name}: structured catalog message uses an HTML-producing path",
        )

    if re.search(r"\b(?:rawMsg|tRaw)\s*\(|\b(?:const|let|var)\s+t\s*=", source):
        add(
            "sink-context",
            f"{name}: escaped/plain catalog compatibility helper survives",
        )

    for match in re.finditer(
        r"\bsetMessageAttr\s*\([^,]+,\s*['\"]([^'\"]+)['\"]", source
    ):
        attr = match.group(1)
        if attr not in I18N_TEXT_ATTRS:
            add(
                "forbidden-catalog-context",
                f"{name}: catalog value targets forbidden attribute {attr!r}",
            )

    # Declarative and helper-owned text paths never target active text or SVG.
    if re.search(r"<(?:script|style|svg)\b[^>]*\bdata-i18n", source, re.I):
        add(
            "forbidden-catalog-context",
            f"{name}: declarative catalog text targets script, CSS, or SVG",
        )
    forbidden_names = {"script", "scriptNode", "style", "styleNode", "svg", "svgNode"}
    created = {
        match.group(1)
        for match in re.finditer(
            r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*"
            r"document\.createElement(?:NS)?\([^;\n]*['\"](?:script|style|svg)['\"]",
            source,
        )
    }
    targets = "|".join(re.escape(target) for target in sorted(forbidden_names | created))
    if targets and re.search(
        rf"\b(?:setMessageText|setMessageWithNodes|setEmphasizedMessage)"
        rf"\s*\(\s*(?:{targets})\b",
        source,
    ):
        add(
            "forbidden-catalog-context",
            f"{name}: catalog text helper targets script, CSS, or SVG",
        )

    body = re.search(r"<script>(.*?)</script>", raw, re.S)
    javascript = strip_comments(body.group(1)) if body else ""
    working = _consume_canonical_lookups(name, javascript, problems)
    catalog_value = r"(?:message|catalogMessage)\s*\("

    # These contexts never accept repository-owned text, even when escaped.
    forbidden_patterns = (
        rf"\.(?:href|src|onclick)\s*=\s*[^;\n]*{catalog_value}",
        rf"\.style(?:\.[A-Za-z_$][\w$]*)?\s*=\s*[^;\n]*{catalog_value}",
        rf"\.setAttribute\s*\([^;\n]*{catalog_value}",
        rf"\b(?:script|scriptNode|style|styleNode)\.textContent\s*=\s*"
        rf"[^;\n]*{catalog_value}",
        rf"\b(?:svg|svgNode)\.(?:textContent|innerHTML)\s*=\s*"
        rf"[^;\n]*{catalog_value}",
    )
    if any(re.search(pattern, working) for pattern in forbidden_patterns):
        add(
            "forbidden-catalog-context",
            f"{name}: catalog value enters URL, style, event, script, CSS, SVG, "
            "or native attribute context",
        )
    for alias in created:
        if re.search(
            rf"\b{re.escape(alias)}\.(?:textContent|innerHTML)\s*=\s*"
            rf"[^;\n]*{catalog_value}",
            working,
        ):
            add(
                "forbidden-catalog-context",
                f"{name}: catalog value enters script, CSS, or SVG alias",
            )

    # Descriptors may be resolved only by escapeHtml; ID-taking DOM helpers
    # resolve their own catalog values and reject a descriptor argument.
    if re.search(
        r"\b(?:setMessageText|setMessageAttr|setMessageWithNodes|"
        r"setEmphasizedMessage)\s*\([^,]+,\s*(?:['\"][^'\"]+['\"]\s*,\s*)?"
        r"catalogMessage\s*\(",
        working,
    ):
        add(
            "sink-context",
            f"{name}: catalog descriptor bypasses its owning HTML sink",
        )
    for write in re.finditer(
        r"\.(?:innerHTML\s*\+?=|insertAdjacentHTML\s*\()([^;\n]*)", working
    ):
        expression = write.group(1)
        if (
            re.search(r"\bcatalogMessage\s*\(", expression)
            and not re.search(
                r"\bescapeHtml\s*\([^;\n]*\bcatalogMessage\s*\(", expression
            )
        ):
            add(
                "sink-context",
                f"{name}: inert catalog descriptor reaches HTML without escapeHtml",
            )
    if re.search(
        r"(?:\.textContent\s*=|createTextNode\s*\()[^;\n]*"
        r"\bcatalogMessage\s*\(",
        working,
    ):
        add(
            "sink-context",
            f"{name}: catalog descriptor bypasses an ID-taking text sink",
        )

    # After the exact canonical bodies and lexical declaration are blanked,
    # any remaining bare resolver identifier is a convention violation. This
    # catches aliases, bind/call access, and destructuring without inferring
    # eventual JavaScript ownership.
    if (
        not any(check == "forbidden-catalog-context" for check, _ in problems)
        and (
            re.search(r"(?<![.\w$-])message\b", working)
            or re.search(r"\[\s*['\"]message['\"]\s*\]", working)
        )
    ):
        add(
            "sink-context",
            f"{name}: raw message resolver is referenced outside canonical sinks",
        )

    return problems


def lint_untagged(name: str, raw: str) -> list:
    errors = []
    body = re.search(r"<script>(.*?)</script>", raw, re.S)
    js = body.group(1) if body else raw

    scanned = js
    # The startup failure has one deliberately unlocalized, dependency-free
    # string. It is useful precisely when no catalog is available.
    scanned = scanned.replace(repr(EMERGENCY_TEXT), "''")
    scanned = scanned.replace(json.dumps(EMERGENCY_TEXT), '""')

    # PUBLISHERS maps a model namespace to its vendor's brand name. Brand
    # names are DATA — they arrive from the model id and are never translated,
    # so the prose detector must not see this table. Excluded by design.
    scanned = re.sub(r"const PUBLISHERS = \{.*?\n\};", "", scanned, flags=re.S)

    # chipHtml is excluded by design, not by oversight.
    scanned = re.sub(r"function chipHtml\(pub\) \{.*?\n\}", "", scanned, flags=re.S)

    for m in DISPLAY_CALLS.finditer(blank_comments(scanned)):
        text = m.group(1)
        if frozen(text):
            continue
        errors.append(unowned_ui_string(name, scanned, m.start(1),
            f"display string {text!r} — route it through message()"))

    stripped = blank_comments(scanned)
    line_start = 0
    for line in stripped.splitlines(keepends=True):
        for m in QUOTED.finditer(line):
            text = m.group(1) if m.group(1) is not None else m.group(2)
            before = line[: m.start()]
            # NOT_DISPLAY applies to the string it GOVERNS, not to the whole
            # line. Matching per line meant one `.toFixed(` anywhere suppressed
            # every string beside it — which is how 'met', 'missed' and
            # 'no eligible traffic' render in English on a line CI calls clean.
            window = before[-NOT_DISPLAY_WINDOW:]
            gov = None
            for g in NOT_DISPLAY.finditer(window):
                gov = g
            # Exempt only if the machinery reaches THIS string — no argument or
            # statement boundary in between.
            if gov and not ARG_BOUNDARY.search(window[gov.end():]):
                continue
            # className assignments are CSS state, even where a conditional
            # puts the chosen class farther than the ordinary lookback window.
            # Keep this to the current statement so it cannot hide a later
            # display string on the same source line.
            if re.search(r"\.className\s*=", before.rsplit(";", 1)[-1]):
                continue
            # `cls:` is the CSS-state field in a returned render descriptor,
            # never operator copy. Keep the exemption to that exact field;
            # a later object field remains scanned normally.
            if re.search(r"\bcls\s*:\s*$", before):
                continue
            # `class="logo cdnchip"` is an attribute value, not display text.
            if ATTR_VALUE.search(before):
                continue
            if looks_like_prose(text):
                errors.append(unowned_ui_string(name, scanned, line_start + m.start(),
                    f"prose {text!r} — route it through message()"))
        line_start += len(line)

    # Bare prose sitting between tags inside a template literal.
    for m in TEMPLATE_LITERAL.finditer(stripped):
        # Interpolations become a separator, not a hole: `${n} validated ·`
        # must still expose "validated" rather than vanishing as `${`-bearing.
        lit = INTERPOLATION.sub("\x00", m.group(0))
        for node in TEXT_NODE.findall(lit):
            for piece in node.split("\x00"):
                piece = piece.strip()
                if piece and looks_like_prose(piece):
                    errors.append(unowned_ui_string(name, scanned, m.start(),
                        f"prose {piece!r} in template markup — route it through message()"))

    # Localizable attributes carrying prose, with no data-i18n-attr beside them.
    markup = strip_scripts(raw)
    for m in re.finditer(r"<[^>]*\s(title|placeholder|aria-label)=\"([^\"]{2,})\"[^>]*>", markup):
        tag, attr, text = m.group(0), m.group(1), m.group(2)
        if "data-i18n-attr" in tag or "${" in text:
            continue
        if frozen(text):
            continue
        errors.append(unowned_ui_string(name, raw, m.start(2),
            f"{attr}={text!r} — add data-i18n-attr"))
    return errors


def unowned_ui_string(name: str, source: str, offset: int, detail: str) -> str:
    """Report prose in its real source, rather than a synthetic bundle line."""
    return (
        f"[unowned-ui-string] {name}:{source.count(chr(10), 0, offset) + 1}: "
        f"{detail}"
    )


class _MarkupTextOwnership(HTMLParser):
    """Find ordinary visible markup text that has no catalog-owning ancestor."""

    # HTMLParser emits ordinary ``handle_starttag`` calls for void HTML
    # elements. They have no end tag to unwind the ownership stack, so pushing
    # one would let a catalog attribute on an <input> hide every later sibling.
    VOID_ELEMENTS = {
        "area", "base", "br", "col", "embed", "hr", "img", "input",
        "link", "meta", "param", "source", "track", "wbr",
    }

    def __init__(self, name: str, source: str):
        super().__init__(convert_charrefs=True)
        self.name = name
        self.source = source
        self.stack = []
        self.errors = []

    def handle_starttag(self, tag, attrs):
        if tag not in self.VOID_ELEMENTS:
            self.stack.append((tag, dict(attrs)))

    def handle_startendtag(self, tag, attrs):
        pass

    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, -1, -1):
            if self.stack[index][0] == tag:
                del self.stack[index:]
                break

    def handle_data(self, data):
        text = data.strip()
        if not text or not looks_like_prose(text):
            return
        if any(any(key.startswith("data-i18n") for key in attrs) for _, attrs in self.stack):
            return
        line, column = self.getpos()
        offset = sum(len(line) + 1 for line in self.source.splitlines()[:line - 1]) + column
        self.errors.append(unowned_ui_string(
            self.name, self.source, offset,
            f"markup text {text!r} — add a catalog-owned data-i18n sink",
        ))


def lint_untagged_markup(name: str, raw: str) -> list:
    parser = _MarkupTextOwnership(name, raw)
    parser.feed(strip_scripts(blank_comments(raw)))
    return parser.errors


PLURAL_SUFFIX = re.compile(
    r"\b(?:[A-Za-z_$][\w$]*\.)?(?:length|lanes)\s*===\s*1\s*\?\s*''\s*:\s*'s'"
)


def lint_plural_suffixes(name: str, raw: str) -> list:
    """A narrow guard for the documented English-only plural suffix shape."""
    return [
        unowned_ui_string(
            name, raw, match.start(),
            "English plural suffix — select explicit catalog variants through Intl.PluralRules",
        )
        for match in PLURAL_SUFFIX.finditer(strip_comments(raw))
    ]



# Terms retired by knowledge/decisions/standard-vocabulary.md. Reintroducing one
# is how a standardized interface drifts back apart: nothing else in the tree
# notices, and the next translation pass bakes the drift into eight languages.
#
# Multi-word and distinctive only, on purpose. Single ambiguous words are NOT
# listed: "window" is still correct for the rate-limit rolling window ("0 / 40
# in window"), "lane" is still correct in metric labels, "Open" and "bench"
# have unrelated legitimate senses. Banning a word that is both a retired label
# and a live domain term is the exact mistake that renamed a rate-limit counter
# during the label sweep.
RETIRED = {
    "Harness": "Client",
    "Harnesses": "Clients",
    "Conversation stickiness": "Session affinity",
    "Model-pressure governor": "Model limits",
    "Where time goes": "Latency breakdown",
    "Rate-limit pressure": "Throttling",
    "Historical provisioning": "Capacity history",
    "Dollars saved": "(removed — no honest per-model rate)",
    "All retained": "All time",
    "Earliest retained snapshot": "Oldest data point",
    "History file": "Data file",
    "exhaustions/min": "Capacity errors/min",
    "Lane slot": "Slot",
    "Avg reply": "Avg response",
    "Tool-offering": "Requests with tools",
    "Tool-using requests": "Requests using tools",
    "No reasoning-token usage seen": "No reasoning tokens",
    "selected window": "selected time range",
    "Default dashboard window": "Default time range",
    "fixed range": "Absolute",
    "following now": "Live",
    "rpm free": "Available",
    "rpm total": "Total",
    "Now rpm": "Current rate",
    "Slots in use": "Enabled keys",
    "Model pressure": "Model limits",
    "governor engaged": "(dropped — 'governor' is implementation vocabulary)",
}


def lint_retired_vocabulary(name: str, raw: str, catalog=None) -> list:
    """No shipped text may reintroduce a retired term.

    Scanning only catalog values was not enough: the whole point of the
    retirement is that operators stop seeing the old word, and a label that
    never made it into the catalog still renders. `rpm total` shipped in
    setup.html's review panel through exactly that hole, with CI green.
    """
    out = []
    if catalog is None:
        catalog = load_catalog(raw)
    for mid, msg in sorted(catalog.items()):
        text = msg["en"] if isinstance(msg, dict) else msg
        for old, new in RETIRED.items():
            if old in text:
                out.append(
                    f"[noncanonical-vocabulary] {name}: {mid} uses retired term {old!r} — "
                    f"standard vocabulary says {new!r}"
                )
    # Everything outside the catalog block: markup, template literals, quoted
    # strings. Comments are blanked rather than deleted so a note *about* a
    # retirement is not itself a violation AND the reported line number still
    # matches the file — collapsing multi-line /* */ blocks (this file is full
    # of them) shifted every number after the first one, and the render gate's
    # own comment says a gate that reports the wrong line is one nobody trusts.
    outside = blank_comments(
        re.sub(
            r'<script type="application/json" id="i18n-catalog">.*?</script>',
            lambda m: "\n" * m.group(0).count("\n"),
            raw,
            flags=re.S,
        )
    )
    # Whitespace-insensitive: a retired term wrapped across a line break is the
    # same term to the operator reading the rendered page. The untagged-prose
    # scan already spans newlines; this one guards a non-negotiable and must not
    # be the weaker of the two.
    flat = re.sub(r"\s+", " ", outside)
    offsets = [m.start() for m in re.finditer(r"\S+", outside)]
    for old, new in RETIRED.items():
        needle = re.sub(r"\s+", " ", old)
        for m in re.finditer(re.escape(needle), flat):
            # Map the flattened offset back to a real line number.
            word_index = flat.count(" ", 0, m.start())
            src_pos = offsets[word_index] if word_index < len(offsets) else 0
            out.append(
                f"[noncanonical-vocabulary] {name}:"
                f"{outside.count(chr(10), 0, src_pos) + 1}: retired term "
                f"{old!r} outside the catalog — standard vocabulary says {new!r}"
            )
    return out


# ---------------------------------------------------------------------------
# Selftest — prove the lints still bite
#
# `locale_v1.py --selftest` has done this for the validator since PR 5, with a
# negative fixture per check. This lint had no equivalent, so every time its
# holes were closed the injections that proved the fix lived in a scratch
# directory and evaporated. Three of the four holes below were opened or missed
# by a change that left `i18n OK` on screen.
#
# Fixtures are source snippets rather than files, because the unit under test is
# a scanner over page source. Each is spliced into a synthetic page and must trip
# its OWN check; the CONTROL rows must trip nothing, because a lint that flags
# CSS and class attributes gets switched off.

SELFTEST_CASES = [
    # (label, snippet, expected: "unowned-ui-string" | "retired" | None)
    ("assignment-single", "const zzq = 'Some fresh label here';", "unowned-ui-string"),
    ("assignment-double", 'const zzq = "Some fresh label here";', "unowned-ui-string"),
    ("equality-compare", "if (x === 'Some fresh label here') {}", "unowned-ui-string"),
    ("textContent-sink", "el.textContent = 'Some fresh label here';", "unowned-ui-string"),
    ("innerHTML-sink", "el.innerHTML = 'Some fresh label here';", "unowned-ui-string"),
    ("adjacent-arg", "bar(v.toFixed(1), 'Some fresh label here');", "unowned-ui-string"),
    ("statement-boundary", "el.style.width = w; bar('Some fresh label here');", "unowned-ui-string"),
    ("colon-prefixed-prose", "const z = `<div>note: some fresh label</div>`;", "unowned-ui-string"),
    ("template-text-node", "const z = `<div><span>Some fresh label</span></div>`;", "unowned-ui-string"),
    ("display-call-arg", "prow('Some fresh label here', 1);", "unowned-ui-string"),
    ("retired-in-markup", "const z = `<div>Harness</div>`;", "retired"),
    # A real multi-word retired key, wrapped. NOT "Latency breakdown" — that is
    # the REPLACEMENT term. Using it here is how a scratch test that grepped for
    # a substring reported this working when only the prose scan had fired.
    ("retired-across-newline", "const z = `<div>Where time\n  goes</div>`;", "retired"),
    ("retired-in-comment", "/* Harness was retired; do not reintroduce */", None),
    ("css-declaration", "const z = 'display:flex;gap:20px';", None),
    ("css-single-value", "const z = 'position:relative';", None),
    ("class-attribute", 'const z = `<div class="logo cdnchip"></div>`;', None),
    ("class-assignment", "el.className = ready ? 'live' : 'live idle';", None),
    ("class-adjacent-display", "el.className = 'live idle'; el.textContent = 'Some fresh label here';", "unowned-ui-string"),
    ("class-state-field", "const state = { cls: 'kstate dim' };", None),
    ("class-field-display", "const state = { cls: 'kstate dim', label: 'Some fresh label here' };", "unowned-ui-string"),
    ("real-machinery", "document.querySelector('#tab-models');", None),
    ("frozen-unit", "const z = 'tok/s';", None),
    ("lowercase-token", "const z = 'sticky';", None),
]

SOURCE_CLASS_SELFTESTS = [
    ("html-label", "tests/fixtures/locales/unowned-html.html",
     '\n<div>Unowned static label</div>', lint_untagged_markup, 2),
    ("js-toast", "tests/fixtures/locales/unowned-toast.js",
     "\nshowToast('Unowned toast message');", lint_untagged, 2),
    ("validation-error", "tests/fixtures/locales/unowned-validation.js",
     "\nnote('err', 'Unowned validation error');", lint_untagged, 2),
    ("dialog-fragment", "tests/fixtures/locales/unowned-dialog.js",
     "\nconst dialog = `<p>Unowned dialog fragment</p>`;", lint_untagged, 2),
    ("accessibility-attribute", "tests/fixtures/locales/unowned-a11y.html",
     '\n<button aria-label="Unowned accessible name"></button>', lint_untagged, 2),
    ("plural-branch", "tests/fixtures/locales/unowned-plural.js",
     "\nconst text = `${cfg.lanes} key${cfg.lanes === 1 ? '' : 's'}`;", lint_plural_suffixes, 2),
    ("settings-label", "src/web/settings.js",
     "\nfunction renderSettings() { const label = 'Unowned settings label'; }", lint_untagged, 2),
    ("void-tag-does-not-own-following-label", "tests/fixtures/locales/unowned-void.html",
     '\n<input data-i18n-attr="placeholder:probe"><span>Unowned following label</span>', lint_untagged_markup, 2),
    ("tagged-ancestor-owns-descendant", "tests/fixtures/locales/owned-ancestor.html",
     '\n<div data-i18n="probe"><span>Owned descendant label</span></div>', lint_untagged_markup, None),
]

SINK_SELFTEST_CASES = [
    # Allowed plain-text contexts.
    ("element-text", "setMessageText(el, 'probe');", None),
    ("attr-title", "setMessageAttr(el, 'title', 'probe');", None),
    ("attr-placeholder", "setMessageAttr(el, 'placeholder', 'probe');", None),
    ("attr-aria-label", "setMessageAttr(el, 'aria-label', 'probe');", None),
    (
        "attr-svg-aria-label",
        "setMessageAttr(svgNode, 'aria-label', 'probe');",
        None,
    ),
    ("attr-alt", "setMessageAttr(el, 'alt', 'probe');", None),
    # Forbidden contexts are independent so a broad check cannot hide a hole.
    ("attr-href", "setMessageAttr(el, 'href', 'probe');", "forbidden-catalog-context"),
    ("attr-src", "setMessageAttr(el, 'src', 'probe');", "forbidden-catalog-context"),
    ("attr-style", "setMessageAttr(el, 'style', 'probe');", "forbidden-catalog-context"),
    ("attr-onclick", "setMessageAttr(el, 'onclick', 'probe');", "forbidden-catalog-context"),
    ("property-href", "el.href = message('probe');", "forbidden-catalog-context"),
    ("property-src", "el.src = message('probe');", "forbidden-catalog-context"),
    ("property-style", "el.style.cssText = message('probe');", "forbidden-catalog-context"),
    ("property-onclick", "el.onclick = message('probe');", "forbidden-catalog-context"),
    ("script-text", "scriptNode.textContent = message('probe');", "forbidden-catalog-context"),
    ("css-text", "styleNode.textContent = message('probe');", "forbidden-catalog-context"),
    ("raw-svg", "svgNode.innerHTML = message('probe');", "forbidden-catalog-context"),
    (
        "raw-svg-concat",
        "svgNode.innerHTML = '<text>' + message('probe') + '</text>';",
        "forbidden-catalog-context",
    ),
    (
        "raw-svg-escaped",
        "svgNode.innerHTML = '<text>' + esc(message('probe')) + '</text>';",
        "forbidden-catalog-context",
    ),
    ("html-string", "el.innerHTML = message('probe');", "sink-context"),
    (
        "escaped-html-string",
        "el.innerHTML = '<span>' + escapeHtml(catalogMessage('probe')) + '</span>';",
        None,
    ),
    (
        "html-owner-string-spoof",
        "el.innerHTML = 'esc(' + message('probe');",
        "sink-context",
    ),
    (
        "adjacent-owner-string-spoof",
        "el.insertAdjacentHTML('beforeend', 'esc(' + message('probe'));",
        "sink-context",
    ),
    ("approved-html-owner", "metricRow(catalogMessage('probe'), '1');", None),
    ("approved-plain-binding", "const windowLabel = catalogMessage('probe');", None),
    (
        "raw-html-concat",
        "el.innerHTML = '<span>' + message('probe') + '</span>';",
        "sink-context",
    ),
    (
        "raw-html-multiline",
        "el.innerHTML = `<span>\n${message('probe')}\n</span>`;",
        "sink-context",
    ),
    (
        "raw-insert-adjacent",
        "el.insertAdjacentHTML('beforeend', message('probe'));",
        "sink-context",
    ),
    (
        "raw-html-plus-equals",
        "el.innerHTML += message('probe');",
        "sink-context",
    ),
    (
        "raw-html-no-semicolon",
        "el.innerHTML = message('probe')",
        "sink-context",
    ),
    (
        "raw-insert-no-semicolon",
        "el.insertAdjacentHTML('beforeend', message('probe'))",
        "sink-context",
    ),
    (
        "direct-setattribute-href",
        "el.setAttribute('href', message('probe'));",
        "forbidden-catalog-context",
    ),
    (
        "direct-setattribute-style",
        "el.setAttribute('style', message('probe'));",
        "forbidden-catalog-context",
    ),
    (
        "direct-setattribute-nested",
        "el.setAttribute('href', message('probe', {x: fn()}));",
        "forbidden-catalog-context",
    ),
    (
        "direct-setattribute-fake-canonical",
        "function setMessageAttr() {} other.setAttribute(attr, message(id, params));",
        "forbidden-catalog-context",
    ),
    (
        "canonical-setattribute-forbidden",
        "function setMessageAttr(node, attr, id, params) { node.setAttribute('href', message(id, params)); }",
        "forbidden-catalog-context",
    ),
    ("unknown-owner", "unknownSink(message('probe'));", "sink-context"),
    ("unknown-binding", "const surprise = message('probe');", "sink-context"),
    ("resolver-alias", "const lookup = message; lookup('probe');", "sink-context"),
    ("resolver-call", "message.call(null, 'probe');", "sink-context"),
    ("resolver-bind", "const lookup = message.bind(null);", "sink-context"),
    ("resolver-window", "window['message']('probe');", "sink-context"),
    (
        "unknown-in-returner",
        "function apiAccessLine() { return unknownSink(message('probe')); }",
        "sink-context",
    ),
    (
        "unknown-in-approved-binding",
        "const windowLabel = unknownSink(message('probe'));",
        "sink-context",
    ),
    (
        "unknown-in-internal",
        "function setMessageWithNodes() { unknownSink(message('probe')); }",
        "sink-context",
    ),
    (
        "script-alias-text",
        "const alias = document.createElement('script'); alias.textContent = message('probe');",
        "forbidden-catalog-context",
    ),
    (
        "style-alias-text",
        "const alias = document.createElement('style'); alias.textContent = message('probe');",
        "forbidden-catalog-context",
    ),
    (
        "svg-alias-text",
        "const alias = document.createElementNS('urn:x', 'svg'); alias.textContent = message('probe');",
        "forbidden-catalog-context",
    ),
    (
        "helper-script-target",
        "setMessageText(scriptNode, 'probe');",
        "forbidden-catalog-context",
    ),
    (
        "helper-style-target",
        "setMessageText(styleNode, 'probe');",
        "forbidden-catalog-context",
    ),
    (
        "helper-svg-target",
        "setMessageText(svgNode, 'probe');",
        "forbidden-catalog-context",
    ),
    (
        "structured-script-target",
        "setMessageWithNodes(scriptNode, 'probe', []);",
        "forbidden-catalog-context",
    ),
    (
        "emphasis-style-target",
        "setEmphasizedMessage(styleNode, 'probe', document.createElement('b'));",
        "forbidden-catalog-context",
    ),
    (
        "text-asi-unknown",
        "el.textContent = 'safe'\nunknownSink(message('probe'));",
        "sink-context",
    ),
    (
        "binding-asi-unknown",
        "const windowLabel = 'safe'\nunknownSink(message('probe'));",
        "sink-context",
    ),
    (
        "return-asi-unknown",
        "function apiAccessLine() { return 'safe'\nsurprise = message('probe') }",
        "sink-context",
    ),
    (
        "approved-returner",
        "function apiAccessLine() { return message('probe'); }",
        "sink-context",
    ),
    (
        "approved-internal",
        "function setMessageWithNodes() { let text = message('probe'); }",
        "sink-context",
    ),
    (
        "descriptor-property-href",
        "el.href = catalogMessage('probe');",
        "forbidden-catalog-context",
    ),
    (
        "descriptor-property-style",
        "el.style.cssText = catalogMessage('probe');",
        "forbidden-catalog-context",
    ),
    (
        "descriptor-native-attr",
        "el.setAttribute('title', catalogMessage('probe'));",
        "forbidden-catalog-context",
    ),
    (
        "descriptor-raw-svg",
        "svgNode.innerHTML = escapeHtml(catalogMessage('probe'));",
        "forbidden-catalog-context",
    ),
    (
        "descriptor-text-helper",
        "setMessageText(el, catalogMessage('probe'));",
        "sink-context",
    ),
    (
        "descriptor-direct-text",
        "el.textContent = catalogMessage('probe');",
        "sink-context",
    ),
    (
        "descriptor-text-node",
        "document.createTextNode(catalogMessage('probe'));",
        "sink-context",
    ),
    (
        "declarative-script-target",
        '<script data-i18n="probe"></script>',
        "forbidden-catalog-context",
    ),
    (
        "declarative-style-target",
        '<style data-i18n="probe"></style>',
        "forbidden-catalog-context",
    ),
    (
        "declarative-svg-target",
        '<svg data-i18n="probe"></svg>',
        "forbidden-catalog-context",
    ),
    (
        "structured-html",
        '<span data-i18n-html="probe"></span>',
        "structured-message-html",
    ),
    ("escaped-compat-helper", "const t = id => id;", "sink-context"),
]

SELFTEST_PAGE = """<!doctype html><html><body>
<script type="application/json" id="i18n-catalog">{"locale":"en-US","messages":{}}</script>
<script>
%s
</script>
</body></html>
"""


def selftest() -> int:
    """Each snippet must trip its OWN check; controls must trip nothing."""
    failures = []
    for label, snippet, want in SELFTEST_CASES:
        page = SELFTEST_PAGE % snippet
        found = set()
        if lint_untagged("selftest", page):
            found.add("unowned-ui-string")
        if lint_retired_vocabulary("selftest", page):
            found.add("retired")
        if want is None:
            if found:
                failures.append(f"{label}: expected to pass, tripped {sorted(found)}")
            else:
                print(f"  ok  {label:24} passes")
        elif want not in found:
            failures.append(
                f"{label}: expected {want!r}, tripped {sorted(found) or 'nothing'}"
            )
        else:
            print(f"  ok  {label:24} trips {want}")

    for label, name, snippet, check, line in SOURCE_CLASS_SELFTESTS:
        found = check(name, snippet)
        if line is None:
            if found:
                failures.append(f"{label}: expected to pass, got {found!r}")
            else:
                print(f"  ok  {label:24} passes")
        elif (len(found) != 1
                or "[unowned-ui-string]" not in found[0]
                or f"{name}:{line}:" not in found[0]):
            failures.append(
                f"{label}: expected one attributed unowned-ui-string, got {found!r}"
            )
        else:
            print(f"  ok  {label:24} trips unowned-ui-string")

    for label, snippet, want in SINK_SELFTEST_CASES:
        page = SELFTEST_PAGE % snippet
        found = {check for check, _ in lint_catalog_sinks("selftest", page)}
        if want is None:
            if found:
                failures.append(f"{label}: expected to pass, tripped {sorted(found)}")
            else:
                print(f"  ok  {label:24} passes")
        elif found != {want}:
            failures.append(
                f"{label}: expected only {want!r}, tripped {sorted(found) or 'nothing'}"
            )
        else:
            print(f"  ok  {label:24} trips {want}")

    vocabulary = lint_retired_vocabulary(
        "selftest",
        SELFTEST_PAGE % "const label = '<span>Harnesses</span>';",
    )
    if not any("[noncanonical-vocabulary]" in problem for problem in vocabulary):
        failures.append(
            "noncanonical-vocabulary: retired vocabulary was found without "
            "the required named check id"
        )
    else:
        print("  ok  noncanonical-vocabulary  trips noncanonical-vocabulary")

    if failures:
        print("\nselftest FAILED:")
        for f in failures:
            print("  -", f)
        return 1
    total = len(SELFTEST_CASES) + len(SOURCE_CLASS_SELFTESTS) + len(SINK_SELFTEST_CASES)
    print(f"\nselftest ok — {total} cases, every check observed to fail")
    return 0


def main() -> int:
    if "--selftest" in sys.argv[1:]:
        return selftest()
    errors = []
    referenced = set()
    catalog_path = ROOT / "src/web/locales/en-US.json"
    catalog = json.loads(catalog_path.read_text())["messages"]

    for name in PAGE_SOURCES:
        path = ROOT / name
        raw = bundled_page(name)
        if 'id="i18n-catalog"' in raw:
            errors.append(f"[inline-catalog] {name}: inline catalog must be removed")
        source = strip_scripts(raw)

        for mid, inner in tagged(source, "data-i18n"):
            referenced.add(mid)
            if mid not in catalog:
                errors.append(f"{name}: data-i18n={mid} has no catalog entry")
                continue
            # Catalog values are plain text. Decode source-markup entities
            # before comparing the authored fallback text with the catalog.
            want = catalog[mid]["en"]
            if htmlmod.unescape(inner).strip() != want.strip():
                errors.append(
                    f"{name}: {mid} markup {htmlmod.unescape(inner).strip()[:50]!r} "
                    f"!= catalog {want[:50]!r}"
                )

        for mid, inner in tagged(source, "data-i18n-text"):
            referenced.add(mid)
            if mid not in catalog:
                errors.append(f"{name}: data-i18n-text={mid} has no catalog entry")
                continue
            first = own_text(inner).strip()
            want = htmlmod.unescape(catalog[mid]["en"]).strip()
            if htmlmod.unescape(first) != want:
                errors.append(
                    f"{name}: {mid} text node {first[:50]!r} != catalog {want[:50]!r}"
                )

        if name == next(iter(PAGE_SOURCES)):
            # No value anywhere may carry raw or entity-encoded markup.
            for mid, msg in catalog.items():
                if "<" in msg["en"] or ">" in msg["en"]:
                    errors.append(f"[catalog-markup] {catalog_path}: {mid} contains raw markup")
                decoded = htmlmod.unescape(msg["en"])
                if decoded != msg["en"] and ("<" in decoded or ">" in decoded):
                    errors.append(
                        f"[catalog-entity-markup] {catalog_path}: {mid} contains "
                        "entity-encoded markup"
                    )

        # Only text-bearing attributes are localizable; the runtime enforces
        # the same set, so a mismatch here is a bug in one of the two.
        localizable = {"title", "placeholder", "aria-label", "alt"}
        for m in re.finditer(r'(<[^>]*\sdata-i18n-attr="([^"]+)"[^>]*>)', source):
            tag, spec = m.group(1), m.group(2)
            for pair in spec.split(","):
                attr, _, mid = pair.partition(":")
                referenced.add(mid)
                if attr not in localizable:
                    errors.append(f"{name}: {mid} targets non-localizable attribute {attr!r}")
                if mid not in catalog:
                    errors.append(f"{name}: data-i18n-attr={mid} has no catalog entry")
                    continue
                # round-trip: the attribute still present in the markup must be
                # exactly what the catalog will substitute
                cur = re.search(rf'\s{re.escape(attr)}="([^"]*)"', tag)
                if cur and htmlmod.unescape(cur.group(1)) != catalog[mid]["en"]:
                    errors.append(
                        f"{name}: {mid} attribute {attr}={cur.group(1)[:40]!r} "
                        f"!= catalog {catalog[mid]['en'][:40]!r}"
                    )

        if name == next(iter(PAGE_SOURCES)):
            for mid, msg in catalog.items():
                got = hashlib.sha256(msg["en"].encode()).hexdigest()[:8]
                if msg.get("hash") != got:
                    errors.append(
                        f"{catalog_path}: {mid} hash {msg.get('hash')} stale, text hashes to {got}"
                    )

        # Executable page code carries catalog ids as quoted values passed to
        # ID-taking sinks or catalogMessage(). The application/json catalog is
        # deliberately excluded, or every orphan would appear referenced.
        js = strip_comments("".join(re.findall(r"<script>(.*?)</script>", raw, re.S)))
        for m in re.finditer(r"""['"]((?:dashboard|settings|setup|login)\.[a-z0-9_.]+)['"]""", js):
            mid = m.group(1)
            referenced.add(mid)
            if mid not in catalog:
                errors.append(f"{name}: message id {mid!r} has no catalog entry")
        for m in re.finditer(
            r"""data-i18n(?:-attr)?=["'][^"']*?((?:dashboard|settings|setup|login)\.[a-z0-9_.]+)""",
            js,
        ):
            mid = m.group(1)
            referenced.add(mid)
            if mid not in catalog:
                errors.append(f"{name}: declarative message id {mid!r} has no catalog entry")

    for mid in catalog:
        if mid not in referenced:
            errors.append(f"{catalog_path}: catalog id {mid} is never referenced (orphan)")

    for index, name in enumerate(PAGE_SOURCES):
        page = bundled_page(name)
        errors += lint_runtime_helpers(name, page)
        errors += [
            f"[{check}] {detail}"
            for check, detail in lint_catalog_sinks(name, page)
        ]
        errors += lint_untagged_markup(name, (ROOT / name).read_text())
        for script in PAGE_SOURCES[name]:
            script_source = (ROOT / script).read_text()
            errors += lint_untagged(script, script_source)
            errors += lint_plural_suffixes(script, script_source)
        errors += lint_retired_vocabulary(
            name,
            page,
            catalog if index == 0 else {},
        )

    if errors:
        print(f"{len(errors)} problem(s):")
        for e in errors:
            print("  -", e)
        return 1
    print(f"i18n OK — {len(referenced)} ids referenced, round-trip clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
