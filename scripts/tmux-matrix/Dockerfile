# Plugin compatibility matrix: build the real tmux-agent-sidebar binary and
# run it against every tmux version the plugin claims to support.
#
# Stages:
#   builder  — rustup on the same ubuntu:22.04 base as the runtime stage, so
#              the release binary's glibc matches (a host-built binary would
#              target the dev machine's newer glibc and fail to exec here).
#   runtime  — tmux release tarballs (3.0a..3.7c + system 3.8 not included;
#              the dev machine covers 3.8) plus the plugin binary and conf.
#
# Build:  docker build -f scripts/tmux-matrix/Dockerfile -t tmux-plugin-matrix .
# Run:    docker run --rm tmux-plugin-matrix
FROM ubuntu:22.04 AS builder
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential curl ca-certificates pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# include_str! sources referenced by src/cli/plugin_state.rs
COPY hook.sh ./hook.sh
COPY hooks ./hooks
RUN cargo build --release

FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential curl ca-certificates pkg-config bison xz-utils \
        libevent-dev libncurses-dev \
    && rm -rf /var/lib/apt/lists/*
ARG VERSIONS="3.0a 3.2a 3.3a 3.4 3.5a 3.6b 3.7c"
RUN set -eux; \
    for v in $VERSIONS; do \
        curl -fsSL "https://github.com/tmux/tmux/releases/download/$v/tmux-$v.tar.gz" \
            -o /tmp/tmux.tar.gz; \
        mkdir -p /tmp/src; \
        tar -xzf /tmp/tmux.tar.gz -C /tmp/src --strip-components=1; \
        (cd /tmp/src && ./configure --prefix="/opt/tmux-$v" >/dev/null \
            && make -j"$(nproc)" >/dev/null && make install >/dev/null); \
        rm -rf /tmp/src /tmp/tmux.tar.gz; \
    done
COPY --from=builder /build/target/release/tmux-agent-sidebar /usr/local/bin/tmux-agent-sidebar
COPY agent-sidebar.conf /opt/plugin/agent-sidebar.conf
COPY scripts/tmux-matrix/probe.sh /probe.sh
COPY scripts/tmux-matrix/plugin-probe.sh /plugin-probe.sh
RUN chmod +x /plugin-probe.sh
ENV TERM=xterm-256color
CMD ["/plugin-probe.sh"]
