"""The language-pack checker finds every marked message and every pack problem."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('check_translations', ROOT / 'scripts/check-translations.py')
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)

RUST = '''
/// Doc example: tr("Not a message")
fn shown() {
    toast(i18n::tr("Saved — done"));
    let s = tr_format!("Copied {photos} photos ({files} files)", photos = n, files = m);
    let t = tr_plural!(items.len(), "Delete this photo?", "Delete {n} photos?");
    const LABEL: &str = tr_noop!("Rotate");
    let c = '"'; let q = '\\''; // a quote in a char literal must not open a string
    /* tr("Also not a message") */
}
#[cfg(test)]
mod tests {
    fn rig() { assert_eq!(tr("Test only"), "x"); let r = r#"{ "}"#; }
}
'''
SLINT = '''
// @tr("falcon" => "Commented out")
Text { text: @tr("falcon" => "LANGUAGE"); }
Text { text: @tr("falcon" => "{0} of {1} files"); }
Text { text: @tr("falcon" => "rated photo" | "rated photos" % count); }
'''
EXPECTED = {'Saved — done', 'Copied {photos} photos ({files} files)', 'Delete this photo?', 'Rotate',
            'LANGUAGE', '{0} of {1} files', 'rated photo'}


def tree(root, rust=RUST, slint=SLINT, languages=(), packs=None):
    native = root / 'falcon/native'
    for sub in ('src', 'ui', 'translations/test'):
        (native / sub).mkdir(parents=True, exist_ok=True)
    (native / 'src/a.rs').write_text(rust, encoding='utf-8')
    (native / 'src/x_tests.rs').write_text('fn t() { tr("Rig only"); }', encoding='utf-8')
    (native / 'ui/a.slint').write_text(slint, encoding='utf-8')
    (native / 'translations/languages.json').write_text(json.dumps({'languages': list(languages)}), encoding='utf-8')
    for code, pack in (packs or {}).items():
        (native / f'translations/{code}.json').write_text(json.dumps(pack, ensure_ascii=False), encoding='utf-8')
    return root


def good_pack():
    return {
        'Saved — done': '已保存', 'Copied {photos} photos ({files} files)': '已复制 {photos} 张照片（{files} 个文件）',
        'Delete this photo?': {'other': '删除 {n} 张照片？'}, 'Rotate': '旋转', 'LANGUAGE': '语言',
        '{0} of {1} files': '{1} 个文件中的 {0} 个', 'rated photo': {'other': '已评分照片'},
    }


ZH = {'code': 'zh-CN', 'name': '简体中文', 'match': ['zh-Hans', 'zh'], 'plural': 'none'}


class Checker(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def problems(self, **kw):
        root = tree(self.root, **kw)
        checker.check(root, write=True)
        return checker.check(root)[2]

    def test_collects_marked_messages_but_not_comments_or_tests(self):
        # Falsifiers: skip `blank_comments` or `strip_test_modules`, or scan *_tests.rs files.
        messages, problems = checker.collect(tree(self.root))
        self.assertEqual(problems, [])
        self.assertEqual(set(messages), EXPECTED)
        self.assertTrue(messages['Delete this photo?']['plural'])
        self.assertEqual(messages['rated photo']['other'], 'rated photos')

    def test_a_complete_pack_passes(self):
        self.assertEqual(self.problems(languages=[ZH], packs={'zh-CN': good_pack()}), [])

    def test_each_pack_problem_is_reported(self):
        # Falsifier: disable any one check in `check_pack` and its line below goes missing.
        pack = good_pack()
        del pack['Rotate']                                                       # missing
        pack['Gone'] = '旧'                                                      # stale
        pack['Copied {photos} photos ({files} files)'] = '已复制 {photos} 张'     # placeholder lost
        pack['Delete this photo?'] = {'one': 'a', 'other': 'b {n}'}               # wrong categories
        pack['LANGUAGE'] = ' '                                                    # empty
        pack['{0} of {1} files'] = '{0} 个 {'                                     # unclosed brace
        found = '\n'.join(self.problems(languages=[ZH], packs={'zh-CN': pack}))
        for expected in ("missing 'Rotate'", "stale 'Gone'", "'Copied {photos} photos ({files} files)' (text) has placeholders",
                         "'Delete this photo?' is counted", "'LANGUAGE' (text) is empty", 'unclosed {'):
            self.assertIn(expected, found)

    def test_placeholder_styles_and_context_are_enforced(self):
        rust = 'fn f() { tr_format!("{0} files", x = 1); tr("Has {name}"); tr_format!("{size:.1} MB", size = s); }'
        slint = 'Text { text: @tr("{photos} photos"); } Text { text: @tr("other" => "x"); } Text { text: @tr("falcon" => "{photos}"); }'
        messages, problems = checker.collect(tree(self.root, rust=rust, slint=slint))
        found = '\n'.join(problems)
        self.assertIn("named placeholders such as {photos}, without format specs; found ['0']", found)
        self.assertIn('needs tr_format! or tr_plural!', found)
        self.assertIn("found ['size:.1']", found)
        self.assertIn('write @tr("falcon" => "...")', found)
        self.assertIn('the @tr context must be "falcon"', found)
        self.assertIn("numbered placeholders", found)

    def test_language_list_problems(self):
        entries = [dict(ZH), {'code': 'en', 'name': 'English', 'match': ['en'], 'plural': 'one_other'},
                   {'code': 'de', 'name': 'Deutsch', 'match': ['de'], 'plural': 'two_forms'}]
        found = '\n'.join(self.problems(languages=entries, packs={'zh-CN': good_pack(), 'fr': {}}))
        self.assertIn('en is English, reserved for tests, or listed twice', found)
        self.assertIn("unknown plural rule 'two_forms'", found)
        self.assertIn('de has no translations/de.json', found)
        self.assertIn('translations/fr.json: a pack with no entry', found)

    def test_the_pseudo_language_must_be_regenerated(self):
        root = tree(self.root)
        checker.check(root, write=True)
        self.assertEqual(checker.check(root)[2], [])
        pseudo = json.loads((root / 'falcon/native/translations/test/xx-TEST.json').read_text(encoding='utf-8'))
        self.assertEqual(pseudo['LANGUAGE'], '⟦LANGUAGE⟧')
        self.assertEqual(pseudo['Delete this photo?'], {'one': '⟦Delete this photo?⟧', 'other': '⟦Delete {n} photos?⟧'})
        (root / 'falcon/native/src/a.rs').write_text(RUST + '\nfn g() { tr("New message"); }', encoding='utf-8')
        self.assertIn('xx-TEST.json is out of date', '\n'.join(checker.check(root)[2]))

    def test_a_crlf_checkout_of_the_test_language_passes(self):
        # Review R1. Falsifier: compare the test language as raw bytes again.
        root = tree(self.root)
        checker.check(root, write=True)
        pseudo = root / 'falcon/native/translations/test/xx-TEST.json'
        pseudo.write_bytes(pseudo.read_bytes().replace(b'\n', b'\r\n'))   # what a Windows checkout holds
        self.assertEqual(checker.check(root)[2], [])

    def test_every_supported_rust_form_is_collected_and_others_are_reported(self):
        # Review R3. Falsifier: go back to the fixed regular expressions (trailing comma and raw
        # strings are then silently skipped), or drop the "could not be read" reports.
        rust = '''
fn tr(english: &'static str) -> &'static str { english }
fn f(label: &'static str) {
    let _ = i18n::tr("Open",);
    let _ = tr_format!(r"Copied {photos} photos", photos = n);
    let _ = tr_format!(r#"Say "hi" to {name}"#, name = n,);
    let _ = tr_plural!(f(a, b), "One {n} file", r"Many {n} files",);
    let _ = tr_noop!("Kept",);
    let _ = tr(label); let _ = tr(PLATFORM.bin); // runtime values marked elsewhere with tr_noop!
    let _ = tr(concat!("Not", "read"));
    let _ = tr_format!["Wrong delimiters {x}", x = 1];
    let _ = tr_noop!(SOME_CONST);
}
'''
        messages, problems = checker.collect(tree(self.root, rust=rust, slint=''))
        self.assertEqual(set(messages), {'Open', 'Copied {photos} photos', 'Say "hi" to {name}', 'One {n} file', 'Kept'})
        self.assertEqual(messages['One {n} file']['other'], 'Many {n} files')
        found = '\n'.join(problems)
        self.assertEqual(len(problems), 3, found)
        self.assertIn('the English text must be a string literal', found)
        self.assertIn('tr_format! must be called with parentheses', found)

    def test_a_singular_may_leave_out_only_the_count(self):
        # Review R4. Falsifier: drop the "only {n} may be left out" check in `check_pack`, or the
        # English singular check in `collect`.
        rust = 'fn f() { tr_plural!(k, "Delete this photo from {place}?", "Delete {n} photos from {place}?", place = p); }'
        de = {'code': 'de', 'name': 'Deutsch', 'match': ['de'], 'plural': 'one_other'}
        drop_place = {'Delete this photo from {place}?': {'one': 'Dieses Foto löschen?', 'other': '{n} Fotos aus {place} löschen?'}}
        drop_count = {'Delete this photo from {place}?': {'one': 'Dieses Foto aus {place} löschen?', 'other': '{n} Fotos aus {place} löschen?'}}
        found = '\n'.join(self.problems(rust=rust, slint='', languages=[de], packs={'de': drop_place}))
        self.assertIn("drops ['place']; only {n} may be left out", found)
        self.assertEqual(self.problems(rust=rust, slint='', languages=[de], packs={'de': drop_count}), [])
        english = 'fn f() { tr_plural!(k, "Delete this photo?", "Delete {n} photos from {place}?", place = p); }'
        self.assertIn('must keep every placeholder of the plural except {n}', '\n'.join(checker.collect(tree(self.root, rust=english, slint=''))[1]))

    def test_runs_with_utf8_mode_off(self):
        # Falsifier: open a file without encoding='utf-8' or print without the error handler.
        tree(self.root, languages=[ZH], packs={'zh-CN': good_pack()})
        checker.check(self.root, write=True)
        env = {k: v for k, v in os.environ.items() if k not in ('PYTHONUTF8', 'PYTHONIOENCODING')}
        run = subprocess.run([sys.executable, '-X', 'utf8=0', str(ROOT / 'scripts/check-translations.py'), '--root', str(self.root)],
                             capture_output=True, env=env)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)

    def test_the_repository_passes(self):
        self.assertEqual(checker.check(ROOT)[2], [])


if __name__ == '__main__':
    unittest.main()
