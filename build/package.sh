#!/usr/bin/env bash
#
# 生产打包：在构建机编译，组装成可上传、解包即用的部署包。
#
# API 二进制把编译期仓库路径写死：它只在 <编译时仓库根>/apps/web/dist 找前端产物
# （docs/operations/production.md §1.1）。所以本脚本用 bubblewrap 把仓库挂到
# /opt/seeai 再编译，把 /opt/seeai 编进二进制；产出的包也只能解到 /opt/seeai。
#
# 前置：Node 24、Rust 1.94+、build-essential cmake perl pkg-config、bubblewrap、binutils。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
# 编译期路径与解包路径都必须是它，见文件头。
readonly PREFIX="/opt/seeai"
readonly TARGET_DIR="$REPO_ROOT/build/.cache/target"

OUT_DIR="$REPO_ROOT/build/dist"
VERSION=""
SKIP_WEB=0
SKIP_BUILD=0

usage() {
    cat <<'EOF'
用法：build/package.sh [--version <标签>] [--out <目录>] [--skip-web] [--skip-build]

  --version <标签>  包名里的版本；缺省用 git describe
  --out <目录>      输出目录；缺省 build/dist
  --skip-web        复用已有的 apps/web/dist，不重新构建前端
  --skip-build      复用已有的 build/.cache/target/release 二进制，不重新编译

前置：Node 24、Rust 1.94+、build-essential cmake perl pkg-config、bubblewrap、binutils。
产出：<out>/seeai-<版本>.tar.gz，解包到 /opt/seeai（见 docs/operations/production.md §2.2）。
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version 需要值}"; shift 2 ;;
        --out) OUT_DIR="${2:?--out 需要值}"; shift 2 ;;
        --skip-web) SKIP_WEB=1; shift ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "未知选项：$1" >&2; usage >&2; exit 2 ;;
    esac
done

require() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "缺少命令 $1。$2" >&2
        exit 1
    }
}

require node "装 Node.js 24。"
require npm "随 Node.js 一起装。"
require cargo "装 Rust 1.94+（rustup）。"
require cc "装 C 工具链：sudo apt-get install -y build-essential。"
require cmake "aws-lc-rs 要从源码构建：sudo apt-get install -y cmake。"
require perl "aws-lc-rs 的构建脚本要用：sudo apt-get install -y perl。"
require pkg-config "sudo apt-get install -y pkg-config。"
require bwrap "装 bubblewrap：sudo apt-get install -y bubblewrap。"
require objdump "随 binutils 装：sudo apt-get install -y binutils。"
require tar "随系统装。"

if [ -z "$VERSION" ]; then
    VERSION="$(git -C "$REPO_ROOT" describe --tags --always --dirty 2>/dev/null || true)"
    [ -n "$VERSION" ] || VERSION="0.0.0"
fi
VERSION="$(printf '%s' "$VERSION" | tr -c 'A-Za-z0-9._-' '-')"

if [ "$SKIP_WEB" -eq 0 ]; then
    echo "== 构建前端产物 =="
    npm --prefix "$REPO_ROOT/apps/web" ci
    npm --prefix "$REPO_ROOT/apps/web" run build
else
    echo "== 跳过前端构建，复用 apps/web/dist =="
fi
[ -f "$REPO_ROOT/apps/web/dist/console.html" ] || {
    echo "apps/web/dist 里没有 console.html：先构建前端，或去掉 --skip-web。" >&2
    exit 1
}

if [ "$SKIP_BUILD" -eq 0 ]; then
    echo "== 编译 Rust 二进制（编译期路径 $PREFIX）=="
    [ -d "$(dirname "$PREFIX")" ] || {
        echo "宿主没有 $(dirname "$PREFIX")：bubblewrap 需要它作挂载点。" >&2
        exit 1
    }
    mkdir -p "$TARGET_DIR"
    # --tmpfs 盖掉宿主 /opt，只在沙箱里造 $PREFIX；仓库挂进去，CARGO_MANIFEST_DIR 才落在生产路径上。
    bwrap --dev-bind / / --tmpfs "$(dirname "$PREFIX")" --bind "$REPO_ROOT" "$PREFIX" --chdir "$PREFIX" env CARGO_TARGET_DIR="$PREFIX/build/.cache/target" cargo build --release --locked -p seeai-api -p seeai-worker
else
    echo "== 跳过编译，复用 $TARGET_DIR/release =="
fi

readonly API_BIN="$TARGET_DIR/release/seeai-api"
readonly WORKER_BIN="$TARGET_DIR/release/seeai-worker"
for bin in "$API_BIN" "$WORKER_BIN"; do
    [ -x "$bin" ] || {
        echo "找不到可执行文件 $bin。" >&2
        exit 1
    }
done

# 二进制里必须留下编译期路径。没有它，包解到 $PREFIX 也找不到前端。
if ! grep -aq "$PREFIX/apps/api" "$API_BIN"; then
    echo "二进制里没有编译期路径 $PREFIX/apps/api：编译时仓库不在 $PREFIX，前端会丢。" >&2
    exit 1
fi

MAX_GLIBC="$(objdump -T "$API_BIN" "$WORKER_BIN" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -n 1 || true)"
[ -n "$MAX_GLIBC" ] || {
    echo "量不出二进制要求的 GLIBC 版本；objdump 或链接方式不对。" >&2
    exit 1
}

echo "== 组装包 =="
STAGE_NAME="seeai-$VERSION"
STAGE="$OUT_DIR/$STAGE_NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/target/release" "$STAGE/apps/api" "$STAGE/apps/web" "$STAGE/config" "$STAGE/deploy/systemd"

install -m 755 "$API_BIN" "$STAGE/target/release/seeai-api"
install -m 755 "$WORKER_BIN" "$STAGE/target/release/seeai-worker"
cp -a "$REPO_ROOT/apps/web/dist" "$STAGE/apps/web/dist"
cp -a "$REPO_ROOT/public-docs" "$STAGE/public-docs"
cp -a "$REPO_ROOT/config/bootstrap" "$STAGE/config/bootstrap"
install -m 644 "$REPO_ROOT/deploy/systemd/seeai-api.service" "$STAGE/deploy/systemd/seeai-api.service"
install -m 644 "$REPO_ROOT/deploy/systemd/seeai-worker.service" "$STAGE/deploy/systemd/seeai-worker.service"

GIT_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
GIT_DIRTY="no"
if [ -n "$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null || true)" ]; then
    GIT_DIRTY="yes"
fi
cat > "$STAGE/PACKAGE.txt" <<EOF
version: $VERSION
built-at: $(date -u +%Y-%m-%dT%H:%M:%SZ)
git-commit: $GIT_COMMIT
git-dirty: $GIT_DIRTY
install-prefix: $PREFIX
arch: $(uname -m)
required-glibc: $MAX_GLIBC
EOF

mkdir -p "$OUT_DIR"
TARBALL="$OUT_DIR/$STAGE_NAME.tar.gz"
rm -f "$TARBALL" "$TARBALL.sha256"
tar -C "$OUT_DIR" -czf "$TARBALL" "$STAGE_NAME"
( cd "$OUT_DIR" && sha256sum "$STAGE_NAME.tar.gz" > "$STAGE_NAME.tar.gz.sha256" )

echo
echo "包：        $TARBALL"
echo "大小：      $(du -h "$TARBALL" | cut -f1)"
echo "SHA-256：   $(cut -d' ' -f1 "$TARBALL.sha256")"
echo "要求 glibc：$MAX_GLIBC（服务器不低于它）"
echo
echo "解包到 $PREFIX："
echo "  sudo install -d -m 755 $PREFIX"
echo "  sudo tar -xzf $STAGE_NAME.tar.gz -C $PREFIX --strip-components=1"
