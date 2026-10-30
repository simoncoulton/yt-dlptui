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
      sha256 "7064fa4fd67ef31d6cfe36e7749189b9b72f7c820cf521842cd8b0d4db75950b"
    end
    on_intel do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "710f8566631e3916ad21a8968541571e6b5c020f3e8c0853edc631c1b553ad50"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c42114f405f5c8f23df2e29b82d7be58791e12068afa055b583b36a037f113f1"
    end
    on_intel do
      url "https://github.com/simoncoulton/yt-dlptui/releases/download/v0.1.0/yt-dlptui-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ab0045a08c83fd596d52cf96209d2403f75c78046eb391f927c000a9422d17bb"
    end
  end

  def install
    bin.install "yt-dlptui"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/yt-dlptui --version")
  end
end
