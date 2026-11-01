#!/bin/sh
# Checks the Cargo workspace layout statically: which crate may use what, and the test-only
# guards that keep `cargo test` off the host. Needs only sh, grep and awk, and builds nothing.
#
#   sh tests/workspace/layout-test.sh

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
core=$root/core
tui=$root/tui
gui=$root/gui

failures=0
check() {   # check <what> <command>...: one line per check, counted
  what=$1
  shift
  if "$@"; then
    printf 'ok   %s\n' "$what"
  else
    printf 'FAIL %s\n' "$what"
    failures=$((failures + 1))
  fi
}
not() { ! "$@"; }
has() { grep -Eq "$1" "$2" 2>/dev/null; }
# Any match in Rust code under a folder, comment lines left out.
code_has() {   # code_has <regex> <dir>
  find "$2" -name '*.rs' -type f -exec cat {} + 2>/dev/null | grep -v '^[[:space:]]*//' | grep -Eq "$1"
}

# The `[section]` header each line of a TOML file sits under, as "<section>\t<line>".
sections() {
  awk '/^[[:space:]]*\[/ { s = $0; gsub(/[[:space:]]/, "", s) } { print s "\t" $0 }' "$1"
}
# `test-support` appears only in [dev-dependencies], never in [dependencies] or `default`.
support_only_in_dev() {   # support_only_in_dev <Cargo.toml>
  ! sections "$1" | grep -v '^\[dev-dependencies\]' | grep -q 'test-support'
}
section_has() {   # section_has <Cargo.toml> <section> <regex>: a line of that section matches
  sections "$1" | grep -F "$2	" | cut -f2- | grep -Eq "$3"
}

# Print test guards in core/src other than `cfg(any(test, feature = "test-support"))`, outside
# test modules. rustfmt ends an inline `#[cfg(test)] mod` with a `}` at the indent of `mod`.
bare_guards() {
  found=0
  test_files=$(awk '
    pending && /^[[:space:]]*mod [a-z_0-9]+;/ { m = $0; sub(/^[[:space:]]*mod /, "", m); sub(/;.*/, "", m); print m ".rs" }
    { pending = ($0 ~ /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/) }
  ' "$core/src/lib.rs")
  for f in "$core"/src/*.rs "$core"/src/*/*.rs; do
    [ -f "$f" ] || continue
    skip=0
    for t in $test_files; do
      [ "$(basename "$f")" = "$t" ] && skip=1
    done
    [ "$skip" -eq 1 ] && continue
    awk -v file="$f" '
      function report() { print file ":" NR ": " $0; bad = 1 }
      in_test { if ($0 == indent "}") in_test = 0; next }
      pending {
        pending = 0
        if ($0 ~ ("^" pindent "mod [a-z_0-9]+ \\{$")) { in_test = 1; indent = pindent; next }
        if ($0 ~ ("^" pindent "mod [a-z_0-9]+;$")) next
        print file ":" NR - 1 ": bare #[cfg(test)] on something other than a test module"; bad = 1
      }
      /^[[:space:]]*\/\// { next }
      /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ { pending = 1; pindent = $0; sub(/#.*/, "", pindent); next }
      /cfg!\(test\)/ || /not\(test\)/ { report(); next }
      /cfg!?\(.*[(, ]test[,)]/ && !/feature = "test-support"/ { report(); next }
      END { exit bad }
    ' "$f" || found=1
  done
  return "${found:-0}"
}

declares_modules() {
  for m in sbx kits model models state tokens bindings headless job insight stats remote \
    startdir skill_source app; do
    grep -Eq "^pub mod $m;" "$core/src/lib.rs" || { echo "core/src/lib.rs: no pub mod $m;"; return 1; }
  done
}

package_name() {   # package_name <Cargo.toml> <name>
  section_has "$1" '[package]' "^name[[:space:]]*=[[:space:]]*\"$2\""
}
# The crate's binary is its package name: no [[bin]] that names another.
binary_is_package() {   # binary_is_package <Cargo.toml> <name>
  package_name "$1" "$2" &&
    ! sections "$1" | grep -F '[[bin]]	' | cut -f2- | grep -E '^name[[:space:]]*=' | grep -vq "\"$2\""
}
member() {   # member <name>
  section_has "$root/Cargo.toml" '[workspace]' "\"$1\""
}
one_lock() { [ -f "$root/Cargo.lock" ] && [ ! -e "$tui/Cargo.lock" ] && [ ! -e "$core/Cargo.lock" ]; }
support_off_by_default() {
  section_has "$core/Cargo.toml" '[features]' '^test-support[[:space:]]*=' &&
    ! section_has "$core/Cargo.toml" '[features]' '^default[[:space:]]*=.*test-support'
}
no_tui_manifest_in_docs() { ! grep -q 'manifest-path tui/Cargo.toml' "$root/README.md" "$root"/docs/*.md; }
crate_mentions() {   # crate_mentions <crate dir> <regex>: its Cargo.toml or a source file matches
  grep -Eq "$2" "$1/Cargo.toml" || code_has "$2" "$1/src"
}
gui_launchers() { [ -f "$root/bin/windows/aisbx-ui.cmd" ] && [ -f "$root/bin/windows/aisbx-ui.ps1" ]; }
ps_function() {   # ps_function <file> <name>: the function's lines, up to its closing brace
  awk -v name="$2" '$0 ~ "^function " name "([ ({]|$)" { on = 1 } on { print } on && /^}/ { exit }' "$1"
}
same_source_walk() {   # both Windows launchers decide "stale" with the same functions
  for f in Get-Sources Test-Stale; do
    a=$(ps_function "$root/bin/windows/aisbx.ps1" "$f")
    [ -n "$a" ] && [ "$a" = "$(ps_function "$root/bin/windows/aisbx-ui.ps1" "$f")" ] || return 1
  done
}

# ---- workspace ------------------------------------------------------------------------------

check "root Cargo.toml is a workspace" has '^\[workspace\]' "$root/Cargo.toml"
check "workspace resolver is 2" has '^resolver[[:space:]]*=[[:space:]]*"2"' "$root/Cargo.toml"
check "member core" member core
check "member tui" member tui
check "one Cargo.lock, at the root" one_lock

# ---- core -----------------------------------------------------------------------------------

check "core is package aisbx-core" package_name "$core/Cargo.toml" aisbx-core
check "core's lib is aisbx_core" section_has "$core/Cargo.toml" '[lib]' '^name[[:space:]]*=[[:space:]]*"aisbx_core"'
check "no UI crate in core/Cargo.toml" not has '(ratatui|crossterm|egui|eframe|winit)' "$core/Cargo.toml"
check "no UI crate in core/src" not code_has '(ratatui|crossterm|egui|eframe|winit)' "$core/src"
check "core declares test-support, off by default" support_off_by_default
check "core/src/lib.rs declares the UI-free modules and app" declares_modules
check "no theme:: in core/src" not code_has 'theme::' "$core/src"
check "core's test guards all include test-support" bare_guards

# ---- tui ------------------------------------------------------------------------------------

check "tui builds the binary aisbx" binary_is_package "$tui/Cargo.toml" aisbx
check "tui depends on aisbx-core" has '^aisbx-core[[:space:]]*=' "$tui/Cargo.toml"
check "test-support only in tui's dev-dependencies" support_only_in_dev "$tui/Cargo.toml"
check "tui's dev-dependencies turn test-support on" section_has "$tui/Cargo.toml" '[dev-dependencies]' 'test-support'

# ---- launchers and docs ---------------------------------------------------------------------

check "bin/unix/aisbx parses (sh -n)" sh -n "$root/bin/unix/aisbx"
check "bin/unix/aisbx builds the workspace package" has '\-p aisbx( |$)' "$root/bin/unix/aisbx"
check "bin/windows/aisbx.ps1 builds the workspace package" has '\-p aisbx( |`|$)' "$root/bin/windows/aisbx.ps1"
check "no manifest-path tui/Cargo.toml in README or docs/" no_tui_manifest_in_docs

# ---- gui ------------------------------------------------------------------------------------

check "member gui" member gui
check "gui builds the binary aisbx-ui" binary_is_package "$gui/Cargo.toml" aisbx-ui
check "no ratatui/crossterm in gui" not crate_mentions "$gui" '(ratatui|crossterm)'
check "no egui/eframe in tui" not crate_mentions "$tui" '(egui|eframe)'
check "no Command::new or std::process in gui/src" not code_has '(Command::new|std::process)' "$gui/src"
check "no set_current_dir in gui/src" not code_has 'set_current_dir' "$gui/src"
check "no read_dir in gui/src" not code_has 'read_dir' "$gui/src"
check "no NOT_PORTED / not_ported in gui/src" not code_has '(NOT_PORTED|not_ported)' "$gui/src"
check "gui depends on aisbx-core" has '^aisbx-core[[:space:]]*=' "$gui/Cargo.toml"
check "test-support only in gui's dev-dependencies" support_only_in_dev "$gui/Cargo.toml"
check "gui's dev-dependencies turn test-support on" section_has "$gui/Cargo.toml" '[dev-dependencies]' 'test-support'
check "bin/unix/aisbx-ui parses (sh -n)" sh -n "$root/bin/unix/aisbx-ui"
check "bin/unix/aisbx-ui builds the gui package" has '\-p aisbx-ui( |$)' "$root/bin/unix/aisbx-ui"
check "bin/windows/aisbx-ui.ps1 builds the gui package" has '\-p aisbx-ui( |`|$)' "$root/bin/windows/aisbx-ui.ps1"
check "bin/windows/aisbx-ui.cmd runs aisbx-ui.ps1" has '%~dp0aisbx-ui\.ps1"' "$root/bin/windows/aisbx-ui.cmd"
check "bin/windows/aisbx-ui.cmd and .ps1 exist" gui_launchers
check "bin/windows/aisbx.ps1 and aisbx-ui.ps1 share Get-Sources and Test-Stale" same_source_walk

if [ "$failures" -ne 0 ]; then
  printf '%s check(s) failed\n' "$failures"
  exit 1
fi
printf 'all checks passed\n'
