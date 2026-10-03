"""Read the real executable's icon resources without launching it or changing associations."""
import ctypes
from ctypes import wintypes
import json
import pathlib
import struct
import sys


def verify(executable, assets=None):
    if sys.platform != 'win32': raise RuntimeError('Requires the Windows resource loader')
    assets = pathlib.Path(assets) if assets else pathlib.Path(__file__).resolve().parents[1] / 'falcon/native/assets/icons'
    resources = json.loads((assets / 'manifest.json').read_text(encoding='utf-8'))['resources']
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.LoadLibraryExW.argtypes = [wintypes.LPCWSTR, wintypes.HANDLE, wintypes.DWORD]
    kernel.LoadLibraryExW.restype = wintypes.HMODULE
    kernel.FindResourceW.argtypes = [wintypes.HMODULE, ctypes.c_void_p, ctypes.c_void_p]
    kernel.FindResourceW.restype = wintypes.HANDLE
    kernel.LoadResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
    kernel.LoadResource.restype = wintypes.HANDLE
    kernel.SizeofResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
    kernel.SizeofResource.restype = wintypes.DWORD
    kernel.LockResource.argtypes = [wintypes.HANDLE]
    kernel.LockResource.restype = ctypes.c_void_p
    kernel.FreeLibrary.argtypes = [wintypes.HMODULE]
    # LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE: never execute entry points.
    handle = kernel.LoadLibraryExW(str(executable.resolve()), None, 0x22)
    if not handle: raise ctypes.WinError(ctypes.get_last_error())
    def load(kind, identity):
        resource = kernel.FindResourceW(handle, identity, kind)
        if not resource: raise ctypes.WinError(ctypes.get_last_error())
        size = kernel.SizeofResource(handle, resource)
        pointer = kernel.LockResource(kernel.LoadResource(handle, resource))
        if not pointer: raise ctypes.WinError(ctypes.get_last_error())
        return ctypes.string_at(pointer, size)
    try:
        for entry in resources:
            ico = (assets / entry['file']).read_bytes()
            group = load(14, entry['id'])
            if group[:6] != ico[:6]: raise ValueError('Wrong group header: ' + entry['file'])
            count = struct.unpack('<H', group[4:6])[0]
            for i in range(count):
                expected = ico[6+16*i:22+16*i]; actual = group[6+14*i:20+14*i]
                # RC normalizes Pillow's ICO planes=0 to one plane in the PE group.
                if expected[:4] != actual[:4] or expected[6:12] != actual[6:12] or actual[4:6] != b'\x01\x00':
                    raise ValueError('Wrong icon dimensions/depth: ' + entry['file'])
                length, offset = struct.unpack('<II', expected[8:16])
                identity = struct.unpack('<H', actual[12:14])[0]
                if load(3, identity) != ico[offset:offset+length]: raise ValueError('Wrong embedded pixels')
        if resources[0] != next(r for r in resources if r['id'] == 1 and r['file'] == 'app.ico'):
            raise ValueError('App resource 1 contract changed')
    finally:
        kernel.FreeLibrary(handle)
    print(f'Verified {len(resources)} embedded icon groups; resource 1 is the app')


if __name__ == '__main__': verify(pathlib.Path(sys.argv[1]))
