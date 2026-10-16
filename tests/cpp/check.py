#!/usr/bin/env python3
"""Optional native C++ oracle. Rust libraries/binaries never link C++.
Requires an existing MMX CMake build containing mmx_iface and vnx_base.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--mmx-root', required=True, type=Path)
parser.add_argument('--build-dir', type=Path)
parser.add_argument('--cxx', default='c++')
args = parser.parse_args()
mmx = args.mmx_root.resolve()
build = (args.build_dir or mmx / 'build').resolve()
repo = Path(__file__).resolve().parents[2]
includes = [mmx / p for p in [
    'include', 'generated/include', 'generated/vm/include', 'generated/contract/include',
    'generated/operation/include', 'generated/solution/include', 'vnx-base/include',
    'vnx-base/generated/include', 'vnx-addons/include', 'vnx-addons/generated/include',
    'vnx-addons/lib/url-cpp/include', 'vnx-addons/lib/llhttp/src/native', 'uint256_t',
    'uint256_t/uint128_t', 'lib/libbech32/include/libbech32', 'lib/secp256k1/include',
]] + [build / 'vnx-base']
with tempfile.TemporaryDirectory(prefix='vnx-cpp-compat-') as temporary:
    root = Path(temporary)
    oracle = root / 'oracle'
    subprocess.run([args.cxx, '-std=c++17', '-O2', *[f'-I{p}' for p in includes],
                    str(repo / 'tests/cpp/oracle.cpp'), '-o', str(oracle),
                    f'-L{build}', '-lmmx_iface', f'-L{build / "vnx-base"}', '-lvnx_base',
                    f'-Wl,-rpath,{build}', f'-Wl,-rpath,{build / "vnx-base"}'], check=True)
    network = root / 'network'
    subprocess.run([args.cxx, '-std=c++17', '-O2', *[f'-I{p}' for p in includes],
                    str(repo / 'tests/cpp/network.cpp'), '-o', str(network),
                    f'-L{build}', '-lmmx_iface', f'-L{build / "vnx-base"}', '-lvnx_base',
                    f'-Wl,-rpath,{build}', f'-Wl,-rpath,{build / "vnx-base"}'], check=True)
    subprocess.run([str(oracle), 'fixtures', str(root)], check=True)
    subprocess.run(['cargo', 'run', '--locked', '--offline', '-q', '-p', 'vnx', '--example',
                    'conformance', '--', str(root)], cwd=repo, check=True)
    for name in ['key', 'log', 'params', 'block', 'request', 'object']:
        expected = json.loads((root / f'{name}.json').read_text())
        for suffix in (['rust', 'rust-big'] if name == 'object' else ['rust']):
            actual = json.loads(subprocess.check_output([str(oracle), 'read', str(root / f'{name}.{suffix}.dat')], text=True))
            assert actual == expected, f'C++ decoded different value for {name}/{suffix}'
    import os
    env = dict(os.environ, VNX_CPP_NETWORK_ORACLE=str(network))
    subprocess.run(['cargo', 'test', '--locked', '--offline', '-p', 'vnx', '--test',
                    'cpp_network', '--', '--ignored'], cwd=repo, env=env, check=True)
print('C++ accepted all 7 Rust files; all live C++ peer and Proxy checks passed.')
