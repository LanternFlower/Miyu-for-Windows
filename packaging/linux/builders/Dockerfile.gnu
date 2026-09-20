# 构建基座 = 支持下限：Ubuntu 24.04 LTS 的 glibc 2.39。在更新的发行版上
# 构建会让二进制引用更高版本的符号，装到 24.04 上直接起不来。
FROM ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3
LABEL io.miyu.distribution.owner="distribution-2026-09-14"
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential clang cmake pkg-config libasound2-dev \
    libssl-dev python3 git binutils xz-utils bzip2 zstd \
    && rm -rf /var/lib/apt/lists/*
ENV CARGO_HOME=/opt/cargo RUSTUP_HOME=/opt/rustup
ENV PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ARG RUST_VERSION=1.96.1
RUN curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain ${RUST_VERSION} \
    && rm /tmp/rustup.sh \
    && rustc --version \
    && dpkg-query -W > /opt/builder-packages.txt
WORKDIR /build
