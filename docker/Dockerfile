FROM quay.io/centos/centos:stream8

# SLURM
RUN dnf config-manager --set-enabled powertools
RUN dnf -y install epel-release
RUN dnf -y install slurm slurm-slurmd slurm-slurmctld
RUN echo 0000000000000000000000000000000000000 > /etc/munge/munge.key && \
    chown munge:munge /etc/munge/munge.key && \
    chmod 600 /etc/munge/munge.key
COPY entrypoint.sh /entrypoint.sh

# Rust toolset
dnf module -y install rust-toolset:rhel8

# Prepare deps
RUN mkdir /src && cd /src && cargo init slurm-spank
WORKDIR /src/slurm-spank
COPY Cargo.toml .
COPY Cargo.lock .
RUN cargo build
COPY ./src
COPY ./tests



ENTRYPOINT [ "/entrypoint.sh" ]
