#!/bin/bash

sed -i "s!CPUs=1!CPUs=$(nproc) RealMemory=100!" /etc/slurm/slurm.conf
sed -i "s!SelectType=.*!SelectType=select/cons_res!" /etc/slurm/slurm.conf
sed -i "s!#SelectTypeParameters=!SelectTypeParameters=CR_Core_Memory!" /etc/slurm/slurm.conf
echo "FirstJobId=100000" >> /etc/slurm/slurm.conf
echo "joe:x:2000:2000:/tmp/:/bin/bash" >> /etc/passwd
echo "ibm:x:4000:joe" >> /etc/group

mkdir /sys/fs/cgroup/leaf
pids=$(</sys/fs/cgroup/cgroup.procs)
echo "$pids" > /sys/fs/cgroup/leaf/cgroup.procs
mkdir /sys/fs/cgroup/system.slice

su -s /bin/sh munge -c munged & slurmctld -D >&/dev/null & valgrind -q --log-file=/tmp/valgrind.log  slurmd -D -L /tmp/slurmd.log >&/dev/null &
timeout 5 bash -c "while ! sinfo -t idle 2>/dev/null | grep -q localhost; do sleep 0.5; done" && "$@"
ret=$?
scontrol shutdown
while pidof slurmd; do sleep 1; echo retry; done

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
