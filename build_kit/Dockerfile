FROM ghcr.io/make87/rust:1-bookworm AS build-image

RUN apt-get update \
    && apt install --no-install-suggests --no-install-recommends -y \
    jq \
    build-essential \
    libclang-dev \
    pkg-config\
    cmake \
    nasm \
    libssl-dev \
    libavformat-dev \
    libavcodec-dev \
    libavutil-dev \
    libswscale-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml .
COPY src ./src

RUN cargo build --release $CARGO_BUILD_FLAGS

RUN binary_name=$(cargo metadata --format-version=1 --no-deps | \
    jq -r '.packages[].targets[] | select(.kind[] == "bin") | .name') \
    && echo "Binary name: $binary_name" \
    && cp target/release/$binary_name /main

RUN mkdir -p /tmp/libs && \
    ldd /main | awk '{print $3}' | grep '^/' | xargs -I '{}' cp --parents '{}' /tmp/libs

FROM ghcr.io/make87/cc-debian12:latest

WORKDIR /app

# Copy the workspace from the builders
COPY --from=build-image /main /app/main
COPY --from=build-image /tmp/libs /

CMD ["/app/main"]
