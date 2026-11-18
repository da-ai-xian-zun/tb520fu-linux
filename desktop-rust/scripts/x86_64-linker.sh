#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
set -euo pipefail

FC_X86_DIR="${TMPDIR:-/tmp}/tb520fu-x86_64-fc-stub"
if [[ ! -f "${FC_X86_DIR}/libfontconfig.so" ]] && [[ -f /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 ]]; then
    mkdir -p "${FC_X86_DIR}"
    ln -sf /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 "${FC_X86_DIR}/libfontconfig.so"
fi

exec cc -L"${FC_X86_DIR}" "$@"
