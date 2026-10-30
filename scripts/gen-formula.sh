#!/usr/bin/env bash
# Usage: gen-formula.sh <version> <sha-aarch64-apple-darwin> <sha-x86_64-apple-darwin> <sha-aarch64-unknown-linux-gnu> <sha-x86_64-unknown-linux-gnu>
# Writes Formula/yt-dlptui.rb pointing at the binaries attached to the GitHub release.
set -euo pipefail
version=$1
base="https://github.com/simoncoulton/yt-dlptui/releases/download/v${version}/yt-dlptui-v${version}"

cat > "$(dirname "$0")/../Formula/yt-dlptui.rb" <<RUBY
class YtDlptui < Formula
  desc "Terminal UI for yt-dlp: pick a format, then download"
  homepage "https://github.com/simoncoulton/yt-dlptui"
  version "${version}"
  license "MIT"

  depends_on "ffmpeg"
  depends_on "yt-dlp"

  on_macos do
    on_arm do
      url "${base}-aarch64-apple-darwin.tar.gz"
      sha256 "${2}"
    end
    on_intel do
      url "${base}-x86_64-apple-darwin.tar.gz"
      sha256 "${3}"
    end
  end

  on_linux do
    on_arm do
      url "${base}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${4}"
    end
    on_intel do
      url "${base}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${5}"
    end
  end

  def install
    bin.install "yt-dlptui"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/yt-dlptui --version")
  end
end
RUBY
