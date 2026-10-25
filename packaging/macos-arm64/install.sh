#!/bin/bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./install.sh [--prefix DIR] [--no-shell]
Offline installation for Apple Silicon, macOS 13.5 or newer; no sudo required.
Default: ~/.local/bin/{vis,vise}, ~/.config/vis/visrc.lua, zsh aliases.
Existing files are backed up before replacement. XDG_CONFIG_HOME and ZDOTDIR
are respected. --no-shell leaves the shell configuration untouched.
EOF
}
fail() { printf 'Error: %s\n' "$*" >&2; exit 1; }
prefix="${HOME}/.local"
shell_setup=true
while (($#)); do
  case "$1" in
    --prefix) (($# >= 2)) || fail '--prefix needs a directory'; prefix=$2; shift 2 ;;
    --no-shell) shell_setup=false; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "Unknown argument: $1" ;;
  esac
done
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] ||
  fail 'Use a native arm64 terminal on an Apple Silicon Mac (not Rosetta).'
os_version=$(sw_vers -productVersion)
IFS=. read -r major minor _ <<< "$os_version"
(( major > 13 || (major == 13 && minor >= 5) )) || fail 'macOS 13.5+ is required.'
[[ $prefix = /* && $prefix != *:* && $prefix != *$'\n'* ]] ||
  fail '--prefix must be an absolute path without colons or newlines.'
bundle=$(cd -- "$(dirname -- "$0")" && pwd -P)
[[ -f "$bundle/CHECKSUMS.sha256" ]] || fail 'Extract the complete release archive first.'
printf 'Verifying package checksums…\n'
(cd "$bundle" && shasum -a 256 -c CHECKSUMS.sha256 > /dev/null) || fail 'Package checksum mismatch.'
version=$(cat "$bundle/VERSION")
[[ $version != *[!a-zA-Z0-9._-]* && -n $version ]] || fail 'Invalid package version.'
base="$prefix/share/vis-quick"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/vis"
shell_rc="${ZDOTDIR:-$HOME}/.zshrc"
mkdir -p "$base/versions" "$base/backups" "$prefix/bin" "$config_dir"
[[ ! -e "$base/current" || -L "$base/current" ]] || fail "$base/current must be a symlink."
for destination in "$prefix/bin/vis" "$prefix/bin/vise" "$config_dir/visrc.lua"; do
  [[ ! -d $destination ]] || fail "Refusing to replace a directory: $destination"
done

# Serialize installs without touching any other user's process or lock.
mkdir "$base/install.lock" 2>/dev/null || fail "Another install holds $base/install.lock"
stage=''
shell_tmp=''
cleanup() {
  [[ -z $shell_tmp ]] || rm -f -- "$shell_tmp"
  [[ -z $stage ]] || rm -rf -- "$stage"
  rmdir "$base/install.lock"
}
trap cleanup EXIT
stage=$(mktemp -d "$base/versions/$version.XXXXXX")
cp -R "$bundle/payload/." "$stage/"
"$stage/launch.sh" -v
"$stage/tools/bin/node" --version
"$stage/tools/bin/lua-language-server" --version

backup=$(mktemp -d "$base/backups/$(date +%Y%m%d-%H%M%S).XXXXXX")
backup_file() {
  if [[ -e $1 || -L $1 ]]; then cp -pP "$1" "$backup/$2"; fi
}
backup_file "$prefix/bin/vis" vis
backup_file "$prefix/bin/vise" vise
backup_file "$config_dir/visrc.lua" visrc.lua
if [[ -L "$base/current" ]]; then readlink "$base/current" > "$backup/previous-current"; fi
if $shell_setup; then
  mkdir -p "$(dirname -- "$shell_rc")"
  # Shell dotfiles are often symlinks; save their content before writing through
  # the link so the backup cannot change along with its original target.
  if [[ -e "$shell_rc" ]]; then cp -pL "$shell_rc" "$backup/zshrc"; fi
  shell_tmp=$(mktemp "${shell_rc}.vis-quick.XXXXXX")
  if [[ -f "$shell_rc" ]]; then
    awk '
      /^# >>> vis-quick >>>$/ { if (skip) exit 2; skip=1; next }
      /^# <<< vis-quick <<<$/{ if (!skip) exit 2; skip=0; next }
      !skip { print }
      END { if (skip) exit 2 }
    ' "$shell_rc" > "$shell_tmp" || fail 'Unbalanced vis-quick block in .zshrc; preserved original.'
    chmod "$(stat -f %Lp "$shell_rc")" "$shell_tmp"
  fi
  {
    printf '\n# >>> vis-quick >>>\n'
    printf 'export PATH=%q/bin:"$PATH"\n' "$prefix"
    # Quote twice: once for the alias definition, once for the command it expands.
    printf -v vis_command '%q' "$prefix/bin/vis"
    printf -v vise_command '%q' "$prefix/bin/vise"
    printf 'alias vis=%q\nalias vise=%q\n' "$vis_command" "$vise_command"
    printf '# <<< vis-quick <<<\n'
  } >> "$shell_tmp"
  /bin/zsh -n "$shell_tmp"
fi

# Build launchers before replacing any user file. They resolve the managed
# current version at execution time, and keep working after an upgrade.
{
  printf '#!/bin/bash\nset -euo pipefail\n'
  printf 'exec %q "$@"\n' "$base/current/launch.sh"
} > "$stage/user-launcher"
chmod 755 "$stage/user-launcher"
ln -sfn "$stage" "$base/current"
installed=$stage
stage=''
# Remove symlinks before copying, so an existing link target is never overwritten.
for name in vis vise; do
  rm -f -- "$prefix/bin/$name"
  cp "$installed/user-launcher" "$prefix/bin/$name"
done
rm -f -- "$config_dir/visrc.lua"
cp "$installed/config/visrc.lua" "$config_dir/visrc.lua"
if $shell_setup; then cat "$shell_tmp" > "$shell_rc"; fi
printf '\nInstalled %s\nBackup: %s\n' "$version" "$backup"
printf 'Start: %q your-file.py\n' "$prefix/bin/vise"
if $shell_setup; then printf 'Reload aliases: source %q\n' "$shell_rc"; fi
if ! /usr/bin/xcode-select -p >/dev/null 2>&1; then
  printf 'C/C++ LSP: install Apple Command Line Tools with xcode-select --install\n'
fi
