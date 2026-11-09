#!/usr/bin/env bash
# slicenv.svg → slicenv.ico (16…256 px) для ресурсів slicenv.exe / slicenv-mcp.exe і Inno Setup.
# Готовий .ico лежить у репозиторії, тож збірка цих інструментів не потребує; запускати
# лише після зміни SVG. Потрібні rsvg-convert (librsvg) і ImageMagick 7 (`magick`).
# Кожен розмір рендериться з SVG окремо (без масштабування растру), 256 px — PNG усередині ICO.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
svg=$root/crates/app/assets/app/slicenv.svg
ico=$root/crates/app/assets/app/slicenv.ico
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pngs=()
for size in 16 20 24 32 40 48 64 256; do
    rsvg-convert --width "$size" --height "$size" --keep-aspect-ratio "$svg" -o "$tmp/$size.png"
    pngs+=("$tmp/$size.png")
done
# 16…64 — BMP-записи (так їх читає будь-який завантажувач), 256 — PNG як є (Vista+).
# -define icon:auto-resize не вживаємо: він перемасштабував би один растр.
magick "${pngs[@]:0:7}" "$tmp/small.ico"
python3 -I - "$tmp/small.ico" "$tmp/256.png" "$ico" <<'PY'
import struct, sys
small, png, out = (open(sys.argv[1], "rb").read(), open(sys.argv[2], "rb").read(), sys.argv[3])
count = struct.unpack_from("<HHH", small)[2]
entries = []
for i in range(count):
    w, h, colors, _, planes, bits, size, offset = struct.unpack_from("<BBBBHHII", small, 6 + 16 * i)
    entries.append((w, h, colors, planes, bits, small[offset:offset + size]))
entries.append((0, 0, 0, 1, 32, png))  # 0 у каталозі ICO означає 256
offset = 6 + 16 * len(entries)
head, body = struct.pack("<HHH", 0, 1, len(entries)), b""
for w, h, colors, planes, bits, data in entries:
    head += struct.pack("<BBBBHHII", w, h, colors, 0, planes, bits, len(data), offset)
    offset += len(data)
    body += data
open(out, "wb").write(head + body)
PY
magick identify "$ico"
