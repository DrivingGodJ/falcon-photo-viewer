"""Fail on unresolved copyright templates and incomplete target licence coverage."""

import importlib.util
from pathlib import Path
from contextlib import contextmanager
import hashlib
import json
import os
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'generate', Path(__file__).with_name('generate-notices.py')
)
generate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generate)


class NoticeGeneration(unittest.TestCase):
    def test_missing_target_package_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'Target dependency packages missing'):
            generate.check_coverage({('present', '1'), ('missing', '1')}, {('present', '1')})

    def test_tool_version_is_checked_before_report_generation(self):
        with patch.object(generate.subprocess, 'check_output', return_value='cargo-about 0.8.0\n'):
            with self.assertRaisesRegex(ValueError, 'Expected cargo-about 0.9.2'):
                generate.generate('synthetic-tool')


class EndToEndNotices(unittest.TestCase):
    def test_reviewed_port_credits_belong_to_the_using_package(self):
        # Falsifier: omit one package's source credit or upstream terms, or attach the
        # rustybuzz credit to skrifa despite no corresponding port in that package.
        packages = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )['packages']
        cases = [
            ('lyon_geom 1.0.19', 'src/cubic_bezier_intersections.rs', 'paper.js', 'paperjs/paper.js/'),
            ('kurbo 0.13.1', 'src/moments.rs', 'momentsPen', 'fonttools/fonttools/'),
            ('resvg 0.47.0', 'src/filter/box_blur.rs', 'fastblur', 'fastblur/'),
            ('simd-adler32 0.3.9', 'src/imp/neon.rs', 'Chromium', 'chromium'),
        ]
        for package, source, credit, upstream in cases:
            with self.subTest(package=package):
                files = packages.get(package, {}).get('files', [])
                headers = [x for x in files if x['origin'] == source]
                self.assertTrue(headers, 'Missing source credit')
                self.assertTrue(any(
                    credit in (generate.ROOT / x['file']).read_text(encoding='utf-8')
                    for x in headers
                ))
                terms = [x for x in files if upstream in x['origin']]
                self.assertTrue(any(
                    any(phrase in (generate.ROOT / x['file']).read_text(encoding='utf-8')
                        for phrase in ('Permission is hereby granted', 'Redistribution and use'))
                    for x in terms
                ), 'Missing upstream terms for this package')
        self.assertFalse(any('rustybuzz/' in x['origin']
                             for x in packages['skrifa 0.42.1']['files']))

    def test_close_port_terms_are_attached_to_each_using_package(self):
        # Falsifier: drop a package's upstream terms/credit even though another package may
        # still carry the same text. Check the actual shipped resources and their full pins.
        manifest = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )['packages']
        cases = [
            (
                'roxmltree 0.21.1',
                'c6a4ae2a0565d17ac7fccae808275596fc5959ffdf507107d001bc4669189e45',
            ),
            ('skrifa 0.42.1', '08c135755dd589039470f1fdbb400daaabaaa50d0b366d19cebff4d22986baa1'),
            (
                'read-fonts 0.39.2',
                '08c135755dd589039470f1fdbb400daaabaaa50d0b366d19cebff4d22986baa1',
            ),
            ('skrifa 0.42.1', 'e2c35c98dc5af86890b92d025343a05f00cc5a08d746665c92d13803691e94ee'),
            (
                'read-fonts 0.39.2',
                'cdb6b2a4e60de61211f8759676ea2251ed3a42d0d3d7b4e83eaccf4af1c15472',
            ),
            (
                'jxl-color 0.11.0',
                '8405932022a556380c2d8c272eff154a923feb197233f348ce5f7334fb0a5ede',
            ),
            (
                'jxl-coding 1.0.1',
                '8405932022a556380c2d8c272eff154a923feb197233f348ce5f7334fb0a5ede',
            ),
            ('jxl-jbr 0.2.1', '8405932022a556380c2d8c272eff154a923feb197233f348ce5f7334fb0a5ede'),
            (
                'jxl-bitstream 1.1.0',
                '8405932022a556380c2d8c272eff154a923feb197233f348ce5f7334fb0a5ede',
            ),
            (
                'fast_image_resize 5.5.0',
                '5bb11d96b393a698df70018069a986248021f286344c437a13f299c3daf1dfd4',
            ),
            (
                'image-webp 0.2.4',
                '5aec868f669e384a22372a4e8a1a6cd7d44c64cd451f960ca69cc170d1e13acf',
            ),
            ('muda 0.19.3', 'e7c7dec4f30e7c6b2093faf66e8ac74dc4194e65af48dee84461aebdeea60348'),
            ('muda 0.19.3', 'b9981623711f17a4d09b73002167393424e52f0566581901db41a39f30b3df06'),
            ('kurbo 0.13.1', 'f5e7195f466771aec6ca3ec5f9bd84521f5ef7a3fe9b03b7e6a2dc53f9360eaa'),
            ('zmij 1.0.21', 'a69b36adb8d116376bcd580772fa45f08fffecc17833cb9822532f4e4f5ec372'),
            (
                'jpeg-encoder 0.6.1',
                'f4a85f8677ca4e41b185c0a9f7c31a30ddb9ce43c561483ee92ef12db37c2a10',
            ),
        ]
        for package, digest in cases:
            with self.subTest(package=package, digest=digest):
                item = next((x for x in manifest[package]['files'] if x['sha256'] == digest), None)
                self.assertIsNotNone(item)
                self.assertEqual(
                    hashlib.sha256(
                        (generate.ROOT / item['file']).read_text(encoding='utf-8').encode()
                    ).hexdigest(),
                    digest,
                )
        for package in ('skrifa 0.42.1', 'read-fonts 0.39.2'):
            with self.subTest(acknowledgement=package):
                self.assertTrue(
                    any(
                        'This software is based in part on the work of the FreeType Team.'
                        in (generate.ROOT / x['file']).read_text(encoding='utf-8')
                        for x in manifest[package]['files']
                    )
                )

    def test_original_freetype_terms_keep_their_instruction_example(self):
        # Falsifier: omit the exact-byte FTL exception, exempt arbitrary FTL-labelled spans,
        # or allow a placeholder outside the complete pinned original terms.
        text = (generate.ROOT / 'docs/licenses/upstream/08c135755dd589039470.txt').read_text(
            encoding='utf-8'
        )
        generate.check_assembled(text)
        for index, invalid in enumerate(
            [
                text + '\nCopyright <year> <owner>\n',
                'Copyright <author>\n' + text,
                text.replace('--- end of FTL.TXT ---', 'Copyright <owner>\n--- end of FTL.TXT ---'),
                text.replace('--- end of FTL.TXT ---', ''),
                text.replace('The FreeType Project LICENSE', 'A different project LICENSE'),
            ]
        ):
            with self.subTest(case=index), self.assertRaisesRegex(ValueError, 'assembled notices'):
                generate.check_assembled(invalid)

    def test_inline_permission_prose_is_not_a_holder(self):
        # Falsifier: remove the inline-declaration rule or allow short prefixes before a
        # copyright noun. These original OFL/CC0 endings have no declaration date or symbol.
        for text in [
            'OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE',
            'remaining Copyright and Related',
            'a Copyright Fine Art',
            '(Copyright Fixture Contributors',
            'OF Copyright Fixture Contributors',
        ]:
            with self.subTest(text=text):
                self.assertFalse(generate.has_holder(text))

    def test_each_legal_noun_and_check_order_are_guarded(self):
        # Falsifiers: remove any named noun, stop stripping punctuation, or run the dated
        # single-token shortcut before this refusal. Other identity rules accept these forms.
        for word in (
            'Holder',
            'Holders',
            'Liable',
            'Be',
            'Notice',
            'Notices',
            'License',
            'Licence',
            'Law',
            'Laws',
            'Act',
            'Permission',
            'Owner',
            'Owners',
            'Licensor',
            'Licensors',
        ):
            for value in (word + ' Fixture', word + ', Fixture', word):
                with self.subTest(word=word, value=value):
                    self.assertFalse(generate.has_holder('Copyright 2026 ' + value))

    def test_generic_holder_shapes_and_named_templates_are_refused(self):
        # Falsifier: accept a leading connector, a whole rights-reservation clause, or any
        # listed name/year template as a real owner. Exercise final generation as well.
        cases = [
            'Copyright and Licensing',
            'COPYRIGHT AND DISCLAIMER',
            'Copyright 2020 All Rights Reserved',
            'COPYRIGHT OWNER OR CONTRIBUTORS',
            'Copyright Licensors and Contributors',
            'Copyright or Contributors',
            'Copyright of Contributors',
            'Copyright for Contributors',
        ]
        for line in cases:
            with self.subTest(line=line), self.fixture(body=line) as root:
                self.assertFalse(generate.has_holder(line))
                with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                    generate.generate('fixture-tool')
        for token in (
            'Your Name',
            'Author Name',
            'Full Name',
            'YEAR, NAME',
            'YEAR, OWNER',
            'YEAR, AUTHOR',
            'YEAR NAME',
            'YEAR OWNER',
            'YEAR AUTHOR',
        ):
            line = 'Copyright (c) 2026 ' + token
            with self.subTest(token=token), self.fixture(addendum=line):
                self.assertFalse(generate.has_holder(line))
                with self.assertRaisesRegex(ValueError, 'assembled notices'):
                    generate.generate('fixture-tool')

    def test_w3c_and_apache_boundaries_do_not_reach_across_blocks(self):
        # Falsifiers: choose the last W3C end; accept the change instruction after its
        # example; omit block splitting; use Apache match rather than fullmatch; accept
        # Apache title or END OF TERMS after the example rather than before the appendix.
        begin = 'BEGINNING OF W3C LICENSE\n'
        instruction = 'Notice of any changes or modifications\n'
        end = '\nEND OF W3C LICENSE\n'
        example = 'Copyright © [YEAR] W3C® (MIT, ERCIM, Keio, Beihang).'
        apache = 'Copyright [yyyy] [name of copyright owner]'
        appendix = 'APPENDIX: How to apply these terms\n'
        title = 'Apache License\n'
        terms = 'END OF TERMS AND CONDITIONS\n'
        tail = '\nlimitations under the License.\n'
        cases = [
            begin + instruction + end + example + end,
            begin + example + instruction + end,
            begin + instruction + '\n' + '=' * 78 + '\n' + example + end,
            title + terms + appendix + apache + ' EXTRA' + tail,
            terms + appendix + apache + tail + title,
            title + appendix + apache + tail + terms,
        ]
        for index, text in enumerate(cases):
            with self.subTest(boundary=index), self.assertRaisesRegex(
                ValueError, 'assembled notices'
            ):
                generate.check_assembled(text)

    def test_w3c_required_example_is_preserved_only_inside_its_terms(self):
        # Falsifier: allow every W3C-shaped placeholder or remove any original-terms boundary.
        source = (generate.ROOT / 'falcon/vendor/winit/src/keyboard.rs').read_text(encoding='utf-8')
        excerpt = '\n'.join(source.splitlines()[2:71]) + '\n'
        generate.check_assembled(excerpt)
        template = 'Copyright © [YEAR] W3C® (MIT, ERCIM, Keio, Beihang).'
        cases = [template, excerpt + '\n' + template]
        cases += [
            excerpt.replace(
                'END OF W3C LICENSE', 'Copyright [year] [fullname]\nEND OF W3C LICENSE'
            ),
            excerpt.replace(template, 'Copyright © [YEAR] Fixture Owner'),
        ]
        cases += [
            excerpt.replace(phrase, '')
            for phrase in (
                'BEGINNING OF W3C LICENSE',
                'END OF W3C LICENSE',
                'Notice of any changes or modifications',
            )
        ]
        for case, text in enumerate(cases):
            with self.subTest(case=case), self.assertRaisesRegex(ValueError, 'assembled notices'):
                generate.check_assembled(text)

    def test_missing_portion_headers_are_pinned_per_package(self):
        # Falsifier: omit any approved compiled-source header from its own crate's addenda.
        manifest = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )['packages']
        for key, origin, line in [
            (
                'tiny-skia-path 0.12.0',
                'src/stroker.rs',
                'Copyright 2008 The Android Open Source Project',
            ),
            (
                'tiny-skia-path 0.12.0',
                'src/path.rs',
                'Copyright 2006 The Android Open Source Project',
            ),
            ('memoffset 0.9.1', 'src/raw_field.rs', 'Ralf Jung'),
            ('unicode-bidi 0.3.18', 'src/utf16.rs', 'Copyright 2023 The Mozilla Foundation'),
            ('cursor-icon 1.2.0', 'src/lib.rs', 'Copyright © 2018 W3C'),
            ('winit 0.30.13', 'src/keyboard.rs', 'END OF W3C SHORT NOTICE'),
            ('harfrust 0.8.4', 'src/hb/unicode_emoji_table.rs', '© 2025 Unicode'),
            ('resvg 0.47.0', 'src/filter/iir_blur.rs', "Licensed under 'Simplified BSD License'."),
            ('roxmltree 0.21.1', 'src/lib.rs', 'License: ISC.'),
            (
                'derive_more 2.1.1',
                'src/as_dyn_error.rs',
                'The initial idea and implementation was taken',
            ),
            ('image 0.25.10', 'src/traits.rs', 'Note copied from the stdlib under MIT license'),
            ('utf8_iter 1.0.4', 'src/indices.rs', 'Rust standard library at revision'),
            ('pulldown-cmark 0.13.4', 'src/utils.rs', 'Its author authorized the use'),
        ]:
            with self.subTest(package=key, origin=origin):
                items = [item for item in manifest[key]['files'] if item['origin'] == origin]
                self.assertTrue(items)
                self.assertTrue(
                    any(
                        line in (generate.ROOT / item['file']).read_text(encoding='utf-8')
                        for item in items
                    )
                )

    def test_instruction_clauses_are_not_holders(self):
        # Falsifier: treat permission prose or plural generic copyright nouns as holders.
        for line in [
            'Copyright Holders may grant permission',
            'Copyright Owners retain rights',
            'Copyright Notices must be kept',
            'Copyright Laws apply',
            '(c) Upon termination of this License',
            '(c) The Licensor grants',
            '(c) Each Contributor hereby grants a licence',
            'Copyright Act of 1976 applies here',
            'Copyright Holders and Contributors',
            'Copyright and License',
            'AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY',
            'Copyright 2026 Owner',
            'Copyright 2026 Author',
            'Copyright 2026 Name',
            'Copyright 2026 Licensor',
            'COPYRIGHTS, TRADEMARKS OR OTHER RIGHTS.',
            '(c) Each Contributor',
            'Copyright Fixture 2020',
            'Copyright (c) fixture',
            'Copyright Contributors',
            'Copyright 123 Authors',
            'Copyright Fixture 123',
        ]:
            with self.subTest(line=line):
                self.assertFalse(generate.has_holder(line))
                with self.fixture(body='Permission without a holder.', addendum=line) as root:
                    with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                        generate.generate('fixture-tool')
                    self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())
        for line in ['Copyright (c) dtolnay', 'Copyright (c) the image-rs developers']:
            with self.subTest(line=line):
                self.assertTrue(generate.has_holder(line))

    def test_real_upstream_identity_shapes_remain_accepted(self):
        # Falsifier: replace identity-shaped parsing with capitalisation alone or require a
        # single rigid date/name order. These retained upstream declarations are not prose.
        for line in [
            'Copyright (c) 2014-2020 Optimal Computing (NZ) Ltd',
            "Copyright (c) 2016 Amanieu d'Antras",
            'Copyright (c) 2016-2019 Ulrik Sverdrup "bluss" and scopeguard developers',
            'Copyright (c) 2020 Élie ROUDNINSKI (marmeladema) <xademax@gmail.com>',
            'Copyright 2021 Developers of the femtovg project',
            'Copyright (c) 2016-2021 Diggory Blake, and other contributors.',
            'Copyright (c) 2018 Sam Rijs, Alex Crichton and contributors',
            'Copyright (c) 2022-2022 Tauri Programme within The Commons Conservancy',
            'This library was forked from upstream, which was copyright 2018 Visly Inc.',
            'Copyright (c) 2014 Mathijs van de Nes',
            'Copyright (c) 2015 Nicholas Allegra (comex).',
            'Copyright (c) HeroicKatora 2020',
            'Copyright 2018 W3C® Consortium',
            'Copyright (c) 2013, 2015, 2016, 2018 Fixture Authors',
            'Copyright 2018 Alex "nickname" Brown',
            'Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.',
            'Copyright 2020 Fixture Author. See the COPYRIGHT file.',
            'Copyright 2020 3Com Corporation',
        ]:
            with self.subTest(line=line):
                self.assertTrue(generate.has_holder(line))

    def test_complete_mit_and_bsd_without_the_holder_line_are_refused(self):
        # Falsifier: allow mid-sentence COPYRIGHT HOLDERS in an all-caps disclaimer to
        # stand in for a copyright declaration. Exercise the actual assembled generator.
        import re

        manifest = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )['packages']
        for key, licence in [('anyhow 1.0.103', 'MIT'), ('arrayvec 0.7.7', 'MIT')]:
            # A pinned complete MIT licence, not the abbreviated synthetic fixture above.
            if key not in manifest:
                continue
            item = next(
                x for x in manifest[key]['files'] if x['origin'] in ('LICENSE-MIT', 'LICENSE')
            )
            body = (generate.ROOT / item['file']).read_text(encoding='utf-8')
            if 'Permission is hereby granted' in body:
                break
        else:
            self.fail('Pinned complete MIT fixture not found')
        # The complete BSD-2 conditions are already retained in the assembled notices.
        notices = (generate.ROOT / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
        bsd = next(
            block.split('\n\n', 1)[1]
            for block in notices.split('\n' + '=' * 78 + '\n')
            if block.startswith('BSD 2-Clause') and 'Used by: arrayref 0.3.9\n' in block
        )
        for licence, original in [('MIT', body), ('BSD-2-Clause', bsd)]:
            stripped = '\n'.join(
                line
                for line in original.splitlines()
                if not re.match(r'^\s*Copyright\b', line, re.I)
            )
            with self.subTest(licence=licence), self.fixture(
                body=stripped, licence=licence
            ) as root:
                with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    def test_author_provenance_matches_report_with_utf8_names(self):
        # Falsifiers: read raw UTF-8 report/manifest bytes as cp1252, skip --check, compare
        # only the first author/as sets, or ignore an empty report list. Keep an accented
        # second author so the first-author-only shortcut cannot satisfy this test.
        names = ['Timothée Haudebourg <author@haudebourg.net>', 'Bastian Köcher <git@kchr.de>']
        for name in names:
            for checking in (False, True):
                for variation in ('valid', 'single', 'double', 'reordered', 'absent'):
                    expected = ['Fixture Author', name]
                    reported = expected.copy()

                    def edit(report):
                        report['crates'][0]['package']['authors'] = reported

                    with self.subTest(
                        name=name, checking=checking, variation=variation
                    ), self.fixture(exception='Reviewed provenance.', edit_report=edit) as root:
                        path = root / 'docs/licenses/notice-sources.json'
                        data = json.loads(path.read_text(encoding='utf-8'))
                        provenance = {'authors': expected.copy()}
                        data['packages']['fixture-lib 1.0.0']['provenance'] = provenance

                        def save():
                            path.write_text(json.dumps(data, ensure_ascii=False), encoding='utf-8')

                        save()
                        if checking:
                            generate.generate('fixture-tool')
                        if variation in ('single', 'double'):
                            for _ in range(1 if variation == 'single' else 2):
                                provenance['authors'][1] = (
                                    provenance['authors'][1].encode('utf-8').decode('cp1252')
                                )
                        elif variation == 'reordered':
                            provenance['authors'].reverse()
                        elif variation == 'absent':
                            reported.clear()
                        save()
                        if variation == 'valid':
                            generate.generate('fixture-tool', check=checking)
                        else:
                            with self.assertRaisesRegex(ValueError, 'authors differ'):
                                generate.generate('fixture-tool', check=checking)
                            if not checking:
                                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    def test_apache_appendix_examples_are_bounded_and_retained(self):
        # Falsifiers: omit any title/terms/start/end requirement or allow a broad template.
        prefix = 'Apache License\nEND OF TERMS AND CONDITIONS\nAPPENDIX: How to apply these terms\n'
        example = 'Copyright [yyyy] [name of copyright owner]'
        suffix = '\nlimitations under the License.\n'
        valid = prefix + example + suffix
        generate.check_assembled(valid)
        invalid = [
            example,
            prefix + suffix + example,
            valid + '\n' + '=' * 78 + '\nMIT\n' + example,
            valid.replace(example, 'Copyright [year] [fullname]'),
        ]
        invalid += [
            valid.replace(part, '')
            for part in (
                'Apache License',
                'END OF TERMS AND CONDITIONS',
                'APPENDIX: How to apply',
                'limitations under the License.',
            )
        ]
        for index, text in enumerate(invalid):
            with self.subTest(case=index), self.assertRaisesRegex(ValueError, 'assembled notices'):
                generate.check_assembled(text)

    def test_author_placeholders_are_refused_with_real_dates(self):
        # Falsifier: accept author templates because a concrete date precedes them.
        for token in ('<author>', '&lt;author&gt;', 'YYYY Author Name'):
            with self.subTest(token=token), self.fixture(
                addendum='Copyright 2026 ' + token
            ) as root:
                with self.assertRaisesRegex(ValueError, 'assembled notices'):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    def test_lgpl_appendix_examples_are_bounded_and_retained(self):
        # Falsifiers: broaden/remove any title, terms, heading or end boundary, or omit either
        # permitted upstream example spelling. No exemption carries into a later licence block.
        prefix = (
            'GNU LESSER GENERAL PUBLIC LICENSE\nEND OF TERMS AND CONDITIONS\n'
            'How to Apply These Terms to Your New Libraries\n'
        )
        suffix = '\nThis library is free software\n'
        for example in ('Copyright (C) {year} {fullname}', 'Copyright (C) year  name of author'):
            valid = prefix + example + suffix
            with self.subTest(example=example), self.fixture(addendum=valid) as root:
                generate.generate('fixture-tool')
                self.assertIn(
                    example, (root / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
                )
            invalid = [
                example,
                prefix + suffix + example,
                valid + '\n' + '=' * 78 + '\nMIT\n' + example,
                prefix
                + example.replace('{fullname}', '<author>').replace('name of author', 'Author Name')
                + suffix,
            ]
            invalid += [
                valid.replace(part, '')
                for part in (
                    'GNU LESSER GENERAL PUBLIC LICENSE',
                    'END OF TERMS AND CONDITIONS',
                    'How to Apply These Terms',
                )
            ]
            for index, text in enumerate(invalid):
                with self.subTest(example=example, case=index):
                    with self.assertRaisesRegex(ValueError, 'assembled notices'):
                        generate.check_assembled(text)

    def test_root_path_and_ijg_explanation_are_checked(self):
        # Falsifiers: drop the exact-root path guard or the IJG portion explanation.
        with self.fixture() as root:
            # A relative ROOT isolates this guard from the independent C:/Users scan.
            (root / generate.SUPPLEMENTS[0]).write_text(root.name, encoding='utf-8')
            previous = Path.cwd()
            try:
                os.chdir(root.parent)
                with patch.object(generate, 'ROOT', Path(root.name)):
                    with self.assertRaisesRegex(ValueError, 'Local path'):
                        generate.generate('fixture-tool')
            finally:
                os.chdir(previous)

        def jpeg_report(report):
            report['crates'][0]['package']['name'] = 'jpeg-encoder'
            report['licenses'][0]['used_by'][0]['crate']['name'] = 'jpeg-encoder'
            report['crates'][0]['package']['manifest_path'] = str(self.jpeg_manifest)

        with self.fixture(licence='IJG', edit_report=jpeg_report) as root:
            (root / 'falcon/Cargo.lock').write_text(
                (root / 'falcon/Cargo.lock')
                .read_text(encoding='utf-8')
                .replace('fixture-lib', 'jpeg-encoder')
            )
            for relative in ['src/fdct.rs', 'src/avx2/fdct.rs']:
                path = self.jpeg_manifest.parent / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('/* Original IJG source notice */', encoding='utf-8')
            generate.generate('fixture-tool')
            text = (root / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
            self.assertIn('Classifier-supplied IJG terms follow.', text)
            self.assertIn('identify the portions used by that crate', text)

    @contextmanager
    def fixture(
        self,
        body='Copyright 2026 Fixture Authors.\nPermission is hereby granted, free of charge.',
        licence='MIT',
        addendum=None,
        exception=None,
        missing=False,
        edit_report=None,
    ):
        # Only external process boundaries are mocked; real generate(), parsing, gates and rendering run.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.jpeg_manifest = root / 'fake-jpeg/Cargo.toml'

            def write(name, text):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding='utf-8', newline='\n')
                return path

            write('falcon/Cargo.toml', '[workspace]\nmembers=[]\n')
            write('falcon/about.toml', 'accepted=["MIT"]\n')
            write('falcon/vendor/winit/Cargo.toml', '[package]\nname="winit"\nversion="0.0.0"\n')
            write(
                'falcon/vendor/zune-jpeg/Cargo.toml',
                '[package]\nname="zune-jpeg"\nversion="0.5.15"\n',
            )
            write(
                'falcon/Cargo.lock',
                '[[package]]\nname="fixture-lib"\nversion="1.0.0"\n[[package]]\nname="slint"\nversion="1.17.0"\n',
            )
            write('docs/licenses/rust-standard-library.html', 'Synthetic runtime notice')
            for name in generate.SUPPLEMENTS:
                write(name, 'Synthetic supplement')
            for name in generate.FONTS:
                write(name, 'Synthetic font bytes')
            write(
                'falcon/native/src/main.rs',
                '\n'.join(
                    'const FONT: &[u8] = include_bytes!("../assets/fonts/' + Path(name).name + '");'
                    for name in generate.FONTS
                ),
            )
            production = generate.ROOT
            text = (production / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
            slint = (
                text.split('\nSlint Royalty-Free 2.0\n\n', 1)[1]
                .split('\n' + '=' * 78, 1)[0]
                .rstrip('\n')
                + '\n'
            )
            self.assertEqual(
                hashlib.sha256(slint.encode()).hexdigest(),
                '5167f5056e850419106ab6265efbdca7cba4d99c849d1445ca0bbf6a1e2315fe',
            )
            manifest = write('fake-slint/Cargo.toml', 'fixture')
            write('fake-slint/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md', slint)
            files = []
            if addendum is not None:
                write('docs/licenses/upstream/fixture.txt', addendum)
                files = [
                    {
                        'file': 'docs/licenses/upstream/fixture.txt',
                        'sha256': hashlib.sha256(addendum.encode()).hexdigest(),
                        'origin': 'fixture licence',
                    }
                ]
            entries = {}
            if files or exception:
                entries['fixture-lib 1.0.0'] = {
                    'files': files,
                    **({'exception': exception} if exception else {}),
                }
            write('docs/licenses/notice-sources.json', json.dumps({'packages': entries}))
            report = {
                'crates': [
                    {
                        'package': {'name': 'fixture-lib', 'version': '1.0.0', 'license': licence},
                        'license': licence,
                    },
                    {
                        'package': {
                            'name': 'slint',
                            'version': '1.17.0',
                            'license': 'LicenseRef-Slint-Royalty-free-2.0',
                            'manifest_path': str(manifest),
                        },
                        'license': 'LicenseRef-Slint-Royalty-free-2.0',
                    },
                ],
                'licenses': [
                    {
                        'name': licence,
                        'id': licence,
                        'text': body,
                        'source_path': None,
                        'used_by': [{'crate': {'name': 'fixture-lib', 'version': '1.0.0'}}],
                    }
                ],
            }
            if edit_report:
                edit_report(report)

            def output(args, **kwargs):
                if args[-1] == '--version':
                    return 'cargo-about 0.9.2'
                self.assertIn('--locked', args)
                self.assertIn(
                    args[args.index('--target') + 1],
                    ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin'],
                )
                return ''.join(
                    x['package']['name'] + ' v' + x['package']['version'] + '\n'
                    for x in report['crates']
                ) + ('missing v1.0.0\n' if missing else '')

            def run(args, **kwargs):
                self.assertIn('--locked', args)
                Path(args[args.index('--output-file') + 1]).write_text(
                    json.dumps(report, ensure_ascii=False), encoding='utf-8'
                )

            with patch.object(generate, 'ROOT', root), patch.object(
                generate.subprocess, 'check_output', side_effect=output
            ), patch.object(generate.subprocess, 'run', side_effect=run):
                yield root

    # Falsifier: render licence['text'] directly, omit addenda, or skip the final assembled scan.
    def test_complete_generation_preserves_headers_and_reproduces(self):
        manifest = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )
        headers = []
        for key, origin in [
            ('brotli-decompressor 5.0.3', 'src/context.rs'),
            ('color_quant 1.1.0', 'src/lib.rs'),
        ]:
            item = next(
                item for item in manifest['packages'][key]['files'] if item['origin'] == origin
            )
            headers.append((generate.ROOT / item['file']).read_text(encoding='utf-8'))
        with self.fixture(
            body='Copyright (c) <year> <owner>\nPermission is hereby granted, free of charge.',
            addendum='\n'.join(headers),
        ) as root:
            generate.generate('fixture-tool')
            text = (root / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
            self.assertIn('Copyright 2013 Google Inc.', text)
            self.assertIn('Copyright (c) 1994 Anthony Dekker', text)
            self.assertIn('copyright notice remain intact', text)
            self.assertNotIn('<year>', text)
            generate.generate('fixture-tool', check=True)
            (root / 'THIRD-PARTY-NOTICES.txt').write_text(text + 'changed', encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'differ'):
                generate.generate('fixture-tool', check=True)

    # Falsifier: skip check_coverage inside generate().
    def test_generation_requires_every_target_package(self):
        with self.fixture(missing=True) as root:
            with self.assertRaisesRegex(ValueError, 'Target dependency packages missing'):
                generate.generate('fixture-tool')
            self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    # Falsifiers: remove one of generate()'s locked/addendum/Slint/coverage/path guards.
    def test_remaining_generation_guards(self):
        cases = [
            ('locked', 'locked package versions'),
            ('addenda', 'package versions change'),
            ('slint', 'Slint terms changed'),
            ('uncovered', 'without licence text'),
            ('path', 'Local path'),
        ]
        for case, message in cases:

            def edit(report):
                if case == 'locked':
                    report['crates'][0]['package']['version'] = '9.0.0'
                    report['licenses'][0]['used_by'][0]['crate']['version'] = '9.0.0'
                if case == 'uncovered':
                    report['licenses'] = []

            with self.subTest(case=case), self.fixture(edit_report=edit) as root:
                if case == 'addenda':
                    p = root / 'docs/licenses/notice-sources.json'
                    p.write_text(
                        json.dumps({'packages': {'stale 9.0.0': {'files': []}}}), encoding='utf-8'
                    )
                if case == 'slint':
                    p = root / 'fake-slint/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md'
                    p.write_bytes(p.read_bytes() + b'changed')
                if case == 'path':
                    p = root / generate.SUPPLEMENTS[0]
                    p.write_text('Local fixture path: C:/Users/test/file', encoding='utf-8')
                with self.assertRaisesRegex(ValueError, message):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    # Falsifier: guard only visible template tokens instead of holder/provenance absence.
    def test_holderless_sections_require_explicit_provenance(self):
        for licence in ['MIT', 'ISC', 'Zlib', 'BSD-2-Clause']:
            with self.subTest(licence=licence), self.fixture(
                body='Permission without a holder.', licence=licence
            ) as root:
                with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'docs/licenses/dependency-inventory.json').exists())
        with self.fixture(body='Copyright (c)\nPermission without a named holder.') as root:
            with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                generate.generate('fixture-tool')
        with self.fixture(
            body='Permission without a holder.',
            exception='Reviewed upstream supplies no holder statement.',
        ) as root:
            generate.generate('fixture-tool')
            self.assertIn(
                '\n' + '=' * 78 + '\nReviewed upstream provenance:',
                (root / 'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8'),
            )

    # Falsifier: count Apache permission prose as a holder, or miss REUSE/SPDX holder forms.
    def test_holder_shape_and_reuse_metadata(self):
        apache = (generate.ROOT / 'LICENSE').read_text(encoding='utf-8')
        self.assertFalse(generate.has_holder(apache))
        for line in [
            'Copyright: 2021 Fixture Author <fixture@example.test>',
            'SPDX-FileCopyrightText: 2021 Fixture Author',
            'Copyright Fixture Authors',
            'Portions copyright 2021 lowercase-holder',
            'Copyright 2018 Visly Inc.',
            'Copyright (c) [2021] [Marvin Countryman]',
            'Copyright (c) zune-image developers',
        ]:
            self.assertTrue(generate.has_holder(line), line)
        with self.fixture(body='Permission without a holder.', addendum=apache) as root:
            with self.assertRaisesRegex(ValueError, 'Missing reviewed copyright'):
                generate.generate('fixture-tool')

    # Falsifier: limit placeholders to one bracket spelling or singular year/owner.
    def test_all_placeholder_spellings_are_rejected(self):
        for token in [
            '<years> <name>',
            '[year] [fullname]',
            '{yyyy}',
            '<copyright holders>',
            'YEAR  NAME',
            '20XX Your Name',
            '$YEAR $OWNER',
        ]:
            with self.subTest(token=token), self.fixture(addendum='Copyright ' + token) as root:
                with self.assertRaisesRegex(ValueError, 'assembled notices'):
                    generate.generate('fixture-tool')

    # Falsifier: remove the real half DEP5 pin or restore its false no-holder exception.
    def test_half_copyright_and_contiguous_upstream_resources_are_pinned(self):
        manifest = json.loads(
            (generate.ROOT / 'docs/licenses/notice-sources.json').read_text(encoding='utf-8')
        )['packages']
        entry = manifest['half 2.7.1']
        self.assertNotIn('exception', entry)
        item = next(x for x in entry['files'] if x['origin'] == '.reuse/dep5')
        text = (generate.ROOT / item['file']).read_text(encoding='utf-8')
        self.assertIn('Copyright: 2021 Kathryn Long', text)
        self.assertEqual(
            hashlib.sha256(text.encode()).hexdigest(),
            '7aa615df0376504c0dfa905f81759d00e64155345640a802db721b72ad4ec19e',
        )
        for key, origin, sha in [
            (
                'libm 0.2.16',
                'src/math/cbrt.rs',
                'f117f16ae37d151238cfc1a3030f060b06f9045ed0298cbdddbdf1ae0b8c0487',
            ),
            (
                'rawler 0.8.0',
                'src/decoders/rw2/v8decompressor.rs',
                '54a3ced604ab572557191858d5a1b85fcad3158d7fe9ed263ef893728892ed9a',
            ),
        ]:
            item = next(x for x in manifest[key]['files'] if x['origin'] == origin)
            self.assertEqual(
                hashlib.sha256(
                    (generate.ROOT / item['file']).read_text(encoding='utf-8').encode()
                ).hexdigest(),
                sha,
            )

    # Falsifier: allow an explanatory note or holder-only header to replace BSD conditions.
    def test_bsd_requires_complete_pinned_conditions(self):
        for addendum in [None, 'Copyright 2026 Fixture Authors.']:
            with self.fixture(
                licence='BSD-3-Clause', addendum=addendum, exception='Reviewed note only.'
            ) as root:
                with self.assertRaisesRegex(ValueError, 'pinned complete licence'):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())

    # Falsifier: do not scan appended source notices, or globally exempt appendix-looking tokens.
    def test_bad_addendum_templates_fail_before_writing(self):
        appendix_then_bad = (generate.ROOT / 'LICENSE').read_text(
            encoding='utf-8'
        ) + '\nMIT section\nCopyright [yyyy] [name of copyright owner]\n'
        for bad in [
            'Copyright <year> <owner>',
            'Copyright [yyyy] [name of copyright owner]',
            appendix_then_bad,
        ]:
            with self.fixture(addendum=bad) as root:
                with self.assertRaisesRegex(ValueError, 'assembled notices'):
                    generate.generate('fixture-tool')
                self.assertFalse((root / 'THIRD-PARTY-NOTICES.txt').exists())


if __name__ == '__main__':
    unittest.main()
