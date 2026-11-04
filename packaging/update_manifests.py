"""Write the Homebrew formula and the Scoop manifest for a release.

    python3 packaging/update_manifests.py 0.2.1 path/to/SHA256SUMS
"""
import json
import sys
from pathlib import Path

REPO = "arthurlunkes/ferrix"
ROOT = Path(__file__).resolve().parent.parent


def main(version, sums_path):
    sums = {}
    for line in Path(sums_path).read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        sums[name.lstrip("*")] = digest
    base = f"https://github.com/{REPO}/releases/download/v{version}"

    def asset(triple, ext="tar.gz"):
        name = f"ferrix-v{version}-{triple}.{ext}"
        return f"{base}/{name}", sums[name]

    mac_arm, mac_intel = asset("aarch64-apple-darwin"), asset("x86_64-apple-darwin")
    linux_arm, linux_intel = asset("aarch64-unknown-linux-musl"), asset("x86_64-unknown-linux-musl")
    formula = f'''class Ferrix < Formula
  desc "High-performance web server and reverse proxy written in Rust, inspired by nginx"
  homepage "https://github.com/{REPO}"
  license "MIT"

  on_macos do
    on_arm do
      url "{mac_arm[0]}"
      sha256 "{mac_arm[1]}"
    end
    on_intel do
      url "{mac_intel[0]}"
      sha256 "{mac_intel[1]}"
    end
  end

  on_linux do
    on_arm do
      url "{linux_arm[0]}"
      sha256 "{linux_arm[1]}"
    end
    on_intel do
      url "{linux_intel[0]}"
      sha256 "{linux_intel[1]}"
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
        #{{etc}}/ferrix/ferrix.yaml
      Run it by hand with:
        ferrix run --config #{{etc}}/ferrix/ferrix.yaml
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/ferrix --version")
  end
end
'''
    (ROOT / "Formula" / "ferrix.rb").write_text(formula)

    win = asset("x86_64-pc-windows-msvc", "zip")
    folder = f"ferrix-v{version}-x86_64-pc-windows-msvc"
    scoop = {
        "version": version,
        "description": "High-performance web server and reverse proxy written in Rust, inspired by nginx",
        "homepage": f"https://github.com/{REPO}",
        "license": "MIT",
        "architecture": {"64bit": {"url": win[0], "hash": win[1], "extract_dir": folder}},
        "bin": "ferrix.exe",
        "checkver": "github",
        "autoupdate": {
            "architecture": {
                "64bit": {
                    "url": f"https://github.com/{REPO}/releases/download/v$version/ferrix-v$version-x86_64-pc-windows-msvc.zip",
                    "extract_dir": "ferrix-v$version-x86_64-pc-windows-msvc",
                }
            },
            "hash": {"url": f"https://github.com/{REPO}/releases/download/v$version/SHA256SUMS"},
        },
    }
    (ROOT / "packaging" / "scoop" / "ferrix.json").write_text(json.dumps(scoop, indent=4) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
