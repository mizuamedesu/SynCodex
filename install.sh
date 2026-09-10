#!/bin/sh
# Install SynCodex and its Codex / Claude Code skills. No sudo or Rust required.
set -eu

sc_error() { printf 'syncodex installer: %s\n' "$*" >&2; exit 1; }
sc_fetch() {
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --retry 3 --connect-timeout 15 --max-time 180 -o "$2" "$1"
}
sc_quote() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}
sc_replace() {
    if [ -L "$2/$3" ] || [ -d "$2/$3" ]; then sc_error "Not a regular installation target: $2/$3"; fi
    _sc_tmpfile=$(mktemp "$2/.syncodex-install.XXXXXX")
    cp "$1" "$_sc_tmpfile"
    chmod "$4" "$_sc_tmpfile"
    mv -f "$_sc_tmpfile" "$2/$3"
}
sc_profile() {
    [ ! -L "$1" ] || sc_error "Refusing to modify symlinked profile: $1 (use --no-modify-path)"
    if ! grep -Fqx "$_sc_source_line" "$1" 2>/dev/null; then
        printf '\n# SynCodex\n%s\n' "$_sc_source_line" >> "$1"
    fi
}
sc_main() {
    _sc_version=${SYNCODEX_VERSION:-latest}
    _sc_bindir=${SYNCODEX_BIN_DIR:-$HOME/.local/bin}
    _sc_codex_dir=${SYNCODEX_CODEX_SKILLS_DIR:-$HOME/.agents/skills}/syncodex
    _sc_claude_dir=${SYNCODEX_CLAUDE_SKILLS_DIR:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills}/syncodex
    _sc_skills=yes
    _sc_path=yes
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version) [ "$#" -ge 2 ] || sc_error '--version needs a value'; _sc_version=$2; shift 2 ;;
            --bin-dir) [ "$#" -ge 2 ] || sc_error '--bin-dir needs a value'; _sc_bindir=$2; shift 2 ;;
            --no-skills) _sc_skills=no; shift ;;
            --no-modify-path) _sc_path=no; shift ;;
            --help) printf '%s\n' 'Usage: sh install.sh [--version vX.Y.Z] [--bin-dir PATH] [--no-skills] [--no-modify-path]'; return ;;
            *) sc_error "Unknown option: $1" ;;
        esac
    done
    case "$_sc_bindir" in /*) ;; *) sc_error '--bin-dir must be absolute' ;; esac
    for _sc_tool in curl tar awk sed mktemp uname; do
        command -v "$_sc_tool" >/dev/null 2>&1 || sc_error "Required command missing: $_sc_tool"
    done
    _sc_os=$(uname -s)
    _sc_arch=$(uname -m)
    case "$_sc_os:$_sc_arch" in
        Linux:x86_64) _sc_target=x86_64-unknown-linux-musl ;;
        Darwin:arm64|Darwin:aarch64) _sc_target=aarch64-apple-darwin ;;
        Darwin:x86_64)
            [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || true)" = 1 ] || sc_error 'Intel Macs are not supported'
            _sc_target=aarch64-apple-darwin ;;
        *) sc_error "Unsupported platform: $_sc_os $_sc_arch" ;;
    esac
    _sc_repo=https://github.com/mizuamedesu/SynCodex
    if [ "$_sc_version" = latest ]; then
        _sc_url=$(curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
            --retry 3 --connect-timeout 15 --max-time 60 -o /dev/null -w '%{url_effective}' "$_sc_repo/releases/latest")
        _sc_version=${_sc_url##*/}
    fi
    printf '%s\n' "$_sc_version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([.-][a-zA-Z0-9.-]+)?$' || sc_error 'Invalid release version'
    _sc_asset=syncodex-$_sc_version-$_sc_target.tar.gz
    _sc_work=$(mktemp -d)
    trap 'rm -rf "$_sc_work"' EXIT
    trap 'exit 1' HUP INT TERM
    sc_fetch "$_sc_repo/releases/download/$_sc_version/$_sc_asset" "$_sc_work/$_sc_asset"
    sc_fetch "$_sc_repo/releases/download/$_sc_version/$_sc_asset.sha256" "$_sc_work/checksum"
    _sc_expected=$(awk 'NR == 1 { print $1 }' "$_sc_work/checksum")
    [ "${#_sc_expected}" -eq 64 ] || sc_error 'Invalid checksum file'
    case "$_sc_expected" in *[!0-9a-fA-F]*) sc_error 'Invalid checksum' ;; esac
    if command -v sha256sum >/dev/null 2>&1; then
        _sc_actual=$(sha256sum "$_sc_work/$_sc_asset" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        _sc_actual=$(shasum -a 256 "$_sc_work/$_sc_asset" | awk '{print $1}')
    else sc_error 'sha256sum or shasum is required'; fi
    [ "$_sc_expected" = "$_sc_actual" ] || sc_error 'Checksum mismatch; nothing installed'
    tar -xzf "$_sc_work/$_sc_asset" -C "$_sc_work" syncodex skills/syncodex/SKILL.md LICENSE
    for _sc_file in "$_sc_work/syncodex" "$_sc_work/skills/syncodex/SKILL.md"; do
        if [ ! -f "$_sc_file" ] || [ -L "$_sc_file" ]; then sc_error 'Invalid release archive'; fi
    done
    [ ! -L "$_sc_bindir/syncodex" ] || sc_error 'Refusing to replace a symlinked binary'
    if [ "$_sc_skills" = yes ]; then
        for _sc_dir in "$_sc_codex_dir" "$_sc_claude_dir"; do
            [ ! -L "$_sc_dir" ] || sc_error "Refusing to replace a symlinked skill: $_sc_dir"
            if [ -e "$_sc_dir" ] && [ ! -f "$_sc_dir/.syncodex-managed" ]; then
                sc_error "Unmanaged skill exists: $_sc_dir; move it aside or use --no-skills"
            fi
        done
    fi
    chmod 755 "$_sc_work/syncodex"
    "$_sc_work/syncodex" --version
    mkdir -p "$_sc_bindir"
    sc_replace "$_sc_work/syncodex" "$_sc_bindir" syncodex 755
    _sc_data=$HOME/.local/share/syncodex
    mkdir -p "$_sc_data"
    sc_replace "$_sc_work/LICENSE" "$_sc_data" LICENSE 644
    if [ "$_sc_skills" = yes ]; then
        for _sc_dir in "$_sc_codex_dir" "$_sc_claude_dir"; do
            mkdir -p "$_sc_dir"
            sc_replace "$_sc_work/skills/syncodex/SKILL.md" "$_sc_dir" SKILL.md 644
            printf '%s\n' "$_sc_version" > "$_sc_dir/.syncodex-managed"
            printf 'Installed skill: %s\n' "$_sc_dir"
        done
    fi
    if [ "$_sc_path" = yes ]; then
        _sc_quoted_bin=$(sc_quote "$_sc_bindir")
        # shellcheck disable=SC2016 # PATH is expanded when the generated file is sourced.
        printf 'case ":$PATH:" in *:%s:*) ;; *) export PATH=%s:"$PATH" ;; esac\n' "$_sc_quoted_bin" "$_sc_quoted_bin" > "$_sc_work/env.sh"
        sc_replace "$_sc_work/env.sh" "$_sc_data" env.sh 644
        _sc_source_line=". $(sc_quote "$_sc_data/env.sh")"
        sc_profile "$HOME/.profile"
        sc_profile "$HOME/.bashrc"
        sc_profile "$HOME/.zshrc"
        if [ -f "$HOME/.bash_profile" ]; then sc_profile "$HOME/.bash_profile"; fi
        printf 'For this terminal: %s\n' "$_sc_source_line"
    fi
    printf 'Installed %s to %s/syncodex\nRestart Codex / Claude Code to discover the skill.\n' "$_sc_version" "$_sc_bindir"
}
sc_main "$@"
