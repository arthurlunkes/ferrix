# Packaging and releases

How Ferrix reaches users' machines, and what a maintainer needs to know to keep it working.

## Releasing a version

1. Change `version` in the root `Cargo.toml` (for example `0.2.0` → `0.3.0`) and push to `main`.
2. The **Release** workflow sees a version with no tag yet and:
   - builds the binaries: static Linux (musl) for x86_64 and arm64, macOS and Windows;
   - builds the Linux packages from the static binaries with [nfpm](https://nfpm.goreleaser.com/) (`nfpm.yaml`): `.deb`, `.rpm` and Arch `.pkg.tar.zst`, for amd64 and arm64;
   - publishes the GitHub release `vX.Y.Z` with every file and a `SHA256SUMS`, the `.rpm` signed with the APT key;
   - commits the Homebrew formula and the Scoop manifest for the new version.
3. When the release finishes, the **Deploy Documentation** workflow rebuilds the site at <https://arthurlunkes.github.io/ferrix>: the documentation, `/install.sh` (this folder's `install.sh`) and the APT repository in `/apt`, made from the release's `.deb` files and signed.

## Files here

| File | Purpose |
| --- | --- |
| `install.sh` | The `curl … \| sh` installer; picks APT, rpm, the Arch package or the plain binary |
| `nfpm.yaml` | Package definition for deb, rpm and Arch |
| `ferrix.service` | systemd unit installed by the packages (runs as the `ferrix` user) |
| `scripts/` | Package install/remove hooks: create the `ferrix` user, start or stop the service |
| `ferrix-archive-keyring.asc` | Public key of the APT repository |
| `aur/` | `ferrix-bin` for the AUR (not published yet) |
| `scoop/ferrix.json` | Scoop manifest (Windows) |
| `update_manifests.py` | Rewrites `scoop/ferrix.json` and `../Formula/ferrix.rb` (Homebrew) from a release's `SHA256SUMS`; the Release workflow runs it and commits the result |

To build the packages locally, put a static binary in `packaging/build/ferrix` and the built dashboard in `packaging/build/dashboard/`, then run for example `VERSION=0.2.0 ARCH=amd64 nfpm package --config packaging/nfpm.yaml --packager deb`.

## The APT signing key

- Fingerprint: `8D01 6191 3151 3806 7A8A  5C5B 4ABD 5DC1 460A DD45` ("Ferrix Packages").
- The private key is the `APT_GPG_PRIVATE_KEY` secret of this repository (`APT_GPG_KEY_ID` holds the fingerprint). Nobody can read a secret back, so the maintainer keeps a backup outside GitHub, together with the key's revocation certificate.
- Users trust exactly this key: every machine that installed Ferrix through APT has it in `/usr/share/keyrings/ferrix-archive-keyring.gpg`.

**Rotating it.** While the old key is still available, sign the repository with both keys for a while and ship the new public key in a package update; users then switch without doing anything. **If the private key is lost**, the repository has to be signed with a new key, and every user must download the new keyring once (the same `curl` line as in the installation guide) before `apt update` works again. Keep the backup.

## Publishing on the AUR

Once the AUR account exists: clone `ssh://aur@aur.archlinux.org/ferrix-bin.git`, copy the files from `aur/`, set `pkgver`, run `updpkgsums` and `makepkg --printsrcinfo > .SRCINFO`, check that `makepkg` builds, then commit and push.
