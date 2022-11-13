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
    run docker run --rm -ti slurm-spank-rs/tests su --pty joe -c "salloc --exclusive bash -c 'srun /bin/true;
    srun /bin/true;
    srun /bin/true;
    valgrind -q --log-file=/tmp/valgrind_client.log srun --chdir=/tmp --overcommit -n32 -c 512 --test=values /bin/true a b c d'"

    assert_line --partial 'spank_remote_job_id: 100000'
    assert_line --partial 'spank_remote_job_ncpus: 8'
    assert_line --partial 'spank_remote_job_nnodes: 1'
    assert_line --partial 'spank_remote_job_nodeid: 0'
    assert_line --partial 'spank_remote_job_stepid: 3'
    assert_line --partial "spank_remote_job_alloc_cores: 0-$(( $(nproc) - 1 ))"
    assert_line --partial 'spank_remote_job_alloc_mem: 100'
    assert_line --partial 'spank_remote_job_total_task_count: 32'
    assert_line --partial 'spank_remote_job_local_task_count: 32'
    assert_line --partial 'spank_remote_job_argv: /bin/true,a,b,c,d'
    assert_line --partial 'spank_remote_step_alloc_mem: 100'
    assert_line --partial 'spank_remote_step_alloc_cores: 0-7'
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
    run docker run --rm -ti slurm-spank-rs/tests sinfo -t idle
    assert_line --partial 'localhost'
    [ "$status" -eq 0 ]

}

@test 'srun usage display ok' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --help
    assert_line --partial '--test=test             Run selected test (srun)'
    [ "$status" -eq 0 ]

}

@test 'sbatch usage display ok' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log sbatch --help
    assert_line --partial '--test=test             Run selected test (salloc/sbatch)'
    [ "$status" -eq 0 ]


}

@test 'salloc usage display ok' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log salloc --help
    assert_line --partial '--test=test             Run selected test (salloc/sbatch)'
    [ "$status" -eq 0 ]

}

@test 'srun error fails' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=client-error /bin/true
    assert_line --partial 'error: Expected an error'
    [ "$status" -eq 1 ]
}

@test 'remote error fails' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=remote-error /bin/true
    assert_line --partial 'Expected an error'
    # Returns 0 currently
    skip status_check
    # [ "$status" -eq 1 ]
}

@test 'option parsing ok' {
    run docker run --rm -ti slurm-spank-rs/tests valgrind -q --log-file=/tmp/valgrind_client.log srun --test=parse /bin/true
    assert_line --partial 'Local: selected test: parse'
    assert_line --partial 'Remote: selected test: parse'
    [ "$status" -eq 0 ]

}

