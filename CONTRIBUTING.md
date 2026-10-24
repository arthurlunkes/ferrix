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

## License

By contributing, you agree that your contributions will be licensed under the project's [MIT License](LICENSE).
