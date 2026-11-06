"""打包发行产物：dist/PromptForge.exe + dist/PromptForge-<版本>-win64.zip

用法（在 prompt-forge-web/ 下执行）:
    python packaging/package.py [--version 1.1.2]

流程:
1. 从 target/release/prompt-forge.exe 拷贝到 dist/PromptForge.exe
2. 以 packaging/README.txt + dist/PromptForge.exe 组装 zip

zip 内为扁平结构（无外层目录），解压即用。
"""
import argparse
import hashlib
import os
import shutil
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
SRC_EXE = ROOT / "target" / "release" / "prompt-forge.exe"
README = ROOT / "packaging" / "README.txt"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", default="1.1.2", help="发行版本号")
    args = ap.parse_args()

    if not SRC_EXE.exists():
        print(f"[错误] 未找到构建产物: {SRC_EXE}\n       请先执行: cargo build --release")
        return 1
    if not README.exists():
        print(f"[错误] 未找到打包说明: {README}")
        return 1

    DIST.mkdir(parents=True, exist_ok=True)

    # 1. 拷贝 exe
    dst_exe = DIST / "PromptForge.exe"
    shutil.copy2(SRC_EXE, dst_exe)
    exe_sha = sha256(dst_exe)

    # 2. 组装 zip（同名旧包先清掉，避免残留）
    zip_path = DIST / f"PromptForge-{args.version}-win64.zip"
    if zip_path.exists():
        zip_path.unlink()
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(dst_exe, "PromptForge.exe")
        z.write(README, "README.txt")

    zip_sha = sha256(zip_path)
    print("打包完成")
    print(f"  exe : {dst_exe}  {dst_exe.stat().st_size:,} B  sha256={exe_sha[:16]}…")
    print(f"  zip : {zip_path}  {zip_path.stat().st_size:,} B  sha256={zip_sha[:16]}…")
    return 0


if __name__ == "__main__":
    sys.exit(main())
