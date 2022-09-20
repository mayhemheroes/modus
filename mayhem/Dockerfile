# Build Stage
FROM ghcr.io/evanrichter/cargo-fuzz:latest as builder

## Add source code to the build stage.
ADD . /src
WORKDIR /src

RUN echo building instrumented harnesses && \
    bash -c "pushd modus-lib/fuzz && cargo +nightly -Z sparse-registry fuzz build && popd" && \
    mv modus-lib/fuzz/target/x86_64-unknown-linux-gnu/release/modusfile /modusfile && \
    mv modus-lib/fuzz/target/x86_64-unknown-linux-gnu/release/dockerfile /dockerfile && \
    echo done

RUN echo building non-instrumented harnesses && \
    export RUSTFLAGS="--cfg fuzzing -Clink-dead-code -Cdebug-assertions -C codegen-units=1" && \
    bash -c "pushd modus-lib/fuzz && cargo +nightly -Z sparse-registry build --release && popd" && \
    mv modus-lib/fuzz/target/release/modusfile /modusfile_no_inst && \
    mv modus-lib/fuzz/target/release/dockerfile /dockerfile_no_inst && \
    echo done

# Package Stage
FROM rustlang/rust:nightly

COPY --from=builder /modusfile /modusfile_no_inst /
COPY --from=builder /dockerfile /dockerfile_no_inst /
