#!/bin/bash

su -s /bin/sh munge -c munged & slurmctld -D >&/dev/null & slurmd -D >&/dev/null & srun /bin/true > /dev/null
exec "$@"
