class Ferrix < Formula
  desc "High-performance web server and reverse proxy written in Rust, inspired by nginx"
  homepage "https://github.com/arthurlunkes/ferrix"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.1/ferrix-v0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "b9d7a8d1900171ff20603087b2ebb9b4e910ef9b323e5fb6689d43b25eeaa293"
    end
    on_intel do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.1/ferrix-v0.2.1-x86_64-apple-darwin.tar.gz"
      sha256 "75e3d259f5d3fb5a3ac1510122555eafe8ace73fab3da2acbe61c565c9f362f7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.1/ferrix-v0.2.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "a6ca12792afabf08b62847d846123bbfbb7bc11aebde46653c1d18f55634b42b"
    end
    on_intel do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.1/ferrix-v0.2.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "fb867148ec686da501621c098334e254733671ca2f75aeb8ecdee9ce35641bb1"
    end
  end

  def install
    bin.install "ferrix"
    (pkgshare/"dashboard").install Dir["ui/*"]
    doc.install "ferrix.example.yaml"
  end

  def post_install
    (etc/"ferrix").mkpath
  end

  service do
    run [opt_bin/"ferrix", "run", "--config", etc/"ferrix/ferrix.yaml"]
    environment_variables FERRIX_SUPERVISOR: "0"
    keep_alive true
    log_path var/"log/ferrix.log"
    error_log_path var/"log/ferrix.log"
  end

  def caveats
    <<~EOS
      Start Ferrix in the background with:
        brew services start ferrix
      Its configuration is written on the first start to:
        #{etc}/ferrix/ferrix.yaml
      Run it by hand with:
        ferrix run --config #{etc}/ferrix/ferrix.yaml
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ferrix --version")
  end
end
