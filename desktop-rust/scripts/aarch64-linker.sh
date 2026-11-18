#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
set -euo pipefail

# Optional arm64 sysroot: $TB520FU_ARM64_SYSROOT_LIB, or a (gitignored) .arm64-sysroot
# symlink at the repository root. Without one, the cross gcc's own libraries are used.
SYSROOT_LIB="${TB520FU_ARM64_SYSROOT_LIB:-$(dirname "$(realpath "$0")")/../.arm64-sysroot/usr/aarch64-linux-gnu/lib}"
FC_STUB_DIR="${TMPDIR:-/tmp}/tb520fu-arm64-fc-stub"

if [[ ! -f "${FC_STUB_DIR}/libfontconfig.so" ]]; then
    mkdir -p "${FC_STUB_DIR}"
    cat > "${FC_STUB_DIR}/fc_stub.c" <<'EOF'
void FcCharSetAddChar(void) {}
void FcCharSetCreate(void) {}
void FcCharSetDestroy(void) {}
void FcConfigBuildFonts(void) {}
void FcConfigDestroy(void) {}
void FcConfigGetFonts(void) {}
void FcConfigSubstitute(void) {}
void FcFontMatch(void) {}
void FcFontRenderPrepare(void) {}
void FcFontSetDestroy(void) {}
void FcFontSort(void) {}
void FcInitLoadConfig(void) {}
void FcLangSetAdd(void) {}
void FcLangSetCreate(void) {}
void FcLangSetDestroy(void) {}
void FcPatternAddCharSet(void) {}
void FcPatternAddLangSet(void) {}
void FcPatternAddString(void) {}
void FcPatternCreate(void) {}
void FcPatternDestroy(void) {}
void FcPatternGetInteger(void) {}
void FcPatternGetString(void) {}
void FcPatternReference(void) {}
EOF
    aarch64-linux-gnu-gcc -shared -nostdlib -fPIC -Wl,-soname,libfontconfig.so.1 \
        -o "${FC_STUB_DIR}/libfontconfig.so.1" "${FC_STUB_DIR}/fc_stub.c"
    ln -sf libfontconfig.so.1 "${FC_STUB_DIR}/libfontconfig.so"
fi

if [[ -f "${SYSROOT_LIB}/Scrt1.o" ]]; then
    exec aarch64-linux-gnu-gcc -B"${SYSROOT_LIB}" -L"${SYSROOT_LIB}" -L"${FC_STUB_DIR}" -Wl,--allow-shlib-undefined "$@"
else
    exec aarch64-linux-gnu-gcc -L"${FC_STUB_DIR}" -Wl,--allow-shlib-undefined "$@"
fi
