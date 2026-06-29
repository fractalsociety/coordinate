class Squad < Formula
  desc "Multi-AI-agent terminal collaboration via simple CLI commands"
  homepage "https://github.com/fractalsociety/coordinate"
  version "0.3.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end

    on_intel do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "squad"
  end

  test do
    assert_match "Multi-AI-agent terminal collaboration", shell_output("#{bin}/squad help")
  end
end
