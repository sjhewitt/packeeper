# PAC Keeper in a container. The records live in the /data volume.
#
#   docker build -t pac-keeper .
#   docker run -p 8080:8080 -v pac-data:/data -e PAC_AUTH=1 pac-keeper    # a real PAC, with sign-in
#   docker run -p 8080:8080 -e PAC_DEMO=1 pac-keeper                      # the public demo
#   docker run -p 8080:8080 -v pac-hosted:/data -e PAC_HOSTED=1 \
#     -e PAC_BASE_URL=https://app.packeeper.ca -e PAC_SMTP_URL=... -e PAC_MAIL_FROM=... pac-keeper   # many PACs

FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY migrations migrations
COPY migrations_control migrations_control
COPY templates templates
COPY static static
COPY typst typst
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /data pac \
    && mkdir /data && chown pac /data
COPY --from=build /src/target/release/pac-keeper /usr/local/bin/pac-keeper
USER pac
VOLUME /data
ENV PAC_DATA_DIR=/data PAC_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["pac-keeper"]
CMD ["serve"]
