"""Check Falcon's language packs against the messages marked in the source.

    python scripts/check-translations.py           # check; exit status 1 on any problem
    python scripts/check-translations.py --write   # also regenerate the test pseudo-language pack

A language pack is one entry in falcon/native/translations/languages.json plus
falcon/native/translations/<code>.json, keyed by the exact English message. This script finds every
marked message (Slint `@tr("falcon" => ...)`; Rust `tr(...)`, `tr_format!`, `tr_plural!`,
`tr_noop!`) and reports, for every pack: missing and stale entries, placeholders that differ from the
English, plural forms that do not match the language's rule, and empty translations. It also checks
the list itself. See docs/development/translations.md. Standard library only; works with Python's
UTF-8 mode off.
"""
import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
NATIVE = Path('falcon/native')
CONTEXT = 'falcon'
PSEUDO = 'xx-TEST'
PSEUDO_RULE = 'one_other'
# Keep in step with falcon/native/src/i18n_rules.rs: category names in gettext form order.
PLURAL_RULES = {'none': ['other'], 'one_other': ['one', 'other']}

STRING = r'"(?:[^"\\]|\\.)*"'
# Every marked Rust form starts with one of these; each is then read argument by argument, and a
# form the reader cannot handle is reported, never skipped (round-1 review R3).
RUST_MARK = re.compile(r'(?<![\w$])(?:(fn\s+)?tr\s*\(|(tr_noop|tr_format|tr_plural)!\s*([(\[{]))')
# `tr(value)` with a runtime value is the documented way to show text marked elsewhere with
# tr_noop!; macro bodies pass `$en`. Both are left alone.
RUNTIME_ARG = re.compile(r'\$?[A-Za-z_][\w.:]*\s*[,)]')
SLINT_TR = re.compile(r'@tr\(\s*(' + STRING + r')\s*=>\s*(' + STRING + r')(?:\s*\|\s*(' + STRING + r')\s*%)?', re.S)
SLINT_ANY_TR = re.compile(r'@tr\(')
NAMED = re.compile(r'[A-Za-z_][A-Za-z0-9_]*\Z')
NUMBERED = re.compile(r'(?:[0-9]*|n)\Z')


class Problem(Exception):
    pass


def blank_comments(text):
    """The text with // and /* */ comments replaced by spaces (newlines kept), strings untouched."""
    out = list(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            i += 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == '\\' else 1
            i += 1
        elif c == 'r' and re.match(r'r(#*)"', text[i:i + 10]) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == '_')):
            hashes = re.match(r'r(#*)"', text[i:]).group(1)
            end = text.find('"' + hashes, i + len(hashes) + 2)
            i = n if end < 0 else end + 1 + len(hashes)
        elif c == "'" and i + 2 < n and (text[i + 1] == '\\' or text[i + 2] == "'"):
            j = text.find("'", i + 3 if text[i + 1] == '\\' else i + 2)
            i = n if j < 0 else j + 1
        elif text.startswith('//', i):
            j = text.find('\n', i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = ' '
            i = j
        elif text.startswith('/*', i):
            j = text.find('*/', i + 2)
            j = n if j < 0 else j + 2
            for k in range(i, j):
                if out[k] != '\n':
                    out[k] = ' '
            i = j
        else:
            i += 1
    return ''.join(out)


def strip_test_modules(text):
    """Drop `#[cfg(test)] mod name { ... }` bodies so test-only messages are not collected."""
    out, pos = [], 0
    for m in re.finditer(r'#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{', text):
        if m.start() < pos:
            continue
        depth, i = 1, m.end()
        while i < len(text) and depth:
            c = text[i]
            if c == '"':
                i += 1
                while i < len(text) and text[i] != '"':
                    i += 2 if text[i] == '\\' else 1
            elif c == 'r' and re.match(r'r(#*)"', text[i:i + 10]):
                hashes = re.match(r'r(#*)"', text[i:]).group(1)
                end = text.find('"' + hashes, i + len(hashes) + 2)
                i = len(text) if end < 0 else end + len(hashes)
            elif c == "'" and i + 2 < len(text) and (text[i + 1] == '\\' or text[i + 2] == "'"):
                j = text.find("'", i + 3 if text[i + 1] == '\\' else i + 2)
                i = len(text) if j < 0 else j
            elif c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
            i += 1
        out.append(text[pos:m.start()])
        out.append('\n' * text.count('\n', m.start(), i))
        pos = i
    out.append(text[pos:])
    return ''.join(out)


def read_literal(text, i):
    """A Rust string literal starting at text[i]: (value, end index), or None if there is none.

    Ordinary `"..."` literals are unescaped; raw `r"..."` / `r#"..."#` literals are verbatim.
    """
    m = re.match(r'r(#*)"', text[i:i + 300])
    if m:
        hashes = m.group(1)
        start = i + m.end()
        end = text.find('"' + hashes, start)
        return (text[start:end], end + 1 + len(hashes)) if end >= 0 else None
    m = re.match(STRING, text[i:])
    if m:
        return unescape(m.group(0)), i + m.end()
    return None


def skip_space(text, i):
    while i < len(text) and text[i].isspace():
        i += 1
    return i


def skip_expression(text, i):
    """Index of the top-level `,` ending the expression at text[i], or None if it never comes."""
    depth = 0
    while i < len(text):
        c = text[i]
        if c in '([{':
            depth += 1
        elif c in ')]}':
            if depth == 0:
                return None
            depth -= 1
        elif c == ',' and depth == 0:
            return i
        elif c == '"' or (c == 'r' and re.match(r'r#*"', text[i:i + 300])):
            lit = read_literal(text, i)
            if lit is None:
                return None
            i = lit[1]
            continue
        elif c == "'" and i + 2 < len(text) and (text[i + 1] == '\\' or text[i + 2] == "'"):
            j = text.find("'", i + 3 if text[i + 1] == '\\' else i + 2)
            i = len(text) if j < 0 else j
        i += 1
    return None


def rust_marked(text):
    """Yield (kind, index, one, other) for each marked message, or (None, index, problem, None)."""
    for m in RUST_MARK.finditer(text):
        if m.group(1):  # `fn tr(` is the definition, not a use
            continue
        start = m.start()
        name = m.group(2) or 'tr'
        if m.group(3) and m.group(3) != '(':
            yield None, start, f'{name}! must be called with parentheses', None
            continue
        i = skip_space(text, m.end())
        if name == 'tr' and RUNTIME_ARG.match(text, i):
            continue
        if name == 'tr_plural':
            comma = skip_expression(text, i)
            if comma is None:
                yield None, start, 'tr_plural!(count, "singular", "plural", ...) could not be read', None
                continue
            i = skip_space(text, comma + 1)
        first = read_literal(text, i)
        if first is None:
            yield None, start, f'{name}: the English text must be a string literal (or, for tr, a value marked with tr_noop!)', None
            continue
        one, i = first
        i = skip_space(text, i)
        other = None
        if name == 'tr_plural':
            if i >= len(text) or text[i] != ',':
                yield None, start, 'tr_plural! needs a singular and a plural string literal', None
                continue
            second = read_literal(text, skip_space(text, i + 1))
            if second is None:
                yield None, start, 'tr_plural!: the plural form must be a string literal', None
                continue
            other, i = second
            i = skip_space(text, i)
        if name in ('tr', 'tr_noop'):
            if i < len(text) and text[i] == ',':
                i = skip_space(text, i + 1)
            if i >= len(text) or text[i] != ')':
                yield None, start, f'{name} takes exactly one string literal', None
                continue
        elif i >= len(text) or text[i] not in ',)':
            yield None, start, f'{name}! could not be read after its English text', None
            continue
        kind = {'tr': 'plain', 'tr_noop': 'plain', 'tr_format': 'format', 'tr_plural': 'plural'}[name]
        yield kind, start, one, other


def unescape(literal):
    """The value of a Rust or Slint string literal (with quotes)."""
    body, out, i = literal[1:-1], [], 0
    while i < len(body):
        c = body[i]
        if c != '\\':
            out.append(c)
            i += 1
            continue
        e = body[i + 1] if i + 1 < len(body) else ''
        if e in '"\\\'':
            out.append(e)
            i += 2
        elif e in 'ntr0':
            out.append({'n': '\n', 't': '\t', 'r': '\r', '0': '\0'}[e])
            i += 2
        elif e == 'u':
            j = body.index('}', i)
            out.append(chr(int(body[i + 3:j], 16)))
            i = j + 1
        elif e == 'x':
            out.append(chr(int(body[i + 2:i + 4], 16)))
            i += 4
        elif e == '\n':
            i += 2
            while i < len(body) and body[i] in ' \t\r\n':
                i += 1
        else:
            raise Problem(f'unsupported escape \\{e} in {literal}')
    return ''.join(out)


def placeholders(template):
    """The `{...}` names in a template, with `{{` and `}}` as literal braces."""
    names, i = [], 0
    while i < len(template):
        if template.startswith('{{', i) or template.startswith('}}', i):
            i += 2
        elif template[i] == '{':
            j = template.find('}', i)
            if j < 0:
                raise Problem(f'unclosed {{ in {template!r}')
            names.append(template[i + 1:j])
            i = j + 1
        elif template[i] == '}':
            raise Problem(f'stray }} in {template!r} (write }}}} for a literal brace)')
        else:
            i += 1
    return names


def line_of(text, index):
    return text.count('\n', 0, index) + 1


def collect(root):
    """Every marked message: {english key: {'kind', 'plural', 'other', 'where'}} and source problems."""
    messages, problems = {}, []

    def add(key, kind, plural, other, where):
        found = messages.get(key)
        if found is None:
            messages[key] = {'kind': kind, 'plural': plural, 'other': other, 'where': [where]}
            return
        found['where'].append(where)
        if found['plural'] != plural or (plural and found['other'] != other):
            problems.append(f'{where}: {key!r} is marked both as a counted and a plain message, or with two plural forms')
        if found['kind'] != kind and placeholders(key):
            problems.append(f'{where}: {key!r} is used from Slint and Rust; give each its own text (placeholder styles differ)')

    for path in sorted((root / NATIVE / 'ui').glob('*.slint')):
        rel = path.relative_to(root).as_posix()
        text = blank_comments(path.read_text(encoding='utf-8'))
        for m in SLINT_ANY_TR.finditer(text):
            if not SLINT_TR.match(text, m.start()):
                problems.append(f'{rel}:{line_of(text, m.start())}: write @tr("{CONTEXT}" => "...")')
        for m in SLINT_TR.finditer(text):
            where = f'{rel}:{line_of(text, m.start())}'
            try:
                if unescape(m.group(1)) != CONTEXT:
                    problems.append(f'{where}: the @tr context must be "{CONTEXT}"')
                one = unescape(m.group(2))
                other = unescape(m.group(3)) if m.group(3) else None
                for t in [one] + ([other] if other else []):
                    bad = [p for p in placeholders(t) if not NUMBERED.match(p) or (p == 'n' and other is None)]
                    if bad:
                        problems.append(f'{where}: Slint text uses numbered placeholders ({{0}}, {{1}}; {{n}} in a plural); found {bad}')
                add(one, 'slint', other is not None, other, where)
            except Problem as e:
                problems.append(f'{where}: {e}')

    for path in sorted((root / NATIVE / 'src').rglob('*.rs')):
        if path.name.endswith('_tests.rs'):
            continue
        rel = path.relative_to(root).as_posix()
        text = strip_test_modules(blank_comments(path.read_text(encoding='utf-8')))
        try:
            found = list(rust_marked(text))
        except Problem as e:
            problems.append(f'{rel}: {e}')
            continue
        for kind, index, one, other in found:
            where = f'{rel}:{line_of(text, index)}'
            if kind is None:
                problems.append(f'{where}: {one}')
                continue
            try:
                for t in [one] + ([other] if other else []):
                    bad = [p for p in placeholders(t) if not NAMED.match(p)]
                    if bad:
                        problems.append(f'{where}: Rust text uses named placeholders such as {{photos}}, without format specs; found {bad}')
                    if kind == 'plain' and placeholders(t):
                        problems.append(f'{where}: a message with placeholders needs tr_format! or tr_plural!')
                add(one, 'rust', kind == 'plural', other, where)
            except Problem as e:
                problems.append(f'{where}: {e}')

    # A plural's English singular may leave out only the count (`n`), never another value.
    for key, msg in messages.items():
        if msg['plural']:
            try:
                one, other = set(placeholders(key)), set(placeholders(msg['other']))
            except Problem:
                continue
            if not (other - {'n'} <= one <= other):
                problems.append(f'{msg["where"][0]}: the singular {key!r} must keep every placeholder of the plural except {{n}}')
    return messages, problems


def read_list(root):
    path = root / NATIVE / 'translations' / 'languages.json'
    problems, langs = [], []
    try:
        doc = json.loads(path.read_text(encoding='utf-8'))
        entries = doc['languages']
        assert isinstance(entries, list)
    except Exception as e:  # noqa: BLE001 - report any malformed list plainly
        return [], [f'{path.relative_to(root).as_posix()}: needs {{"languages": [...]}} ({e})']
    seen = set()
    for e in entries:
        code = e.get('code') if isinstance(e, dict) else None
        if not isinstance(code, str) or not re.fullmatch(r'[A-Za-z0-9-]+', code):
            problems.append(f'languages.json: entry {e!r} needs a code of letters, digits and hyphens')
            continue
        for field, kind in (('name', str), ('match', list), ('plural', str)):
            if not isinstance(e.get(field), kind):
                problems.append(f'languages.json: {code} needs "{field}"')
        if e.get('plural') not in PLURAL_RULES:
            problems.append(f'languages.json: {code} names an unknown plural rule {e.get("plural")!r}; known: {sorted(PLURAL_RULES)}')
        if code.lower() == 'en' or code == PSEUDO or code.lower() in seen:
            problems.append(f'languages.json: {code} is English, reserved for tests, or listed twice')
        seen.add(code.lower())
        langs.append((code, e.get('plural')))
    listed = {c for c, _ in langs}
    for pack in sorted((root / NATIVE / 'translations').glob('*.json')):
        if pack.name != 'languages.json' and pack.stem not in listed:
            problems.append(f'translations/{pack.name}: a pack with no entry in languages.json')
    for code in sorted(listed):
        if not (root / NATIVE / 'translations' / f'{code}.json').is_file():
            problems.append(f'languages.json: {code} has no translations/{code}.json')
    return langs, problems


def check_pack(path, rule, messages, label):
    problems = []
    try:
        pack = json.loads(path.read_text(encoding='utf-8'))
        assert isinstance(pack, dict)
    except Exception as e:  # noqa: BLE001
        return [f'{label}: not a JSON object of messages ({e})']
    categories = PLURAL_RULES.get(rule, [])
    for key in sorted(messages.keys() - pack.keys()):
        problems.append(f'{label}: missing {key!r}')
    for key in sorted(pack.keys() - messages.keys()):
        problems.append(f'{label}: stale {key!r} (no longer marked in the source)')
    for key in sorted(pack.keys() & messages.keys()):
        msg, value = messages[key], pack[key]
        english = set(placeholders(msg['other'] if msg['plural'] else key))
        if msg['plural']:
            if not isinstance(value, dict) or sorted(value) != sorted(categories):
                problems.append(f'{label}: {key!r} is counted; give exactly the forms {categories}')
                continue
            forms = value
        else:
            if not isinstance(value, str):
                problems.append(f'{label}: {key!r} is plain text, not plural forms')
                continue
            forms = {'text': value}
        for name, text in forms.items():
            if not isinstance(text, str) or not text.strip():
                problems.append(f'{label}: {key!r} ({name}) is empty')
                continue
            try:
                found = set(placeholders(text))
            except Problem as e:
                problems.append(f'{label}: {key!r} ({name}): {e}')
                continue
            if msg['plural'] and name != 'other':
                # A singular-like form may leave out the count, and nothing else (review R4).
                if not found <= english:
                    problems.append(f'{label}: {key!r} ({name}) has placeholders {sorted(found - english)} the English lacks')
                if not english - {'n'} <= found:
                    problems.append(f'{label}: {key!r} ({name}) drops {sorted(english - {"n"} - found)}; only {{n}} may be left out')
            elif found != english:
                problems.append(f'{label}: {key!r} ({name}) has placeholders {sorted(found)}; the English has {sorted(english)}')
    return problems


def pseudo_pack(messages):
    """Every message wrapped in ⟦…⟧, so untranslated text and clipping stand out."""
    pack = {}
    for key, msg in sorted(messages.items()):
        if msg['plural']:
            pack[key] = {'one': f'⟦{key}⟧', 'other': f'⟦{msg["other"]}⟧'}
        else:
            pack[key] = f'⟦{key}⟧'
    return json.dumps(pack, ensure_ascii=False, indent=2) + '\n'


def check(root, write=False):
    messages, problems = collect(root)
    langs, list_problems = read_list(root)
    problems += list_problems
    pseudo = root / NATIVE / 'translations' / 'test' / f'{PSEUDO}.json'
    if write and not problems:
        pseudo.parent.mkdir(parents=True, exist_ok=True)
        pseudo.write_bytes(pseudo_pack(messages).encode('utf-8'))
    for code, rule in langs:
        path = root / NATIVE / 'translations' / f'{code}.json'
        if path.is_file():
            problems += check_pack(path, rule, messages, f'translations/{code}.json')
    # Compared as text: a Windows checkout gives the committed LF file CRLF endings (review R1).
    if not pseudo.is_file() or pseudo.read_text(encoding='utf-8') != pseudo_pack(messages):
        problems.append(f'translations/test/{PSEUDO}.json is out of date: run python scripts/check-translations.py --write')
    return messages, langs, problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--write', action='store_true', help=f'regenerate translations/test/{PSEUDO}.json')
    parser.add_argument('--root', type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    try:
        sys.stdout.reconfigure(errors='backslashreplace')
    except AttributeError:
        pass
    messages, langs, problems = check(args.root, args.write)
    for p in problems:
        print(p)
    names = ', '.join(c for c, _ in langs) or 'none yet'
    print(f'{len(messages)} marked messages; language packs: {names}; {len(problems)} problem(s).')
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
