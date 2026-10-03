"""Validate/copy the readable materials accompanying each Falcon binary."""
import hashlib
import json
import re
import os
import stat
from pathlib import Path
import shutil
import tomllib

FILES = ('LICENSE', 'NOTICE', 'THIRD-PARTY-NOTICES.txt', 'REBUILDING.md')
SUPPLEMENTS = ('falcon/native/assets/fonts/Inter-OFL.txt', 'falcon/vendor/winit/LICENSE',
               'docs/third-party/winit-hosted-view.txt', 'docs/third-party/chromium-chrome02.txt',
               'falcon/vendor/zune-jpeg/FALCON-CHANGES.md',
               'falcon/vendor/femtovg/FALCON-CHANGES.md')
FONTS = tuple('falcon/native/assets/fonts/Inter-'+face+'.ttf'
              for face in ('Regular', 'Medium', 'SemiBold', 'Bold'))


def build_guide(root):
    # The private checkout stores public prose as a template. Exports map it to the root.
    guide = root/'BUILDING.md'
    return guide if guide.is_file() else root/'docs/distribution/building.md'


def text_hash(path):
    return hashlib.sha256(Path(path).read_text(encoding='utf-8-sig').encode('utf-8')).hexdigest()


def notice_sources(root):
    manifest = 'docs/licenses/notice-sources.json'
    data = json.loads((root/manifest).read_text(encoding='utf-8'))['packages']
    inputs = {manifest:text_hash(root/manifest)}
    for entry in data.values():
        for item in entry['files']:
            name = item['file']; path = root/name
            if not name.startswith('docs/licenses/upstream/') or not path.resolve().is_relative_to((root/'docs/licenses/upstream').resolve()):
                raise ValueError('Invalid notice source path')
            digest = text_hash(path)
            if digest != item['sha256']: raise ValueError('Upstream notice changed: '+name)
            inputs[name] = digest
    return data, inputs


def embedded_fonts(root):
    found = set()
    bases = [root/'falcon/native/src', *(root/'falcon/crates').glob('*/src')]
    for source in (p for base in bases for p in base.rglob('*.rs')):
        text = source.read_text(encoding='utf-8')
        for match in re.finditer(r'include_bytes!\s*\(\s*"([^"]+\.(?:ttf|otf|woff2?))"\s*\)', text):
            path = (source.parent/match[1]).resolve()
            if not path.is_relative_to(root.resolve()): raise ValueError('Embedded font outside source')
            found.add(path.relative_to(root.resolve()).as_posix())
    # A new embedded face must have its provenance/terms reviewed, rather than silently inheriting Inter's licence.
    if found != set(FONTS): raise ValueError('Embedded font declarations changed; review their licences')
    return sorted(found)


def validate(root):
    root = Path(root)
    for name in FILES:
        if not (root/name).is_file() or not (root/name).stat().st_size:
            raise ValueError('Missing release material: '+name)
    guide = build_guide(root)
    if not guide.is_file() or not guide.stat().st_size:
        raise ValueError('Missing release material: BUILDING.md')
    inventory = json.loads((root/'docs/licenses/dependency-inventory.json').read_text(encoding='utf-8'))
    _, source_inputs = notice_sources(root)
    if inventory.get('notice_source_inputs') != source_inputs:
        raise ValueError('Copyright source inventory is stale')
    if inventory['lock_sha256'] != text_hash(root/'falcon/Cargo.lock'):
        raise ValueError('Licence inventory is stale for Cargo.lock')
    if inventory['notices_sha256'] != text_hash(root/'THIRD-PARTY-NOTICES.txt'):
        raise ValueError('Generated third-party notices changed')
    manifests = tomllib.loads((root/'falcon/Cargo.toml').read_text(encoding='utf-8'))
    inputs = {'falcon/Cargo.toml', 'falcon/about.toml', 'falcon/vendor/winit/Cargo.toml',
              'falcon/vendor/zune-jpeg/Cargo.toml', 'falcon/vendor/femtovg/Cargo.toml'}
    inputs.update('falcon/'+member+'/Cargo.toml' for member in manifests['workspace']['members'])
    if set(inventory.get('project_inputs', {})) != inputs:
        raise ValueError('Licence inventory does not cover the current project manifests')
    for name in inputs:
        path = root/name
        if not path.resolve().is_relative_to(root.resolve()) or text_hash(path) != inventory['project_inputs'][name]:
            raise ValueError('Licence inventory is stale for project configuration: '+name)
    if inventory.get('targets') != ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin']:
        raise ValueError('Licence inventory targets changed')
    if set(inventory.get('supplementary_inputs', {})) != set(SUPPLEMENTS):
        raise ValueError('Licence inventory does not cover supplementary notices')
    for name in SUPPLEMENTS:
        if text_hash(root/name) != inventory['supplementary_inputs'][name]:
            raise ValueError('Licence inventory is stale for supplementary notice: '+name)
    if set(inventory.get('font_inputs', {})) != set(embedded_fonts(root)):
        raise ValueError('Licence inventory does not cover embedded fonts')
    for name in FONTS:
        if hashlib.sha256((root/name).read_bytes()).hexdigest() != inventory['font_inputs'][name]:
            raise ValueError('Licence inventory is stale for embedded font: '+name)
    locked = {(p['name'], p['version']) for p in tomllib.loads((root/'falcon/Cargo.lock').read_text())['package']}
    packages = inventory.get('packages', [])
    if not packages or any((p['name'], p['version']) not in locked or not p.get('selected') for p in packages):
        raise ValueError('Licence package coverage is missing or stale')
    notices = (root/'THIRD-PARTY-NOTICES.txt').read_text(encoding='utf-8')
    for required in ['Independent JPEG Group', 'Slint Royalty-Free', 'GNU LESSER GENERAL PUBLIC LICENSE']:
        if required not in notices:
            raise ValueError('Missing notice: '+required)
    runtime = root/'docs/licenses/rust-standard-library.html'
    if not runtime.is_file() or not runtime.stat().st_size or text_hash(runtime) != inventory.get('runtime_notice_sha256'):
        raise ValueError('Rust runtime notices missing or changed')


MATERIAL_MARKER = '.falcon-generated-materials'
MATERIAL_OWNER = b'Falcon generated licence materials v1\n'


def linked(path):
    # is_junction is unavailable in Python 3.11; lstat attributes cover Windows reparse points.
    return path.is_symlink() or bool(getattr(path.lstat(), 'st_file_attributes', 0) & 0x400)


def copy(root, destination):
    root, destination = Path(root), Path(destination)
    validate(root)
    licences=destination/'docs/licenses'
    # Refuse before overwriting even the top-level guides. Only our own generated tree is reusable.
    if not licences.resolve().is_relative_to(destination.resolve()):
        raise ValueError('Licence output escaped package destination')
    if os.path.lexists(licences):
        if linked(licences) or not licences.is_dir():
            raise ValueError('Licence output escaped package destination')
        marker=licences/MATERIAL_MARKER
        if not marker.is_file() or linked(marker) or marker.read_bytes()!=MATERIAL_OWNER:
            raise ValueError('Licence output is not owned by this packager')
        pending=[licences]
        while pending:
            for path in pending.pop().iterdir():
                if linked(path):raise ValueError('Linked content in generated licence output')
                if path.is_dir():pending.append(path)
        def clear_readonly(function, path, error):
            target=Path(path)
            if linked(target) or not target.resolve().is_relative_to(licences.resolve()):
                raise error[1]
            os.chmod(target,stat.S_IWRITE|stat.S_IREAD)
            function(path)
        shutil.rmtree(licences,onerror=clear_readonly)
    destination.mkdir(parents=True, exist_ok=True)
    # Claim our new tree before copying so a interrupted copy remains safely retryable.
    licences.mkdir(parents=True, exist_ok=True)
    (licences/MATERIAL_MARKER).write_bytes(MATERIAL_OWNER)
    for name in FILES:
        shutil.copyfile(root/name, destination/name)
    shutil.copyfile(build_guide(root), destination/'BUILDING.md')
    # Only pinned notice resources are package inputs; do not inherit OneDrive directory flags.
    inventory=json.loads((root/'docs/licenses/dependency-inventory.json').read_text(encoding='utf-8'))
    resources=set(inventory['notice_source_inputs'])|{'docs/licenses/dependency-inventory.json','docs/licenses/rust-standard-library.html'}
    for name in sorted(resources):
        target=destination/name;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(root/name,target)


if __name__ == '__main__':
    validate(Path(__file__).resolve().parents[1])
    print('Release notices match the locked source')
