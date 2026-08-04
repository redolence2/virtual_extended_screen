#!/usr/bin/env bash
set -euo pipefail

# Generate protobuf code for both Swift and Rust from proto/ definitions.
# Uses local protoc + plugins installed in tools/bin/ — no system-wide installs.
#
# Modes:
#   generate_proto.sh          — (re)generate Swift sources into mac-host/Sources/Protocol
#   generate_proto.sh --check  — regenerate into a temp dir and diff against the
#                                committed Swift sources; nonzero exit on drift.
#                                (CI regen-clean check, plan v11 §12 A0.0.)
#
# Rust codegen runs inside cargo via prost-build (crates/protocol/build.rs);
# this script only validates the .proto files for Rust consumers.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TOOLS_BIN="$SCRIPT_DIR/bin"
PROTO_DIR="$PROJECT_ROOT/proto"

# Pinned versions. SWIFT_PROTOBUF_VERSION MUST match the runtime pin in
# mac-host/Package.resolved (plan v11 §11.6 / review-7 M4).
PROTOC_VERSION="27.3"
SWIFT_PROTOBUF_VERSION="1.36.1"

SWIFT_OUT="$PROJECT_ROOT/mac-host/Sources/Protocol"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
log() { echo -e "${GREEN}[proto]${NC} $*"; }
warn() { echo -e "${YELLOW}[proto]${NC} $*"; }
err() { echo -e "${RED}[proto]${NC} $*" >&2; }

ensure_protoc() {
    local protoc="$TOOLS_BIN/protoc"
    if [[ -x "$protoc" ]]; then
        local ver
        ver=$("$protoc" --version 2>/dev/null | grep -oE '[0-9]+(\.[0-9]+)+' | head -1)
        if [[ "$ver" == "$PROTOC_VERSION" ]]; then
            log "protoc v$ver (pinned) found"
            return 0
        fi
        warn "protoc v$ver != pinned v$PROTOC_VERSION — reinstalling"
        rm -f "$protoc"
    fi

    log "Installing protoc v${PROTOC_VERSION} to $TOOLS_BIN..."
    mkdir -p "$TOOLS_BIN"

    local os arch zip_name url
    os="$(uname -s)"; arch="$(uname -m)"
    case "$os" in
        Darwin)
            case "$arch" in
                arm64) zip_name="protoc-${PROTOC_VERSION}-osx-aarch_64.zip" ;;
                x86_64) zip_name="protoc-${PROTOC_VERSION}-osx-x86_64.zip" ;;
                *) err "Unsupported arch: $arch"; return 1 ;;
            esac ;;
        Linux)
            case "$arch" in
                x86_64|amd64) zip_name="protoc-${PROTOC_VERSION}-linux-x86_64.zip" ;;
                aarch64|arm64) zip_name="protoc-${PROTOC_VERSION}-linux-aarch_64.zip" ;;
                *) err "Unsupported arch: $arch"; return 1 ;;
            esac ;;
        *) err "Unsupported OS: $os"; return 1 ;;
    esac

    url="https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/${zip_name}"
    local tmpdir; tmpdir=$(mktemp -d)
    curl -sSL "$url" -o "$tmpdir/protoc.zip"
    unzip -qo "$tmpdir/protoc.zip" -d "$tmpdir/protoc"
    cp "$tmpdir/protoc/bin/protoc" "$TOOLS_BIN/protoc"
    chmod +x "$TOOLS_BIN/protoc"
    mkdir -p "$SCRIPT_DIR/include"
    cp -r "$tmpdir/protoc/include/"* "$SCRIPT_DIR/include/" 2>/dev/null || true
    rm -rf "$tmpdir"
    log "protoc v${PROTOC_VERSION} installed"
}

# The plugin is only trusted if its --version matches the pin exactly
# (review-9 nonblocking note: a preinstalled plugin of unknown version must
# not be silently accepted).
ensure_swift_plugin() {
    local plugin="$TOOLS_BIN/protoc-gen-swift"
    if [[ -x "$plugin" ]]; then
        local pver
        pver=$("$plugin" --version 2>/dev/null | grep -oE '[0-9]+(\.[0-9]+)+' | head -1 || true)
        if [[ "$pver" == "$SWIFT_PROTOBUF_VERSION" ]]; then
            log "protoc-gen-swift v$pver (pinned) found"
            return 0
        fi
        warn "protoc-gen-swift v${pver:-unknown} != pinned v$SWIFT_PROTOBUF_VERSION — rebuilding"
        rm -f "$plugin"
    fi

    log "Building protoc-gen-swift v${SWIFT_PROTOBUF_VERSION} (requires swift)..."
    if ! command -v swift &>/dev/null; then
        err "swift not found — cannot build protoc-gen-swift"
        return 1
    fi

    local tmpdir; tmpdir=$(mktemp -d)
    # --recurse-submodules: the 1.36.x manifest references sources from the
    # upstream protobuf submodule; without it SwiftPM fails manifest resolution
    # ("target 'protoc' referenced in product 'protoc' is empty").
    git clone --depth 1 --branch "${SWIFT_PROTOBUF_VERSION}" \
        --recurse-submodules --shallow-submodules \
        https://github.com/apple/swift-protobuf.git "$tmpdir/swift-protobuf"
    (cd "$tmpdir/swift-protobuf" && swift build -c release --product protoc-gen-swift)
    cp "$tmpdir/swift-protobuf/.build/release/protoc-gen-swift" "$TOOLS_BIN/"
    chmod +x "$TOOLS_BIN/protoc-gen-swift"
    rm -rf "$tmpdir"
    log "protoc-gen-swift v${SWIFT_PROTOBUF_VERSION} installed"
}

generate_swift_into() {
    local out_dir="$1"
    mkdir -p "$out_dir"
    "$TOOLS_BIN/protoc" \
        --proto_path="$PROTO_DIR" \
        --plugin="protoc-gen-swift=$TOOLS_BIN/protoc-gen-swift" \
        --swift_out="$out_dir" \
        --swift_opt=Visibility=Public \
        "$PROTO_DIR"/*.proto
}

validate_protos() {
    "$TOOLS_BIN/protoc" --proto_path="$PROTO_DIR" \
        --descriptor_set_out=/dev/null "$PROTO_DIR"/*.proto
    log "All .proto files validate"
}

main() {
    local check_mode=0
    [[ "${1:-}" == "--check" ]] && check_mode=1

    log "Proto source: $PROTO_DIR"
    log "Proto files: $(ls "$PROTO_DIR"/*.proto 2>/dev/null | xargs -n1 basename | tr '\n' ' ')"

    ensure_protoc
    validate_protos
    ensure_swift_plugin

    if [[ $check_mode -eq 1 ]]; then
        local tmpout; tmpout=$(mktemp -d)
        generate_swift_into "$tmpout"
        if diff -r -q "$tmpout" "$SWIFT_OUT" >/dev/null 2>&1; then
            log "regen-clean: committed Swift sources match generator output"
            rm -rf "$tmpout"
        else
            err "regen drift: committed Swift sources differ from generator output:"
            diff -r -q "$tmpout" "$SWIFT_OUT" || true
            rm -rf "$tmpout"
            exit 1
        fi
    else
        log "Generating Swift code → $SWIFT_OUT"
        generate_swift_into "$SWIFT_OUT"
        log "Swift protobuf generated: $(ls "$SWIFT_OUT"/*.swift 2>/dev/null | wc -l | tr -d ' ') files"
    fi

    log "Rust protobuf: generated via prost-build in crates/protocol/build.rs (validated above)"
    log "Done."
}

main "$@"
