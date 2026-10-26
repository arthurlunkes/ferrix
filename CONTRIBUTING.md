# Contributing to Ferrix

Thanks for your interest! All help is welcome: bug fixes, new features, documentation, tests or ideas.

## Reporting issues

Open an _issue_ describing:

- what you did (commands, the relevant part of `ferrix.json` — **without tokens or passwords**);
- what you expected to happen;
- what actually happened (error messages, logs);
- your operating system and Rust and Node.js versions.

## Submitting changes

1. Fork the repository and create a branch from `main`:
   `git checkout -b my-improvement`
2. Make your changes following the style of the surrounding code.
3. Make sure everything builds and the tests pass:

   ```bash
   cargo fmt --all
   cargo clippy --workspace
   cargo test --workspace
   (cd backend && npm install && npm run build)
   (cd frontend && npm install && npm run build)
   ```

4. Write clear commit messages (English or Portuguese) explaining _why_ the change was made.
5. Open a _pull request_ describing the problem it solves and how you tested it.

For larger changes, please open an _issue_ first to agree on the approach.

## Versioning & Releases

Ferrix strictly adheres to [Semantic Versioning (SemVer 2.0.0)](https://semver.org/):

- **MAJOR (`x.0.0`)**: Incompatible changes to configuration schemas or APIs.
- **MINOR (`0.x.0`)**: Backwards-compatible new features, directives, or algorithms.
- **PATCH (`0.0.x`)**: Backwards-compatible bug fixes, security patches, and performance improvements.

### Release Process
1. Update `[workspace.package]` in root `Cargo.toml` and frontend/backend `package.json`.
2. Update `CHANGELOG.md` with release notes under `## [X.Y.Z] - YYYY-MM-DD`.
3. Create a git tag matching `vX.Y.Z` (e.g. `git tag -a v0.1.0 -m "Release v0.1.0"`).
4. Push the tag (`git push origin v0.1.0`). GitHub Actions will automatically run CI, build binaries for Linux, macOS, and Windows, bundle frontend assets, and publish the GitHub Release.

## License

By contributing, you agree that your contributions will be licensed under the project's [Apache-2.0 License](LICENSE).
