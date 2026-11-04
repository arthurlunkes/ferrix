class Ferrix < Formula
  desc "High-performance web server and reverse proxy written in Rust, inspired by nginx"
  homepage "https://github.com/arthurlunkes/ferrix"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.0/ferrix-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "974d683f5e6d11cb93b85a3f4a919661ea41a98140cc38fc2b60ba85eb4fd0ac"
    end
    on_intel do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.0/ferrix-v0.2.0-x86_64-apple-darwin.tar.gz"
      sha256 "9815773f67aa628dd317295b4aeb53bd2182515c6c919aa98d860e1beb22ca12"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.0/ferrix-v0.2.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f8e83065fa9d69eb652a98558f424f97701c78110267377f0173c3078771e6fd"
    end
    on_intel do
      url "https://github.com/arthurlunkes/ferrix/releases/download/v0.2.0/ferrix-v0.2.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "7d2f62b6fde4b681b24ddad0c756226d999066c6e8c9f607ce76a745da63495f"
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
