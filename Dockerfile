# Alouette for several people over SSH.
#
#   docker build -t alouette .
#   docker run -d --name alouette -p 2222:2222 -v alouette:/data alouette
#   docker exec alouette alouette serve add-user ann --github ann-on-github
#   ssh -p 2222 ann@localhost
#
# Everything the server keeps (host key, users, their settings and data) is in /data.
# Settings for the server itself go in /data/alouette.toml, under [serve].

FROM rust:1.96-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang make perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --profile serve -p alouette \
    && cp target/serve/alouette /usr/local/bin/alouette

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin alouette \
    && mkdir /data && chown alouette:alouette /data
COPY --from=build /usr/local/bin/alouette /usr/local/bin/alouette
USER alouette
ENV ALOUETTE_HOME=/data \
    ALOUETTE_CONFIG=/data/alouette.toml \
    ALOUETTE_SERVE_ROOT=/data/serve
VOLUME /data
EXPOSE 2222
ENTRYPOINT ["alouette"]
CMD ["serve", "--listen", "0.0.0.0:2222"]
