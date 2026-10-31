#!/bin/sh
# Prints the Homebrew formula for a fussycat release: its prebuilt archives,
# pinned by the release's SHA256SUMS. release.yml runs it to update
# reccles/homebrew-vault; run it by hand to see or fix what a release wrote.
#
#   scripts/homebrew-formula.sh v0.1.0 SHA256SUMS > Formula/fussycat.rb
set -eu
[ $# -eq 2 ] || { echo "usage: $0 TAG SHA256SUMS" >&2; exit 2; }
tag=$1
sums=$2
case $tag in v[0-9]*) ;; *) echo "homebrew-formula: $tag isn't a vX.Y.Z tag" >&2; exit 2 ;; esac
base="https://github.com/reccles/fussycat/releases/download/$tag"

# The url and sha256 lines for one target's archive, indented for its block.
archive() {
  file="fussycat-$tag-$1.tar.gz"
  sha=$(awk -v f="$file" '$2 == f { print $1 }' "$sums")
  [ -n "$sha" ] || { echo "homebrew-formula: $file isn't in $sums" >&2; exit 1; }
  printf '      url "%s/%s"\n      sha256 "%s"\n' "$base" "$file" "$sha"
}
mac_arm=$(archive aarch64-apple-darwin)
linux_intel=$(archive x86_64-unknown-linux-musl)
linux_arm=$(archive aarch64-unknown-linux-musl)

cat <<EOF
# Written by fussycat's release workflow (scripts/homebrew-formula.sh):
# edit the script, not this file.
class Fussycat < Formula
  desc "AI agent that behaves like a Unix tool: task on stdin, answer on stdout"
  homepage "https://github.com/reccles/fussycat"
  version "${tag#v}"
  license "MIT"

  on_macos do
    depends_on arch: :arm64
    on_arm do
$mac_arm
    end
  end

  on_linux do
    on_intel do
$linux_intel
    end
    on_arm do
$linux_arm
    end
  end

  def install
    bin.install "fussycat"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/fussycat --version")
  end
end
EOF
