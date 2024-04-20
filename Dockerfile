FROM quay.io/fedora/fedora:40

# SLURM
RUN dnf -y install git jq dnf-utils util-linux dbus-devel munge pmix-devel gcc valgrind procps-ng hwinfo
RUN git clone --depth 1 --branch slurm-23-11-5-1 https://github.com/SchedMD/slurm.git
RUN yum-builddep -y slurm/slurm.spec
RUN cd slurm/ && ./configure --program-prefix= --prefix=/usr --exec-prefix=/usr \
    --bindir=/usr/bin --sbindir=/usr/sbin --sysconfdir=/etc/slurm \
    --datadir=/usr/share --includedir=/usr/include --libdir=/usr/lib64 \
    --libexecdir=/usr/libexec --localstatedir=/var --sharedstatedir=/var/lib \
    --mandir=/usr/share/man --infodir=/usr/share/info --runstatedir=/run  \
    --with-bpf --with-pmix  && make -j  && make install && make clean

RUN echo 0000000000000000000000000000000000000 > /etc/munge/munge.key && \
    chown munge:munge /etc/munge/munge.key && \
    chmod 600 /etc/munge/munge.key && \
    mkdir /run/munge && \
    chown munge:munge /run/munge

RUN useradd slurm
RUN mkdir /etc/slurm
RUN mkdir /var/spool/slurmd && chown slurm:slurm /var/spool/slurmd
RUN mkdir /var/spool/slurmctld && chown slurm:slurm /var/spool/slurmctld

RUN cp /slurm/etc/slurm.conf.example /etc/slurm/slurm.conf
RUN cp /slurm/etc/cgroup.conf.example /etc/slurm/cgroup.conf
RUN sed -i 's!linux0!localhost!' /etc/slurm/slurm.conf
RUN sed -i 's!linux\[1-32\]!localhost!' /etc/slurm/slurm.conf
RUN sed -i 's!.*SelectType=.*!SelectType=select/cons_tres!' /etc/slurm/slurm.conf
RUN sed -i "s!.*SelectTypeParameters=.*!SelectTypeParameters=CR_Core_Memory!" /etc/slurm/slurm.conf
RUN echo IgnoreSystemd=yes >> /etc/slurm/cgroup.conf

# Rust toolset
RUN dnf -y install rust cargo
RUN dnf -y install clang

# Prepare deps
RUN mkdir /build && cd /build && cargo init --lib slurm-spank && find /build/slurm-spank -exec touch -t 200001010000 {} \;
WORKDIR /build/slurm-spank
COPY Cargo.toml build.rs wrapper.h ./
RUN cargo init --lib test_plugin
WORKDIR /build/slurm-spank/test_plugin
COPY test/Cargo.toml ./
RUN cargo build
RUN find .. -exec touch -t  200001010000 {} \;

# Copy sources
COPY example /build/slurm-spank/example
COPY src /build/slurm-spank/src
COPY test/src /build/slurm-spank/test_plugin/src

# Build test lib
RUN cargo build

# Build examples
RUN cd /build/slurm-spank/example/hello && cargo build
RUN cd /build/slurm-spank/example/renice && cargo build

RUN echo required /build/slurm-spank/target/debug/libslurm_spank_tests.so arg1 arg2 >/etc/slurm/plugstack.conf
RUN echo required /build/slurm-spank/example/hello/target/debug/libslurm_spank_hello.so >>/etc/slurm/plugstack.conf
RUN echo required /build/slurm-spank/example/renice/target/debug/libslurm_spank_example.so >>/etc/slurm/plugstack.conf

COPY docker/entrypoint.sh /entrypoint.sh
ENTRYPOINT [ "/entrypoint.sh" ]
