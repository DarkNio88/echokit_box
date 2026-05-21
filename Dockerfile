FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential clang pkg-config libssl-dev cmake git unzip \
    python3 python3-venv python3-pip python3-distutils libffi-dev libusb-1.0-0-dev wget \
 && rm -rf /var/lib/apt/lists/*

# Install rustup (non-interactive)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH=/root/.cargo/bin:$PATH

# Install prebuilt espup binary if available, otherwise build from source with clang
RUN set -eux; \
    if curl -L -f https://github.com/esp-rs/espup/releases/latest/download/espup-x86_64-unknown-linux-gnu.tar.gz -o /tmp/espup.tar.gz; then \
        mkdir -p /tmp/espupdir; \
        tar -xzf /tmp/espup.tar.gz -C /tmp/espupdir; \
        cp "$(find /tmp/espupdir -type f -name espup | head -n1)" /usr/local/bin/espup; \
        chmod +x /usr/local/bin/espup; \
        rm -rf /tmp/espup.tar.gz /tmp/espupdir; \
    else \
        echo 'prebuilt espup download failed; building espup from source'; \
        CC=clang CXX=clang++ cargo install --locked espup; \
    fi

# Use stable toolchain and make cargo-generate, ldproxy and espflash available
RUN rustup default stable \
 && /root/.cargo/bin/cargo install --locked cargo-generate ldproxy espflash

WORKDIR /workspace

CMD ["/bin/bash"]
