"""Generate readable release notices from cargo-about 0.9.2 plus pinned supplements.

Run from any directory. cargo-about is invoked inside falcon/ with the locked native
manifest. Its raw JSON stays temporary because it contains local build paths.
"""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import re
import tomllib
from release_materials import SUPPLEMENTS, FONTS, notice_sources, embedded_fonts

ROOT = Path(__file__).resolve().parents[1]


def sha(data):
    return hashlib.sha256(data).hexdigest()


PLACEHOLDER = re.compile(
    r'(?:[<\[{]|&lt;)\s*(?:years?|yyyy|authors?|owners?|copyright holders?|fullnames?|name(?: of copyright owner)?)\s*(?:[>\]}]|&gt;)'
    r'|\bYEAR,?\s+(?:Author\s+)?(?:NAME|OWNER|AUTHOR)\b|\bYYYY\s+Author\s+Name\b|\b20XX\s+Your\s+Name\b|\$(?:YEAR|OWNER)\b'
    r'|\b(?:Your|Author|Full)\s+Name\b',
    re.I,
)
PERMISSIVE = {'MIT', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib'}


DATE = r'\[?\d{4}(?:\s*[-–]+\s*\d{2,4})?\]?'


def identity_shape(value, dated=False):
    """Require a complete identity-shaped declaration, not a capitalised sentence.

    This is a conservative syntax guard, not proof of ownership. Exact upstream text and
    reviewed provenance remain the authority; unfamiliar spellings can be reviewed explicitly.
    """
    value = re.sub(r'\s*<[^<>\s]+@[^<>\s]+>\s*', ' ', value).strip(' [].,')
    value = re.sub(r'^(?:the |by )', '', value, flags=re.I)
    words = value.split()
    if not words:
        return False
    if (
        words[0].casefold() in {'and', 'or', 'of', 'for'}
        or value.casefold() == 'all rights reserved'
    ):
        return False
    # These are permission/disclaimer nouns, never an identity on their own. Reject before
    # collective-name handling so "Holders and Contributors" cannot pass that branch.
    if any(
        word.strip('.,').casefold()
        in {
            'holder',
            'holders',
            'liable',
            'be',
            'notice',
            'notices',
            'license',
            'licence',
            'law',
            'laws',
            'act',
            'permission',
            'owner',
            'owners',
            'licensor',
            'licensors',
        }
        for word in words
    ):
        return False
    if len(words) == 1 and words[0].casefold() in {'owner', 'author', 'name', 'licensor'}:
        return False
    if re.fullmatch(r'[\w.+-]+', value) and (dated or value == 'dtolnay'):
        return True
    if words[-1].casefold() in {'authors', 'developers', 'contributors'}:
        return len(words) > 1 and all(
            re.fullmatch(r'[\w.,\'’"\-]+', word) and any(c.isalpha() for c in word)
            for word in words[:-1]
        )
    if re.fullmatch(
        r'(?:Developers|Authors|Contributors) of (?:the )?[\w.-]+ (?:project|library)', value
    ):
        return True
    # Proper names, initials, organisations and quoted nicknames; no arbitrary trailing clause.
    return len(words) >= 2 and all(
        (
            re.fullmatch(r'[\w.\'’®,-]+', word)
            and (word[0].isupper() or (word[0].isdigit() and any(c.isalpha() for c in word)))
        )
        or word in {'of', 'and', 'the', 'for', 'within', 'van', 'de', 'der', 'da'}
        or re.fullmatch(r'[a-z]{1,2}[\'’][A-Z][\w.\'’-]*', word)
        or re.fullmatch(r'\([\w.-]+\)', word)
        or re.fullmatch(r'"[\w-]+"', word)
        for word in words
    )


def has_holder(text):
    for line in text.splitlines():
        line = re.sub(r'^\s*(?://+|/\*+|\*+)?\s*', '', line)
        match = re.search(
            r'(?i)(SPDX-FileCopyrightText|\bcopyright\b|©|\(c\))\s*:?[ \t]*((?:(?:copyright)?\s*(?:\(c\)|©))?)\s*(.+)',
            line,
        )
        if not match or PLACEHOLDER.search(line):
            continue
        value = match[3].strip()
        date = re.match(DATE + r'(?:\s*,\s*' + DATE + r')*[,\s]*', value)
        # Inline declarations exist in upstream fork acknowledgements, but ordinary prose
        # such as "AUTHORS OR COPYRIGHT HOLDERS BE LIABLE" is not a declaration.
        if match.start() and not (date or match[2] or match[1] in {'©', '(c)', '(C)'}):
            continue
        if match[1].lower() == '(c)' and not date:
            continue
        dated = bool(date)
        if date:
            value = value[date.end() :]
        # Explicit copyright-symbol declarations also occur with dates after the identity.
        elif match[2]:
            value, count = re.subn(r'\s+' + DATE + r'\s*$', '', value)
            dated = bool(count)
        value = re.split(r'\.?\s+(?:All rights reserved|See the COPYRIGHT)\b', value, flags=re.I)[0]
        if identity_shape(value, dated=dated):
            return True
    return False


def complete_bsd(text):
    holder = has_holder(text)
    text = ' '.join(text.lower().split())
    return holder and all(
        part in text
        for part in (
            'redistribution and use',
            'redistributions of source code',
            'redistributions in binary form',
            'neither',
            'as is',
        )
    )


def check_assembled(text):
    # Allow only the exact example holder lines inside bounded standard appendix suffixes.
    # A later MIT block must not inherit an earlier Apache appendix exemption.
    for block in text.split('\n' + '=' * 78 + '\n'):
        allowed = []
        # The complete original FTL contains its own <year> instructions. Only its exact
        # reviewed bytes may contain that example; an FTL heading alone grants no exception.
        for start in re.finditer(
            re.escape('                    The FreeType Project LICENSE\n'), block
        ):
            terminator = '--- end of FTL.TXT ---\n'
            end = block.find(terminator, start.start())
            if end >= 0:
                end += len(terminator)
                if (
                    sha(block[start.start() : end].encode('utf-8'))
                    == '08c135755dd589039470f1fdbb400daaabaaa50d0b366d19cebff4d22986baa1'
                ):
                    allowed.append((start.start(), end))
        # The W3C licence requires retaining its own change-notice example. Exempt only that
        # exact template inside the original bounded licence instructions, not other placeholders.
        for example in re.finditer(
            re.escape('Copyright © [YEAR] W3C® (MIT, ERCIM, Keio, Beihang).'), block
        ):
            start = block.rfind('BEGINNING OF W3C LICENSE', 0, example.start())
            end = block.find('END OF W3C LICENSE', start) if start >= 0 else -1
            if (
                start >= 0
                and end >= example.end()
                and 'Notice of any changes or modifications' in block[start : example.start()]
            ):
                allowed.append(example.span())
        for line in re.finditer(r'(?m)^[ \t]*Copyright[^\n]*$', block):
            prefix = block[: line.start()]
            value = line[0].strip()
            apache_start = prefix.rfind('APPENDIX: How to apply')
            apache_end = (
                block.find('limitations under the License.', apache_start)
                if apache_start >= 0
                else -1
            )
            apache = (
                re.fullmatch(
                    r'Copyright\s+[\[{]yyyy[\]}]\s+[\[{]name of copyright owner[\]}]', value
                )
                and apache_start >= 0
                and apache_end >= line.end()
                and 'Apache License' in prefix[:apache_start]
                and 'END OF TERMS AND CONDITIONS' in prefix[:apache_start]
            )
            lgpl_start = prefix.rfind('How to Apply These Terms')
            lgpl_end = (
                block.find('This library is free software', lgpl_start) if lgpl_start >= 0 else -1
            )
            lgpl = (
                re.fullmatch(
                    r'Copyright \(C\)\s+(?:\{year\}\s+\{fullname\}|year\s+name of author)', value
                )
                and lgpl_start >= 0
                and lgpl_end >= line.end()
                and 'GNU LESSER GENERAL PUBLIC LICENSE' in prefix[:lgpl_start]
                and 'END OF TERMS AND CONDITIONS' in prefix[:lgpl_start]
            )
            if apache or lgpl:
                allowed.append(line.span())
        for match in PLACEHOLDER.finditer(block):
            if not any(start <= match.start() and match.end() <= end for start, end in allowed):
                raise ValueError('Unresolved holder template in assembled notices')


def checked_licence_text(licence, addenda):
    """A canonical permission text alone is not a package's copyright notice."""
    body = licence['text']
    if licence['id'] not in PERMISSIVE:
        check_assembled(body)
        return body
    if licence['id'] in PERMISSIVE:
        for use in licence['used_by']:
            package = use['crate']
            key = package['name'] + ' ' + package['version']
            entry = addenda.get(key, {})
            files = [
                (ROOT / item['file']).read_text(encoding='utf-8') for item in entry.get('files', [])
            ]
            if not (
                has_holder(body)
                or any(has_holder(text) for text in files)
                or entry.get('exception')
            ):
                raise ValueError('Missing reviewed copyright holder/provenance for ' + key)
            if licence['id'] == 'BSD-3-Clause' and not any(complete_bsd(text) for text in files):
                raise ValueError('BSD-3 requires a pinned complete licence file for ' + key)
    if not PLACEHOLDER.search(body):
        return body
    for use in licence['used_by']:
        package = use['crate']
        key = package['name'] + ' ' + package['version']
        if key not in addenda or not (addenda[key].get('files') or addenda[key].get('exception')):
            raise ValueError('Unresolved placeholder copyright for ' + key)
    if licence['id'] == 'MIT':
        start = body.find('Permission is hereby granted')
        if start < 0:
            raise ValueError('Unexpected MIT template')
        body = (
            'Copyright notices and source provenance: see the package addenda below.\n\n'
            + body[start:]
        )
        if PLACEHOLDER.search(body):
            raise ValueError('Unresolved licence template')
        return body
    # The reviewed full BSD file supplies both actual holders and permission conditions.
    if licence['id'] == 'BSD-3-Clause':
        return 'Complete copyright and BSD conditions: see the package addenda below.'
    raise ValueError('Unreviewed placeholder licence: ' + licence['id'])


def check_coverage(expected, reported):
    if not expected <= reported:
        raise ValueError(
            'Target dependency packages missing from licence report: '
            + str(sorted(expected - reported))
        )


def generate(tool, check=False):
    version = subprocess.check_output([str(tool), '--version'], text=True).strip()
    if version != 'cargo-about 0.9.2':
        raise ValueError('Expected cargo-about 0.9.2, got ' + version)
    addenda, source_inputs = notice_sources(ROOT)
    with tempfile.TemporaryDirectory(prefix='falcon-licences-') as scratch:
        report = Path(scratch) / 'licences.json'
        subprocess.run(
            [
                str(tool),
                'generate',
                '--locked',
                '--fail',
                '--format',
                'json',
                '--manifest-path',
                'native/Cargo.toml',
                '--config',
                'about.toml',
                '--output-file',
                str(report),
            ],
            cwd=ROOT / 'falcon',
            check=True,
        )
        data = json.loads(report.read_text(encoding='utf-8-sig'))
    packages = {(r['package']['name'], r['package']['version']): r for r in data['crates']}
    locked = {
        (p['name'], p['version'])
        for p in tomllib.loads((ROOT / 'falcon/Cargo.lock').read_text(encoding='utf-8'))['package']
    }
    if not set(packages) <= locked:
        raise ValueError('Licence report does not match current locked package versions')
    expected = set()
    for target in ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin']:
        tree = subprocess.check_output(
            [
                'cargo',
                'tree',
                '--locked',
                '-p',
                'falcon-native',
                '--target',
                target,
                '--edges',
                'normal,build',
                '--prefix',
                'none',
                '--format',
                '{p}',
            ],
            cwd=ROOT / 'falcon',
            text=True,
            encoding='utf-8',
        )
        expected.update(re.findall(r'^([^\s]+) v([^\s]+)', tree, re.M))
    check_coverage(expected, set(packages))
    if not {tuple(k.rsplit(' ', 1)) for k in addenda} <= set(packages):
        raise ValueError('Copyright addenda must be reviewed when package versions change')
    for key, entry in addenda.items():
        provenance = entry.get('provenance', {})
        if 'authors' in provenance:
            reported = packages[tuple(key.rsplit(' ', 1))]['package'].get('authors', [])
            if provenance['authors'] != reported:
                raise ValueError('Recorded provenance authors differ from cargo-about for ' + key)
    covered = set()
    chunks = [
        'Falcon Photo Viewer — third-party notices\n',
        'Generated for the locked Windows x86_64 and macOS arm64 native dependency graph,\n'
        'including build dependencies. Some components are used only on one platform.\n'
        'Dependencies retain their own terms; Falcon’s Apache-2.0 licence does not replace them.\n',
        'JPEG acknowledgement: this software is based in part on the work of the Independent JPEG Group.\n',
        'RAW development uses rawler under LGPL-2.1. Its corresponding source and the application\n'
        'rebuild instructions accompany the release; see REBUILDING.md.\n',
    ]
    selected = {}
    for licence in data['licenses']:
        used = sorted({(x['crate']['name'], x['crate']['version']) for x in licence['used_by']})
        covered.update(used)
        for item in used:
            selected.setdefault(item, set()).add(licence['id'])
        lead = (
            'Classifier-supplied IJG terms follow. The jpeg-encoder source-header supplements below\n'
            'identify the portions used by that crate and retain their original notices.\n\n'
            if licence['id'] == 'IJG' and any(n == 'jpeg-encoder' for n, _ in used)
            else ''
        )
        chunks.append(
            '\n'
            + '=' * 78
            + '\n'
            + licence['name']
            + '\nUsed by: '
            + ', '.join(f'{n} {v}' for n, v in used)
            + '\n\n'
            + lead
            + checked_licence_text(licence, addenda)
            + '\n'
        )
    texts = {}
    for key, entry in sorted(addenda.items()):
        if entry.get('exception'):
            chunks.append(
                '\n'
                + '=' * 78
                + '\nReviewed upstream provenance: '
                + key
                + '\n'
                + entry['exception']
                + '\n'
            )
        for item in entry['files']:
            texts.setdefault(item['file'], []).append(key + ' / ' + item['origin'])
    for path, owners in sorted(texts.items()):
        chunks.append(
            '\n'
            + '=' * 78
            + '\nPreserved upstream copyright/licence material\nUsed by: '
            + ', '.join(owners)
            + '\n\n'
            + (ROOT / path).read_text(encoding='utf-8')
            + '\n'
        )
    # cargo-about accepts custom SPDX LicenseRef expressions but does not emit their
    # text. Supply the actual pinned Slint terms explicitly; never replace them with GPL.
    slint_text = None
    for key, row in packages.items():
        if row['license'] != 'LicenseRef-Slint-Royalty-free-2.0':
            continue
        source = (
            Path(row['package']['manifest_path']).parent
            / 'LICENSES/LicenseRef-Slint-Royalty-free-2.0.md'
        )
        raw = source.read_bytes()
        if sha(raw) != '5167f5056e850419106ab6265efbdca7cba4d99c849d1445ca0bbf6a1e2315fe':
            raise ValueError('Slint terms changed; review before generating notices')
        slint_text = raw.decode('utf-8')
        covered.add(key)
        selected.setdefault(key, set()).add(row['license'])
    if slint_text is None:
        raise ValueError('Slint licence missing from the native graph')
    chunks.append('\n' + '=' * 78 + '\nSlint Royalty-Free 2.0\n\n' + slint_text + '\n')
    if set(packages) != covered:
        raise ValueError('Packages without licence text: ' + str(sorted(set(packages) - covered)))
    # Code-header and ancillary notices are not reliably found by licence classifiers.
    supplements = [
        ('jpeg-encoder', 'src/fdct.rs', True),
        ('jpeg-encoder', 'src/avx2/fdct.rs', True),
        ('cfg_aliases', 'NOTICES.md', False),
        ('uds_windows', 'THIRDPARTYNOTICES', False),
        ('i-slint-common', 'sharedfontique/Inter-VariableFont.ttf.license', False),
    ]
    for name, relative, header_only in supplements:
        matches = [(key, row) for key, row in packages.items() if key[0] == name]
        if not matches:
            continue  # This target union does not ship the component.
        for key, row in matches:
            text = (Path(row['package']['manifest_path']).parent / relative).read_text(
                encoding='utf-8'
            )
            if header_only:
                if not text.startswith('/*') or '*/' not in text:
                    raise ValueError(
                        'Expected upstream copyright/licence header: ' + name + '/' + relative
                    )
                text = text.split('*/', 1)[0] + '*/'
            chunks.append(
                '\n'
                + '=' * 78
                + f'\nAdditional upstream notice: {key[0]} {key[1]} / {relative}\n\n'
                + text
                + '\n'
            )
    for relative in SUPPLEMENTS:
        chunks.append(
            '\n'
            + '=' * 78
            + '\n'
            + relative
            + '\n\n'
            + (ROOT / relative).read_text(encoding='utf-8')
            + '\n'
        )
    chunks.append(
        '\nRust standard-library notices are supplied in docs/licenses/rust-standard-library.html.\n'
        'Windows builds use the Microsoft runtime supplied by the native MSVC toolchain.\n'
        'No CUDA/nvJPEG or HEVC decoder DLL is bundled; optional acceleration uses installed runtimes.\n'
    )
    text = '\n'.join(chunks).replace('\r\n', '\n').replace('\r', '\n')
    check_assembled(text)
    if str(ROOT) in text or re.search(r'(?i)[a-z]:[\\/]+Users[\\/]+', text):
        raise ValueError('Local path in generated notices')
    workspace = tomllib.loads((ROOT / 'falcon/Cargo.toml').read_text(encoding='utf-8'))
    project_inputs = [
        'falcon/Cargo.toml',
        'falcon/about.toml',
        'falcon/vendor/winit/Cargo.toml',
        'falcon/vendor/zune-jpeg/Cargo.toml',
    ]
    project_inputs += [
        'falcon/' + member + '/Cargo.toml' for member in workspace['workspace']['members']
    ]
    inventory = {
        'schema': 1,
        'tool': 'cargo-about 0.9.2',
        'lock_sha256': sha(
            (ROOT / 'falcon/Cargo.lock').read_text(encoding='utf-8-sig').encode('utf-8')
        ),
        'text_hash_format': 'UTF-8, LF line endings',
        'project_inputs': {
            p: sha((ROOT / p).read_text(encoding='utf-8-sig').encode('utf-8'))
            for p in sorted(project_inputs)
        },
        'notice_source_inputs': source_inputs,
        'supplementary_inputs': {
            p: sha((ROOT / p).read_text(encoding='utf-8-sig').encode('utf-8')) for p in SUPPLEMENTS
        },
        'font_inputs': {p: sha((ROOT / p).read_bytes()) for p in embedded_fonts(ROOT)},
        'runtime_notice_sha256': sha(
            (ROOT / 'docs/licenses/rust-standard-library.html')
            .read_text(encoding='utf-8-sig')
            .encode('utf-8')
        ),
        'targets': ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin'],
        'packages': [
            {
                'name': n,
                'version': v,
                'declared': packages[(n, v)]['package']['license'],
                'selected': sorted(selected[(n, v)]),
            }
            for n, v in sorted(packages)
        ],
        'notices_sha256': sha(text.encode('utf-8')),
    }
    serialized = json.dumps(inventory, indent=2) + '\n'
    target = ROOT / 'docs/licenses/dependency-inventory.json'
    if check:
        if (ROOT / 'THIRD-PARTY-NOTICES.txt').read_text(
            encoding='utf-8'
        ) != text or target.read_text(encoding='utf-8') != serialized:
            raise ValueError('Freshly regenerated release notices/inventory differ')
        print('Fresh regeneration matches release notices and inventory')
        return
    (ROOT / 'THIRD-PARTY-NOTICES.txt').write_text(text, encoding='utf-8', newline='\n')
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(serialized, encoding='utf-8', newline='\n')
    print(f'{len(packages)} packages covered; {len(text.encode("utf-8"))} notice bytes')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cargo-about', type=Path, default=Path('cargo-about'))
    parser.add_argument(
        '--check',
        action='store_true',
        help='Regenerate in memory and compare without changing files',
    )
    args = parser.parse_args()
    generate(args.cargo_about, args.check)
