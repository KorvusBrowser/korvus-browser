#!/usr/bin/env python3
"""make_bodies.py: writes this directory's coded bodies (`*.bin`) and `cases.json` -- run once; the files are the
fixture, served byte for byte by `record.py` (Chrome) and by `tests/content_coding_live.rs` (korvus).

A truncated stream is cut inside a STORED (level 0) block: every decoder hands out exactly the stored bytes it was
given, so a partial body's length is the stream's, not an implementation's buffering."""
import gzip, io, json, os, zlib
import brotli, zstandard

HERE = os.path.dirname(os.path.abspath(__file__))
TEXT = b''.join(b'korvus content coding line %03d\n' % i for i in range(40))   # 1240 bytes
OTHER = b'the second gzip member\n' * 4

def gz(data, level=6):
    buf = io.BytesIO()
    with gzip.GzipFile(fileobj=buf, mode='wb', compresslevel=level, mtime=0) as f:
        f.write(data)
    return buf.getvalue()

def raw_deflate(data, level=6):
    c = zlib.compressobj(level, zlib.DEFLATED, -15)
    return c.compress(data) + c.flush()

def zl(data, level=6):
    return zlib.compress(data, level)

full = gz(TEXT)
stored = gz(TEXT, 0)
bad_crc = bytearray(full); bad_crc[-8] ^= 0xff
bodies = {
    'gz': full,
    'gz-trunc': stored[:600],                 # header, then part of a stored block
    'gz-header-only': stored[:5],             # inside the header
    'gz-badcrc': bytes(bad_crc),
    'gz-two': full + gz(OTHER),
    'gz-junk-after': full + b'trailing junk after the member',
    'deflate-zlib': zl(TEXT),
    'deflate-raw': raw_deflate(TEXT),
    'deflate-zlib-trunc': zl(TEXT, 0)[:700],
    'br': brotli.compress(TEXT),
    'zstd': zstandard.ZstdCompressor(level=3).compress(TEXT),
    'garbage': b'this is not a gzip stream at all, not even close',
}
for k, v in bodies.items():
    open(os.path.join(HERE, k + '.bin'), 'wb').write(v)

# path -> [method, [Content-Encoding values (one header line each)], body file]
cases = {
    'head-gz': ['HEAD', ['gzip'], 'gz'],
    'empty-gz': ['GET', ['gzip'], None],
    'gz': ['GET', ['gzip'], 'gz'],
    'gz-trunc': ['GET', ['gzip'], 'gz-trunc'],
    'gz-header-only': ['GET', ['gzip'], 'gz-header-only'],
    'gz-badcrc': ['GET', ['gzip'], 'gz-badcrc'],
    'gz-two': ['GET', ['gzip'], 'gz-two'],
    'gz-junk-after': ['GET', ['gzip'], 'gz-junk-after'],
    'gz-identity': ['GET', ['gzip, identity'], 'gz'],
    'gz-empty-token': ['GET', ['gzip,'], 'gz'],
    'identity-gz': ['GET', ['identity', 'gzip'], 'gz'],
    'x-gzip': ['GET', ['X-GZIP'], 'gz'],
    'deflate-zlib': ['GET', ['deflate'], 'deflate-zlib'],
    'deflate-raw': ['GET', ['deflate'], 'deflate-raw'],
    'deflate-zlib-trunc': ['GET', ['deflate'], 'deflate-zlib-trunc'],
    'br': ['GET', ['br'], 'br'],
    'empty-br': ['GET', ['br'], None],
    'zstd': ['GET', ['zstd'], 'zstd'],
    'empty-zstd': ['GET', ['zstd'], None],
    'garbage-gz': ['GET', ['gzip'], 'garbage'],
    'head-garbage-gz': ['HEAD', ['gzip'], 'garbage'],
}
json.dump(cases, open(os.path.join(HERE, 'cases.json'), 'w'), indent=1)
print({k: len(v) for k, v in bodies.items()})
