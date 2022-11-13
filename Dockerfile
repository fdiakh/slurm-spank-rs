FROM quay.io/fedora/fedora:36

# SLURM
# RUN dnf config-manager --set-enabled powertools
# RUN dnf -y install epel-release
RUN dnf -y install slurm slurm-slurmd slurm-slurmctld util-linux
RUN echo 0000000000000000000000000000000000000 > /etc/munge/munge.key && \
    chown munge:munge /etc/munge/munge.key && \
    chmod 600 /etc/munge/munge.key && \
    mkdir /run/munge && \
    chown munge:munge /run/munge

# Easier to run non-privileged without cgroups
RUN sed -i 's!proctrack/cgroup!proctrack/pgid!' /etc/slurm/slurm.conf

# Rust toolset
RUN dnf -y install rust cargo
RUN dnf -y install clang

# Prepare deps
RUN dnf -y install slurm-devel
RUN mkdir /build && cd /build && cargo init --lib slurm-spank && find /build/slurm-spank -exec touch -t 200001010000 {} \;
WORKDIR /build/slurm-spank
COPY Cargo.toml build.rs wrapper.h ./
RUN cargo init --lib test_plugin
WORKDIR /build/slurm-spank/test_plugin
COPY test/Cargo.toml ./
RUN cargo build
RUN find . -exec touch -t  200001010000 {} \;

# Copy sources
COPY src /build/slurm-spank/src
COPY test/src /build/slurm-spank/test_plugin/src

# Build lib
RUN cargo build
RUN echo required /build/slurm-spank/test_plugin/target/debug/libslurm_spank_tests.so arg1 arg2>/etc/slurm/plugstack.conf


RUN dnf install -y valgrind procps-ng hwinfo
COPY docker/entrypoint.sh /entrypoint.sh

ENTRYPOINT [ "/entrypoint.sh" ]
