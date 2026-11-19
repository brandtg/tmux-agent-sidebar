# Multi-version tmux builder for backwards-compatibility probes.
#
# Builds official tmux release tarballs into /opt/tmux-<tag>/bin/tmux so the
# pre-3.8 behavior of format variables, hook names, and command flags can be
# probed per version (see probe.sh in this directory).
#
# Build:  docker build -t tmux-matrix .
# Run:    docker run --rm tmux-matrix /probe.sh
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
COPY probe.sh /probe.sh
RUN chmod +x /probe.sh
# The probes start detached servers; a TERM keeps tmux's client-side happy.
ENV TERM=xterm-256color
CMD ["/probe.sh"]
