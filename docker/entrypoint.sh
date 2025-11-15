#!/bin/bash

mkdir /sys/fs/cgroup/leaf
pids=$(</sys/fs/cgroup/cgroup.procs)
echo "$pids" > /sys/fs/cgroup/leaf/cgroup.procs
echo '+cpuset +cpu +io +memory +pids' > /sys/fs/cgroup/cgroup.subtree_control
mkdir /sys/fs/cgroup/system.slice/
sed -i "s!CPUs=1!CPUs=$(nproc) RealMemory=100!" /etc/slurm/slurm.conf
echo "FirstJobId=100000" >> /etc/slurm/slurm.conf
echo "joe:x:2000:2000:/tmp/:/bin/bash" >> /etc/passwd
echo "ibm:x:4000:joe" >> /etc/group

su -s /bin/sh munge -c munged & slurmctld -D >&/dev/null & valgrind -q --log-file=/tmp/valgrind.log slurmd -D -L /tmp/slurmd.log >&/dev/null &
timeout 10 bash -c "while ! timeout 1 sinfo -t idle | grep -q localhost; do sleep 0.5; done; sinfo > /dev/null" && "$@"
ret=$?
scontrol shutdown
while pidof slurmd; do sleep 1; done

echo
echo "===== debug logs ====="
if [[ -s  /tmp/valgrind.log ]]; then
    echo "Valgrind error"
    cat /tmp/valgrind.log
    exit 99
elif [[ -s  /tmp/valgrind_client.log ]]; then
    echo "Valgrind error"
    cat /tmp/valgrind_client.log
    exit 99
else
    cat /tmp/slurmd.log
    exit $ret
fi
