class YtDlptui < Formula
  desc "Terminal UI for yt-dlp: pick a format, then download"
  homepage "https://github.com/simoncoulton/yt-dlptui"
  version "0.1.0"
  license "MIT"

  depends_on "ffmpeg"
  depends_on "yt-dlp"

  on_macos do
    on_arm do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "yt-dlptui"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/yt-dlptui --version")
  end
end
