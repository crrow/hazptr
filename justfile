fmt:
    @cargo +nightly fmt --all

check:
    @cargo +nightly check --workspace
    @cargo clippy

build-hello:
    @cargo +stable build --bin hello

cloc:
    @cloc . --exclude-dir=target

test:
    @cargo +stable test --workspace