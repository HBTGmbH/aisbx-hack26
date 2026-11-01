# Contributing

Bug reports, questions and pull requests are welcome.

## Report a bug

Open a [GitHub issue](https://github.com/ilja82/aisbx/issues/new/choose). Give the output of `sbx version`,
your platform, the backend and addons, and the steps that show the problem. Remove keys, tokens and gateway
URLs from logs before you paste them.

To report a security problem, do not open an issue. Follow [SECURITY.md](SECURITY.md).

## Send a pull request

1. For a large change, open an issue first, so that we can agree on the approach.
2. Fork the repository and make one change per pull request.
3. Build and test as [Development](docs/development.md#build-and-test) describes. Each pull request must pass:

   ```console
   $ cargo fmt --all --check
   $ cargo clippy --workspace --all-targets -- -D warnings
   $ cargo test --workspace
   $ sh tests/workspace/layout-test.sh
   ```

   If you change a kit, also run its test under `tests/kit-hooks/`, on Linux.
4. Add or change the tests for each change in behavior.
5. Update the user pages in `README.md` and `docs/` when the behavior that users see changes.
   `cargo test` checks every link and anchor in them.

CI runs these checks on Linux and macOS, and builds on Windows.

## License

By contributing, you agree that your contribution is licensed under the [MIT License](LICENSE).
