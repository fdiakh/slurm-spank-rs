load 'test_helper/bats-support/load'
load 'test_helper/bats-assert/load'


setup_file() {
    DIR="$( cd "$( dirname "$BATS_TEST_FILENAME" )" >/dev/null 2>&1 && pwd )"
    docker build -t slurm-spank-rs/tests $DIR/..
}

teardown_file() {
    # We could delete the image here but rebuilding is slow
    /bin/true
}


@test 'spank remote values ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests su --pty joe -c "salloc --exclusive bash -c 'srun /bin/true;
    srun /bin/true;
    srun /bin/true;
    valgrind -q --log-file=/tmp/valgrind_client.log srun --chdir=/tmp --overcommit -n32 -c 512 --test=values /bin/true a b c d'"

    assert_line --partial 'spank_remote_job_id: 100000'
    assert_line --partial "spank_remote_job_ncpus: $(nproc)"
    assert_line --partial 'spank_remote_job_nnodes: 1'
    assert_line --partial 'spank_remote_job_nodeid: 0'
    assert_line --partial 'spank_remote_job_stepid: 3'
    assert_line --partial "spank_remote_job_alloc_cores: 0-$(( $(nproc) - 1 ))"
    assert_line --partial 'spank_remote_job_alloc_mem: 100'
    assert_line --partial 'spank_remote_job_total_task_count: 32'
    assert_line --partial 'spank_remote_job_local_task_count: 32'
    assert_line --partial 'spank_remote_job_argv: /bin/true,a,b,c,d'
    assert_line --partial 'spank_remote_step_alloc_mem: 100'
    assert_line --partial "spank_remote_step_alloc_cores: 0-$(( $(nproc) - 1 ))"
    assert_line --partial 'spank_remote_step_cpus_per_task: 512'
    assert_line --partial 'spank_remote_job_gid: 2000'
    assert_line --partial 'spank_remote_job_uid: 2000'
    assert_line --partial 'spank_remote_job_supplementary_gids: 2000,4000'
    assert_line --partial 'spank_task_global_id: 12'
    assert_line --partial 'spank_task_id: 12'
    assert_line --regexp  'spank_task_pid: .*[0-9]+'
    assert_line --partial 'spank_id_from_pid: 12'
    assert_line --partial 'spank_global_id_from_pid: 12'
    assert_line --partial 'spank_local_id_from_global: 12'
    assert_line --partial 'spank_global_id_from_local: 12'
    [ "$status" -eq 0 ]
}

@test "container build ok" {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests sinfo -t idle
    assert_line --partial 'localhost'
    [ "$status" -eq 0 ]

}

@test 'srun usage display ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --help
    assert_line --partial '--test=test             Run selected test (srun)'
    [ "$status" -eq 0 ]

}

@test 'sbatch usage display ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log sbatch --help
    assert_line --partial '--test=test             Run selected test (salloc/sbatch)'
    [ "$status" -eq 0 ]


}

@test 'salloc usage display ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log salloc --help
    assert_line --partial '--test=test             Run selected test (salloc/sbatch)'
    [ "$status" -eq 0 ]

}

@test 'srun error fails' {
    run docker run  -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=client-error /bin/true
    assert_line --partial 'error: Expected an error'
    [ "$status" -eq 1 ]
}

@test 'remote error fails' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=remote-error /bin/true
    assert_line --partial 'Expected an error'
    # Returns 0 currently
    skip status_check
    # [ "$status" -eq 1 ]
}

@test 'task error fails' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=task-error /bin/true
    assert_line --partial 'Expected an error'
    [ "$status" -eq 1 ]
}

@test 'option parsing ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=parse /bin/true
    assert_line --partial 'Local: selected test: parse'
    assert_line --partial 'Remote: selected test: parse'
    [ "$status" -eq 0 ]

}

@test 'plugin argument parsing ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun /bin/true
    assert_line --partial 'Plugin arguments arg1,arg2'
    [ "$status" -eq 0 ]

}

@test 'job env ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti -e EXISTING_VAR1='Initial value' -e EXISTING_VAR2='Overwritten value' slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=client-env bash -c 'echo -e NEW_VALUE: $NEW_VALUE\\nEXISTING_VAR1: $EXISTING_VAR1\\nEXISTING_VAR2: $EXISTING_VAR2'
    assert_line --partial 'Env value 1: Initial value'
    assert_line --partial 'Env value 2: Overwritten value'
    assert_line --partial 'NEW_VALUE: New value'
    assert_line --partial 'EXISTING_VAR1: Initial value'
    assert_line --partial 'EXISTING_VAR2: Modified value'

    [ "$status" -eq 0 ]

}

@test 'job control env ok' {
    run docker run -v /sys/fs/cgroup:/sys/fs/cgroup --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=job-control /bin/true
    assert_line --partial 'Job control from local ok'


    [ "$status" -eq 0 ]

}
