FROM quay.io/centos/centos:stream8

# SLURM
RUN dnf config-manager --set-enabled powertools
RUN dnf -y install epel-release
RUN dnf -y install slurm slurm-slurmd slurm-slurmctld
RUN echo 0000000000000000000000000000000000000 > /etc/munge/munge.key && \
    chown munge:munge /etc/munge/munge.key && \
    chmod 600 /etc/munge/munge.key
COPY docker/entrypoint.sh /entrypoint.sh

# Rust toolset
RUN dnf module -y install rust-toolset:rhel8
RUN dnf -y module install llvm-toolset

# Prepare deps
RUN dnf -y install slurm-devel
RUN mkdir /build && cd /build && cargo init --lib slurm-spank && find /build/slurm-spank -exec touch -t 200001010000 {} \;
WORKDIR /build/slurm-spank
COPY Cargo.toml Cargo.lock  build.rs wrapper.h ./
RUN cargo init --lib tests
WORKDIR /build/slurm-spank/tests
COPY tests/Cargo.toml tests/Cargo.lock ./
RUN cargo build
RUN find . -exec touch -t  200001010000 {} \;

# Copy sources
COPY src /build/slurm-spank/src
COPY tests/src /build/slurm-spank/tests/src

# Build lib
RUN cargo build
RUN echo required /build/slurm-spank/tests/target/debug/libslurm_spank_example.so > /etc/slurm/plugstack.conf


ENTRYPOINT [ "/entrypoint.sh" ]
